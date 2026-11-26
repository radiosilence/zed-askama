//! A private rust-analyzer: the binary and settings the user runs for Rust, fed in-memory
//! overlays of the Rust files that define template items.
//!
//! Zed gives an extension no way to add documents to the rust-analyzer it runs for Rust, so the
//! server runs one of its own per workspace. It starts when that workspace's templates are
//! first needed and stops after [`IDLE`] without a question, since it holds the whole
//! workspace's analysis in memory.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::{LanguageServer, ServerSocket};
use lsp_types::notification::{
    LogMessage, Notification, Progress, PublishDiagnostics, ShowMessage,
};
use lsp_types::request::{
    CodeLensRefresh, DocumentDiagnosticRequest, InlayHintRefreshRequest, RegisterCapability,
    SemanticTokensRefresh, ShowMessageRequest, UnregisterCapability, WorkDoneProgressCreate,
    WorkspaceConfiguration, WorkspaceDiagnosticRefresh,
};
use lsp_types::{
    ClientCapabilities, CompletionClientCapabilities, CompletionItemCapability, Diagnostic,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentDiagnosticParams,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, GeneralClientCapabilities,
    GotoCapability, HoverClientCapabilities, InitializeParams, InitializedParams, MarkupKind,
    PublishDiagnosticsClientCapabilities, RenameClientCapabilities,
    SignatureHelpClientCapabilities, TextDocumentClientCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier, WindowClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceFolder,
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::process::Child;
use tokio::sync::{Notify, watch};
use tower::ServiceBuilder;

use crate::text::Encoding;

/// How long the private rust-analyzer may sit unasked before it is stopped. It holds the
/// workspace's analysis in memory, measured by prior art at 3.9 GB on a 49,000-line crate,
/// and restarting it costs seconds.
pub const IDLE: Duration = Duration::from_secs(10 * 60);

/// How long a request waits for rust-analyzer to finish loading before it answers without it.
pub const READY: Duration = Duration::from_secs(3);

/// How to run rust-analyzer, as the Zed extension resolves it from the user's settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Absent when the extension found no rust-analyzer: templates then get askama's own
    /// errors only.
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// The user's `lsp.rust-analyzer.initialization_options`.
    pub initialization_options: Option<Value>,
    /// The user's `lsp.rust-analyzer.settings`.
    pub settings: Option<Value>,
}

impl Config {
    /// From the server's initialization options. Unknown fields are ignored, since the
    /// extension and the server are released together but updated apart. A client that sends
    /// none, as an editor other than Zed may, gets `rust-analyzer` from the PATH.
    pub fn from_initialization_options(options: Option<&Value>) -> Self {
        match options.and_then(|options| options.get("rustAnalyzer")) {
            Some(value) => serde_json::from_value(value.clone()).unwrap_or_else(|err| {
                tracing::warn!(%err, "rustAnalyzer options are malformed; using rust-analyzer from the PATH");
                Self::from_path()
            }),
            None => Self::from_path(),
        }
    }

    fn from_path() -> Self {
        Self {
            path: Some(PathBuf::from("rust-analyzer")),
            ..Self::default()
        }
    }

    /// The user's settings with what this instance needs on top.
    fn settings(&self) -> Value {
        let mut settings = self
            .settings
            .clone()
            .or_else(|| self.initialization_options.clone())
            .unwrap_or_else(|| json!({}));
        merge(&mut settings, overrides());
        settings
    }
}

/// What the private instance changes from the user's settings, and why.
fn overrides() -> Value {
    json!({
        // The user's own rust-analyzer runs `cargo check`; this one answers about templates.
        "checkOnSave": false,
        // Its own target directory, so its builds never wait on the user's cargo lock.
        "cargo": { "targetDir": true },
        // An import or postfix edit would land in Rust code, not in the template.
        "completion": { "autoimport": { "enable": false }, "postfix": { "enable": false } },
        // It watches files itself: the server does not forward the editor's file events.
        "files": { "watcher": "server" },
        "lens": { "enable": false },
        // Unknown fields and methods are among rust-analyzer's experimental checks; they are
        // the errors a template most often has.
        "diagnostics": { "experimental": { "enable": true } },
    })
}

