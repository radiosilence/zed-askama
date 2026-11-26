//! One Cargo workspace's templates: the packages that use askama, the items deriving
//! `Template` in their Rust files, what askama generated for each, and the problems that come
//! out of it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use askama_analyzer_codegen::{Files, Generated, Input, Origin};

use crate::generator::Generator;
use crate::index::{self, TemplateItem};
use crate::project::{self, Package};
use crate::vfs::Vfs;

/// An error askama reports for a template, where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    /// A byte range of the file's text as the analysis read it.
    pub range: Range<usize>,
    pub message: String,
}

/// In file order, so a file's problems print and publish top to bottom.
impl Ord for Problem {
    fn cmp(&self, other: &Self) -> Ordering {
        (&self.path, self.range.start, self.range.end, &self.message).cmp(&(
            &other.path,
            other.range.start,
            other.range.end,
            &other.message,
        ))
    }
}

impl PartialOrd for Problem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An item deriving `Template`, and what askama generated for it last.
#[derive(Debug)]
pub struct Item {
    pub found: TemplateItem,
    /// `None` until generated; an `Err` when the generator itself failed, as opposed to
    /// askama reporting an error, which is in [`Generated::errors`].
    pub generated: Option<Result<Generated, String>>,
}

/// A Rust file that defines template items.
#[derive(Debug)]
pub struct RustFile {
    package: usize,
    /// The text the items' ranges refer to.
    pub text: Arc<str>,
    pub items: Vec<Item>,
}

pub struct Analysis {
    root: PathBuf,
    pub packages: Vec<Package>,
    pub vfs: Vfs,
    generator: Generator,
    files: BTreeMap<PathBuf, RustFile>,
}

impl Analysis {
    /// Finds the packages under `root` that use askama, indexes their Rust files and generates
    /// every template item.
    pub async fn load(root: &Path, generator: Generator, vfs: Vfs) -> anyhow::Result<Self> {
        let mut analysis = Self {
            root: root.to_owned(),
            packages: Vec::new(),
            vfs,
            generator,
            files: BTreeMap::new(),
        };
        analysis.reload().await?;
        Ok(analysis)
    }

