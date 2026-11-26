//! The Zed extension for Askama templates.
//!
//! The languages, queries and snippets are data in this directory; this crate
//! is the part that runs. It chooses the `askama-analyzer` binary Zed starts
//! and the rust-analyzer the server drives, and passes the user's settings
//! through. Zed talks to the server directly once it has started, so nothing
//! here sits between them.
//!
//! Both binaries are found the same way: the path the user configured, then
//! one on the worktree's `PATH`, then the latest GitHub release, downloaded
//! into the extension's work directory. A failed check for a release falls back
//! to the newest copy already downloaded, so the server starts offline.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;

use zed_extension_api::serde_json::{Map, Value, json};
use zed_extension_api::settings::{CommandSettings, LspSettings};
use zed_extension_api::{
    self as zed, Architecture, Command, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
    LanguageServerInstallationStatus, Os, Worktree,
};

/// The server's name in `extension.toml` and in the user's `lsp` settings.
const SERVER: &str = "askama-analyzer";
const SERVER_REPOSITORY: &str = "radiosilence/zed-askama";
/// rust-analyzer's name in the user's `lsp` settings, which Zed's Rust support
/// reads too, so both instances run the same binary with the same options.
const RUST_ANALYZER: &str = "rust-analyzer";
const RUST_ANALYZER_REPOSITORY: &str = "rust-lang/rust-analyzer";

/// A binary published as a GitHub release asset.
#[derive(Debug, PartialEq, Eq)]
struct Asset {
    /// The asset's file name on the release.
    name: String,
    file_type: DownloadedFileType,
    /// The executable's file name, inside the archive or as the unpacked file.
    executable: &'static str,
}

/// The `askama-analyzer` archive the release pipeline builds for a platform.
fn server_asset(os: Os, architecture: Architecture) -> zed::Result<Asset> {
    let target = match (os, architecture) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-musl",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-musl",
        (Os::Windows, Architecture::Aarch64) => "aarch64-pc-windows-msvc",
        (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
        (os, architecture) => {
            return Err(format!("{SERVER} has no build for {os:?} on {architecture:?}"));
        }
    };
    Ok(match os {
        Os::Windows => Asset {
            name: format!("{SERVER}-{target}.zip"),
            file_type: DownloadedFileType::Zip,
            executable: "askama-analyzer.exe",
        },
        Os::Mac | Os::Linux => Asset {
            name: format!("{SERVER}-{target}.tar.gz"),
            file_type: DownloadedFileType::GzipTar,
            executable: "askama-analyzer",
        },
    })
}

/// The rust-analyzer build Zed itself downloads for a platform.
fn rust_analyzer_asset(os: Os, architecture: Architecture) -> zed::Result<Asset> {
    let target = match (os, architecture) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (Os::Windows, Architecture::Aarch64) => "aarch64-pc-windows-msvc",
        (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
        (os, architecture) => {
            return Err(format!("rust-analyzer has no build for {os:?} on {architecture:?}"));
        }
    };
    Ok(match os {
        Os::Windows => Asset {
            name: format!("{RUST_ANALYZER}-{target}.zip"),
            file_type: DownloadedFileType::Zip,
            executable: "rust-analyzer.exe",
        },
        Os::Mac | Os::Linux => Asset {
            name: format!("{RUST_ANALYZER}-{target}.gz"),
            file_type: DownloadedFileType::Gzip,
            executable: "rust-analyzer",
        },
    })
}

