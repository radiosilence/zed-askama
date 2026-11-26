//! Holds the grammar to askama's own parser over askama's test suite.
//!
//! The suite is vendored in `vendor/askama-testing` at the repository root by
//! `scripts/vendor-askama-testing.py`. The inputs are its templates and every
//! `source = "…"` or `source = r#"…"#` literal in its tests, except those in an
//! attribute that sets `syntax` (custom delimiters, which this grammar does not
//! parse). Sources built from macro arguments (`source = $source`) are left out,
//! since they cannot be read without expanding the macro.
//!
//! Every input is parsed by both `askama_parser` and the grammar. Where one
//! accepts and the other rejects, the input must be listed in
//! `test/divergences.txt` with the reason, and every listed input must still
//! diverge, so the list stays exact.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

/// Askama's parser recurses once per nesting level, up to 128 levels, with
/// large frames; the default 2 MiB test thread overflows on its fuzzed inputs.
const STACK_SIZE: usize = 256 * 1024 * 1024;

fn suite() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/askama-testing")
}

fn files_under(root: &Path, directory: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.join(directory)];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("the vendored suite is readable") {
            let path = entry.expect("the vendored suite is readable").path();
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
    let bytes = fs::read(path).expect("the vendored file is readable");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Decodes the Rust string literal at the start of `text`, returning it and the
/// number of bytes it spans, or `None` if `text` does not start with one.
fn rust_string_literal(text: &str) -> Option<(String, usize)> {
    if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        let body = rest[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = body.find(&close)?;
        return Some((body[..end].to_owned(), 1 + hashes + 1 + end + close.len()));
    }
    let body = text.strip_prefix('"')?;
    let mut decoded = String::new();
    let mut chars = body.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((decoded, 1 + index + 1)),
            '\\' => match chars.next()?.1 {
                'n' => decoded.push('\n'),
                'r' => decoded.push('\r'),
                't' => decoded.push('\t'),
                '0' => decoded.push('\0'),
                '\\' => decoded.push('\\'),
                '\'' => decoded.push('\''),
                '"' => decoded.push('"'),
                '\n' => {
                    while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
                }
                'x' => {
                    let digits: String = (0..2).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    decoded.push(char::from(u8::from_str_radix(&digits, 16).ok()?));
                }
                'u' => {
                    chars.next_if(|(_, c)| *c == '{')?;
                    let mut digits = String::new();
                    while let Some((_, c)) = chars.next_if(|(_, c)| *c != '}') {
                        digits.push(c);
                    }
                    chars.next();
                    decoded.push(char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?);
                }
                _ => return None,
            },
            c => decoded.push(c),
        }
    }
    None
}