    /// Discovers the packages again and rebuilds everything, as after a manifest changes.
    pub async fn reload(&mut self) -> anyhow::Result<()> {
        let root = self.root.clone();
        self.packages = tokio::task::spawn_blocking(move || project::discover(&root)).await??;
        self.files.clear();
        for package in 0..self.packages.len() {
            let found = self.packages[package].clone();
            let paths = tokio::task::spawn_blocking(move || project::rust_files(&found)).await?;
            for path in paths {
                self.index(&path, package).await;
            }
        }
        let paths: Vec<PathBuf> = self.files.keys().cloned().collect();
        for path in paths {
            self.generate_file(&path).await;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The Rust files that define template items.
    pub fn files(&self) -> impl Iterator<Item = (&Path, &RustFile)> {
        self.files.iter().map(|(path, file)| (path.as_path(), file))
    }

    pub fn file(&self, path: &Path) -> Option<&RustFile> {
        self.files.get(path)
    }

    /// Every template file askama read for some item.
    pub fn templates(&self) -> Vec<PathBuf> {
        let mut templates: Vec<PathBuf> = self
            .files
            .values()
            .flat_map(|file| &file.items)
            .filter_map(|item| item.generated.as_ref()?.as_ref().ok())
            .flat_map(|generated| generated.templates.iter().map(|path| path.to_path_buf()))
            .collect();
        templates.sort();
        templates.dedup();
        templates
    }

    /// The items that read `template`, as their file and index, those whose generation
    /// succeeded first.
    pub fn readers(&self, template: &Path) -> Vec<(PathBuf, usize)> {
        let mut readers: Vec<(PathBuf, usize, bool)> = Vec::new();
        for (path, file) in &self.files {
            for (at, item) in file.items.iter().enumerate() {
                if let Some(Ok(generated)) = &item.generated
                    && generated.templates.iter().any(|read| **read == *template)
                {
                    readers.push((path.clone(), at, generated.errors.is_empty()));
                }
            }
        }
        readers.sort_by_key(|(_, _, clean)| !clean);
        readers
            .into_iter()
            .map(|(path, at, _)| (path, at))
            .collect()
    }

    /// What an item generates with `template` read as `text` rather than as it is: the probe a
    /// completion asks about.
    pub async fn probe(
        &self,
        rust_file: &Path,
        item: usize,
        template: &Path,
        text: Arc<str>,
    ) -> Option<Generated> {
        let file = self.files.get(rust_file)?;
        let item_text = file.text[file.items.get(item)?.found.range.clone()].to_owned();
        let manifest_dir = self.packages[file.package].manifest_dir.clone();
        let mut vfs = self.vfs.clone();
        vfs.set(template.to_owned(), text);
        let snapshot = vfs.snapshot();
        let rust_file = rust_file.to_owned();
        self.generator
            .run(move || {
                let files: Rc<dyn Files> = Rc::new(snapshot);
                askama_analyzer_codegen::generate(
                    Input {
                        item: &item_text,
                        file: &rust_file,
                        manifest_dir: &manifest_dir,
                    },
                    files,
                )
                .ok()
            })
            .await
            .ok()
            .flatten()
    }

    /// How many items derive `Template`.
    pub fn item_count(&self) -> usize {
        self.files.values().map(|file| file.items.len()).sum()
    }

    /// Applies changed files: Rust files are indexed and generated again, templates regenerate
    /// the items that read them, and a manifest or `askama.toml` reloads everything. `changed`
    /// paths must already hold their new text in [`Analysis::vfs`].
    pub async fn apply(&mut self, changed: &BTreeSet<PathBuf>) -> anyhow::Result<()> {
        if changed.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name == "Cargo.toml" || name == "askama.toml")
        }) {
            return self.reload().await;
        }
        let mut regenerate: BTreeSet<PathBuf> = BTreeSet::new();
        for path in changed {
            if path.extension().is_some_and(|ext| ext == "rs") {
                if let Some(package) = self.package_of(path) {
                    self.index(path, package).await;
                    regenerate.insert(path.clone());
                }
                continue;
            }
            let owners = self.owners(path);
            if owners.is_empty() {
                // A template no item has read: perhaps new, or read by an item that failed
                // before reaching it. Those items get another try.
                regenerate.extend(self.failing_files());
            } else {
                regenerate.extend(owners);
            }
        }
        for path in regenerate {
            self.generate_file(&path).await;
        }
        Ok(())
    }

    /// The Rust files whose items read `template`.
    pub fn owners(&self, template: &Path) -> BTreeSet<PathBuf> {
        self.files
            .iter()
            .filter(|(_, file)| {
                file.items.iter().any(|item| {
                    matches!(&item.generated, Some(Ok(generated))
                        if generated.templates.iter().any(|read| **read == *template))
                })
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Whether any item read `template`.
    pub fn owns(&self, template: &Path) -> bool {
        !self.owners(template).is_empty()
    }

    /// Every error askama reports for a template, by template file.
    pub fn problems(&self) -> BTreeMap<PathBuf, BTreeSet<Problem>> {
        let mut problems: BTreeMap<PathBuf, BTreeSet<Problem>> = BTreeMap::new();
        for file in self.files.values() {
            for item in &file.items {
                let Some(Ok(generated)) = &item.generated else {
                    continue;
                };
                for error in &generated.errors {
                    if let Some(Origin::Template { path, range }) = &error.origin {
                        problems
                            .entry(path.to_path_buf())
                            .or_default()
                            .insert(Problem {
                                path: path.to_path_buf(),
                                range: range.clone(),
                                message: error.message.clone(),
                            });
                    }
                }
            }
        }
        problems
    }

    /// Items whose generation failed outright, by name, for logs and `check`.
    pub fn failures(&self) -> Vec<(String, String)> {
        self.files
            .values()
            .flat_map(|file| &file.items)
            .filter_map(|item| match &item.generated {
                Some(Err(failure)) => Some((item.found.name.clone(), failure.clone())),
                _ => None,
            })
            .collect()
    }

    fn package_of(&self, path: &Path) -> Option<usize> {
        self.packages
            .iter()
            .position(|package| package.source_dirs.iter().any(|dir| path.starts_with(dir)))
    }

    fn failing_files(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(_, file)| {
                file.items.iter().any(|item| match &item.generated {
                    Some(Ok(generated)) => !generated.errors.is_empty(),
                    _ => true,
                })
            })
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Finds the template items in a Rust file. A file that no longer parses keeps what was
    /// found before, since it is probably being typed.
    async fn index(&mut self, path: &Path, package: usize) {
        let Ok(text) = self.vfs.read(path) else {
            self.files.remove(path);
            return;
        };
        // Cheap enough to skip most files without parsing them.
        if !text.contains("Template") {
            self.files.remove(path);
            return;
        }
        let parse = Arc::clone(&text);
        match self
            .generator
            .run(move || index::template_items(&parse).map_err(|err| err.to_string()))
            .await
        {
            Ok(Ok(found)) if found.is_empty() => {
                self.files.remove(path);
            }
            Ok(Ok(found)) => {
                let items = found
                    .into_iter()
                    .map(|found| Item {
                        found,
                        generated: None,
                    })
                    .collect();
                self.files.insert(
                    path.to_owned(),
                    RustFile {
                        package,
                        text,
                        items,
                    },
                );
            }
            Ok(Err(err)) => {
                tracing::debug!(path = %path.display(), %err, "keeping the last index of a file that does not parse");
            }
            Err(err) => tracing::warn!(path = %path.display(), %err, "indexing failed"),
        }
    }

    async fn generate_file(&mut self, path: &Path) {
        let Some(file) = self.files.get(path) else {
            return;
        };
        let manifest_dir = self.packages[file.package].manifest_dir.clone();
        let jobs: Vec<String> = file
            .items
            .iter()
            .map(|item| file.text[item.found.range.clone()].to_owned())
            .collect();
        for (at, item_text) in jobs.into_iter().enumerate() {
            let snapshot = self.vfs.snapshot();
            let rust_file = path.to_owned();
            let manifest_dir = manifest_dir.clone();
            let generated = self
                .generator
                .run(move || {
                    let files: Rc<dyn Files> = Rc::new(snapshot);
                    askama_analyzer_codegen::generate(
                        Input {
                            item: &item_text,
                            file: &rust_file,
                            manifest_dir: &manifest_dir,
                        },
                        files,
                    )
                    .map_err(|err| err.to_string())
                })
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
            if let Some(item) = self
                .files
                .get_mut(path)
                .and_then(|file| file.items.get_mut(at))
            {
                item.generated = Some(generated);
            }
        }
    }
}

/// askama's syntax error in a template no item reads, with the default syntax.
pub fn syntax_problem(path: &Path, text: &str) -> Option<Problem> {
    // askama drops one trailing newline from a template file before parsing it.
    let text = text.strip_suffix('\n').unwrap_or(text);
    let syntax = askama_parser::Syntax::default();
    let err = askama_parser::Ast::from_str(text, None, &syntax).err()?;
    Some(Problem {
        path: path.to_owned(),
        range: err.offset..err.offset,
        message: err.message.map_or_else(
            || "this template does not parse".to_owned(),
            |message| message.into_owned(),
        ),
    })
}
