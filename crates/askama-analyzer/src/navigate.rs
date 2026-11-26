//! Go to definition for what only templates know: the file a path in `extends`, `include` or
//! `import` names, the `{% macro %}` a `{% call %}` names, and the block a child's
//! `{% block %}` overrides in the template it extends.
//!
//! Each answers only with the cursor on the name itself (the path, the callee, the block's
//! name), so an expression elsewhere in the tag is still rust-analyzer's to answer. Template
//! paths resolve among the files askama itself read for some item, looking where askama looks:
//! beside the template first, then up through the directories above it.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// A place in a file, as a byte range of its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub range: Range<usize>,
}

/// What the cursor at `offset` of `text` (the template at `path`) refers to, if it is on a
/// template-level name. `known` are the template files askama read; `read` reads one.
pub fn definition(
    path: &Path,
    text: &str,
    offset: usize,
    known: &[PathBuf],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Option<Target> {
    let (start, tag) = enclosing_tag(text, offset)?;
    let words = words(tag, start);
    let (keyword, _) = *words.first()?;
    let on = |span: &Range<usize>| span.start <= offset && offset <= span.end;
    match keyword {
        "extends" | "include" | "import" => {
            let (name, span) = quoted(tag, start)?;
            on(&span).then_some(())?;
            Some(Target {
                path: resolve(name, path, known)?,
                range: 0..0,
            })
        }
        "call" => {
            let (callee, at) = *words.get(1)?;
            let callee = callee.split('(').next()?;
            on(&(at..at + callee.len())).then_some(())?;
            let (scope, name) = match callee.split_once("::") {
                Some((scope, name)) => (Some(scope), name),
                None => (None, callee),
            };
            match scope {
                None => find_tag(text, "macro", name).map(|range| Target {
                    path: path.to_owned(),
                    range,
                }),
                Some(scope) => {
                    let file = import_of(text, path, scope, known)?;
                    let imported = read(&file)?;
                    find_tag(&imported, "macro", name).map(|range| Target { path: file, range })
                }
            }
        }
        "block" => {
            let (name, at) = *words.get(1)?;
            on(&(at..at + name.len())).then_some(())?;
            let mut current = (path.to_owned(), text.to_owned());
            // A bound on the chain, which askama itself refuses to let loop.
            for _ in 0..32 {
                let parent = extends_of(&current.1, &current.0, known)?;
                let parent_text = read(&parent)?;
                if let Some(range) = find_tag(&parent_text, "block", name) {
                    return Some(Target {
                        path: parent,
                        range,
                    });
                }
                current = (parent, parent_text);
            }
            None
        }
        _ => None,
    }
}

/// The tag holding `offset`: where its trimmed inside starts, and the inside.
fn enclosing_tag(text: &str, offset: usize) -> Option<(usize, &str)> {
    let open = text[..offset].rfind("{%")?;
    if text[open..offset].contains("%}") {
        return None;
    }
    let close = offset + text[offset..].find("%}")?;
    let inside = &text[open + 2..close];
    let trimmed = inside.trim_start_matches(['-', '+', '~']).trim_start();
    let start = open + 2 + (inside.len() - trimmed.len());
    Some((start, trimmed.trim_end_matches(['-', '+', '~']).trim_end()))
}

/// The tag's words, each with its offset in the file.
fn words(tag: &str, start: usize) -> Vec<(&str, usize)> {
    let mut found = Vec::new();
    let mut rest = 0;
    for word in tag.split_whitespace() {
        let at = rest + tag[rest..].find(word).unwrap_or(0);
        found.push((word, start + at));
        rest = at + word.len();
    }
    found
}

/// The tag's quoted path, and where it sits in the file.
fn quoted(tag: &str, start: usize) -> Option<(&str, Range<usize>)> {
    let open = tag.find('"')? + 1;
    let close = open + tag[open..].find('"')?;
    Some((&tag[open..close], start + open..start + close))
}

/// The known template file a tag's path names, resolved as askama does: among the known files
/// whose path ends with the name, those in a directory at or above the reading template's, the
/// nearest first.
fn resolve(name: &str, from: &Path, known: &[PathBuf]) -> Option<PathBuf> {
    let here = from.parent()?;
    let depth = Path::new(name).components().count();
    known
        .iter()
        .filter(|file| file.ends_with(name))
        .filter_map(|file| {
            let base = file.ancestors().nth(depth)?;
            here.starts_with(base)
                .then(|| (base.components().count(), file))
        })
        .max_by_key(|(base_depth, _)| *base_depth)
        .map(|(_, file)| file.clone())
}

/// The file `{% import "..." as scope %}` brings in.
fn import_of(text: &str, from: &Path, scope: &str, known: &[PathBuf]) -> Option<PathBuf> {
    tags(text).find_map(|(at, tag)| {
        let words = words(tag, at);
        (words.first().map(|(word, _)| *word) == Some("import")
            && words.last().map(|(word, _)| *word) == Some(scope))
        .then(|| quoted(tag, at).and_then(|(name, _)| resolve(name, from, known)))
        .flatten()
    })
}

/// The file `{% extends "..." %}` names.
fn extends_of(text: &str, from: &Path, known: &[PathBuf]) -> Option<PathBuf> {
    tags(text).find_map(|(at, tag)| {
        tag.starts_with("extends")
            .then(|| quoted(tag, at).and_then(|(name, _)| resolve(name, from, known)))
            .flatten()
    })
}

/// The range of `name` in the first `{% keyword name ... %}` tag.
fn find_tag(text: &str, keyword: &str, name: &str) -> Option<Range<usize>> {
    tags(text).find_map(|(start, tag)| {
        let words = words(tag, start);
        let (first, _) = *words.first()?;
        let (word, at) = *words.get(1)?;
        (first == keyword && word.split('(').next() == Some(name)).then(|| at..at + name.len())
    })
}

/// Each `{% ... %}` tag: where its trimmed inside starts, and the inside.
fn tags(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let open = rest + text[rest..].find("{%")?;
        let close = open + text[open..].find("%}")?;
        rest = close + 2;
        let inside = &text[open + 2..close];
        let trimmed = inside.trim_start_matches(['-', '+', '~']).trim_start();
        let start = open + 2 + (inside.len() - trimmed.len());
        Some((start, trimmed.trim_end_matches(['-', '+', '~']).trim_end()))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::definition;

    fn files() -> HashMap<PathBuf, String> {
        HashMap::from([
            (
                PathBuf::from("/t/base.html"),
                "<title>{% block c %}x{% endblock %}</title>".to_owned(),
            ),
            (
                PathBuf::from("/t/admin/base.html"),
                "{% block c %}admin{% endblock %}".to_owned(),
            ),
            (
                PathBuf::from("/t/macros.html"),
                "{% macro m(x) %}<li>{{ x }}</li>{% endmacro %}".to_owned(),
            ),
        ])
    }

    fn go(text: &str, needle: &str) -> Option<super::Target> {
        let files = files();
        let known: Vec<PathBuf> = files.keys().cloned().collect();
        let offset = text.find(needle).expect("fixture holds the needle") + 1;
        definition(Path::new("/t/page.html"), text, offset, &known, &|path| {
            files.get(path).cloned()
        })
    }

    #[test]
    fn a_path_resolves_beside_the_template_not_in_a_deeper_directory() {
        let target = go("{% extends \"base.html\" %}", "base").expect("resolves");
        assert_eq!(
            target.path,
            PathBuf::from("/t/base.html"),
            "not /t/admin/base.html"
        );
    }

    #[test]
    fn a_scoped_call_goes_to_the_macros_name_not_its_keyword() {
        let text = "{% import \"macros.html\" as s %}{% call s::m(1) %}{% endcall %}";
        let target = go(text, "s::m").expect("resolves");
        assert_eq!(target.path, PathBuf::from("/t/macros.html"));
        let defined = &files()[&target.path];
        assert_eq!(
            target.range.start,
            defined.find("m(").expect("defined"),
            "the name, not the `m` of `macro`"
        );
    }

    #[test]
    fn an_argument_of_a_call_is_left_to_rust_analyzer() {
        let text = "{% macro m(x) %}{% endmacro %}{% call m(user.name) %}{% endcall %}";
        assert_eq!(go(text, "name"), None);
    }

    #[test]
    fn an_overriding_block_goes_to_the_parents_block_name() {
        let text = "{% extends \"base.html\" %}{% block c %}y{% endblock %}";
        let target = go(text, "c %}y").expect("resolves");
        assert_eq!(target.path, PathBuf::from("/t/base.html"));
        let defined = &files()[&target.path];
        assert_eq!(
            target.range.start,
            defined.find("c %}").expect("defined"),
            "the name, not the `c` of `block`"
        );
    }

    #[test]
    fn an_expression_is_not_a_template_name() {
        assert_eq!(go("{{ user.name }}", "user"), None);
    }
}