/// Merges `over` into `base`, object by object.
fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, over) => *base = over,
    }
}

/// Ends the connection's main loop. async-lsp panics in a loop whose last handle goes away
/// while it runs, so a session sends this when it is dropped.
struct Stop;

/// rust-analyzer's `experimental/serverStatus`, which says when it has finished loading.
enum ServerStatus {}

impl Notification for ServerStatus {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "experimental/serverStatus";
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatusParams {
    /// `ok`, `warning` or `error`: whether it loaded the workspace.
    health: String,
    quiescent: bool,
    message: Option<String>,
}

/// Each document's latest diagnostics, with the version they are for.
type Published = HashMap<Url, (Option<i32>, Vec<Diagnostic>)>;

/// What rust-analyzer tells the session between requests.
struct Heard {
    /// The latest diagnostics for each document, with the version they are for.
    diagnostics: Mutex<Published>,
    quiescent: watch::Sender<bool>,
    /// What rust-analyzer said was wrong when it last reported itself unhealthy.
    trouble: Mutex<Option<String>>,
    /// Wakes the lane, so new diagnostics are published.
    wake: Arc<Notify>,
}

/// A running private rust-analyzer.
pub struct Session {
    server: ServerSocket,
    /// Killed when the session is dropped.
    _child: Child,
    heard: Arc<Heard>,
    quiescent: watch::Receiver<bool>,
    /// The version of each document the session has opened.
    open: HashMap<Url, i32>,
    pub last_used: tokio::time::Instant,
}

impl Session {
    /// Starts rust-analyzer on the workspace at `root`.
    pub async fn start(
        root: &Path,
        config: &Config,
        encoding: Encoding,
        wake: Arc<Notify>,
    ) -> anyhow::Result<Self> {
        let path = config
            .path
            .as_ref()
            .context("no rust-analyzer is configured")?;
        let mut child = tokio::process::Command::new(path)
            .args(&config.args)
            .envs(&config.env)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", path.display()))?;
        let stdin = child.stdin.take().context("rust-analyzer's stdin")?;
        let stdout = child.stdout.take().context("rust-analyzer's stdout")?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "rust_analyzer", "{line}");
                }
            });
        }

        let (quiescent_tx, quiescent) = watch::channel(false);
        let heard = Arc::new(Heard {
            diagnostics: Mutex::new(HashMap::new()),
            quiescent: quiescent_tx,
            trouble: Mutex::new(None),
            wake,
        });
        let settings = config.settings();
        let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_| {
            ServiceBuilder::new()
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(client_router(Arc::clone(&heard), settings.clone()))
        });
        tokio::spawn(async move {
            let stdout = tokio_util::compat::TokioAsyncReadCompatExt::compat(stdout);
            let stdin = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(stdin);
            if let Err(err) = mainloop.run_buffered(stdout, stdin).await {
                tracing::info!(%err, "rust-analyzer's connection ended");
            }
        });

        let root_uri = Url::from_file_path(root)
            .map_err(|()| anyhow!("{} is not absolute", root.display()))?;
        let initialized = server
            .initialize(InitializeParams {
                process_id: Some(std::process::id()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: root.file_name().map_or_else(
                        || "workspace".to_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                }]),
                capabilities: capabilities(encoding),
                initialization_options: Some(settings),
                ..InitializeParams::default()
            })
            .await
            .context("initialising rust-analyzer")?;
        let negotiated = initialized.capabilities.position_encoding;
        if negotiated
            .as_ref()
            .is_some_and(|kind| *kind != encoding.kind())
            || negotiated.is_none() && encoding != Encoding::Utf16
        {
            return Err(anyhow!(
                "rust-analyzer chose position encoding {negotiated:?}, not {:?}",
                encoding.kind()
            ));
        }
        server.initialized(InitializedParams {})?;
        Ok(Self {
            server,
            _child: child,
            heard,
            quiescent,
            open: HashMap::new(),
            last_used: tokio::time::Instant::now(),
        })
    }

    /// Sends a document's current text, opening it the first time, and returns its version.
    pub fn sync(&mut self, path: &Path, text: &str) -> anyhow::Result<i32> {
        let uri = Url::from_file_path(path)
            .map_err(|()| anyhow!("{} is not absolute", path.display()))?;
        match self.open.get_mut(&uri) {
            Some(version) => {
                *version += 1;
                let version = *version;
                self.server.did_change(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier { uri, version },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: text.to_owned(),
                    }],
                })?;
                Ok(version)
            }
            None => {
                self.server.did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: "rust".to_owned(),
                        version: 1,
                        text: text.to_owned(),
                    },
                })?;
                self.open.insert(uri, 1);
                Ok(1)
            }
        }
    }

    /// Waits until rust-analyzer has loaded the workspace, for at most `limit`.
    pub async fn ready(&mut self, limit: Duration) -> bool {
        self.last_used = tokio::time::Instant::now();
        tokio::time::timeout(limit, self.quiescent.wait_for(|quiescent| *quiescent))
            .await
            .is_ok_and(|waited| waited.is_ok())
    }

    /// The diagnostics rust-analyzer last published for a document, and the version they
    /// are for.
    pub fn diagnostics(&self, path: &Path) -> Option<(Option<i32>, Vec<Diagnostic>)> {
        let uri = Url::from_file_path(path).ok()?;
        self.heard.diagnostics.lock().get(&uri).cloned()
    }

    /// Whether rust-analyzer has loaded the workspace.
    pub fn is_ready(&self) -> bool {
        *self.quiescent.borrow()
    }

    /// Asks for a document's diagnostics now, rather than waiting for them to be pushed, and
    /// keeps them as the latest for the document's current version.
    pub async fn pull(&mut self, path: &Path) -> Option<Vec<Diagnostic>> {
        let uri = Url::from_file_path(path).ok()?;
        let version = *self.open.get(&uri)?;
        let result = self
            .server
            .request::<DocumentDiagnosticRequest>(DocumentDiagnosticParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                identifier: None,
                previous_result_id: None,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await
            .map_err(|err| tracing::debug!(%err, "pulling diagnostics"))
            .ok()?;
        let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(report)) = result
        else {
            return None;
        };
        let items = report.full_document_diagnostic_report.items;
        self.heard
            .diagnostics
            .lock()
            .insert(uri, (Some(version), items.clone()));
        Some(items)
    }

    /// What rust-analyzer last said was wrong with loading the workspace, if anything. While
    /// it cannot load the workspace, types are unknown and hover, completion and errors suffer.
    pub fn trouble(&self) -> Option<String> {
        self.heard.trouble.lock().clone()
    }

    /// The connection, for requests.
    pub fn server(&mut self) -> ServerSocket {
        self.last_used = tokio::time::Instant::now();
        self.server.clone()
    }
}

