//! One workspace root: its analysis, the overlays sent to its private rust-analyzer, and the
//! translation of rust-analyzer's answers into template positions.
//!
//! A template position becomes a question to rust-analyzer in three steps: the generator's map
//! says which generated token came from it, the overlay says where that token sits in the
//! overlaid Rust file, and rust-analyzer is asked there. Answers come back the same way in
//! reverse, and an answer in the user's own Rust code passes through unchanged.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama_analyzer_codegen::{Generated, Origin};
use lsp_types::{DiagnosticSeverity, Location, Position, Url};
use tokio::sync::Notify;

use crate::analysis::{Analysis, Problem};
use crate::overlay::{Overlay, Place};
use crate::ra::{Config, Session};
use crate::text::{Encoding, Lines};

/// Where in a generated item a template offset was asked about.
pub struct Asked {
    pub rust_file: PathBuf,
    pub item: usize,
    /// The question's position in the overlay of `rust_file`.
    pub position: Position,
    pub uri: Url,
}

/// An overlay as rust-analyzer last received it.
struct Sent {
    overlay: Overlay,
    version: i32,
}

enum Rust {
    /// Not started; it starts when this workspace's templates are first needed.
    Idle,
    /// Boxed: a session is far larger than the other states.
    Running(Box<Session>),
    /// It could not start; templates get askama's own errors. Retried after a reload.
    Failed,
}

pub struct Workspace {
    pub analysis: Analysis,
    /// How positions count columns, with the client and with rust-analyzer alike.
    encoding: Encoding,
    rust: Rust,
    sent: BTreeMap<PathBuf, Sent>,
    /// Overlays whose diagnostics rust-analyzer has not been asked for since they changed.
    /// rust-analyzer computes them only when asked.
    stale: BTreeSet<PathBuf>,
    /// Whether the client has been told rust-analyzer is loading, and not yet that it is done.
    announced: bool,
}

impl Workspace {
    pub fn new(analysis: Analysis, encoding: Encoding) -> Self {
        Self {
            analysis,
            encoding,
            rust: Rust::Idle,
            sent: BTreeMap::new(),
            stale: BTreeSet::new(),
            announced: false,
        }
    }

    /// Whether `path` is a template this workspace's items read, or a Rust file it indexes.
    pub fn knows(&self, path: &Path) -> bool {
        self.analysis.owns(path) || self.analysis.file(path).is_some()
    }

    /// Starts rust-analyzer if it is not running, reporting why when it cannot.
    pub async fn start(&mut self, config: &Config, wake: &Arc<Notify>) -> Option<String> {
        if !matches!(self.rust, Rust::Idle) || config.path.is_none() {
            return None;
        }
        match Session::start(
            self.analysis.root(),
            config,
            self.encoding,
            Arc::clone(wake),
        )
        .await
        {
            Ok(session) => {
                tracing::info!(root = %self.analysis.root().display(), "rust-analyzer started");
                self.rust = Rust::Running(Box::new(session));
                self.sent.clear();
                None
            }
            Err(err) => {
                self.rust = Rust::Failed;
                Some(format!(
                    "askama-analyzer could not start rust-analyzer: {err:#}. Templates get askama's own errors only."
                ))
            }
        }
    }

    /// Stops rust-analyzer when it has sat unasked for [`crate::ra::IDLE`].
    pub fn stop_if_idle(&mut self, now: tokio::time::Instant) {
        if let Rust::Running(session) = &self.rust
            && now.duration_since(session.last_used) >= crate::ra::IDLE
        {
            tracing::info!(root = %self.analysis.root().display(), "rust-analyzer stopped while idle");
            self.rust = Rust::Idle;
            self.sent.clear();
        }
    }

    /// When rust-analyzer goes idle next, if it runs.
    pub fn idle_at(&self) -> Option<tokio::time::Instant> {
        match &self.rust {
            Rust::Running(session) => Some(session.last_used + crate::ra::IDLE),
            _ => None,
        }
    }

    /// `Some(true)` once when rust-analyzer starts loading the workspace, `Some(false)` once when
    /// it has finished, so the client can show it as progress.
    pub fn loading_changed(&mut self) -> Option<bool> {
        let loading = match &self.rust {
            Rust::Running(session) => !session.is_ready(),
            _ => false,
        };
        (loading != self.announced).then(|| {
            self.announced = loading;
            loading
        })
    }

    pub fn reset_failure(&mut self) {
        if matches!(self.rust, Rust::Failed) {
            self.rust = Rust::Idle;
        }
    }

