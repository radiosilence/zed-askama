//! Running the generator outside a procedural macro, as an editor's language server does.
//!
//! A compiler gives the generator three things that a library caller must supply instead:
//! the manifest directory, the file that holds the derive, and the template files as they
//! are now, which in an editor may be unsaved. The caller supplies them through [`Host`].
//!
//! Inside a compiler only an inline `source` gets template locations, and templates read
//! from files get them only on nightly. Here every template file is given a located literal
//! in proc-macro2's fallback source map (which needs its `span-locations` feature), so each
//! emitted token's span says where in which file it came from: [`Expansion::sources`] says
//! which range of proc-macro2's span space each file occupies.
//!
//! While a host is set, the process-wide caches of template files and parsed templates are
//! bypassed, since an editor changes files that a compiler treats as fixed, and a cache
//! keyed by content would grow with every edit.

#[cfg(feature = "tooling")]
pub use imp::{Expansion, Host, expand};
pub(crate) use imp::{active, caller_dir, manifest_dir, read, source_span};

#[cfg(feature = "tooling")]
mod imp {
    use std::cell::RefCell;
    use std::io;
    use std::ops::Range;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;

    use proc_macro2::{Literal, TokenStream};

    use crate::spans::SourceSpan;

    /// What the generator would otherwise take from the compiler and the file system.
    pub trait Host {
        /// The directory holding the expanding crate's `Cargo.toml`, as `CARGO_MANIFEST_DIR`
        /// would give it.
        fn manifest_dir(&self) -> &Path;

        /// The Rust file that holds the derive, as the compiler's call-site span would give it.
        fn caller_file(&self) -> Option<&Path>;

        /// A file's current contents: a template, or the configuration file.
        fn read(&self, path: &Path) -> io::Result<String>;
    }

    /// One expansion, and where its template files sit in proc-macro2's span space.
    pub struct Expansion {
        /// What the derive expands to, as [`crate::derive_template`] returns it.
        pub tokens: TokenStream,
        /// Each template file read for the expansion, with the range of proc-macro2's span
        /// space its text occupies: a token whose `span().byte_range()` starts at `s` inside
        /// `range` came from byte `s - range.start` of that file. An inline `source` is not
        /// listed, since its literal is part of the input the caller parsed.
        pub sources: Vec<(Arc<Path>, Range<usize>)>,
    }

    struct State {
        host: Rc<dyn Host>,
        sources: Vec<(Arc<Path>, Range<usize>)>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    /// Clears the host when an expansion ends, also when the generator panics, so a caller
    /// that catches the panic can go on using the thread.
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            STATE.with(|state| state.borrow_mut().take());
        }
    }

    /// Expands `input`, an item deriving `Template`, as [`crate::derive_template`] does, with
    /// everything it reads coming from `host`.
    ///
    /// The returned spans live in this thread's proc-macro2 source map. Read what you need
    /// from them, then call `proc_macro2::extra::invalidate_current_thread_spans` to free it.
    pub fn expand(
        input: TokenStream,
        import_askama: fn() -> TokenStream,
        host: Rc<dyn Host>,
    ) -> Expansion {
        let reset = Reset;
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                host,
                sources: Vec::new(),
            });
        });
        let tokens = crate::derive_template(input, import_askama);
        let sources = STATE.with(|state| {
            state
                .borrow_mut()
                .as_mut()
                .map(|state| std::mem::take(&mut state.sources))
                .unwrap_or_default()
        });
        drop(reset);
        Expansion { tokens, sources }
    }

    pub(crate) fn active() -> bool {
        STATE.with(|state| state.borrow().is_some())
    }

    pub(crate) fn manifest_dir() -> Option<PathBuf> {
        STATE.with(|state| Some(state.borrow().as_ref()?.host.manifest_dir().to_owned()))
    }

    pub(crate) fn caller_dir() -> Option<PathBuf> {
        STATE.with(|state| {
            let state = state.borrow();
            let file = state.as_ref()?.host.caller_file()?;
            Some(file.parent()?.to_owned())
        })
    }

    /// `None` when no host is set, so the caller reads the file system as a compiler would.
    pub(crate) fn read(path: &Path) -> Option<io::Result<String>> {
        // The host is cloned out first, so a host that reads through the generator again
        // cannot find the state borrowed.
        let host = STATE.with(|state| Some(Rc::clone(&state.borrow().as_ref()?.host)))?;
        Some(host.read(path))
    }

    /// A located span source for a template file, recorded in [`Expansion::sources`].
    pub(crate) fn source_span(path: &Arc<Path>, source: &str) -> Option<SourceSpan> {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut()?;
            let hashes = "#".repeat(raw_hashes_needed(source));
            // A raw literal holds the text unescaped, so offsets inside it are the file's.
            let literal: Literal = format!("r{hashes}\"{source}\"{hashes}").parse().ok()?;
            let prefix = hashes.len() + 2;
            let start = literal.span().byte_range().start + prefix;
            state
                .sources
                .push((Arc::clone(path), start..start + source.len()));
            Some(SourceSpan::located(literal, prefix, source.len()))
        })
    }

    /// The fewest `#`s that no `"` in `source` is followed by.
    fn raw_hashes_needed(source: &str) -> usize {
        let bytes = source.as_bytes();
        bytes
            .iter()
            .enumerate()
            .filter(|&(_, &byte)| byte == b'"')
            .map(|(at, _)| bytes[at + 1..].iter().take_while(|&&c| c == b'#').count() + 1)
            .max()
            .unwrap_or(0)
    }

    #[cfg(test)]
    mod tests {
        use super::raw_hashes_needed;

        #[test]
        fn a_raw_literal_needs_one_more_hash_than_any_quote_is_followed_by() {
            assert_eq!(raw_hashes_needed("plain"), 0);
            assert_eq!(raw_hashes_needed(r#"a "quote""#), 1);
            assert_eq!(raw_hashes_needed(r###"end "## here"###), 3);
        }
    }
}

#[cfg(not(feature = "tooling"))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::spans::SourceSpan;

    pub(crate) fn active() -> bool {
        false
    }

    pub(crate) fn manifest_dir() -> Option<PathBuf> {
        None
    }

    pub(crate) fn caller_dir() -> Option<PathBuf> {
        None
    }

    pub(crate) fn read(_: &Path) -> Option<io::Result<String>> {
        None
    }

    pub(crate) fn source_span(_: &Arc<Path>, _: &str) -> Option<SourceSpan> {
        None
    }
}
