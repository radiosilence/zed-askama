//! `askama-analyzer check`: the language server's template errors, printed for a terminal or
//! CI.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;

use crate::analysis::Analysis;
use crate::generator::Generator;
use crate::vfs::Vfs;

pub async fn run(root: &Path) -> anyhow::Result<ExitCode> {
    let root = root
        .canonicalize()
        .with_context(|| format!("finding {}", root.display()))?;
    let analysis = Analysis::load(&root, Generator::spawn()?, Vfs::default()).await?;

    let problems = analysis.problems();
    let mut count = 0;
    for (path, list) in &problems {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let shown = path.strip_prefix(&root).unwrap_or(path).display();
        for problem in list {
            let (line, column) = line_column(&text, problem.range.start);
            println!("{shown}:{line}:{column}: {}", problem.message);
            count += 1;
        }
    }
    for (name, failure) in analysis.failures() {
        println!("{name}: askama-analyzer could not generate this item: {failure}");
        count += 1;
    }
    eprintln!(
        "{} template items in {} packages; {count} problems",
        analysis.item_count(),
        analysis.packages.len()
    );
    Ok(if count == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// One-based line and character column, as editors and compilers print them.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |line| line.chars().count())
        + 1;
    (line, column)
}