/// Compares two release versions part by part, numerically where both parts
/// are numbers: semantic versions (`0.10.0` after `0.9.1`) and rust-analyzer's
/// dated releases (`2026-11-23`) both order correctly.
fn compare_versions(left: &str, right: &str) -> Ordering {
    let parts = |version: &str| {
        version
            .split(['.', '-'])
            .map(|part| part.parse::<u64>().map_err(|_| part.to_owned()))
            .collect::<Vec<_>>()
    };
    let (left, right) = (parts(left), parts(right));
    for (l, r) in left.iter().zip(&right) {
        let order = match (l, r) {
            (Ok(l), Ok(r)) => l.cmp(r),
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            (Err(l), Err(r)) => l.cmp(r),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

/// The newest version among directories named `<prefix>-<version>`.
fn newest_version<'a>(prefix: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names
        .into_iter()
        .filter_map(|name| name.strip_prefix(prefix)?.strip_prefix('-'))
        .filter(|version| !version.is_empty())
        .max_by(|left, right| compare_versions(left, right))
}

/// The server's environment: the worktree's shell environment, so it finds
/// `cargo` and `rust-analyzer` as the user's shell would, with the variables
/// the user configured taking precedence.
fn merge_env(shell: Vec<(String, String)>, configured: Option<HashMap<String, String>>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = shell;
    for (name, value) in configured.into_iter().flatten() {
        match env.iter_mut().find(|(existing, _)| *existing == name) {
            Some(entry) => entry.1 = value,
            None => env.push((name, value)),
        }
    }
    env
}

/// The rust-analyzer the server should drive, as the contract with the server
/// spells it. `path` is absent when none could be found or downloaded, in
/// which case the server offers only what it can do without rust-analyzer.
fn rust_analyzer_options(path: Option<String>, settings: LspSettings) -> Value {
    let binary = settings.binary;
    let mut options = Map::new();
    if let Some(path) = path {
        options.insert("path".into(), Value::String(path));
    }
    let args = binary.as_ref().and_then(|binary| binary.arguments.clone()).unwrap_or_default();
    options.insert("args".into(), json!(args));
    let env = binary.and_then(|binary| binary.env).unwrap_or_default();
    options.insert("env".into(), json!(env));
    if let Some(initialization_options) = settings.initialization_options {
        options.insert("initializationOptions".into(), initialization_options);
    }
    if let Some(settings) = settings.settings {
        options.insert("settings".into(), settings);
    }
    Value::Object(options)
}

/// The user's own initialization options for the server, with `rustAnalyzer`
/// added or replaced. Options that are not an object are replaced, since the
/// server reads an object.
fn initialization_options(configured: Option<Value>, rust_analyzer: Value) -> Value {
    let mut options = match configured {
        Some(Value::Object(options)) => options,
        _ => Map::new(),
    };
    options.insert("rustAnalyzer".into(), rust_analyzer);
    Value::Object(options)
}

/// A binary from a GitHub release, kept in the work directory as
/// `<prefix>-<version>/<executable>`.
struct Download {
    prefix: &'static str,
    repository: &'static str,
    asset: Asset,
}

impl Download {
    fn executable(&self, version: &str) -> String {
        format!("{}-{version}/{}", self.prefix, self.asset.executable)
    }

    /// The newest downloaded version with its executable in place.
    fn newest_cached(&self) -> Option<String> {
        let entries = fs::read_dir(".").ok()?;
        let names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        let complete = names
            .iter()
            .map(String::as_str)
            .filter(|name| fs::metadata(format!("{name}/{}", self.asset.executable)).is_ok_and(|m| m.is_file()));
        let version = newest_version(self.prefix, complete)?;
        Some(self.executable(version))
    }

    /// The latest release's executable, downloading it if it is not here yet.
    /// When the release cannot be checked, or has no build for this platform,
    /// the newest copy already downloaded.
    fn fetch(&self, server: &LanguageServerId) -> zed::Result<String> {
        zed::set_language_server_installation_status(server, &LanguageServerInstallationStatus::CheckingForUpdate);
        let options = GithubReleaseOptions { require_assets: true, pre_release: false };
        let release = zed::latest_github_release(self.repository, options).and_then(|release| {
            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name == self.asset.name)
                .ok_or_else(|| format!("{} {} has no asset {}", self.repository, release.version, self.asset.name))?;
            Ok((release.version.trim_start_matches('v').to_owned(), asset.download_url.clone()))
        });
        let (version, url) = match release {
            Ok(found) => found,
            Err(error) => {
                return self.newest_cached().ok_or_else(|| {
                    format!("cannot find a {} release for this platform, and none is downloaded: {error}", self.prefix)
                });
            }
        };

        let executable = self.executable(&version);
        if fs::metadata(&executable).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(executable);
        }

        zed::set_language_server_installation_status(server, &LanguageServerInstallationStatus::Downloading);
        let directory = format!("{}-{version}", self.prefix);
        fs::create_dir_all(&directory).map_err(|error| format!("creating {directory}: {error}"))?;
        let destination = match self.asset.file_type {
            DownloadedFileType::Gzip | DownloadedFileType::Uncompressed => &executable,
            DownloadedFileType::GzipTar | DownloadedFileType::Zip => &directory,
        };
        if let Err(error) = zed::download_file(&url, destination, self.asset.file_type) {
            let _ = fs::remove_dir_all(&directory);
            return Err(format!("downloading {url}: {error}"));
        }
        zed::make_file_executable(&executable)?;

        // Older versions go only once the new one is in place.
        for entry in fs::read_dir(".").into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_older = name
                .strip_prefix(self.prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|other| other != version);
            if is_older {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(executable)
    }
}

/// The absolute path of a file in the extension's work directory, which is the
/// process's current directory.
fn absolute(relative: &str) -> zed::Result<String> {
    let directory = std::env::current_dir().map_err(|error| format!("reading the work directory: {error}"))?;
    Ok(directory.join(relative).to_string_lossy().into_owned())
}

#[derive(Default)]
struct AskamaExtension {
    /// The downloaded server, once found in this session.
    server: Option<String>,
    /// The downloaded rust-analyzer, once found in this session.
    rust_analyzer: Option<String>,
}

impl AskamaExtension {
    fn downloaded_server(&mut self, server: &LanguageServerId) -> zed::Result<String> {
        if let Some(path) = self.server.as_ref().filter(|path| fs::metadata(path).is_ok()) {
            return Ok(path.clone());
        }
        let (os, architecture) = zed::current_platform();
        let download = Download {
            prefix: SERVER,
            repository: SERVER_REPOSITORY,
            asset: server_asset(os, architecture)?,
        };
        let path = absolute(&download.fetch(server)?)?;
        self.server = Some(path.clone());
        Ok(path)
    }

    /// The rust-analyzer the user configured, the one on the worktree's `PATH`,
    /// or the latest release. `None` when none of them is available.
    fn rust_analyzer_path(
        &mut self,
        server: &LanguageServerId,
        worktree: &Worktree,
        binary: Option<&CommandSettings>,
    ) -> Option<String> {
        if let Some(path) = binary.and_then(|binary| binary.path.clone()) {
            return Some(path);
        }
        if let Some(path) = worktree.which(RUST_ANALYZER) {
            return Some(path);
        }
        if let Some(path) = self.rust_analyzer.as_ref().filter(|path| fs::metadata(path).is_ok()) {
            return Some(path.clone());
        }
        let (os, architecture) = zed::current_platform();
        let found = rust_analyzer_asset(os, architecture).and_then(|asset| {
            let download = Download { prefix: RUST_ANALYZER, repository: RUST_ANALYZER_REPOSITORY, asset };
            absolute(&download.fetch(server)?)
        });
        match found {
            Ok(path) => {
                self.rust_analyzer = Some(path.clone());
                Some(path)
            }
            Err(error) => {
                zed::set_language_server_installation_status(
                    server,
                    &LanguageServerInstallationStatus::Failed(format!(
                        "no rust-analyzer: {error}; set lsp.rust-analyzer.binary.path or put rust-analyzer on PATH"
                    )),
                );
                None
            }
        }
    }
}

impl zed::Extension for AskamaExtension {
    fn new() -> Self {
        Self::default()
    }

    fn language_server_command(&mut self, server: &LanguageServerId, worktree: &Worktree) -> zed::Result<Command> {
        let binary = LspSettings::for_worktree(SERVER, worktree).ok().and_then(|settings| settings.binary);
        let (path, args, env) = match binary {
            Some(binary) => (binary.path, binary.arguments, binary.env),
            None => (None, None, None),
        };
        let command = match path.or_else(|| worktree.which(SERVER)) {
            Some(command) => command,
            None => match self.downloaded_server(server) {
                Ok(command) => command,
                Err(error) => {
                    zed::set_language_server_installation_status(
                        server,
                        &LanguageServerInstallationStatus::Failed(error.clone()),
                    );
                    return Err(error);
                }
            },
        };
        zed::set_language_server_installation_status(server, &LanguageServerInstallationStatus::None);
        Ok(Command {
            command,
            args: args.unwrap_or_default(),
            env: merge_env(worktree.shell_env(), env),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        server: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<Option<Value>> {
        let configured = LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.initialization_options);
        let rust_analyzer = LspSettings::for_worktree(RUST_ANALYZER, worktree).unwrap_or_default();
        let path = self.rust_analyzer_path(server, worktree, rust_analyzer.binary.as_ref());
        Ok(Some(initialization_options(configured, rust_analyzer_options(path, rust_analyzer))))
    }

    fn language_server_workspace_configuration(
        &mut self,
        _server: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<Option<Value>> {
        Ok(LspSettings::for_worktree(SERVER, worktree)
            .ok()
            .and_then(|settings| settings.settings))
    }
}

zed::register_extension!(AskamaExtension);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_platform_maps_to_the_archive_the_release_builds() {
        let cases = [
            (Os::Mac, Architecture::Aarch64, "askama-analyzer-aarch64-apple-darwin.tar.gz"),
            (Os::Mac, Architecture::X8664, "askama-analyzer-x86_64-apple-darwin.tar.gz"),
            (Os::Linux, Architecture::Aarch64, "askama-analyzer-aarch64-unknown-linux-musl.tar.gz"),
            (Os::Linux, Architecture::X8664, "askama-analyzer-x86_64-unknown-linux-musl.tar.gz"),
            (Os::Windows, Architecture::Aarch64, "askama-analyzer-aarch64-pc-windows-msvc.zip"),
            (Os::Windows, Architecture::X8664, "askama-analyzer-x86_64-pc-windows-msvc.zip"),
        ];
        for (os, architecture, name) in cases {
            let asset = server_asset(os, architecture).expect("the platform is supported");
            assert_eq!(asset.name, name, "wrong archive for {os:?} on {architecture:?}");
        }
    }

    #[test]
    fn windows_archives_are_zips_holding_an_exe_and_the_rest_tarballs() {
        let windows = server_asset(Os::Windows, Architecture::X8664).expect("supported");
        assert_eq!((windows.file_type, windows.executable), (DownloadedFileType::Zip, "askama-analyzer.exe"));
        let linux = server_asset(Os::Linux, Architecture::X8664).expect("supported");
        assert_eq!((linux.file_type, linux.executable), (DownloadedFileType::GzipTar, "askama-analyzer"));
    }

    #[test]
    fn a_32_bit_platform_has_no_server_build() {
        assert!(server_asset(Os::Linux, Architecture::X86).is_err(), "x86 should have no build");
    }

    #[test]
    fn rust_analyzer_downloads_match_its_release_assets() {
        let mac = rust_analyzer_asset(Os::Mac, Architecture::Aarch64).expect("supported");
        assert_eq!(mac.name, "rust-analyzer-aarch64-apple-darwin.gz");
        assert_eq!(mac.file_type, DownloadedFileType::Gzip);
        let linux = rust_analyzer_asset(Os::Linux, Architecture::X8664).expect("supported");
        assert_eq!(linux.name, "rust-analyzer-x86_64-unknown-linux-gnu.gz");
        let windows = rust_analyzer_asset(Os::Windows, Architecture::Aarch64).expect("supported");
        assert_eq!(
            (windows.name.as_str(), windows.executable),
            ("rust-analyzer-aarch64-pc-windows-msvc.zip", "rust-analyzer.exe")
        );
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert_eq!(compare_versions("0.10.0", "0.9.1"), Ordering::Greater);
        assert_eq!(compare_versions("2026-11-23", "2026-02-01"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn the_newest_cached_version_ignores_other_prefixes() {
        let names = ["askama-analyzer-0.9.0", "askama-analyzer-0.10.0", "rust-analyzer-2026-11-23", "askama-analyzer-"];
        assert_eq!(newest_version("askama-analyzer", names), Some("0.10.0"));
        assert_eq!(newest_version("rust-analyzer", names), Some("2026-11-23"));
        assert_eq!(newest_version("askama-analyzer", ["unrelated"]), None);
    }

    #[test]
    fn configured_variables_override_the_shell_environment() {
        let shell = vec![("PATH".to_owned(), "/usr/bin".to_owned()), ("HOME".to_owned(), "/home/u".to_owned())];
        let configured = HashMap::from([("PATH".to_owned(), "/opt/bin".to_owned()), ("RUST_LOG".to_owned(), "debug".to_owned())]);
        let env = merge_env(shell, Some(configured));
        let lookup = |name: &str| env.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        assert_eq!(lookup("PATH"), Some("/opt/bin"), "the configured PATH should win");
        assert_eq!(lookup("HOME"), Some("/home/u"), "shell variables should be kept");
        assert_eq!(lookup("RUST_LOG"), Some("debug"), "new variables should be added");
        assert_eq!(env.len(), 3, "no variable should appear twice");
    }

    #[test]
    fn rust_analyzer_options_carry_the_users_binary_and_settings() {
        let settings = LspSettings {
            binary: Some(CommandSettings {
                path: Some("/opt/ra".to_owned()),
                arguments: Some(vec!["--log".to_owned()]),
                env: Some(HashMap::from([("RA_LOG".to_owned(), "info".to_owned())])),
            }),
            initialization_options: Some(json!({ "cargo": { "features": "all" } })),
            settings: Some(json!({ "checkOnSave": false })),
        };
        let options = rust_analyzer_options(Some("/opt/ra".to_owned()), settings);
        assert_eq!(
            options,
            json!({
                "path": "/opt/ra",
                "args": ["--log"],
                "env": { "RA_LOG": "info" },
                "initializationOptions": { "cargo": { "features": "all" } },
                "settings": { "checkOnSave": false },
            })
        );
    }

    #[test]
    fn unset_rust_analyzer_options_are_omitted_and_lists_default_empty() {
        let options = rust_analyzer_options(None, LspSettings::default());
        assert_eq!(options, json!({ "args": [], "env": {} }), "path and unset options should be absent");
    }

    #[test]
    fn the_users_initialization_options_keep_their_keys_and_gain_rust_analyzer() {
        let configured = json!({ "trace": true, "rustAnalyzer": "stale" });
        let options = initialization_options(Some(configured), json!({ "path": "/ra" }));
        assert_eq!(options, json!({ "trace": true, "rustAnalyzer": { "path": "/ra" } }));
    }

    #[test]
    fn initialization_options_that_are_not_an_object_are_replaced() {
        let options = initialization_options(Some(json!("odd")), json!({}));
        assert_eq!(options, json!({ "rustAnalyzer": {} }));
        assert_eq!(initialization_options(None, json!({})), json!({ "rustAnalyzer": {} }));
    }
}
