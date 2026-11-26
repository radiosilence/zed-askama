//! The language server: the editor's side of the protocol, and the one ordered lane that owns
//! the analysis.
//!
//! Notifications only record what changed, in a table of documents shared with the lane, and
//! wake it. The lane waits for the edits to settle, so a burst of keystrokes is analysed once,
//! then applies every change in order, sends rust-analyzer the overlays that changed and
//! publishes the diagnostics that changed. A change is a whole document's text, never a delta,
//! so a lane that falls behind loses intermediate states, never a change. Requests go down the
//! same lane, after any change that arrived before them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, LanguageClient, ResponseError};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    Initialized, Progress, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
    Completion, Formatting, GotoDefinition, HoverRequest, Initialize, PrepareRenameRequest,
    References, Rename, Shutdown,
};
use lsp_types::{
    CompletionOptions, Diagnostic, DiagnosticSeverity, DidChangeWatchedFilesRegistrationOptions,
    FileSystemWatcher, GlobPattern, HoverProviderCapability, InitializeParams, InitializeResult,
    MessageType, NumberOrString, OneOf, ProgressParams, ProgressParamsValue,
    PublishDiagnosticsParams, Registration, RegistrationParams, RenameOptions, ServerCapabilities,
    ServerInfo, ShowMessageParams, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, Url, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd,
};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc, oneshot};
use tower::ServiceBuilder;

use crate::analysis::{self, Analysis, Problem};
use crate::features::{self, Query};
use crate::generator::Generator;
use crate::ra::Config;
use crate::text::{Encoding, Lines};
use crate::vfs::Vfs;
use crate::workspace::Workspace;

/// A burst of keystrokes is analysed once, after this much quiet: shorter than a pause that
/// reads as lag.
const SETTLE: Duration = Duration::from_millis(150);
/// The longest the lane waits while edits keep arriving, so continuous typing still gets
/// answers.
const MAX_SETTLE: Duration = Duration::from_millis(1000);
/// How long the client has to accept a progress token before loading goes unshown.
const PROGRESS_CREATE: Duration = Duration::from_secs(5);
/// Requests waiting for the lane. An editor sends a handful at a time; the bound holds back a
/// client that floods it rather than growing without limit.
const QUEUE: usize = 64;
/// The language identifier the Zed extension gives every Askama language.
const ASKAMA_LANGUAGE: &str = "askama";
/// The language identifier of Rust files, which hold no templates the server reads yet.
const RUST_LANGUAGE: &str = "rust";

/// Serves the protocol on stdin and stdout until the client exits.
pub async fn run() -> anyhow::Result<ExitCode> {
    let (server, _) = async_lsp::MainLoop::new_server(|client| {
        let (queries, inbox) = mpsc::channel(QUEUE);
        let state = State {
            client: client.clone(),
            shared: Arc::new(Shared::default()),
            encoding: Encoding::Utf16,
            roots: Vec::new(),
            config: Config::default(),
            queries,
            inbox: Some(inbox),
        };
        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .layer(ClientProcessMonitorLayer::new(client))
            .service(router(state))
    });
    let stdin = tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin());
    let stdout = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout());
    server.run_buffered(stdin, stdout).await?;
    Ok(ExitCode::SUCCESS)
}

struct State {
    client: ClientSocket,
    shared: Arc<Shared>,
    encoding: Encoding,
    roots: Vec<PathBuf>,
    config: Config,
    queries: mpsc::Sender<Query>,
    /// Handed to the lane when it starts.
    inbox: Option<mpsc::Receiver<Query>>,
}

/// The documents as the editor has them, and which files changed since the lane last looked.
#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    /// Wakes the lane: an edit, or rust-analyzer having published.
    wake: Arc<Notify>,
    /// rust-analyzer loading as the client shows it: `None` when the client takes no
    /// server-made progress, else each loading workspace root, `true` once its bar is shown.
    loading: Arc<Mutex<Option<HashMap<PathBuf, bool>>>>,
}

