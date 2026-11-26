//! The formatter over real templates: askama's test suite and, when given, real projects.
//!
//! `format` checks its own result (the same tree as the original, and nothing left to change on
//! a second pass) and refuses otherwise, so every template askama parses must format without
//! an error. Templates askama rejects are skipped: there is nothing to keep.
//!
//! Real projects are read when `ASKAMA_GRAMMAR_TEMPLATE_DIRS` names them, as for the grammar's
//! real-template test:
//!
//! ```sh
//! ASKAMA_GRAMMAR_TEMPLATE_DIRS=~/src/slsk-mcp:~/src/koan cargo test -p askama-analyzer --test format_corpus
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use askama_analyzer::format::{FormatError, format};
use askama_parser::Syntax;

fn suite() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/askama-testing")
}

fn files_under(directory: &Path, keep: &dyn Fn(&Path) -> bool, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some("target" | "node_modules" | ".git")
            ) {
                files_under(&path, keep, found);
            }
        } else if keep(&path) {
            found.push(path);
        }
    }
}

/// Every plain or raw string literal written as `source = …` in a Rust file.
fn inline_sources(text: &str) -> Vec<String> {
    let mut sources = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("source") {
        rest = &rest[at + "source".len()..];
        let Some(literal) = rest.trim_start().strip_prefix('=').map(str::trim_start) else {
            continue;
        };
        if let Some(raw) = literal.strip_prefix('r') {
            let hashes = raw.len() - raw.trim_start_matches('#').len();
            if let Some(body) = raw[hashes..].strip_prefix('"') {
                let close = format!("\"{}", "#".repeat(hashes));
                if let Some(end) = body.find(&close) {
                    sources.push(body[..end].to_owned());
                }
            }
        } else if let Some(body) = literal.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = body.chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some('\n') => {
                            while chars.clone().next().is_some_and(char::is_whitespace) {
                                chars.next();
                            }
                        }
                        Some(other @ ('\\' | '"' | '\'')) => out.push(other),
                        // Escapes this reader does not decode; the literal is skipped.
                        _ => break,
                    },
                    c => out.push(c),
                }
            }
            if closed {
                sources.push(out);
            }
        }
    }
    sources
}

/// Formats each named source, returning how many formatted, how many changed and how many
/// askama rejects, and failing the test on any other error.
fn check(sources: Vec<(String, String)>) -> (usize, usize, usize) {
    let syntax = Syntax::default();
    let (mut formatted, mut changed, mut rejected) = (0, 0, 0);
    let mut failures = Vec::new();
    for (name, source) in sources {
        match format(&source, &syntax) {
            Ok(output) => {
                formatted += 1;
                if output != source {
                    changed += 1;
                }
            }
            Err(FormatError::Parse(_)) => rejected += 1,
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} templates could not be formatted:\n{}",
        failures.len(),
        failures.join("\n")
    );
    (formatted, changed, rejected)
}

fn read(path: &Path) -> String {
    String::from_utf8_lossy(&fs::read(path).expect("the file is readable")).into_owned()
}

#[test]
fn every_askama_test_template_formats_to_the_same_tree_and_stays_formatted() {
    let mut paths = Vec::new();
    files_under(&suite().join("templates"), &|_| true, &mut paths);
    let sources = paths
        .iter()
        .map(|path| (path.display().to_string(), read(path)))
        .collect();
    let (formatted, changed, rejected) = check(sources);
    eprintln!("{formatted} templates formatted ({changed} changed), {rejected} rejected by askama");
    assert!(
        formatted > 100,
        "only {formatted} of askama's templates parsed"
    );
}

#[test]
fn every_inline_source_in_askamas_tests_formats_to_the_same_tree_and_stays_formatted() {
    let mut paths = Vec::new();
    files_under(
        &suite().join("tests"),
        &|path| path.extension().is_some_and(|ext| ext == "rs"),
        &mut paths,
    );
    let mut sources = Vec::new();
    for path in &paths {
        for (index, source) in inline_sources(&read(path)).into_iter().enumerate() {
            sources.push((format!("{} #{index}", path.display()), source));
        }
    }
    let (formatted, changed, rejected) = check(sources);
    eprintln!(
        "{formatted} inline sources formatted ({changed} changed), {rejected} rejected by askama"
    );
    // 489 of the 865 sources this reader decodes parse at 0.16.1; the rest are compile-fail
    // cases in tests/ui.
    assert!(formatted >= 480, "only {formatted} inline sources parsed");
}

#[test]
fn every_template_in_the_given_projects_formats_to_the_same_tree_and_stays_formatted() {
    let Some(directories) = std::env::var_os("ASKAMA_GRAMMAR_TEMPLATE_DIRS") else {
        eprintln!("skipped: ASKAMA_GRAMMAR_TEMPLATE_DIRS is not set");
        return;
    };
    let mut paths = Vec::new();
    for directory in std::env::split_paths(&directories) {
        files_under(
            &directory,
            &|path| {
                path.components()
                    .any(|part| part.as_os_str() == "templates")
            },
            &mut paths,
        );
    }
    let sources = paths
        .iter()
        .map(|path| (path.display().to_string(), read(path)))
        .collect();
    let (formatted, changed, rejected) = check(sources);
    eprintln!("{formatted} templates formatted ({changed} changed), {rejected} rejected by askama");
    assert_eq!(
        rejected, 0,
        "askama rejects templates in the given projects"
    );
}
