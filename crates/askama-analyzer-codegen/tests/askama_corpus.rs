//! askama's generator, through this crate, over askama's own passing tests.
//!
//! Every item deriving `Template` in `vendor/askama-testing/tests/*.rs` (the compile-fail
//! cases in `tests/ui/` aside) is generated with the manifest directory askama's tests use,
//! reading templates from disk. Each must generate without an askama error, as it does under
//! `cargo test` in askama, unless `tests/askama_corpus_errors.txt` lists it with the reason;
//! its code must parse as Rust; and every token mapped to template text must point at text
//! that exists.
//!
//! askama gives a whole expression's span to the helper tokens it writes around a name, so
//! a name's range collects several tokens, and the name itself should be among them. The run
//! counts the names in tags where it is not, and how many of the names in tags map at all,
//! and holds both to what was measured: the tooling patch's job is to keep these locations,
//! and a regression there would otherwise pass silently. It prints the total time and the
//! slowest items, a first benchmark of the generator.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use askama_analyzer_codegen::{Files, Generated, Input, Origin, generate};
use syn::spanned::Spanned;
use syn::visit::Visit;

/// The share of names in templates' tags that must map back, measured at 0.16.1 (see the
/// test's output for the current figure). Names that never reach Rust as written, such as
/// block and macro names, keep it below 1.
const COVERAGE_FLOOR: f64 = 0.63;

/// Names written in tags that tokens map to without any of them being that name, measured at
/// 0.16.1. askama writes a macro's argument where the parameter is used, so the parameter's
/// range carries the caller's expression (`param` in macro.html, 22 of them); and a filter
/// named in a `filter` block or a call's arguments maps only with the helper path around it
/// (`lower`, `urlencode`, `value`). A higher count means the tooling patch now puts template
/// ranges on the wrong tokens.
const MISSES_CEILING: usize = 33;

/// The items `syn` finds in askama's 0.16.1 tests: 494 of the 505 deriving `Template`, the
/// rest written inside `macro_rules!` bodies, whose sources are macro arguments.
const ITEMS_FLOOR: usize = 494;

/// askama's generator recurses deeply on its fuzzed inputs.
const STACK_SIZE: usize = 256 * 1024 * 1024;

fn suite() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/askama-testing")
}

/// Templates read from disk, kept so the checks can look at the text askama saw.
#[derive(Default)]
struct Disk(RefCell<HashMap<PathBuf, String>>);

impl Files for Disk {
    fn read(&self, path: &Path) -> io::Result<String> {
        if let Some(text) = self.0.borrow().get(path) {
            return Ok(text.clone());
        }
        let text = fs::read_to_string(path)?;
        self.0.borrow_mut().insert(path.to_owned(), text.clone());
        Ok(text)
    }

    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// An item deriving `Template`, as a byte range of its file.
struct Item {
    name: String,
    line: usize,
    range: Range<usize>,
}

#[derive(Default)]
struct Finder(Vec<Item>);

impl Finder {
    fn consider(&mut self, attrs: &[syn::Attribute], ident: &syn::Ident, span: proc_macro2::Span) {
        let derives_template = attrs.iter().any(|attr| {
            attr.path().is_ident("derive")
                && attr
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                    )
                    .is_ok_and(|paths| {
                        paths
                            .iter()
                            .any(|path| path.segments.last().is_some_and(|s| s.ident == "Template"))
                    })
        });
        if derives_template {
            self.0.push(Item {
                name: ident.to_string(),
                line: span.start().line,
                range: span.byte_range(),
            });
        }
    }
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.consider(&item.attrs, &item.ident, item.span());
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.consider(&item.attrs, &item.ident, item.span());
        syn::visit::visit_item_enum(self, item);
    }
}

fn template_items(text: &str) -> Vec<Item> {
    let file = syn::parse_file(text).expect("askama's test file parses as Rust");
    let mut finder = Finder::default();
    finder.visit_file(&file);
    // Spans are only needed until the ranges are read out.
    proc_macro2::extra::invalidate_current_thread_spans();
    finder.0
}