#[derive(Default)]
struct Inner {
    docs: HashMap<PathBuf, Doc>,
    changed: BTreeSet<PathBuf>,
}

#[derive(Clone)]
struct Doc {
    text: Arc<str>,
    version: i32,
    language: String,
}

impl Shared {
    fn change(&self, path: PathBuf, edit: impl FnOnce(&mut HashMap<PathBuf, Doc>)) {
        let mut inner = self.inner.lock();
        edit(&mut inner.docs);
        inner.changed.insert(path);
        drop(inner);
        self.wake.notify_one();
    }

    /// The documents now, and the files changed since the last call.
    fn take(&self, docs: &mut HashMap<PathBuf, Doc>, changed: &mut BTreeSet<PathBuf>) {
        let mut inner = self.inner.lock();
        docs.clone_from(&inner.docs);
        changed.append(&mut inner.changed);
    }

    fn language(&self, uri: &Url) -> Option<String> {
        let path = file_path(uri)?;
        self.inner
            .lock()
            .docs
            .get(&path)
            .map(|doc| doc.language.clone())
    }
}

fn router(state: State) -> Router<State> {
    let mut router = Router::new(state);
    router
        .request::<Initialize, _>(|state, params| {
            let result = initialize(state, &params);
            async move { Ok(result) }
        })
        .request::<Shutdown, _>(|_, ()| async { Ok(()) })
        .request::<HoverRequest, _>(|state, params| {
            let uri = params
                .text_document_position_params
                .text_document
                .uri
                .clone();
            ask(state, &uri, |reply| Query::Hover(params, reply))
        })
        .request::<GotoDefinition, _>(|state, params| {
            let uri = params
                .text_document_position_params
                .text_document
                .uri
                .clone();
            ask(state, &uri, |reply| Query::Definition(params, reply))
        })
        .request::<References, _>(|state, params| {
            let uri = params.text_document_position.text_document.uri.clone();
            ask(state, &uri, |reply| Query::References(params, reply))
        })
        .request::<PrepareRenameRequest, _>(|state, params| {
            let uri = params.text_document.uri.clone();
            ask(state, &uri, |reply| Query::PrepareRename(params, reply))
        })
        .request::<Rename, _>(|state, params| {
            let uri = params.text_document_position.text_document.uri.clone();
            ask(state, &uri, |reply| Query::Rename(params, reply))
        })
        .request::<Formatting, _>(|state, params| {
            let edits = format_document(state, &params.text_document.uri);
            async move { Ok(edits) }
        })
        .request::<Completion, _>(|state, params| {
            let uri = params.text_document_position.text_document.uri.clone();
            ask(state, &uri, |reply| Query::Completion(params, reply))
        })
        .notification::<Initialized>(|state, _| {
            register_watchers(&state.client);
            if let Some(inbox) = state.inbox.take() {
                tokio::spawn(lane(
                    state.client.clone(),
                    Arc::clone(&state.shared),
                    state.roots.clone(),
                    state.encoding,
                    state.config.clone(),
                    inbox,
                ));
            }
            ControlFlow::Continue(())
        })
        .notification::<DidOpenTextDocument>(|state, params| {
            if let Some(path) = file_path(&params.text_document.uri) {
                let doc = Doc {
                    text: Arc::from(params.text_document.text),
                    version: params.text_document.version,
                    language: params.text_document.language_id,
                };
                state.shared.change(path.clone(), |docs| {
                    docs.insert(path, doc);
                });
            }
            ControlFlow::Continue(())
        })
        .notification::<DidChangeTextDocument>(|state, params| {
            if let Some(path) = file_path(&params.text_document.uri) {
                let encoding = state.encoding;
                let version = params.text_document.version;
                state.shared.change(path.clone(), |docs| {
                    if let Some(doc) = docs.get_mut(&path) {
                        let mut text = doc.text.to_string();
                        for change in params.content_changes {
                            apply_change(&mut text, change, encoding);
                        }
                        doc.text = Arc::from(text);
                        doc.version = version;
                    }
                });
            }
            ControlFlow::Continue(())
        })
        .notification::<DidCloseTextDocument>(|state, params| {
            if let Some(path) = file_path(&params.text_document.uri) {
                state.shared.change(path.clone(), |docs| {
                    docs.remove(&path);
                });
            }
            ControlFlow::Continue(())
        })
        .notification::<DidChangeWatchedFiles>(|state, params| {
            for change in params.changes {
                if let Some(path) = file_path(&change.uri) {
                    state.shared.change(path, |_| {});
                }
            }
            ControlFlow::Continue(())
        })
        // Everything else the client says (configuration, saves, traces) changes nothing here.
        .unhandled_notification(|_, _| ControlFlow::Continue(()));
    router
}

