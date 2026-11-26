//! Askama's own code generator run on one template item, with a map from every generated
//! token back to the text it came from.
//!
//! The analyser asks rust-analyzer about the code `cargo build` compiles, so it runs askama's
//! generator (vendored with a tooling patch; see ARCHITECTURE.md) instead of an imitation of
//! it. The generator gives each token it emits the template range it came from; this crate
//! prints the tokens as Rust and keeps those ranges as a [`Mapping`] per token.
//!
//! No I/O happens here: files come from the caller through [`Files`], so unsaved buffers are
//! what askama reads.

use std::any::Any;
use std::io;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use proc_macro2::{Delimiter, Spacing, Span, TokenStream, TokenTree};

/// One item deriving `Template`, and where askama would find what it needs.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The item's source text: a struct or enum with its attributes, as written in its file.
    pub item: &'a str,
    /// The Rust file holding the item, which askama resolves relative paths against.
    pub file: &'a Path,
    /// The crate's manifest directory, where askama finds `askama.toml` and `templates/`.
    pub manifest_dir: &'a Path,
}

/// The files askama reads: templates, and the crate's `askama.toml`.
pub trait Files {
    /// The file's current contents, which may be an unsaved buffer. A missing file is an
    /// [`io::ErrorKind::NotFound`] error, which askama reports as it would at compile time.
    fn read(&self, path: &Path) -> io::Result<String>;

    /// Whether a file exists, which decides where askama resolves a template path. An
    /// unsaved buffer for a file not yet on disk counts.
    fn exists(&self, path: &Path) -> bool {
        self.read(path).is_ok()
    }
}

/// Where a generated token came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A template file, at a byte range of its text as read through [`Files`].
    Template {
        path: Arc<Path>,
        range: Range<usize>,
    },
    /// The item's own text, at a byte range of [`Input::item`]: the item's names and types, and
    /// an inline `source` template, whose expressions point into the string literal.
    Item { range: Range<usize> },
}

/// A generated token and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// The token's byte range in [`Generated::code`].
    pub generated: Range<usize>,
    pub origin: Origin,
}

/// An error askama reported for the item, as `compile_error!` in its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationError {
    pub message: String,
    /// Where askama pointed, when it gave a location.
    pub origin: Option<Origin>,
    /// The `compile_error!` statement in [`Generated::code`]. A caller that reports the error
    /// itself blanks this range, so the code's analysis does not report it a second time.
    pub generated: Range<usize>,
}

/// What askama generates for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// The generated Rust: one `const _: () = { ... };` item, as the derive expands to.
    pub code: String,
    /// One entry per located token, in order of [`Mapping::generated`]; ranges do not overlap.
    pub map: Vec<Mapping>,
    /// Errors askama reported. With any, `code` holds askama's fallback implementation, which
    /// renders nothing, as a compiler would see it.
    pub errors: Vec<GenerationError>,
    /// Every template file askama read for the item: its own, then those it extends, includes
    /// and imports. An edit to any of them changes what the item generates.
    pub templates: Vec<Arc<Path>>,
}

/// Why nothing could be generated for an item.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The item's text is not valid Rust tokens, as while it is being typed. Asking again
    /// after the text changes may succeed.
    #[error("the item is not valid Rust tokens: {0}")]
    Lex(String),
    /// Askama's generator panicked on this input. Asking again with the same input changes
    /// nothing; the message is askama's.
    #[error("askama's generator panicked: {0}")]
    Panicked(String),
}

/// Runs askama's generator on `input`, reading files through `files`.
///
/// Runs on the calling thread and frees that thread's proc-macro2 source map before
/// returning, so a thread can generate any number of times without its memory growing.
pub fn generate(input: Input<'_>, files: Rc<dyn Files>) -> Result<Generated, Error> {
    let result = catch_unwind(AssertUnwindSafe(|| generate_located(input, files)));
    // Every span from this run has been read into plain ranges by now.
    proc_macro2::extra::invalidate_current_thread_spans();
    result.unwrap_or_else(|panic| Err(Error::Panicked(panic_message(&*panic))))
}

fn generate_located(input: Input<'_>, files: Rc<dyn Files>) -> Result<Generated, Error> {
    // A marker token at offset 0 says where the item's text starts in proc-macro2's span space.
    let text = format!("_ {}", input.item);
    let stream: TokenStream = text
        .parse()
        .map_err(|err: proc_macro2::LexError| Error::Lex(err.to_string()))?;
    let mut trees = stream.into_iter();
    let marker = trees
        .next()
        .ok_or_else(|| Error::Lex("the item is empty".to_owned()))?;
    let item_start = marker.span().byte_range().start + 2;
    let item_file = marker.span().file();
    let item: TokenStream = trees.collect();

    let host = Rc::new(Host {
        manifest_dir: input.manifest_dir.to_owned(),
        caller_file: input.file.to_owned(),
        files,
    });
    let expansion = askama_derive::tooling::expand(item, import_askama, host);

    let locate = Locate {
        item_file,
        item: item_start..item_start + input.item.len(),
        sources: expansion.sources,
    };
    let mut printer = Printer {
        code: String::new(),
        map: Vec::new(),
        errors: Vec::new(),
        locate: &locate,
        glue: true,
        statement_start: 0,
        open_error: None,
    };
    printer.stream(expansion.tokens);

    let mut templates: Vec<Arc<Path>> = Vec::new();
    for source in &locate.sources {
        if !templates.contains(&source.path) {
            templates.push(Arc::clone(&source.path));
        }
    }
    Ok(Generated {
        code: printer.code,
        map: printer.map,
        errors: printer.errors,
        templates,
    })
}

