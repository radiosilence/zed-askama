//! Holds the grammar to askama's own parser over askama's test suite.
//!
//! The inputs are vendored in `test/askama-corpus` by
//! `scripts/vendor-askama-corpus.py`: askama's test templates and the inline
//! `source = "…"` templates from its tests. Every input is parsed by both
//! `askama_parser` and the grammar. Where one accepts and the other rejects,
//! the input must be listed in `test/divergences.txt` with the reason, and every
//! listed input must still diverge, so the list stays exact.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

/// Askama's parser recurses once per nesting level, up to 128 levels, with
/// large frames; the default 2 MiB test thread overflows on its fuzzed inputs.
const STACK_SIZE: usize = 256 * 1024 * 1024;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("test/askama-corpus")
}

fn files_under(root: &Path, directory: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.join(directory)];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("the corpus directory is readable") {
            let path = entry.expect("the corpus directory is readable").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).expect("walked from the root");
                found.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found.sort();
    found
}

fn read(path: &Path) -> String {
    let bytes = fs::read(path).expect("the corpus file is readable");
    String::from_utf8_lossy(&bytes).into_owned()
}

fn grammar_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&askama_analyzer_grammar::LANGUAGE.into())
        .expect("the grammar was generated for this tree-sitter version");
    parser
}

/// The first `ERROR` or `MISSING` node in the tree, as `kind at row:column`.
fn first_error(parser: &mut Parser, source: &str) -> Option<String> {
    let tree = parser.parse(source, None).expect("no timeout or cancellation is set");
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    fn find(node: Node<'_>) -> Option<String> {
        if node.is_error() || node.is_missing() {
            let position = node.start_position();
            let kind = if node.is_missing() {
                format!("MISSING {}", node.kind())
            } else {
                "ERROR".to_owned()
            };
            return Some(format!("{kind} at {}:{}", position.row + 1, position.column + 1));
        }
        let mut cursor = node.walk();
        node.children(&mut cursor).find_map(find)
    }
    find(root).or_else(|| Some("an error".to_owned()))
}

fn askama_accepts(source: &str) -> Result<(), String> {
    askama_parser::Ast::from_str(source, None, &askama_parser::Syntax::default())
        .map(|_| ())
        .map_err(|error| error.to_string().lines().next().unwrap_or_default().to_owned())
}

fn divergences() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/divergences.txt");
    read(&path)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (input, reason) = line
                .split_once(char::is_whitespace)
                .unwrap_or_else(|| panic!("divergence `{line}` gives no reason"));
            (input.to_owned(), reason.trim().to_owned())
        })
        .collect()
}

fn on_large_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(test)
        .expect("the test thread starts")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn every_valid_askama_template_parses_without_errors() {
    let root = corpus();
    let compile_fail = read(&root.join("compile-fail.txt"));
    let compile_fail: Vec<&str> = compile_fail.lines().collect();
    let mut parser = grammar_parser();

    let failures: Vec<String> = files_under(&root, "templates")
        .into_iter()
        .filter(|path| !compile_fail.contains(&path.trim_start_matches("templates/")))
        .filter_map(|path| {
            first_error(&mut parser, &read(&root.join(&path))).map(|error| format!("{path}: {error}"))
        })
        .collect();

    assert!(failures.is_empty(), "valid askama templates failed to parse:\n{}", failures.join("\n"));
}

#[test]
fn the_grammar_accepts_exactly_what_askama_parser_accepts() {
    on_large_stack(|| {
        let root = corpus();
        let mut listed = divergences();
        let mut parser = grammar_parser();
        let mut unexpected = Vec::new();

        let inputs = files_under(&root, "templates").into_iter().chain(files_under(&root, "inline"));
        for input in inputs {
            let source = read(&root.join(&input));
            let askama = askama_accepts(&source);
            let grammar = first_error(&mut parser, &source);
            let diverges = askama.is_ok() != grammar.is_none();
            let is_listed = listed.remove(&input).is_some();
            match (diverges, is_listed) {
                (true, false) => unexpected.push(match askama {
                    Ok(()) => format!("{input}: askama accepts it, the grammar finds {}", grammar.unwrap_or_default()),
                    Err(error) => format!("{input}: askama rejects it ({error}), the grammar accepts it"),
                }),
                (false, true) => unexpected.push(format!("{input}: listed as a divergence but both now agree")),
                _ => {}
            }
        }
        unexpected.extend(listed.into_keys().map(|input| format!("{input}: listed as a divergence but not in the corpus")));

        assert!(
            unexpected.is_empty(),
            "the grammar and askama_parser disagree on {} inputs not listed in test/divergences.txt:\n{}",
            unexpected.len(),
            unexpected.join("\n"),
        );
    });
}