    /// Builds each Rust file's overlay and sends rust-analyzer the ones that changed.
    pub fn sync(&mut self) {
        let Rust::Running(session) = &mut self.rust else {
            return;
        };
        let mut seen = BTreeSet::new();
        for (path, file) in self.analysis.files() {
            seen.insert(path.to_owned());
            let items: Vec<_> = file
                .items
                .iter()
                .map(|item| {
                    (
                        &item.found,
                        item.generated.as_ref().and_then(|g| g.as_ref().ok()),
                    )
                })
                .collect();
            let overlay = Overlay::build(&file.text, &items);
            if self
                .sent
                .get(path)
                .is_some_and(|sent| sent.overlay == overlay)
            {
                continue;
            }
            match session.sync(path, overlay.text()) {
                Ok(version) => {
                    self.sent.insert(path.to_owned(), Sent { overlay, version });
                    self.stale.insert(path.to_owned());
                }
                Err(err) => tracing::warn!(path = %path.display(), %err, "sending an overlay"),
            }
        }
        tracing::debug!(overlays = self.sent.len(), "rust-analyzer has the overlays");
        // A file that no longer defines templates goes back to its text on disk.
        let gone: Vec<PathBuf> = self
            .sent
            .keys()
            .filter(|path| !seen.contains(*path))
            .cloned()
            .collect();
        for path in gone {
            if let Ok(text) = self.analysis.vfs.read(&path) {
                let _ = session.sync(&path, &text);
            }
            self.sent.remove(&path);
        }
    }

    /// Asks rust-analyzer for the diagnostics of every overlay that changed since it was last
    /// asked, once it has loaded the workspace; before that, types are unknown and answers
    /// would miss errors.
    pub async fn pull_stale(&mut self) {
        let Rust::Running(session) = &mut self.rust else {
            return;
        };
        if !session.is_ready() {
            return;
        }
        for path in std::mem::take(&mut self.stale) {
            let count = session.pull(&path).await.map(|items| items.len());
            tracing::debug!(path = %path.display(), ?count, "pulled rust-analyzer's diagnostics");
        }
    }

    /// askama's errors, and rust-analyzer's errors in generated code, by template.
    pub fn problems(&self) -> BTreeMap<PathBuf, BTreeSet<Problem>> {
        let mut problems = self.analysis.problems();
        let Rust::Running(session) = &self.rust else {
            return problems;
        };
        for (path, sent) in &self.sent {
            let Some((version, diagnostics)) = session.diagnostics(path) else {
                tracing::debug!(path = %path.display(), "no diagnostics from rust-analyzer yet");
                continue;
            };
            if version.is_some_and(|version| version != sent.version) {
                tracing::debug!(path = %path.display(), ?version, sent = sent.version, "diagnostics for an older overlay");
                continue;
            }
            let Some(file) = self.analysis.file(path) else {
                continue;
            };
            tracing::debug!(path = %path.display(), count = diagnostics.len(), "rust-analyzer diagnostics");
            if let Some(dump) = std::env::var_os("ASKAMA_ANALYZER_DUMP_OVERLAYS") {
                let _ = std::fs::write(
                    std::path::Path::new(&dump).join(path.file_name().unwrap_or_default()),
                    sent.overlay.text(),
                );
            }
            let lines = Lines::new(sent.overlay.text(), self.encoding);
            for diagnostic in diagnostics {
                tracing::debug!(path = %path.display(), message = %diagnostic.message, range = ?diagnostic.range, severity = ?diagnostic.severity, "rust-analyzer diagnostic");
                // An error about `!` or `{unknown}` follows from an earlier error that left a
                // type unknown, which is reported on its own.
                if diagnostic.severity != Some(DiagnosticSeverity::ERROR)
                    || diagnostic.message.contains("`!")
                    || diagnostic.message.contains("{unknown}")
                {
                    continue;
                }
                let Some(start) = lines.offset(diagnostic.range.start) else {
                    continue;
                };
                let end = lines.offset(diagnostic.range.end).unwrap_or(start);
                let Some((template, range)) = self.template_range(file, sent, start..end) else {
                    continue;
                };
                problems
                    .entry(template.clone())
                    .or_default()
                    .insert(Problem {
                        path: template,
                        range,
                        message: diagnostic.message,
                    });
            }
        }
        problems
    }

    /// The question rust-analyzer should be asked for a template offset, from the item that
    /// reads the template and generated cleanly, if any does.
    pub fn ask(&self, template: &Path, offset: usize) -> Option<Asked> {
        for (rust_file, item) in self.analysis.readers(template) {
            let Some(Some(Ok(generated))) = self
                .analysis
                .file(&rust_file)
                .and_then(|file| file.items.get(item))
                .map(|item| item.generated.as_ref())
            else {
                continue;
            };
            let Some(at) = generated_offset(generated, template, offset) else {
                continue;
            };
            let Some(sent) = self.sent.get(&rust_file) else {
                continue;
            };
            let Some(overlay_at) = sent.overlay.from_generated(item, at) else {
                continue;
            };
            let uri = Url::from_file_path(&rust_file).ok()?;
            return Some(Asked {
                position: Lines::new(sent.overlay.text(), self.encoding).position(overlay_at),
                rust_file,
                item,
                uri,
            });
        }
        None
    }

