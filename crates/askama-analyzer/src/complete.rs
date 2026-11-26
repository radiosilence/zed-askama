//! Completion that needs no rust-analyzer: tag keywords, askama's built-in filters, and the
//! template paths that `extends`, `include` and `import` take. Each looks at the text before the
//! cursor only, so it answers while the template is half typed.

use std::path::{Path, PathBuf};

use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat};

/// The block tags, and the tag that closes each.
const BLOCKS: &[(&str, &str)] = &[
    ("if", "endif"),
    ("for", "endfor"),
    ("match", "endmatch"),
    ("block", "endblock"),
    ("macro", "endmacro"),
    ("call", "endcall"),
    ("filter", "endfilter"),
    ("raw", "endraw"),
];

/// Every tag keyword, with the snippet it inserts.
const TAGS: &[(&str, &str)] = &[
    ("if", "if ${1:condition}"),
    ("else", "else"),
    ("else if", "else if ${1:condition}"),
    ("for", "for ${1:item} in ${2:items}"),
    ("match", "match ${1:value}"),
    ("when", "when ${1:pattern}"),
    ("let", "let ${1:name} = ${2:value}"),
    ("block", "block ${1:name}"),
    ("macro", "macro ${1:name}(${2})"),
    ("call", "call ${1:name}(${2})"),
    ("filter", "filter ${1:name}"),
    ("include", "include \"${1}\""),
    ("extends", "extends \"${1}\""),
    ("import", "import \"${1}\" as ${2:scope}"),
    ("raw", "raw"),
    ("break", "break"),
    ("continue", "continue"),
];

/// askama 0.16's built-in filters.
pub const FILTERS: &[&str] = &[
    "assigned_or",
    "capitalize",
    "center",
    "default",
    "defined_or",
    "deref",
    "e",
    "escape",
    "filesizeformat",
    "fmt",
    "format",
    "indent",
    "join",
    "json",
    "linebreaks",
    "linebreaksbr",
    "lower",
    "lowercase",
    "paragraphbreaks",
    "pluralize",
    "ref",
    "reject",
    "safe",
    "title",
    "titlecase",
    "tojson",
    "trim",
    "truncate",
    "unique",
    "upper",
    "uppercase",
    "urlencode",
    "urlencode_strict",
    "value",
    "wordcount",
];

/// Where the cursor is, as far as completion without rust-analyzer cares.
#[derive(Debug, PartialEq, Eq)]
pub enum Spot<'a> {
    /// Right after `{%`, possibly part-way through a keyword.
    Keyword { typed: &'a str },
    /// Inside the quotes after `extends`, `include` or `import`.
    Path { typed: &'a str },
    /// Right after `|` in an expression, possibly part-way through a filter's name.
    Filter { typed: &'a str },
    /// Anywhere else, where rust-analyzer answers if anything does.
    Other,
}

/// What kind of completion the cursor at `offset` wants.
pub fn spot(text: &str, offset: usize) -> Spot<'_> {
    let before = &text[..offset];
    let word_start = before
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_alphanumeric() || c == '_')
        .last()
        .map_or(offset, |(at, _)| at);
    let typed = &before[word_start..];
    let tag = before
        .rfind("{%")
        .filter(|&open| !before[open..].contains("%}"));
    if let Some(open) = tag {
        let inside = &before[open + 2..];
        let inside = inside.trim_start_matches(['-', '+', '~']);
        if inside.trim_start() == typed {
            return Spot::Keyword { typed };
        }
        let quote = inside.rfind('"');
        if let Some(quote) = quote
            && inside[..quote].matches('"').count().is_multiple_of(2)
        {
            let keyword = inside[..quote].trim();
            if matches!(keyword, "extends" | "include" | "import") {
                return Spot::Path {
                    typed: &inside[quote + 1..],
                };
            }
        }
    }
    // A `when` arm's `|` separates patterns; no filter goes there.
    let in_pattern = tag.is_some_and(|open| {
        before[open + 2..]
            .trim_start_matches(['-', '+', '~'])
            .trim_start()
            .starts_with("when")
    });
    let in_expression = tag.is_some() && !in_pattern
        || before
            .rfind("{{")
            .is_some_and(|open| !before[open..].contains("}}"));
    if in_expression
        && before[..word_start].trim_end().ends_with('|')
        && !before[..word_start].trim_end().ends_with("||")
    {
        return Spot::Filter { typed };
    }
    Spot::Other
}

/// The block tags open at `offset`, innermost last.
fn open_blocks(text: &str, offset: usize) -> Vec<&'static str> {
    let mut open = Vec::new();
    let mut rest = &text[..offset];
    while let Some(at) = rest.find("{%") {
        rest = &rest[at + 2..];
        let word: String = rest
            .trim_start_matches(['-', '+', '~'])
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if let Some(&(opener, _)) = BLOCKS.iter().find(|(opener, _)| *opener == word) {
            // `{% let x %}` without a value opens a block too, but is rare enough to leave out.
            open.push(opener);
        } else if let Some(&(opener, _)) = BLOCKS.iter().find(|(_, closer)| *closer == word)
            && let Some(at) = open.iter().rposition(|known| *known == opener)
        {
            open.truncate(at);
        }
    }
    open
}