/// Sends a request down the lane and waits for its answer. A Rust file is answered at once
/// with nothing: rust-analyzer answers there, and Zed asks every server attached to a buffer.
fn ask<T: Send + 'static, Q: FnOnce(oneshot::Sender<Option<T>>) -> Query>(
    state: &State,
    uri: &Url,
    query: Q,
) -> impl Future<Output = Result<Option<T>, ResponseError>> + use<T, Q> {
    let skip = state.shared.language(uri).as_deref() == Some(RUST_LANGUAGE);
    let (reply, answer) = oneshot::channel();
    let query = query(reply);
    let queries = state.queries.clone();
    async move {
        if skip || queries.send(query).await.is_err() {
            return Ok(None);
        }
        Ok(answer.await.unwrap_or(None))
    }
}

/// The whole document's formatting as one edit, or none when it is formatted already. Only
/// Askama documents are formatted: HTML and Rust have formatters of their own. A template the
/// formatter refuses (it does not parse, or formatting would change what it means) is left as
/// it is.
fn format_document(state: &State, uri: &Url) -> Option<Vec<lsp_types::TextEdit>> {
    let path = file_path(uri)?;
    let doc = state.shared.inner.lock().docs.get(&path).cloned()?;
    if doc.language != ASKAMA_LANGUAGE {
        return None;
    }
    let formatted = match crate::format::format(&doc.text, &askama_parser::Syntax::default()) {
        Ok(formatted) => formatted,
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "not formatting");
            return None;
        }
    };
    if *formatted == *doc.text {
        return Some(Vec::new());
    }
    let lines = Lines::new(&doc.text, state.encoding);
    Some(vec![lsp_types::TextEdit {
        range: lines.range(&(0..doc.text.len())),
        new_text: formatted,
    }])
}

fn initialize(state: &mut State, params: &InitializeParams) -> InitializeResult {
    state.encoding = Encoding::negotiate(
        params
            .capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_deref()),
    );
    state.config = Config::from_initialization_options(params.initialization_options.as_ref());
    let progress = params
        .capabilities
        .window
        .as_ref()
        .and_then(|window| window.work_done_progress)
        .unwrap_or(false);
    *state.shared.loading.lock() = progress.then(HashMap::new);
    state.roots = params
        .workspace_folders
        .iter()
        .flatten()
        .filter_map(|folder| file_path(&folder.uri))
        .collect();
    #[expect(deprecated, reason = "older clients send only root_uri")]
    let root_uri = params.root_uri.as_ref();
    if state.roots.is_empty()
        && let Some(root) = root_uri.and_then(file_path)
    {
        state.roots.push(root);
    }
    InitializeResult {
        capabilities: ServerCapabilities {
            position_encoding: Some(state.encoding.kind()),
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    ..TextDocumentSyncOptions::default()
                },
            )),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            document_formatting_provider: Some(OneOf::Left(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: Default::default(),
            })),
            completion_provider: Some(CompletionOptions {
                // A field after `.`, a filter after `|`, a path after `::`.
                trigger_characters: Some(vec![".".to_owned(), "|".to_owned(), ":".to_owned()]),
                ..CompletionOptions::default()
            }),
            ..ServerCapabilities::default()
        },
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    }
}

