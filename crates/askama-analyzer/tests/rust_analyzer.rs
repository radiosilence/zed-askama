//! The analyser against the `shop` fixture crate with a real rust-analyzer, as the editor runs
//! it: errors, hover, definitions, references and completion, each in template positions.
//!
//! rust-analyzer's path comes from `ASKAMA_ANALYZER_RUST_ANALYZER`; without it every test says
//! it skipped. Run them with
//! `ASKAMA_ANALYZER_RUST_ANALYZER="$(rustup which rust-analyzer)" cargo test -p askama-analyzer --test rust_analyzer`.

use std::path::{Path, PathBuf};

use askama_analyzer::check;
use askama_analyzer::features::{self, Query};
use askama_analyzer::text::{Encoding, Lines};
use askama_analyzer::workspace::Workspace;
use lsp_types::{
    CompletionParams, CompletionResponse, GotoDefinitionParams, GotoDefinitionResponse,
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Url,
};
use tokio::sync::oneshot;

fn rust_analyzer() -> Option<PathBuf> {
    let path = std::env::var_os("ASKAMA_ANALYZER_RUST_ANALYZER").map(PathBuf::from);
    if path.is_none() {
        eprintln!("skipped: set ASKAMA_ANALYZER_RUST_ANALYZER to a rust-analyzer binary");
    }
    path
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop")
}

async fn shop(rust_analyzer: PathBuf) -> Workspace {
    check::analyse(&fixture(), Some(rust_analyzer), Encoding::Utf8)
        .await
        .expect("the fixture loads and rust-analyzer checks it")
}

/// The position just after `needle`'s first occurrence in a template, plus `into` bytes.
fn after(template: &Path, needle: &str, into: usize) -> (Url, Position) {
    let text = std::fs::read_to_string(template).expect("the fixture template exists");
    let at = text.find(needle).expect("the fixture holds the needle") + needle.len() + into;
    let uri = Url::from_file_path(template).expect("the fixture path is absolute");
    (uri, Lines::new(&text, Encoding::Utf8).position(at))
}

#[tokio::test]
async fn a_field_the_struct_does_not_have_is_an_error_on_its_name_in_the_template() {
    let Some(rust_analyzer) = rust_analyzer() else {
        return;
    };
    let workspace = shop(rust_analyzer).await;
    let problems = workspace.problems();
    let broken = fixture().join("templates/broken.html");
    let text = std::fs::read_to_string(&broken).expect("broken.html exists");
    // rust-analyzer reports an unknown field on the whole field access.
    let list = problems
        .get(&broken)
        .unwrap_or_else(|| panic!("broken.html has no problem; all problems: {problems:?}"));
    assert!(
        list.iter()
            .any(|problem| text[problem.range.clone()] == *"product.nme"),
        "no error covers `nme`: {list:?}"
    );
    assert!(
        !problems.contains_key(&fixture().join("templates/shop.html")),
        "shop.html renders cleanly but has problems: {problems:?}"
    );
}

#[tokio::test]
async fn hover_on_a_field_shows_its_documentation() {
    let Some(rust_analyzer) = rust_analyzer() else {
        return;
    };
    let mut workspaces = vec![shop(rust_analyzer).await];
    let (uri, position) = after(&fixture().join("templates/shop.html"), "product.", 1);
    let (reply, answer) = oneshot::channel();
    features::answer(
        &mut workspaces,
        Query::Hover(
            HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position,
                },
                work_done_progress_params: Default::default(),
            },
            reply,
        ),
    )
    .await;
    let hover = answer
        .await
        .expect("the lane answers")
        .expect("hover has an answer");
    let shown = match hover.contents {
        HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        shown.contains("What the product is called"),
        "the hover does not show the field's docs: {shown}"
    );
}

#[tokio::test]
async fn going_to_a_fields_definition_lands_on_the_field_in_rust() {
    let Some(rust_analyzer) = rust_analyzer() else {
        return;
    };
    let mut workspaces = vec![shop(rust_analyzer).await];
    let (uri, position) = after(&fixture().join("templates/shop.html"), "product.", 1);
    let (reply, answer) = oneshot::channel();
    features::answer(
        &mut workspaces,
        Query::Definition(
            GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            reply,
        ),
    )
    .await;
    let Some(GotoDefinitionResponse::Array(locations)) = answer.await.expect("the lane answers")
    else {
        panic!("no definition");
    };
    let lib = fixture().join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).expect("lib.rs exists");
    let lines = Lines::new(&text, Encoding::Utf8);
    assert!(
        locations.iter().any(|location| {
            location.uri.to_file_path().ok().as_deref() == Some(lib.as_path())
                && lines
                    .offset(location.range.start)
                    .is_some_and(|start| text[start..].starts_with("name"))
        }),
        "no definition lands on `name` in lib.rs: {locations:?}"
    );
}

#[tokio::test]
async fn completing_after_a_dot_offers_the_fields_and_methods_of_the_loop_variable() {
    let Some(rust_analyzer) = rust_analyzer() else {
        return;
    };
    let mut workspaces = vec![shop(rust_analyzer).await];
    let (uri, position) = after(&fixture().join("templates/shop.html"), "{{ product.", 0);
    let (reply, answer) = oneshot::channel();
    features::answer(
        &mut workspaces,
        Query::Completion(
            CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position,
                },
                context: None,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            reply,
        ),
    )
    .await;
    let labels: Vec<String> = match answer.await.expect("the lane answers") {
        Some(CompletionResponse::List(list)) => {
            list.items.into_iter().map(|item| item.label).collect()
        }
        Some(CompletionResponse::Array(items)) => {
            items.into_iter().map(|item| item.label).collect()
        }
        None => Vec::new(),
    };
    for wanted in ["name", "price_pence"] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "`{wanted}` is not offered: {labels:?}"
        );
    }
    assert!(
        labels.iter().any(|label| label.starts_with("price(")),
        "the method `price` is not offered: {labels:?}"
    );
}

#[tokio::test]
async fn completing_a_bare_name_offers_the_loop_variable_in_scope() {
    let Some(rust_analyzer) = rust_analyzer() else {
        return;
    };
    let mut workspaces = vec![shop(rust_analyzer).await];
    // Inside the loop, on the `p` of `product.name`: a bare name being typed.
    let (uri, position) = after(&fixture().join("templates/shop.html"), "<li>{{ p", 0);
    let (reply, answer) = oneshot::channel();
    features::answer(
        &mut workspaces,
        Query::Completion(
            CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position,
                },
                context: None,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            reply,
        ),
    )
    .await;
    let labels: Vec<String> = match answer.await.expect("the lane answers") {
        Some(CompletionResponse::List(list)) => {
            list.items.into_iter().map(|item| item.label).collect()
        }
        Some(CompletionResponse::Array(items)) => {
            items.into_iter().map(|item| item.label).collect()
        }
        None => Vec::new(),
    };
    assert!(
        labels.iter().any(|label| label == "product"),
        "the loop variable `product` is not offered: {labels:?}"
    );
}