fn is_identifier(text: &str) -> bool {
    let text = text.strip_prefix("r#").unwrap_or(text);
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Words with a meaning of their own in askama's tags, which name nothing in the template's
/// Rust and so are not expected to map.
const TAG_KEYWORDS: &[&str] = &[
    "if",
    "elif",
    "else",
    "endif",
    "for",
    "in",
    "endfor",
    "match",
    "when",
    "endwhen",
    "endmatch",
    "block",
    "endblock",
    "macro",
    "endmacro",
    "call",
    "endcall",
    "filter",
    "endfilter",
    "let",
    "set",
    "endlet",
    "endset",
    "mut",
    "decl",
    "declare",
    "raw",
    "endraw",
    "extends",
    "include",
    "import",
    "as",
    "is",
    "not",
    "defined",
    "with",
    "or",
    "bitor",
    "bitand",
    "xor",
    "break",
    "continue",
    "true",
    "false",
    "loop",
    "caller",
    "self",
    "super",
    "crate",
    "Self",
    "_",
];

/// Byte ranges of the names written inside `{{ }}` and `{% %}` tags, string and character
/// literals skipped. Lexical, so it over-counts slightly (a filter's name, a block's name),
/// which the floor absorbs.
fn tag_names(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let close = match &bytes[i..i + 2] {
            b"{{" => b"}}",
            b"{%" => b"%}",
            _ => {
                i += 1;
                continue;
            }
        };
        // A raw block's body is text.
        let tag_start = i;
        i += 2;
        while i + 1 < bytes.len() && &bytes[i..i + 2] != close {
            let c = bytes[i];
            if c == b'"' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            } else if c == b'_' || c.is_ascii_alphabetic() {
                let start = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let word = &text[start..i];
                let after_dot = start > 0 && bytes[start - 1] == b'.';
                if after_dot || !TAG_KEYWORDS.contains(&word) {
                    names.push(start..i);
                }
            } else {
                i += 1;
            }
        }
        if text[tag_start..i.min(text.len())]
            .trim_start_matches(['{', '%', '-', '+', '~', ' '])
            .starts_with("raw")
            && let Some(end) = text[i..].find("endraw")
        {
            i += end;
        }
        i += 2;
    }
    names
}

struct Outcome {
    key: String,
    elapsed: Duration,
}

#[derive(Default)]
struct Coverage {
    names: usize,
    mapped: usize,
    /// How often each name that did not map was written, for the report.
    unmapped: HashMap<String, usize>,
    /// Names in tags whose tokens all carry other text.
    misses: Vec<String>,
}

/// A range of template text: in a template file, or (`None`) in the item's inline source.
type Place = (Option<Arc<Path>>, Range<usize>);

