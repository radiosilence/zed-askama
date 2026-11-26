//! Parses templates from real projects, which are not part of this repository.
//!
//! Set `ASKAMA_GRAMMAR_TEMPLATE_DIRS` to a list of directories, separated as
//! `PATH` is, and every file under a `templates` directory in them must parse
//! without errors:
//!
//! ```sh
//! ASKAMA_GRAMMAR_TEMPLATE_DIRS=~/src/slsk-mcp:~/src/koan cargo test --test real_templates
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::Parser;

fn templates_under(directory: &Path, inside_templates: bool, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if matches!(name.to_str(), Some("target" | "node_modules" | ".git")) {
                continue;
            }
            templates_under(&path, inside_templates || name == "templates", found);
        } else if inside_templates {
            found.push(path);
        }
    }
}

#[test]
fn every_template_in_the_given_projects_parses_without_errors() {
    let Some(directories) = std::env::var_os("ASKAMA_GRAMMAR_TEMPLATE_DIRS") else {
        eprintln!("skipped: ASKAMA_GRAMMAR_TEMPLATE_DIRS is not set");
        return;
    };
    let mut templates = Vec::new();
    for directory in std::env::split_paths(&directories) {
        templates_under(&directory, false, &mut templates);
    }
    assert!(
        !templates.is_empty(),
        "no templates found under ASKAMA_GRAMMAR_TEMPLATE_DIRS"
    );

    let mut parser = Parser::new();
    parser
        .set_language(&askama_analyzer_grammar::LANGUAGE.into())
        .expect("the grammar was generated for this tree-sitter version");
    let failures: Vec<String> = templates
        .iter()
        .filter(|path| {
            let bytes = fs::read(path).expect("the template is readable");
            let tree = parser
                .parse(String::from_utf8_lossy(&bytes).as_ref(), None)
                .expect("no timeout or cancellation is set");
            tree.root_node().has_error()
        })
        .map(|path| path.display().to_string())
        .collect();

    eprintln!(
        "{} templates, {} with errors",
        templates.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "templates failed to parse:\n{}",
        failures.join("\n")
    );
}