/// What the session tells rust-analyzer it can do: what the server forwards to Zed, in the
/// encoding agreed with Zed, so positions in files the server does not rewrite pass through.
fn capabilities(encoding: Encoding) -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![encoding.kind()]),
            ..GeneralClientCapabilities::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..WorkspaceClientCapabilities::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..WindowClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    documentation_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                    ..CompletionItemCapability::default()
                }),
                ..CompletionClientCapabilities::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                ..HoverClientCapabilities::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities::default()),
            definition: Some(GotoCapability {
                link_support: Some(false),
                ..GotoCapability::default()
            }),
            type_definition: Some(GotoCapability {
                link_support: Some(false),
                ..GotoCapability::default()
            }),
            rename: Some(RenameClientCapabilities {
                prepare_support: Some(true),
                ..RenameClientCapabilities::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                version_support: Some(true),
                ..PublishDiagnosticsClientCapabilities::default()
            }),
            ..TextDocumentClientCapabilities::default()
        }),
        experimental: Some(json!({ "serverStatusNotification": true })),
    }
}

fn client_router(heard: Arc<Heard>, settings: Value) -> Router<Arc<Heard>> {
    let mut router = Router::new(heard);
    router
        .request::<WorkspaceConfiguration, _>(move |_, params| {
            let answers = params.items.iter().map(|_| settings.clone()).collect();
            async move { Ok(answers) }
        })
        .request::<WorkDoneProgressCreate, _>(|_, _| async { Ok(()) })
        .request::<RegisterCapability, _>(|_, _| async { Ok(()) })
        .request::<UnregisterCapability, _>(|_, _| async { Ok(()) })
        .request::<SemanticTokensRefresh, _>(|_, ()| async { Ok(()) })
        .request::<InlayHintRefreshRequest, _>(|_, ()| async { Ok(()) })
        .request::<CodeLensRefresh, _>(|_, ()| async { Ok(()) })
        .request::<WorkspaceDiagnosticRefresh, _>(|_, ()| async { Ok(()) })
        .request::<ShowMessageRequest, _>(|_, params| {
            tracing::info!(target: "rust_analyzer", "{}", params.message);
            async { Ok(None) }
        })
        .notification::<PublishDiagnostics>(|heard, params| {
            heard
                .diagnostics
                .lock()
                .insert(params.uri, (params.version, params.diagnostics));
            heard.wake.notify_one();
            ControlFlow::Continue(())
        })
        .notification::<ServerStatus>(|heard, params| {
            *heard.trouble.lock() = (params.health != "ok").then(|| {
                params.message.unwrap_or_else(|| {
                    format!("rust-analyzer reports its health as {}", params.health)
                })
            });
            heard.quiescent.send_replace(params.quiescent);
            if params.quiescent {
                heard.wake.notify_one();
            }
            ControlFlow::Continue(())
        })
        .notification::<ShowMessage>(|_, params| {
            tracing::info!(target: "rust_analyzer", "{}", params.message);
            ControlFlow::Continue(())
        })
        .notification::<LogMessage>(|_, params| {
            tracing::debug!(target: "rust_analyzer", "{}", params.message);
            ControlFlow::Continue(())
        })
        .notification::<Progress>(|_, _| ControlFlow::Continue(()))
        .unhandled_notification(|_, _| ControlFlow::Continue(()))
        .event::<Stop>(|_, _| ControlFlow::Break(Ok(())));
    router
}