    /// A location rust-analyzer answered with, as a location in the user's files: inside
    /// generated code it is the template the code came from, in an overlaid Rust file it is
    /// the same place in the user's text, and anywhere else it is unchanged.
    pub fn user_location(&self, location: &Location) -> Option<Location> {
        let encoding = self.encoding;
        let path = location.uri.to_file_path().ok()?;
        let Some(sent) = self.sent.get(&path) else {
            return Some(location.clone());
        };
        let file = self.analysis.file(&path)?;
        let lines = Lines::new(sent.overlay.text(), encoding);
        let start = lines.offset(location.range.start)?;
        let end = lines.offset(location.range.end).unwrap_or(start);
        match (sent.overlay.place(start), sent.overlay.place(end)) {
            (Place::Original(start), Place::Original(end)) => {
                let original = Lines::new(&file.text, encoding);
                Some(Location {
                    uri: location.uri.clone(),
                    range: original.range(&(start..end.max(start))),
                })
            }
            _ => {
                let (template, range) = self.template_range(file, sent, start..end)?;
                let text = self.analysis.vfs.read(&template).ok()?;
                Some(Location {
                    uri: Url::from_file_path(&template).ok()?,
                    range: Lines::new(&text, encoding).range(&range),
                })
            }
        }
    }

    /// The template range an overlay range of generated code came from.
    fn template_range(
        &self,
        file: &crate::analysis::RustFile,
        sent: &Sent,
        range: Range<usize>,
    ) -> Option<(PathBuf, Range<usize>)> {
        let Place::Generated {
            item,
            offset: start,
        } = sent.overlay.place(range.start)
        else {
            return None;
        };
        let Some(Ok(generated)) = file.items.get(item)?.generated.as_ref() else {
            return None;
        };
        let (template, start) = template_offset(generated, start)?;
        let end = match sent.overlay.place(range.end) {
            Place::Generated {
                item: end_item,
                offset,
            } if end_item == item => template_offset(generated, offset)
                .filter(|(end_template, end)| *end_template == template && *end >= start)
                .map(|(_, end)| end),
            _ => None,
        }
        .unwrap_or(start);
        Some((template.to_path_buf(), start..end))
    }

    /// The running session, started on demand.
    pub fn session(&mut self) -> Option<&mut Session> {
        match &mut self.rust {
            Rust::Running(session) => Some(session),
            _ => None,
        }
    }

    pub fn overlay_text(&self, rust_file: &Path) -> Option<&str> {
        self.sent.get(rust_file).map(|sent| sent.overlay.text())
    }

    /// Sends rust-analyzer a probe: the file's overlay with one item's code replaced.
    pub fn send_probe(
        &mut self,
        rust_file: &Path,
        item: usize,
        generated: &Generated,
    ) -> Option<(Overlay, i32)> {
        let file = self.analysis.file(rust_file)?;
        let items: Vec<_> = file
            .items
            .iter()
            .enumerate()
            .map(|(at, found)| {
                let code = if at == item {
                    Some(generated)
                } else {
                    found.generated.as_ref().and_then(|g| g.as_ref().ok())
                };
                (&found.found, code)
            })
            .collect();
        let overlay = Overlay::build(&file.text, &items);
        let Rust::Running(session) = &mut self.rust else {
            return None;
        };
        let version = session.sync(rust_file, overlay.text()).ok()?;
        Some((overlay, version))
    }

    /// Puts back the overlay a probe replaced.
    pub fn restore(&mut self, rust_file: &Path) {
        let Some(text) = self
            .sent
            .get(rust_file)
            .map(|sent| sent.overlay.text().to_owned())
        else {
            return;
        };
        let Rust::Running(session) = &mut self.rust else {
            return;
        };
        if let Ok(version) = session.sync(rust_file, &text)
            && let Some(sent) = self.sent.get_mut(rust_file)
        {
            sent.version = version;
        }
    }

    /// Whether any open file is a template this workspace's items read, which is when its
    /// rust-analyzer is wanted.
    pub fn wants_rust<'a>(&self, mut open: impl Iterator<Item = &'a Path>) -> bool {
        open.any(|path| self.analysis.owns(path))
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }
}

/// The generated offset a template offset corresponds to: within the most specific token from
/// that template covering it.
pub fn generated_offset(generated: &Generated, template: &Path, offset: usize) -> Option<usize> {
    generated
        .map
        .iter()
        .filter_map(|mapping| match &mapping.origin {
            Origin::Template { path, range }
                if **path == *template && range.start <= offset && offset <= range.end =>
            {
                Some((mapping, range))
            }
            _ => None,
        })
        .min_by_key(|(mapping, range)| (range.len(), mapping.generated.start))
        .map(|(mapping, range)| {
            let inside = if mapping.generated.len() == range.len() {
                offset - range.start
            } else {
                0
            };
            mapping.generated.start + inside
        })
}

/// The template and offset a generated offset came from.
pub fn template_offset(generated: &Generated, offset: usize) -> Option<(Arc<Path>, usize)> {
    let at = generated
        .map
        .partition_point(|mapping| mapping.generated.end < offset);
    let mapping = generated
        .map
        .get(at)
        .filter(|mapping| mapping.generated.start <= offset)?;
    let Origin::Template { path, range } = &mapping.origin else {
        return None;
    };
    let inside = (offset - mapping.generated.start).min(range.len());
    let at = if mapping.generated.len() == range.len() {
        range.start + inside
    } else {
        range.start
    };
    Some((Arc::clone(path), at))
}