/// The template sources written inline in a test file, keyed by line.
fn inline_sources(text: &str) -> Vec<(usize, String)> {
    let mut sources = Vec::new();
    let mut search = 0;
    while let Some(found) = text[search..].find("source") {
        let start = search + found;
        search = start + "source".len();
        let before_is_word = text[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = text[search..].trim_start();
        let Some(after) = after.strip_prefix('=').filter(|_| !before_is_word) else {
            continue;
        };
        let literal_start = text.len() - after.trim_start().len();
        let Some((source, length)) = rust_string_literal(&text[literal_start..]) else {
            continue;
        };
        let literal_end = literal_start + length;
        let attribute_start = text[..start].rfind("#[template(").unwrap_or(start);
        let attribute_end = text[literal_end..].find(")]").map_or(literal_end, |end| literal_end + end);
        let attribute = format!("{}{}", &text[attribute_start..start], &text[literal_end..attribute_end]);
        let sets_syntax = attribute
            .match_indices("syntax")
            .any(|(at, _)| attribute[at + "syntax".len()..].trim_start().starts_with('='));
        if !sets_syntax {
            sources.push((text[..start].matches('\n').count() + 1, source));
        }
    }
    sources
}

/// Template names in `path = "…"` attributes and `extends`, `include` and
/// `import` tags.
fn referenced_templates(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for marker in ["path", "extends", "include", "import"] {
        for (at, _) in text.match_indices(marker) {
            let rest = text[at + marker.len()..].trim_start();
            let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
            if let Some(name) = rest.strip_prefix('"').and_then(|r| r.split('"').next()) {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

struct Input {
    name: String,
    source: String,
}

/// askama's test templates and inline sources, and the names of the templates
/// that only its compile-fail tests (`tests/ui/`) use.
fn corpus() -> (Vec<Input>, BTreeSet<String>) {
    let root = suite();
    let mut inputs = Vec::new();
    let mut from_ui = BTreeSet::new();
    let mut from_elsewhere = BTreeSet::new();

    for test in files_under(&root, "tests").into_iter().filter(|path| path.ends_with(".rs")) {
        let text = read(&root.join(&test));
        let names = referenced_templates(&text);
        if test.starts_with("tests/ui/") {
            from_ui.extend(names);
        } else {
            from_elsewhere.extend(names);
        }
        for (line, source) in inline_sources(&text) {
            inputs.push(Input { name: format!("{test}:{line}"), source });
        }
    }

    let templates = files_under(&root, "templates");
    for template in &templates {
        let source = read(&root.join(template));
        let short = template.trim_start_matches("templates/");
        if from_elsewhere.contains(short) {
            from_elsewhere.extend(referenced_templates(&source));
        }
        inputs.push(Input { name: template.clone(), source });
    }
    let compile_fail = templates
        .iter()
        .map(|template| template.trim_start_matches("templates/").to_owned())
        .filter(|name| from_ui.contains(name) && !from_elsewhere.contains(name))
        .map(|name| format!("templates/{name}"))
        .collect();
    (inputs, compile_fail)
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
    if !tree.root_node().has_error() {
        return None;
    }
    fn find(node: Node<'_>) -> Option<String> {
        if node.is_error() || node.is_missing() {
            let position = node.start_position();
            let kind = if node.is_missing() { format!("MISSING {}", node.kind()) } else { "ERROR".to_owned() };
            return Some(format!("{kind} at {}:{}", position.row + 1, position.column + 1));
        }
        let mut cursor = node.walk();
        node.children(&mut cursor).find_map(find)
    }
    find(tree.root_node()).or_else(|| Some("an error".to_owned()))
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
fn the_vendored_suite_yields_templates_and_inline_sources() {
    let (inputs, compile_fail) = corpus();
    let templates = inputs.iter().filter(|input| input.name.starts_with("templates/")).count();
    let inline = inputs.len() - templates;
    assert!(templates > 100, "found only {templates} templates in vendor/askama-testing");
    assert!(inline > 500, "extracted only {inline} inline sources from vendor/askama-testing");
    assert!(compile_fail.contains("templates/invalid_syntax.html"), "invalid_syntax.html is not marked compile-fail");
}

#[test]
fn every_valid_askama_template_parses_without_errors() {
    let (inputs, compile_fail) = corpus();
    let mut parser = grammar_parser();
    let failures: Vec<String> = inputs
        .iter()
        .filter(|input| input.name.starts_with("templates/") && !compile_fail.contains(&input.name))
        .filter_map(|input| first_error(&mut parser, &input.source).map(|error| format!("{}: {error}", input.name)))
        .collect();
    assert!(failures.is_empty(), "valid askama templates failed to parse:\n{}", failures.join("\n"));
}

#[test]
fn the_grammar_accepts_exactly_what_askama_parser_accepts() {
    on_large_stack(|| {
        let (inputs, _) = corpus();
        let mut listed = divergences();
        let mut parser = grammar_parser();
        let mut unexpected = Vec::new();

        for Input { name, source } in inputs {
            let askama = askama_accepts(&source);
            let grammar = first_error(&mut parser, &source);
            let diverges = askama.is_ok() != grammar.is_none();
            match (diverges, listed.remove(&name).is_some()) {
                (true, false) => unexpected.push(match askama {
                    Ok(()) => format!("{name}: askama accepts it, the grammar finds {}", grammar.unwrap_or_default()),
                    Err(error) => format!("{name}: askama rejects it ({error}), the grammar accepts it"),
                }),
                (false, true) => unexpected.push(format!("{name}: listed as a divergence but both now agree")),
                _ => {}
            }
        }
        unexpected.extend(listed.into_keys().map(|name| format!("{name}: listed as a divergence but not in the suite")));

        assert!(
            unexpected.is_empty(),
            "the grammar and askama_parser disagree on {} inputs not listed in test/divergences.txt:\n{}",
            unexpected.len(),
            unexpected.join("\n"),
        );
    });
}