/// Checks one item's output; returns its failures.
fn check_item(
    key: &str,
    item_text: &str,
    generated: &Generated,
    disk: &Disk,
    coverage: &mut Coverage,
    failures: &mut Vec<String>,
) {
    if let Err(error) = syn::parse_file(&generated.code) {
        failures.push(format!("{key}: the generated code is not Rust: {error}"));
    }
    proc_macro2::extra::invalidate_current_thread_spans();

    let files = disk.0.borrow();
    // Mapped ranges per template file, and per inline source (as item ranges).
    let mut mapped: HashMap<Option<Arc<Path>>, Vec<Range<usize>>> = HashMap::new();
    // Each identifier range of template text that tokens map to, and whether one of them is
    // that identifier.
    let mut names: HashMap<Place, (String, bool)> = HashMap::new();
    let inline = inline_source(item_text).and_then(|source| {
        item_text
            .find(source)
            .map(|start| start..start + source.len())
    });
    // The names written in tags, where the check below applies: askama also maps tokens to
    // template text and to keywords, which name nothing.
    let mut tag_name_ranges: std::collections::HashSet<Place> = std::collections::HashSet::new();
    for path in &generated.templates {
        if let Some(text) = files.get(&**path) {
            tag_name_ranges.extend(
                tag_names(text)
                    .into_iter()
                    .map(|r| (Some(Arc::clone(path)), r)),
            );
        }
    }
    if let (Some(source), Some(literal)) = (inline_source(item_text), inline.as_ref()) {
        tag_name_ranges.extend(
            tag_names(source)
                .into_iter()
                .map(|r| (None, r.start + literal.start..r.end + literal.start)),
        );
    }
    for mapping in &generated.map {
        let token = &generated.code[mapping.generated.clone()];
        let (source, range, owner) = match &mapping.origin {
            Origin::Template { path, range } => match files.get(&**path) {
                Some(text) => (text.as_str(), range.clone(), Some(Arc::clone(path))),
                None => {
                    failures.push(format!(
                        "{key}: a token maps to {}, which askama never read",
                        path.display()
                    ));
                    continue;
                }
            },
            Origin::Item { range } => (item_text, range.clone(), None),
        };
        let Some(original) = source.get(range.clone()) else {
            failures.push(format!(
                "{key}: `{token}` maps to {range:?}, outside its text of {} bytes",
                source.len()
            ));
            continue;
        };
        // askama gives a whole expression's span to the helper tokens it writes around a
        // name, so one range collects several tokens; the name itself must be among them.
        if is_identifier(token) && is_identifier(original) {
            let bare = |text: &str| text.strip_prefix("r#").unwrap_or(text).to_owned();
            if tag_name_ranges.contains(&(owner.clone(), range.clone())) {
                let entry = names
                    .entry((owner.clone(), range.clone()))
                    .or_insert((bare(original), false));
                entry.1 |= bare(token) == entry.0;
            }
        }
        mapped.entry(owner).or_default().push(range);
    }
    for ((owner, range), (name, found)) in &names {
        if !found {
            let place = owner.as_ref().map_or_else(
                || "the inline source".to_owned(),
                |path| path.display().to_string(),
            );
            coverage.misses.push(format!(
                "{key}: no token from `{name}` at {range:?} in {place} is `{name}`"
            ));
        }
    }

    // Coverage over the item's template files; inline sources count where the literal's text
    // can be found verbatim in the item, so offsets line up.
    let mut count = |text: &str, offset: usize, ranges: Option<&Vec<Range<usize>>>| {
        for name in tag_names(text) {
            let word = &text[name.clone()];
            let name = name.start + offset..name.end + offset;
            coverage.names += 1;
            if ranges.is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|r| r.start <= name.start && name.end <= r.end)
            }) {
                coverage.mapped += 1;
            } else {
                *coverage.unmapped.entry(word.to_owned()).or_default() += 1;
            }
        }
    };
    for path in &generated.templates {
        if let Some(text) = files.get(&**path) {
            count(text, 0, mapped.get(&Some(Arc::clone(path))));
        }
    }
    if let Some(source) = inline_source(item_text)
        && let Some(offset) = item_text.find(source)
    {
        count(source, offset, mapped.get(&None));
    }
}

/// The text of a raw-string `source = r#"…"#` in an item, which appears verbatim in it.
fn inline_source(item: &str) -> Option<&str> {
    let start = item.find("source")?;
    let rest = item[start + "source".len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let rest = rest.strip_prefix('r')?;
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    let body = rest[hashes..].strip_prefix('"')?;
    let close = format!("\"{}", "#".repeat(hashes));
    Some(&body[..body.find(&close)?])
}

fn allowed_errors() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/askama_corpus_errors.txt");
    fs::read_to_string(path)
        .expect("tests/askama_corpus_errors.txt is readable")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (key, reason) = line
                .split_once(char::is_whitespace)
                .unwrap_or_else(|| panic!("`{line}` in askama_corpus_errors.txt gives no reason"));
            (key.to_owned(), reason.trim().to_owned())
        })
        .collect()
}