/// Asks the client to report changes made outside it, such as a checkout, to the files the
/// analysis reads.
fn register_watchers(client: &ClientSocket) {
    let watchers = [
        "**/*.rs",
        "**/Cargo.toml",
        "**/askama.toml",
        "**/templates/**",
    ]
    .into_iter()
    .map(|glob| FileSystemWatcher {
        glob_pattern: GlobPattern::String(glob.to_owned()),
        kind: None,
    })
    .collect();
    let Ok(options) = serde_json::to_value(DidChangeWatchedFilesRegistrationOptions { watchers })
    else {
        return;
    };
    let mut client = client.clone();
    tokio::spawn(async move {
        let registered = client
            .register_capability(RegistrationParams {
                registrations: vec![Registration {
                    id: "askama-analyzer-watched-files".to_owned(),
                    method: "workspace/didChangeWatchedFiles".to_owned(),
                    register_options: Some(options),
                }],
            })
            .await;
        if let Err(err) = registered {
            tracing::info!(%err, "the client watches no files for us; changes outside it are seen on the next edit");
        }
    });
}

/// The one ordered lane: loads each workspace root, then applies changes as they settle,
/// answers requests in the order they arrive, and stops rust-analyzer when it idles.
async fn lane(
    client: ClientSocket,
    shared: Arc<Shared>,
    roots: Vec<PathBuf>,
    encoding: Encoding,
    config: Config,
    mut inbox: mpsc::Receiver<Query>,
) {
    let generator = match Generator::spawn() {
        Ok(generator) => generator,
        Err(err) => {
            tracing::error!(%err, "cannot start the generator; no templates will be analysed");
            return;
        }
    };
    let mut workspaces = Vec::new();
    for root in &roots {
        match Analysis::load(root, generator.clone(), Vfs::default()).await {
            Ok(analysis) => {
                tracing::info!(
                    root = %root.display(),
                    packages = analysis.packages.len(),
                    items = analysis.item_count(),
                    "loaded"
                );
                workspaces.push(Workspace::new(analysis, encoding));
            }
            Err(err) => tracing::info!(root = %root.display(), "{err:#}; nothing to analyse there"),
        }
    }
    if config.path.is_none() {
        warn(
            &client,
            "askama-analyzer found no rust-analyzer, so templates get askama's own errors only. Set lsp.rust-analyzer.binary.path, or install rust-analyzer.".to_owned(),
        );
    }

    let mut published: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    let mut docs: HashMap<PathBuf, Doc> = HashMap::new();
    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
    let mut dirty = true;
    loop {
        if dirty {
            refresh(
                &client,
                &mut workspaces,
                &docs,
                &changed,
                &config,
                &shared,
                encoding,
                &mut published,
            )
            .await;
            changed.clear();
            dirty = false;
        }
        let idle_at = workspaces.iter().filter_map(Workspace::idle_at).min();
        tokio::select! {
            () = shared.wake.notified() => {
                settle(&shared).await;
                shared.take(&mut docs, &mut changed);
                dirty = true;
            }
            query = inbox.recv() => {
                let Some(query) = query else { return };
                // The request is about the documents as they are now.
                shared.take(&mut docs, &mut changed);
                if !changed.is_empty() {
                    refresh(&client, &mut workspaces, &docs, &changed, &config, &shared, encoding, &mut published).await;
                    changed.clear();
                }
                tracing::debug!(uri = %query.uri(), "answering");
                features::answer(&mut workspaces, query).await;
            }
            () = sleep_until(idle_at) => {
                let now = tokio::time::Instant::now();
                for workspace in &mut workspaces {
                    workspace.stop_if_idle(now);
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Applies changed files, starts rust-analyzer where templates are open, sends it what
/// changed, and publishes.
#[expect(
    clippy::too_many_arguments,
    reason = "the lane's state, passed apart so the borrows stay simple"
)]
async fn refresh(
    client: &ClientSocket,
    workspaces: &mut [Workspace],
    docs: &HashMap<PathBuf, Doc>,
    changed: &BTreeSet<PathBuf>,
    config: &Config,
    shared: &Shared,
    encoding: Encoding,
    published: &mut BTreeMap<PathBuf, Vec<Diagnostic>>,
) {
    for workspace in workspaces.iter_mut() {
        for path in changed {
            match docs.get(path) {
                Some(doc) => workspace
                    .analysis
                    .vfs
                    .set(path.clone(), Arc::clone(&doc.text)),
                None => workspace.analysis.vfs.close(path),
            }
        }
        if let Err(err) = workspace.analysis.apply(changed).await {
            tracing::warn!("{err:#}");
        }
        if changed.iter().any(|path| path.ends_with("Cargo.toml")) {
            workspace.reset_failure();
        }
        if workspace.wants_rust(docs.keys().map(PathBuf::as_path))
            && let Some(message) = workspace.start(config, &shared.wake).await
        {
            warn(client, message);
        }
        workspace.sync();
        workspace.pull_stale().await;
        if let Some(loading) = workspace.loading_changed() {
            progress(client, &shared.loading, workspace.analysis.root(), loading);
        }
    }
    publish(client, workspaces, docs, encoding, published);
}

/// Shows rust-analyzer loading a workspace as progress in the client, so the first seconds of
/// unknown types are explained. Only for a client that declared it takes server-made progress,
/// and never on the lane: a client slow to accept the token holds up nothing, and one that
/// refuses it, or answers after loading ended, is never sent its end.
fn progress(
    client: &ClientSocket,
    shown: &Arc<Mutex<Option<HashMap<PathBuf, bool>>>>,
    root: &Path,
    loading: bool,
) {
    let token = NumberOrString::String(format!("askama-analyzer/rust-analyzer/{}", root.display()));
    let mut guard = shown.lock();
    let Some(roots) = guard.as_mut() else {
        return;
    };
    if !loading {
        if roots.remove(root) == Some(true) {
            let _ = client.notify::<Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                    WorkDoneProgressEnd::default(),
                )),
            });
        }
        return;
    }
    roots.insert(root.to_owned(), false);
    drop(guard);
    let mut client = client.clone();
    let shown = Arc::clone(shown);
    let root = root.to_owned();
    tokio::spawn(async move {
        let created = tokio::time::timeout(
            PROGRESS_CREATE,
            client.work_done_progress_create(WorkDoneProgressCreateParams {
                token: token.clone(),
            }),
        )
        .await;
        let mut guard = shown.lock();
        let Some(state) = guard.as_mut().and_then(|roots| roots.get_mut(&root)) else {
            return;
        };
        if matches!(created, Ok(Ok(()))) {
            *state = true;
            let _ = client.notify::<Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: "Loading rust-analyzer for templates".to_owned(),
                        ..WorkDoneProgressBegin::default()
                    },
                )),
            });
        }
    });
}

