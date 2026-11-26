//! The generator against in-memory crates: what it maps, what it reads and what it reports.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{Files, Generated, Input, Origin, generate};

const MANIFEST_DIR: &str = "/crate";
const RUST_FILE: &str = "/crate/src/lib.rs";

/// A crate's files held in memory, which tests may change between generations.
#[derive(Default)]
struct Memory(RefCell<HashMap<PathBuf, String>>);

impl Memory {
    fn with(files: &[(&str, &str)]) -> Rc<Self> {
        let memory = Self::default();
        for (path, text) in files {
            memory.set(path, text);
        }
        Rc::new(memory)
    }

    fn set(&self, path: &str, text: &str) {
        self.0
            .borrow_mut()
            .insert(Path::new(MANIFEST_DIR).join(path), text.to_owned());
    }
}

impl Files for Memory {
    fn read(&self, path: &Path) -> io::Result<String> {
        self.0
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

fn run(item: &str, files: Rc<Memory>) -> Generated {
    let input = Input {
        item,
        file: Path::new(RUST_FILE),
        manifest_dir: Path::new(MANIFEST_DIR),
    };
    generate(input, files).expect("the item should generate")
}

/// The generated text of every token that came from `want` at the given range.
fn tokens_from(generated: &Generated, want: impl Fn(&Origin) -> bool) -> Vec<&str> {
    generated
        .map
        .iter()
        .filter(|mapping| want(&mapping.origin))
        .map(|mapping| &generated.code[mapping.generated.clone()])
        .collect()
}

fn template_range(origin: &Origin, file: &str) -> Option<std::ops::Range<usize>> {
    match origin {
        Origin::Template { path, range } if path.ends_with(file) => Some(range.clone()),
        _ => None,
    }
}

#[test]
fn an_inline_source_maps_its_expressions_into_the_string_literal() {
    let item = r#"#[derive(Template)]
#[template(source = "Hello {{ name }}!", ext = "txt")]
struct Hello { name: String }"#;
    let generated = run(item, Memory::with(&[]));
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );

    let name_in_literal = item
        .find("{{ name }}")
        .expect("fixture holds the expression")
        + 3;
    let names = tokens_from(
        &generated,
        |origin| matches!(origin, Origin::Item { range } if range.start == name_in_literal),
    );
    assert!(
        names.contains(&"name"),
        "no generated `name` token points at the literal's `name`; code:\n{}",
        generated.code
    );
    assert!(
        generated.templates.is_empty(),
        "an inline source reads no template files"
    );
}

#[test]
fn a_template_file_maps_its_expressions_into_the_file() {
    let template = "<p>{{ user.name }}</p>";
    let files = Memory::with(&[("templates/page.html", template)]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { user: User }"#,
        files,
    );
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );

    let user = template.find("user").expect("fixture holds `user`");
    let field = template.find("name").expect("fixture holds `name`");
    let from_file: Vec<_> = generated
        .map
        .iter()
        .filter_map(|mapping| {
            template_range(&mapping.origin, "page.html")
                .map(|range| (range, &generated.code[mapping.generated.clone()]))
        })
        .collect();
    assert!(
        from_file
            .iter()
            .any(|(range, text)| range.start == user && *text == "user"),
        "`user` is not mapped to page.html at {user}: {from_file:?}"
    );
    assert!(
        from_file
            .iter()
            .any(|(range, text)| range.start == field && *text == "name"),
        "`name` is not mapped to page.html at {field}: {from_file:?}"
    );
    assert_eq!(
        generated.templates.len(),
        1,
        "page.html is the only template read"
    );
}

#[test]
fn an_included_file_maps_to_itself_and_not_to_the_template_including_it() {
    let files = Memory::with(&[
        (
            "templates/page.html",
            "{% include \"header.html\" %}<main>{{ body }}</main>",
        ),
        ("templates/header.html", "<h1>{{ title }}</h1>"),
    ]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { title: String, body: String }"#,
        files,
    );
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );

    let title_in_header = "<h1>{{ title }}</h1>"
        .find("title")
        .expect("fixture holds `title`");
    let titles = tokens_from(&generated, |origin| {
        template_range(origin, "header.html").is_some_and(|range| range.start == title_in_header)
    });
    assert!(
        titles.contains(&"title"),
        "`title` is not mapped to header.html; code:\n{}",
        generated.code
    );

    let bodies = tokens_from(&generated, |origin| {
        template_range(origin, "page.html").is_some()
    });
    assert!(
        bodies.contains(&"body"),
        "`body` is not mapped to page.html; code:\n{}",
        generated.code
    );
    assert_eq!(
        generated.templates.len(),
        2,
        "both files are reported as read"
    );
}

#[test]
fn a_parent_template_maps_to_itself_when_a_child_extends_it() {
    let files = Memory::with(&[
        (
            "templates/base.html",
            "<title>{{ title }}</title>{% block content %}{% endblock %}",
        ),
        (
            "templates/child.html",
            "{% extends \"base.html\" %}{% block content %}{{ text }}{% endblock %}",
        ),
    ]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "child.html")] struct Child { title: String, text: String }"#,
        files,
    );
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );
    assert!(
        tokens_from(&generated, |origin| template_range(origin, "base.html")
            .is_some())
        .contains(&"title"),
        "`title` is not mapped to base.html; code:\n{}",
        generated.code
    );
    assert!(
        tokens_from(&generated, |origin| template_range(origin, "child.html")
            .is_some())
        .contains(&"text"),
        "`text` is not mapped to child.html; it maps to {:?}; code:\n{}",
        generated
            .map
            .iter()
            .filter(|mapping| &generated.code[mapping.generated.clone()] == "text")
            .map(|mapping| &mapping.origin)
            .collect::<Vec<_>>(),
        generated.code
    );
}

