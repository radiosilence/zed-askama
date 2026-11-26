//! The files the analysis reads: the editor's open buffers, over the disk.
//!
//! An unsaved buffer is the truth for its file, so askama's generator, the index and
//! rust-analyzer all read through here rather than from the disk.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama_analyzer_codegen::Files;

/// The largest file read from disk. Templates and Rust files are far smaller; the bound stops
/// a stray binary in a template directory from being read whole.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Open buffers by path, read before the disk.
#[derive(Debug, Default, Clone)]
pub struct Vfs {
    open: HashMap<PathBuf, Arc<str>>,
}

impl Vfs {
    pub fn set(&mut self, path: PathBuf, text: Arc<str>) {
        self.open.insert(path, text);
    }

    pub fn close(&mut self, path: &Path) {
        self.open.remove(path);
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.open.contains_key(path)
    }

    pub fn open_paths(&self) -> impl Iterator<Item = &Path> {
        self.open.keys().map(PathBuf::as_path)
    }

    /// The file's text: its open buffer, or what is on disk.
    pub fn read(&self, path: &Path) -> io::Result<Arc<str>> {
        match self.open.get(path) {
            Some(text) => Ok(Arc::clone(text)),
            None => read_disk(path).map(Arc::from),
        }
    }

    /// The open buffers as they are now, for work on another thread.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            open: self.open.clone(),
        }
    }
}

/// A frozen copy of the open buffers, read before the disk.
#[derive(Debug, Clone)]
pub struct Snapshot {
    open: HashMap<PathBuf, Arc<str>>,
}

impl Files for Snapshot {
    fn read(&self, path: &Path) -> io::Result<String> {
        match self.open.get(path) {
            Some(text) => Ok(text.to_string()),
            None => read_disk(path),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        self.open.contains_key(path) || path.is_file()
    }
}

fn read_disk(path: &Path) -> io::Result<String> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {size} bytes, over the {MAX_FILE_BYTES}-byte limit",
                path.display()
            ),
        ));
    }
    std::fs::read_to_string(path)
}
