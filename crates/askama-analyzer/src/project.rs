//! The Cargo packages that use askama, and their Rust files.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// A workspace member that depends on askama.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    /// The directory holding its `Cargo.toml`, where askama looks for `askama.toml` and
    /// `templates/`.
    pub manifest_dir: PathBuf,
    /// The directories holding its targets' sources, walked for Rust files.
    pub source_dirs: Vec<PathBuf>,
}

/// The members of the Cargo workspace at `root` that depend on askama. Reads the manifests
/// only (`--no-deps --offline`), so it needs no network and resolves nothing.
pub fn discover(root: &Path) -> anyhow::Result<Vec<Package>> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(root)
        .no_deps()
        .other_options(vec!["--offline".to_owned()])
        .exec()
        .with_context(|| format!("reading the Cargo workspace at {}", root.display()))?;
    let mut packages = Vec::new();
    for package in metadata.workspace_packages() {
        if !package
            .dependencies
            .iter()
            .any(|dependency| dependency.name == "askama")
        {
            continue;
        }
        let Some(manifest_dir) = package.manifest_path.parent() else {
            continue;
        };
        let mut source_dirs: Vec<PathBuf> = Vec::new();
        for target in &package.targets {
            let Some(dir) = target.src_path.parent() else {
                continue;
            };
            let dir = dir.as_std_path().to_path_buf();
            if source_dirs.iter().any(|known| dir.starts_with(known)) {
                continue;
            }
            source_dirs.retain(|known| !known.starts_with(&dir));
            source_dirs.push(dir);
        }
        packages.push(Package {
            name: package.name.to_string(),
            manifest_dir: manifest_dir.as_std_path().to_path_buf(),
            source_dirs,
        });
    }
    Ok(packages)
}

/// Deeper than any real source tree; it bounds a walk that something unexpected has made
/// recursive.
const MAX_DEPTH: usize = 16;

/// The package's Rust files. Symbolic links are not followed, so the walk cannot loop.
pub fn rust_files(package: &Package) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in &package.source_dirs {
        walk(dir, 0, &mut files);
    }
    files.sort();
    files
}

fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(&path, depth + 1, files);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}