#[test]
fn unsaved_contents_are_read_afresh_on_every_generation() {
    let files = Memory::with(&[("templates/page.html", "{{ first }}")]);
    let item = r#"#[derive(Template)] #[template(path = "page.html")] struct Page { first: u8, second: u8 }"#;
    let before = run(item, Rc::clone(&files));
    files.set("templates/page.html", "{{ second }}");
    let after = run(item, files);

    let names = |generated: &Generated| {
        tokens_from(generated, |origin| {
            template_range(origin, "page.html").is_some()
        })
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    assert!(
        names(&before).contains(&"first".to_owned()),
        "the first generation reads `first`"
    );
    assert!(
        names(&after).contains(&"second".to_owned()),
        "the second generation must see the edit, not a cached copy; got {:?}",
        names(&after)
    );
}

#[test]
fn an_unknown_macro_is_reported_at_the_call_in_the_template() {
    let template = "<p>{% call nowhere() %}{% endcall %}</p>";
    let files = Memory::with(&[("templates/page.html", template)]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page;"#,
        files,
    );
    let error = generated
        .errors
        .first()
        .expect("an unknown macro is an error");
    assert!(
        error.message.contains("nowhere"),
        "the message names the macro: {}",
        error.message
    );
    let range = error
        .origin
        .as_ref()
        .and_then(|origin| template_range(origin, "page.html"))
        .expect("the error points into page.html");
    let tag_start = template
        .find("{% call")
        .expect("fixture holds the call tag");
    let tag_end = tag_start + "{% call nowhere() %}".len();
    assert!(
        tag_start <= range.start && range.end <= tag_end,
        "the error points at {range:?} ({:?}), outside the call tag",
        &template[range.clone()]
    );
}

#[test]
fn a_missing_template_file_is_reported_as_an_error() {
    let generated = run(
        r#"#[derive(Template)] #[template(path = "missing.html")] struct Page;"#,
        Memory::with(&[]),
    );
    assert!(
        generated
            .errors
            .iter()
            .any(|error| error.message.contains("missing.html")),
        "the missing file is reported: {:?}",
        generated.errors
    );
}

#[test]
fn an_item_that_is_not_rust_tokens_is_a_lex_error_not_a_panic() {
    let input = Input {
        item: r#"#[template(source = "unterminated) struct X;"#,
        file: Path::new(RUST_FILE),
        manifest_dir: Path::new(MANIFEST_DIR),
    };
    assert!(
        matches!(
            generate(input, Memory::with(&[])),
            Err(super::Error::Lex(_))
        ),
        "an unterminated string is reported as a lex error"
    );
}

#[test]
fn generated_code_is_valid_rust() {
    let files = Memory::with(&[(
        "templates/page.html",
        "{% for item in items %}{{ loop.index }}: {{ item|upper }}{% else %}none{% endfor %}",
    )]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { items: Vec<String> }"#,
        files,
    );
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );
    syn::parse_file(&generated.code)
        .unwrap_or_else(|err| panic!("not valid Rust: {err}\n{}", generated.code));
}

/// koan's pages pass a loop variable's field to an imported macro. askama turns that argument
/// into a string and splits it on `.`, which only works when it is printed as the compiler
/// prints it.
#[test]
fn a_macro_argument_that_is_a_field_of_a_loop_variable_generates() {
    let files = Memory::with(&[(
        "templates/page.html",
        "{% macro row(label) %}<li>{{ label }}</li>{% endmacro %}\
         {% for a in items %}{% call row(a.name) %}{% endcall %}{% endfor %}",
    )]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { items: Vec<Item> }"#,
        files,
    );
    assert!(
        generated.errors.is_empty(),
        "unexpected errors: {:?}",
        generated.errors
    );
    syn::parse_file(&generated.code)
        .unwrap_or_else(|err| panic!("not valid Rust: {err}\n{}", generated.code));
}

#[test]
fn a_compile_error_statement_is_located_in_the_generated_code() {
    let files = Memory::with(&[("templates/page.html", "{% if open %}")]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { open: bool }"#,
        files,
    );
    let error = generated
        .errors
        .first()
        .expect("an unclosed `if` is an error");
    let statement = &generated.code[error.generated.clone()];
    assert!(
        statement.contains("compile_error") && statement.trim_end().ends_with(';'),
        "the range covers the whole `compile_error!` statement, not {statement:?}"
    );
}

#[test]
fn a_syntax_error_is_reported_where_the_parser_stopped_in_the_template() {
    let template = "<p>{% if ready %}</p>";
    let files = Memory::with(&[("templates/page.html", template)]);
    let generated = run(
        r#"#[derive(Template)] #[template(path = "page.html")] struct Page { ready: bool }"#,
        files,
    );
    let error = generated
        .errors
        .first()
        .expect("an unclosed `if` is an error");
    let range = error
        .origin
        .as_ref()
        .and_then(|origin| template_range(origin, "page.html"))
        .unwrap_or_else(|| panic!("the error points into page.html: {error:?}"));
    assert!(
        range.start <= template.len(),
        "the error points inside the template: {range:?}"
    );
    assert!(
        !error.message.contains("-->"),
        "the location is the origin, not text in the message: {}",
        error.message
    );
}
