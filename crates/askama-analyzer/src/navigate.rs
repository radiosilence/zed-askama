//! Go to definition for what only templates know: the file a path in `extends`, `include` or
//! `import` names, the `{% macro %}` a `{% call %}` names, and the block a child's
//! `{% block %}` overrides in the template it extends.
//!
//! Template paths resolve among the files askama itself read for some item, so they resolve as
//! askama resolves them, without a second resolver.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// A place in a file, as a byte range of its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub range: Range<usize>,
}

/// What the cursor at `offset` of `text` (the template at `path`) refers to, if it is a
/// template-level name. `known` are the template files askama read; `read` reads one.
pub fn definition(
    path: &Path,
    text: &str,
    offset: usize,
    known: &[PathBuf],
    read: &dyn Fn(&Path) -> Option<String>,
) -> Option<Target> {
    let tag = enclosing_tag(text, offset)?;
    let words: Vec<&str> = tag.split_whitespace().collect();
    let keyword = *words.first()?;
    match keyword {
        "extends" | "include" | "import" => {
            let name = quoted(tag)?;
            let file = resolve(name, known)?;
            Some(Target {
                path: file,
                range: 0..0,
            })
        }
        "call" => {
            let callee = words.get(1)?.split('(').next()?;
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
                    let file = import_of(text, scope, known)?;
                    let imported = read(&file)?;
                    find_tag(&imported, "macro", name).map(|range| Target { path: file, range })
                }
            }
        }
        "block" => {
            let name = *words.get(1)?;
            let mut current = text.to_owned();
            // A bound on the chain, which askama itself refuses to let loop.
            for _ in 0..32 {
                let parent = extends_of(&current, known)?;
                let parent_text = read(&parent)?;
                if let Some(range) = find_tag(&parent_text, "block", name) {
                    return Some(Target {
                        path: parent,
                        range,
                    });
                }
                current = parent_text;
            }
            None
        }
        _ => None,
    }
}

/// The inside of the `{% ... %}` tag holding `offset`, without delimiters or whitespace control.
fn enclosing_tag(text: &str, offset: usize) -> Option<&str> {
    let open = text[..offset].rfind("{%")?;
    if text[open..offset].contains("%}") {
        return None;
    }
    let close = offset + text[offset..].find("%}")?;
    Some(
        text[open + 2..close]
            .trim_start_matches(['-', '+', '~'])
            .trim_end_matches(['-', '+', '~'])
            .trim(),
    )
}

fn quoted(tag: &str) -> Option<&str> {
    let start = tag.find('"')? + 1;
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// The known template file a tag's path names: its path ends with the name, component by
/// component.
fn resolve(name: &str, known: &[PathBuf]) -> Option<PathBuf> {
    known.iter().find(|file| file.ends_with(name)).cloned()
}

/// The file `{% import "..." as scope %}` brings in.
fn import_of(text: &str, scope: &str, known: &[PathBuf]) -> Option<PathBuf> {
    tags(text).find_map(|(_, tag)| {
        let words: Vec<&str> = tag.split_whitespace().collect();
        (words.first() == Some(&"import") && words.last() == Some(&scope))
            .then(|| quoted(tag).and_then(|name| resolve(name, known)))
            .flatten()
    })
}

/// The file `{% extends "..." %}` names.
fn extends_of(text: &str, known: &[PathBuf]) -> Option<PathBuf> {
    tags(text).find_map(|(_, tag)| {
        tag.starts_with("extends")
            .then(|| quoted(tag).and_then(|name| resolve(name, known)))
            .flatten()
    })
}

/// The range of `name` in the first `{% keyword name ... %}` tag.
fn find_tag(text: &str, keyword: &str, name: &str) -> Option<Range<usize>> {
    tags(text).find_map(|(start, tag)| {
        let mut words = tag.split_whitespace();
        if words.next() != Some(keyword) {
            return None;
        }
        let word = words.next()?;
        if word.split('(').next() != Some(name) {
            return None;
        }
        let at = start + text[start..].find(name)?;
        Some(at..at + name.len())
    })
}

/// Each `{% ... %}` tag: where its inside starts, and its trimmed inside.
fn tags(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let open = rest + text[rest..].find("{%")?;
        let close = open + text[open..].find("%}")?;
        rest = close + 2;
        let inside = &text[open + 2..close];
        let trimmed = inside.trim_start_matches(['-', '+', '~']);
        let start = open + 2 + (inside.len() - trimmed.len());
        Some((start, trimmed.trim_end_matches(['-', '+', '~']).trim()))
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
                "<title>{% block title %}x{% endblock %}</title>".to_owned(),
            ),
            (
                PathBuf::from("/t/macros.html"),
                "{% macro row(x) %}<li>{{ x }}</li>{% endmacro %}".to_owned(),
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
    fn a_path_in_extends_goes_to_the_file() {
        let target = go("{% extends \"base.html\" %}", "base").expect("resolves");
        assert_eq!(target.path, PathBuf::from("/t/base.html"));
    }

    #[test]
    fn a_scoped_call_goes_to_the_macro_in_the_imported_file() {
        let text = "{% import \"macros.html\" as m %}{% call m::row(1) %}{% endcall %}";
        let target = go(text, "m::row").expect("resolves");
        assert_eq!(target.path, PathBuf::from("/t/macros.html"));
        let defined = &files()[&target.path];
        assert_eq!(&defined[target.range], "row", "lands on the macro's name");
    }

    #[test]
    fn a_local_call_goes_to_the_macro_in_the_same_file() {
        let text = "{% macro cell() %}{% endmacro %}{% call cell() %}{% endcall %}";
        let target = go(text, "call cell").expect("resolves");
        assert_eq!(&text[target.range], "cell");
        assert_eq!(
            target.range.start,
            text.find("cell").expect("defined"),
            "the definition, not the call"
        );
    }

    #[test]
    fn an_overriding_block_goes_to_the_block_in_the_parent() {
        let text = "{% extends \"base.html\" %}{% block title %}y{% endblock %}";
        let target = go(text, "block title").expect("resolves");
        assert_eq!(target.path, PathBuf::from("/t/base.html"));
    }

    #[test]
    fn an_expression_is_not_a_template_name() {
        assert_eq!(go("{{ user.name }}", "user"), None);
    }
}