fn warn(client: &ClientSocket, message: String) {
    tracing::warn!("{message}");
    let _ = client.notify::<ShowMessage>(ShowMessageParams {
        typ: MessageType::WARNING,
        message,
    });
}

/// Waits until no change has arrived for [`SETTLE`], or [`MAX_SETTLE`] has passed.
async fn settle(shared: &Shared) {
    let deadline = tokio::time::Instant::now() + MAX_SETTLE;
    loop {
        let quiet = tokio::time::Instant::now() + SETTLE;
        tokio::select! {
            () = tokio::time::sleep_until(quiet.min(deadline)) => return,
            () = shared.wake.notified() => {}
        }
    }
}

/// Publishes every file whose diagnostics differ from what was last published, including
/// files whose problems are gone.
fn publish(
    client: &ClientSocket,
    workspaces: &[Workspace],
    docs: &HashMap<PathBuf, Doc>,
    encoding: Encoding,
    published: &mut BTreeMap<PathBuf, Vec<Diagnostic>>,
) {
    let mut problems: BTreeMap<PathBuf, BTreeSet<Problem>> = BTreeMap::new();
    for workspace in workspaces {
        for (path, list) in workspace.problems() {
            problems.entry(path).or_default().extend(list);
        }
    }
    // An open template no item reads still gets askama's syntax errors.
    for (path, doc) in docs {
        if doc.language == ASKAMA_LANGUAGE
            && !features::is_template(workspaces, path)
            && let Some(problem) = analysis::syntax_problem(path, &doc.text)
        {
            problems.entry(path.clone()).or_default().insert(problem);
        }
    }

    let mut now: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for (path, list) in &problems {
        let Some(text) = text_of(path, docs, workspaces) else {
            continue;
        };
        let lines = Lines::new(&text, encoding);
        let diagnostics = list
            .iter()
            .map(|problem| {
                Diagnostic::new(
                    lines.range(&visible(&text, &problem.range)),
                    Some(DiagnosticSeverity::ERROR),
                    None,
                    Some("askama".to_owned()),
                    problem.message.clone(),
                    None,
                    None,
                )
            })
            .collect();
        now.insert(path.clone(), diagnostics);
    }

    let paths: BTreeSet<PathBuf> = published.keys().chain(now.keys()).cloned().collect();
    for path in paths {
        let diagnostics = now.get(&path).cloned().unwrap_or_default();
        if published.get(&path) == Some(&diagnostics) {
            continue;
        }
        let Ok(uri) = Url::from_file_path(&path) else {
            continue;
        };
        let version = docs.get(&path).map(|doc| doc.version);
        if let Err(err) = client.notify::<PublishDiagnostics>(PublishDiagnosticsParams {
            uri,
            diagnostics: diagnostics.clone(),
            version,
        }) {
            tracing::warn!(%err, "publishing diagnostics");
            return;
        }
        if diagnostics.is_empty() {
            published.remove(&path);
        } else {
            published.insert(path, diagnostics);
        }
    }
}