/// What askama's own derive macro imports, so the output matches a compiler's token for token.
fn import_askama() -> TokenStream {
    quote::quote!(
        extern crate askama;
    )
}

struct Host {
    manifest_dir: PathBuf,
    caller_file: PathBuf,
    files: Rc<dyn Files>,
}

impl askama_derive::tooling::Host for Host {
    fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }

    fn caller_file(&self) -> Option<&Path> {
        Some(&self.caller_file)
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        self.files.read(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.exists(path)
    }
}

/// Turns spans into origins: the item's text, or one of the template files askama read.
///
/// A span's byte range counts from the start of the parsed string it came from, so a span
/// is placed by that string's name first and its range second.
struct Locate {
    /// proc-macro2's name for the parsed string holding the item.
    item_file: String,
    item: Range<usize>,
    sources: Vec<askama_derive::tooling::Source>,
}

impl Locate {
    fn origin(&self, span: Span) -> Option<Origin> {
        let range = span.byte_range();
        if range.is_empty() && range.start == 0 {
            // The call site, which locates nothing.
            return None;
        }
        let file = span.file();
        let within = |outer: &Range<usize>| outer.start <= range.start && range.end <= outer.end;
        if file == self.item_file && within(&self.item) {
            return Some(Origin::Item {
                range: range.start - self.item.start..range.end - self.item.start,
            });
        }
        self.sources
            .iter()
            .find(|source| source.file == file && within(&source.range))
            .map(|source| Origin::Template {
                path: Arc::clone(&source.path),
                range: range.start - source.range.start..range.end - source.range.start,
            })
    }
}

/// Writes tokens as Rust text, recording where each located one came from.
struct Printer<'a> {
    code: String,
    map: Vec<Mapping>,
    errors: Vec<GenerationError>,
    locate: &'a Locate,
    /// No space before the next token: after a joint punctuation mark, a `.` or an opening
    /// delimiter, and at the start.
    glue: bool,
    /// Where the statement being printed starts: after the last `;`, `{` or `}`.
    statement_start: usize,
    /// The error whose `compile_error!` statement is being printed, until its `;`.
    open_error: Option<usize>,
}

impl Printer<'_> {
    fn stream(&mut self, stream: TokenStream) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        for (at, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(group) => {
                    let (open, close) = match group.delimiter() {
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::None => ("", ""),
                    };
                    if !open.is_empty() {
                        self.word(open, group.span_open());
                        self.glue = true;
                    }
                    if group.delimiter() == Delimiter::Brace {
                        self.code.push('\n');
                        self.statement_start = self.code.len();
                    }
                    self.stream(group.stream());
                    if !close.is_empty() {
                        self.glue = true;
                        self.word(close, group.span_close());
                    }
                    if group.delimiter() == Delimiter::Brace {
                        self.code.push('\n');
                        self.glue = true;
                        self.statement_start = self.code.len();
                    }
                }
                TokenTree::Ident(ident) => {
                    self.compile_error(ident, &trees[at + 1..]);
                    self.word(&ident.to_string(), ident.span());
                }
                TokenTree::Punct(punct) => {
                    let mark = punct.as_char();
                    if matches!(mark, '.' | ',' | ';' | '?') {
                        self.glue = true;
                    }
                    self.word(mark.encode_utf8(&mut [0; 4]), punct.span());
                    self.glue = punct.spacing() == Spacing::Joint || mark == '.';
                    if mark == ';' && punct.spacing() == Spacing::Alone {
                        if let Some(error) = self.open_error.take() {
                            self.errors[error].generated.end = self.code.len();
                        }
                        self.code.push('\n');
                        self.glue = true;
                        self.statement_start = self.code.len();
                    }
                }
                TokenTree::Literal(literal) => self.word(&literal.to_string(), literal.span()),
            }
        }
    }

    fn word(&mut self, text: &str, span: Span) {
        if !self.glue && !self.code.ends_with(char::is_whitespace) {
            self.code.push(' ');
        }
        self.glue = false;
        let start = self.code.len();
        self.code.push_str(text);
        if let Some(origin) = self.locate.origin(span) {
            self.map.push(Mapping {
                generated: start..self.code.len(),
                origin,
            });
        }
    }

    /// Records askama's error when `ident` starts `compile_error!("...")`.
    fn compile_error(&mut self, ident: &proc_macro2::Ident, rest: &[TokenTree]) {
        let [TokenTree::Punct(bang), TokenTree::Group(args), ..] = rest else {
            return;
        };
        if ident != "compile_error" || bang.as_char() != '!' {
            return;
        }
        let Some(TokenTree::Literal(message)) = args.stream().into_iter().next() else {
            return;
        };
        let message = syn::parse_str::<syn::LitStr>(&message.to_string())
            .map_or_else(|_| message.to_string(), |literal| literal.value());
        self.open_error = Some(self.errors.len());
        self.errors.push(GenerationError {
            message,
            origin: self.locate.origin(ident.span()),
            generated: self.statement_start..self.code.len(),
        });
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned())
}

#[cfg(test)]
mod tests;