impl Drop for Session {
    fn drop(&mut self) {
        // Ends the main loop before its last handle goes; the process is killed after.
        let _ = self.server.emit(Stop);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Config, merge};

    #[test]
    fn overrides_win_over_the_users_settings_and_keep_the_rest() {
        let config = Config {
            settings: Some(json!({ "checkOnSave": true, "cargo": { "features": "all" } })),
            ..Config::default()
        };
        let settings = config.settings();
        assert_eq!(
            settings["checkOnSave"],
            json!(false),
            "the private instance never checks on save"
        );
        assert_eq!(
            settings["cargo"]["features"],
            json!("all"),
            "the user's cargo features are kept"
        );
        assert_eq!(
            settings["cargo"]["targetDir"],
            json!(true),
            "and its own target directory added"
        );
    }

    #[test]
    fn options_without_a_path_mean_no_rust_analyzer() {
        let options = json!({ "rustAnalyzer": { "args": [], "env": {} } });
        let config = Config::from_initialization_options(Some(&options));
        assert_eq!(
            config.path, None,
            "the extension found none, so none is run"
        );
    }

    #[test]
    fn no_options_at_all_mean_rust_analyzer_from_the_path() {
        let config = Config::from_initialization_options(None);
        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("rust-analyzer"))
        );
    }

    #[test]
    fn merging_replaces_leaves_and_recurses_into_objects() {
        let mut base = json!({ "a": { "b": 1, "c": 2 }, "d": [1] });
        merge(&mut base, json!({ "a": { "b": 3 }, "d": [2] }));
        assert_eq!(base, json!({ "a": { "b": 3, "c": 2 }, "d": [2] }));
    }
}