fn text_of(
    path: &Path,
    docs: &HashMap<PathBuf, Doc>,
    workspaces: &[Workspace],
) -> Option<Arc<str>> {
    if let Some(doc) = docs.get(path) {
        return Some(Arc::clone(&doc.text));
    }
    workspaces
        .iter()
        .find_map(|workspace| workspace.analysis.vfs.read(path).ok())
}

/// askama points at where a node starts; an empty range draws nothing, so it is widened to
/// the word there, or one character.
fn visible(text: &str, range: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    if !range.is_empty() || range.start >= text.len() {
        return range.clone();
    }
    let rest = &text[range.start..];
    let word = rest
        .char_indices()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '_'))
        .map_or(rest.len(), |(at, _)| at);
    let len = if word > 0 {
        word
    } else {
        rest.chars().next().map_or(0, char::len_utf8)
    };
    range.start..range.start + len
}

fn apply_change(
    text: &mut String,
    change: lsp_types::TextDocumentContentChangeEvent,
    encoding: Encoding,
) {
    let Some(range) = change.range else {
        *text = change.text;
        return;
    };
    let lines = Lines::new(text, encoding);
    let (Some(start), Some(end)) = (lines.offset(range.start), lines.offset(range.end)) else {
        tracing::warn!(
            ?range,
            "a change outside the document; taking the whole text from the next one"
        );
        return;
    };
    if start <= end {
        text.replace_range(start..end, &change.text);
    }
}

fn file_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}