/// Tag keywords for the cursor after `{%`, the tag that closes the innermost open block first.
pub fn keywords(text: &str, offset: usize) -> Vec<CompletionItem> {
    let closer = open_blocks(text, offset)
        .last()
        .and_then(|opener| BLOCKS.iter().find(|(known, _)| known == opener))
        .map(|(_, closer)| *closer);
    let mut items: Vec<CompletionItem> = Vec::new();
    if let Some(closer) = closer {
        items.push(CompletionItem {
            label: closer.to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("closes the innermost open block".to_owned()),
            sort_text: Some("0".to_owned()),
            preselect: Some(true),
            ..CompletionItem::default()
        });
    }
    for (label, snippet) in TAGS {
        items.push(CompletionItem {
            label: (*label).to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            insert_text: Some((*snippet).to_owned()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: Some(format!("1{label}")),
            ..CompletionItem::default()
        });
    }
    items
}

/// askama's built-in filters.
pub fn filters() -> Vec<CompletionItem> {
    FILTERS
        .iter()
        .map(|name| CompletionItem {
            label: (*name).to_owned(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("askama filter".to_owned()),
            ..CompletionItem::default()
        })
        .collect()
}

/// The directories askama looks for a package's templates in: `askama.toml`'s
/// `[general] dirs`, relative to the package, or `templates` when it names none.
pub fn template_dirs(manifest_dir: &Path, config: Option<&str>) -> Vec<PathBuf> {
    let named: Vec<PathBuf> = config
        .and_then(|config| {
            let rest = &config[config.find("dirs")?..];
            let list = &rest[rest.find('[')? + 1..rest.find(']')?];
            Some(
                list.split(',')
                    .map(|dir| dir.trim().trim_matches(['"', '\'']))
                    .filter(|dir| !dir.is_empty())
                    .map(|dir| manifest_dir.join(dir))
                    .collect(),
            )
        })
        .unwrap_or_default();
    if named.is_empty() {
        vec![manifest_dir.join("templates")]
    } else {
        named
    }
}

/// The template files under `dirs`, as the relative paths a tag names them by.
pub fn paths(dirs: &[PathBuf]) -> Vec<CompletionItem> {
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, dir, 0, &mut found);
    }
    found.sort();
    found.dedup();
    found
        .into_iter()
        .map(|path| CompletionItem {
            label: path,
            kind: Some(CompletionItemKind::FILE),
            ..CompletionItem::default()
        })
        .collect()
}

/// Deeper than any real template tree; a bound for a walk something made recursive.
const MAX_DEPTH: usize = 16;

fn walk(root: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    if depth > MAX_DEPTH {
        return;
    }
    for entry in entries.flatten() {
        let path = entry.path();
        // Followed through symbolic links, as askama follows them; the depth bounds a loop.
        let Ok(kind) = std::fs::metadata(&path).map(|metadata| metadata.file_type()) else {
            continue;
        };
        if kind.is_dir() {
            walk(root, &path, depth + 1, found);
        } else if kind.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Spot, keywords, spot, template_dirs};

    fn at(text: &str) -> (String, usize) {
        let offset = text.find('^').expect("the fixture marks the cursor with ^");
        (text.replacen('^', "", 1), offset)
    }

    #[test]
    fn the_cursor_after_a_tag_opening_wants_a_keyword() {
        let (text, offset) = at("<p>{% fo^ %}");
        assert_eq!(spot(&text, offset), Spot::Keyword { typed: "fo" });
        let (text, offset) = at("<p>{%- ^ %}");
        assert_eq!(spot(&text, offset), Spot::Keyword { typed: "" });
    }

    #[test]
    fn the_cursor_inside_an_include_string_wants_a_path() {
        let (text, offset) = at("{% include \"parts/he^ %}");
        assert_eq!(spot(&text, offset), Spot::Path { typed: "parts/he" });
    }

    #[test]
    fn the_cursor_after_a_pipe_wants_a_filter_but_not_after_or() {
        let (text, offset) = at("{{ name|up^ }}");
        assert_eq!(spot(&text, offset), Spot::Filter { typed: "up" });
        let (text, offset) = at("{% if a || b^ %}");
        assert_eq!(spot(&text, offset), Spot::Other, "`||` is or, not a filter");
    }

    #[test]
    fn a_pipe_between_patterns_is_not_a_filter() {
        let (text, offset) = at("{% match x %}{% when Some(a) | No^ %}");
        assert_eq!(spot(&text, offset), Spot::Other);
    }

    #[test]
    fn askama_toml_dirs_replace_templates() {
        let root = std::path::Path::new("/p");
        assert_eq!(
            template_dirs(root, Some("[general]\ndirs = [\"views\", 'more']\n")),
            vec![root.join("views"), root.join("more")]
        );
        assert_eq!(template_dirs(root, None), vec![root.join("templates")]);
    }

    #[test]
    fn an_expression_is_left_to_rust_analyzer() {
        let (text, offset) = at("{{ user.na^ }}");
        assert_eq!(spot(&text, offset), Spot::Other);
    }

    #[test]
    fn the_closing_tag_of_the_innermost_open_block_comes_first() {
        let (text, offset) = at("{% for x in xs %}{% if x %}{% endif %}{% ^");
        let items = keywords(&text, offset);
        assert_eq!(
            items[0].label, "endfor",
            "the `if` is closed, the `for` is not"
        );
    }
}