#[test]
fn askamas_own_tests_generate_parse_and_map_back_to_their_templates() {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(run)
        .expect("the test thread starts")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

fn run() {
    let root = suite()
        .canonicalize()
        .expect("vendor/askama-testing exists");
    let mut tests: Vec<PathBuf> = fs::read_dir(root.join("tests"))
        .expect("vendor/askama-testing/tests is readable")
        .map(|entry| entry.expect("the directory is readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    tests.sort();

    let mut allowed = allowed_errors();
    let mut failures = Vec::new();
    let mut outcomes = Vec::new();
    let mut coverage = Coverage::default();
    let started = Instant::now();

    for test in &tests {
        let text = fs::read_to_string(test).expect("the test file is readable");
        let file_name = test.file_name().expect("a file").to_string_lossy();
        for item in template_items(&text) {
            let key = format!("tests/{file_name}:{}:{}", item.line, item.name);
            let item_text = &text[item.range.clone()];
            let disk = Rc::new(Disk::default());
            let input = Input {
                item: item_text,
                file: test,
                manifest_dir: &root,
            };
            let item_started = Instant::now();
            let generated = match generate(input, Rc::clone(&disk) as Rc<dyn Files>) {
                Ok(generated) => generated,
                Err(error) => {
                    failures.push(format!("{key}: generation failed: {error}"));
                    continue;
                }
            };
            outcomes.push(Outcome {
                key: key.clone(),
                elapsed: item_started.elapsed(),
            });

            match (generated.errors.is_empty(), allowed.remove(&key)) {
                (false, None) => failures.push(format!(
                    "{key}: askama reported {}",
                    generated
                        .errors
                        .iter()
                        .map(|error| error.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
                (true, Some(_)) => failures.push(format!(
                    "{key}: listed in askama_corpus_errors.txt but generates cleanly"
                )),
                _ => {}
            }
            check_item(
                &key,
                item_text,
                &generated,
                &disk,
                &mut coverage,
                &mut failures,
            );
        }
    }
    failures.extend(
        allowed
            .into_keys()
            .map(|key| format!("{key}: listed in askama_corpus_errors.txt but not found")),
    );

    let total = started.elapsed();
    outcomes.sort_by_key(|outcome| std::cmp::Reverse(outcome.elapsed));
    let share = coverage.mapped as f64 / coverage.names.max(1) as f64;
    eprintln!(
        "generated {} items from {} files in {:.2?}; {} of {} template names mapped ({:.1}%)",
        outcomes.len(),
        tests.len(),
        total,
        coverage.mapped,
        coverage.names,
        share * 100.0,
    );
    let mut unmapped: Vec<_> = coverage.unmapped.iter().collect();
    unmapped.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let listing: Vec<String> = unmapped
        .iter()
        .take(20)
        .map(|(word, count)| format!("{word} {count}"))
        .collect();
    eprintln!("most often unmapped: {}", listing.join(", "));
    eprintln!(
        "{} names whose tokens carry other text:",
        coverage.misses.len()
    );
    for miss in &coverage.misses {
        eprintln!("  {miss}");
    }
    eprintln!("slowest:");
    for outcome in outcomes.iter().take(10) {
        eprintln!("  {:>9.2?}  {}", outcome.elapsed, outcome.key);
    }

    assert!(
        outcomes.len() >= ITEMS_FLOOR,
        "found only {} template items in askama's tests",
        outcomes.len()
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        coverage.misses.len() <= MISSES_CEILING,
        "{} names in tags map only to tokens with other text, above the {MISSES_CEILING} measured; see the list above",
        coverage.misses.len()
    );
    assert!(
        share >= COVERAGE_FLOOR,
        "only {:.1}% of template names map back, below the floor of {:.1}%",
        share * 100.0,
        COVERAGE_FLOOR * 100.0
    );
}
