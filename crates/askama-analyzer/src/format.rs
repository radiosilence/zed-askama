//! The template formatter behind `askama-analyzer fmt` and `textDocument/formatting`.
//!
//! Only the inside of tags changes: one space inside `{{ }}`, `{% %}` and `{# #}`, and
//! canonical spacing between the tokens of expressions, patterns, filter chains and
//! arguments (`a + b`, `f(a, b)`, `x|filter(y)`, `a..b`). Everything askama renders byte for
//! byte stays as written: text outside tags, `raw` blocks, comments' text, whitespace-control
//! markers and Rust macro arguments (which askama keeps as text). Whitespace inside a tag
//! that holds a line break is kept as written too, so a tag laid out over several lines keeps
//! its layout; re-indenting changes rendered whitespace and is left for an opt-in.
//!
//! The formatter checks its own work. The result must parse to the same `askama_parser` tree
//! as the original, spans aside, and formatting it again must change nothing; otherwise it
//! refuses and says which tag it could not format.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use askama_parser::{Ast, Syntax};

/// Why a template was not formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FormatError {
    /// The template does not parse, so there is no tree to keep. Fix the template first.
    Parse(String),
    /// Formatting the tag at this position changed what the template means, or would change
    /// again on a second pass. A bug in the formatter; the template is left as it was.
    Unsafe {
        line: usize,
        column: usize,
        tag: String,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "the template does not parse: {message}"),
            Self::Unsafe { line, column, tag } => write!(
                f,
                "formatting the tag at {line}:{column} would change the template, so it was left as written: {tag}"
            ),
        }
    }
}

impl std::error::Error for FormatError {}

/// Formats `source`, a template in `syntax`, returning the formatted text.
pub fn format(source: &str, syntax: &Syntax<'_>) -> Result<String, FormatError> {
    let original = tree(source, syntax).map_err(FormatError::Parse)?;
    let segments = segments(source, syntax);
    let formatted = render(source, &segments, None);
    let second = render(&formatted, &self::segments(&formatted, syntax), None);
    if tree(&formatted, syntax).as_ref() == Ok(&original) && second == formatted {
        return Ok(formatted);
    }
    // Find the first tag that, formatted on its own, changes the tree or is not stable.
    for (index, segment) in segments.iter().enumerate() {
        if !matches!(segment, Segment::Tag { .. }) {
            continue;
        }
        let alone = render(source, &segments, Some(index));
        let again = render(&alone, &self::segments(&alone, syntax), None);
        let stable = render(&again, &self::segments(&again, syntax), None) == again;
        if tree(&alone, syntax).as_ref() != Ok(&original) || !stable {
            return Err(unsafe_at(source, segment.range()));
        }
    }
    Err(unsafe_at(source, 0..source.len().min(40)))
}

fn unsafe_at(source: &str, range: std::ops::Range<usize>) -> FormatError {
    let before = &source[..range.start];
    let line = before.matches('\n').count() + 1;
    let column = before.len() - before.rfind('\n').map_or(0, |newline| newline + 1) + 1;
    let tag: String = source[range].chars().take(80).collect();
    FormatError::Unsafe { line, column, tag }
}

/// The template's tree without spans, which `Debug` leaves out, and without comments' text,
/// whose spacing the formatter may change.
fn tree(source: &str, syntax: &Syntax<'_>) -> Result<String, String> {
    let ast = Ast::from_str(source, None, syntax).map_err(|error| error.to_string())?;
    Ok(blank_comment_text(&format!("{:?}", ast.nodes())))
}

fn blank_comment_text(debug: &str) -> String {
    const MARKER: &str = "Comment { ws: ";
    let mut out = String::with_capacity(debug.len());
    let mut rest = debug;
    while let Some(start) = rest.find(MARKER) {
        let Some(content) = rest[start..].find("content: \"") else {
            break;
        };
        let text_start = start + content + "content: \"".len();
        out.push_str(&rest[..text_start]);
        let mut end = text_start;
        let bytes = rest.as_bytes();
        while end < bytes.len() && bytes[end] != b'"' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        rest = &rest[end.min(rest.len())..];
    }
    out.push_str(rest);
    out
}

/// A part of the template: text and raw blocks copied as they are, tags formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Verbatim(std::ops::Range<usize>),
    Tag {
        kind: TagKind,
        /// The whole tag, delimiters included.
        range: std::ops::Range<usize>,
        /// Between the delimiters and any whitespace-control markers.
        inner: std::ops::Range<usize>,
    },
}

impl Segment {
    fn range(&self) -> std::ops::Range<usize> {
        match self {
            Self::Verbatim(range) | Self::Tag { range, .. } => range.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Expression,
    Block,
    Comment,
}

fn is_marker(byte: u8) -> bool {
    matches!(byte, b'-' | b'+' | b'~')
}

/// Splits `source` into text, raw blocks and tags, by the delimiters of `syntax`.
fn segments(source: &str, syntax: &Syntax<'_>) -> Vec<Segment> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        let (kind, open, close) = if rest.starts_with(syntax.expr_start) {
            (TagKind::Expression, syntax.expr_start, syntax.expr_end)
        } else if rest.starts_with(syntax.block_start) {
            (TagKind::Block, syntax.block_start, syntax.block_end)
        } else if rest.starts_with(syntax.comment_start) {
            (TagKind::Comment, syntax.comment_start, syntax.comment_end)
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let found = match kind {
            TagKind::Comment => comment_end(source, i + open.len(), syntax),
            _ => tag_end(source, i + open.len(), close),
        };
        let Some(end) = found else {
            // An unclosed tag: the rest is left alone (the template does not parse anyway).
            break;
        };
        let range = i..end;
        if i > text_start {
            out.push(Segment::Verbatim(text_start..i));
        }
        let mut inner = i + open.len()..end - close.len();
        if inner.start < inner.end && is_marker(bytes[inner.start]) {
            inner.start += 1;
        }
        if inner.start < inner.end && is_marker(bytes[inner.end - 1]) {
            inner.end -= 1;
        }
        if kind == TagKind::Block && first_word(&source[inner.clone()]) == "raw" {
            let end = raw_end(source, end, syntax).unwrap_or(source.len());
            out.push(Segment::Verbatim(i..end));
            text_start = end;
            i = end;
            continue;
        }
        out.push(Segment::Tag { kind, range, inner });
        text_start = end;
        i = end;
    }
    if text_start < source.len() {
        out.push(Segment::Verbatim(text_start..source.len()));
    }
    out
}

fn first_word(text: &str) -> &str {
    let text = text.trim_start();
    let end = text
        .find(|c: char| !(c == '_' || c.is_alphanumeric()))
        .unwrap_or(text.len());
    &text[..end]
}

/// The end of a tag closed by `close`, after its contents: string and character literals may
/// hold the closing delimiter.
fn tag_end(source: &str, start: usize, close: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if source[i..].starts_with(close) {
            return Some(i + close.len());
        }
        match bytes[i] {
            b'"' => i = skip_string(bytes, i),
            b'\'' => i = skip_char(bytes, i),
            _ => i += 1,
        }
    }
    None
}

fn skip_string(bytes: &[u8], quote: usize) -> usize {
    let mut i = quote + 1;
    while i < bytes.len() && bytes[i] != b'"' {
        i += if bytes[i] == b'\\' { 2 } else { 1 };
    }
    i + 1
}

/// A character literal (`'x'`, `'\n'`, `'\u{1F600}'`), or else one byte: a lifetime in a
/// macro's arguments.
fn skip_char(bytes: &[u8], quote: usize) -> usize {
    let mut i = quote + 1;
    if bytes.get(i) == Some(&b'\\') {
        i += 2;
        while i < bytes.len() && bytes[i] != b'\'' && i - quote < 12 {
            i += 1;
        }
        return if bytes.get(i) == Some(&b'\'') {
            i + 1
        } else {
            quote + 1
        };
    }
    // One character, which may be several bytes.
    let width = std::str::from_utf8(&bytes[i..bytes.len().min(i + 4)])
        .ok()
        .or_else(|| std::str::from_utf8(&bytes[i..bytes.len().min(i + 1)]).ok())
        .and_then(|text| text.chars().next())
        .map_or(1, char::len_utf8);
    if bytes.get(i + width) == Some(&b'\'') {
        i + width + 1
    } else {
        quote + 1
    }
}

/// The end of a comment whose opening delimiter ends at `start`; comments nest.
fn comment_end(source: &str, start: usize, syntax: &Syntax<'_>) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while i < source.len() {
        let rest = &source[i..];
        if rest.starts_with(syntax.comment_start) {
            depth += 1;
            i += syntax.comment_start.len();
        } else if rest.starts_with(syntax.comment_end) {
            i += syntax.comment_end.len();
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// The end of the `endraw` tag closing a raw block whose opening tag ends at `start`, found as
/// askama finds it: the block start, an optional marker, spaces, `endraw`, spaces, an optional
/// marker and the block end.
fn raw_end(source: &str, start: usize, syntax: &Syntax<'_>) -> Option<usize> {
    let mut from = start;
    while let Some(found) = source[from..].find(syntax.block_start) {
        let open = from + found;
        let mut i = open + syntax.block_start.len();
        let bytes = source.as_bytes();
        if i < bytes.len() && is_marker(bytes[i]) {
            i += 1;
        }
        let after = source[i..].trim_start_matches(|c: char| c.is_ascii_whitespace());
        if let Some(after) = after.strip_prefix("endraw") {
            let after = after.trim_start_matches(|c: char| c.is_ascii_whitespace());
            let after = after.strip_prefix(['-', '+', '~']).unwrap_or(after);
            if after.starts_with(syntax.block_end) {
                return Some(source.len() - after.len() + syntax.block_end.len());
            }
        }
        from = open + syntax.block_start.len();
    }
    None
}

/// The template with its tags formatted: all of them, or only the one at `only`.
fn render(source: &str, segments: &[Segment], only: Option<usize>) -> String {
    let mut out = String::with_capacity(source.len());
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            Segment::Tag { kind, range, inner } if only.is_none_or(|only| only == index) => {
                out.push_str(&source[range.start..inner.start]);
                let body = &source[inner.clone()];
                match kind {
                    TagKind::Comment => out.push_str(&pad(body, body.trim())),
                    TagKind::Expression | TagKind::Block => match format_tokens(body) {
                        Some(tokens) => out.push_str(&pad(body, &tokens)),
                        None => out.push_str(body),
                    },
                }
                out.push_str(&source[inner.end..range.end]);
            }
            other => out.push_str(&source[other.range()]),
        }
    }
    out
}

/// `formatted` with one space on each side, except where the original's whitespace on that
/// side holds a line break, which is kept. An empty tag stays empty.
fn pad(original: &str, formatted: &str) -> String {
    if formatted.is_empty() {
        return original.to_owned();
    }
    let leading = &original[..original.len() - original.trim_start().len()];
    let trailing = &original[original.trim_end().len()..];
    let side = |space: &str| {
        if space.contains('\n') {
            space.to_owned()
        } else {
            " ".to_owned()
        }
    };
    format!("{}{}{}", side(leading), formatted, side(trailing))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Literal,
    Punct,
    /// A Rust macro's arguments, delimiters included, kept as written.
    Verbatim,
}

#[derive(Debug, Clone, Copy)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    /// The whitespace before the token, when it holds a line break and so is kept.
    kept_gap: Option<&'a str>,
}

const PUNCTUATION: &[&str] = &[
    "..=", "<<=", ">>=", "...", "::", "..", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "=>", "->", "+", "-", "*", "/", "%", "&", "|", "^",
    "!", "=", "<", ">", "(", ")", "[", "]", "{", "}", ",", ";", ":", ".", "?", "@", "~", "#",
];

/// The tag's tokens, or `None` when something in it is not understood, in which case the tag
/// is left as written.
fn tokenize(body: &str) -> Option<Vec<Token<'_>>> {
    let bytes = body.as_bytes();
    let mut tokens: Vec<Token<'_>> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let gap_start = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        let gap = &body[gap_start..i];
        let kept_gap = (gap.contains('\n') && !tokens.is_empty()).then_some(gap);
        let start = i;
        let c = bytes[i];
        let rest = &body[i..];
        let kind = if c == b'"' || (matches!(c, b'b' | b'c') && bytes.get(i + 1) == Some(&b'"')) {
            i = skip_string(bytes, i + usize::from(c != b'"'));
            Kind::Literal
        } else if c == b'\'' || (c == b'b' && bytes.get(i + 1) == Some(&b'\'')) {
            let quote = i + usize::from(c == b'b');
            i = skip_char(bytes, quote);
            if i == quote + 1 {
                return None;
            }
            Kind::Literal
        } else if c.is_ascii_digit() {
            i = number_end(bytes, i);
            Kind::Literal
        } else if c == b'_' || rest.chars().next().is_some_and(char::is_alphabetic) {
            if rest.starts_with("r#") {
                i += 2;
            }
            i += body[i..]
                .find(|ch: char| !(ch == '_' || ch.is_alphanumeric()))
                .unwrap_or(body.len() - i);
            Kind::Word
        } else {
            // Anything else is not understood, and the tag is left as written.
            let punct = PUNCTUATION.iter().find(|p| rest.starts_with(**p))?;
            i += punct.len();
            // A Rust macro call: `name!` then a group, kept as written.
            if *punct == "!" && tokens.last().is_some_and(|t| t.kind == Kind::Word) {
                let after = body[i..].trim_start();
                if after.starts_with(['(', '[', '{']) {
                    tokens.push(Token {
                        kind: Kind::Punct,
                        text: "!",
                        kept_gap,
                    });
                    let group_start = body.len() - after.len();
                    let group_end = group_end(body, group_start)?;
                    tokens.push(Token {
                        kind: Kind::Verbatim,
                        text: &body[group_start..group_end],
                        kept_gap: None,
                    });
                    i = group_end;
                    continue;
                }
            }
            Kind::Punct
        };
        tokens.push(Token {
            kind,
            text: &body[start..i],
            kept_gap,
        });
    }
    Some(tokens)
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let hex = bytes
        .get(start + 1)
        .is_some_and(|b| matches!(b, b'x' | b'X'))
        && bytes[start] == b'0';
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        // Digits, a suffix, a fraction (`1.5` but not `1..`), and an exponent's sign.
        let fraction = b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
        let exponent_sign = matches!(b, b'+' | b'-')
            && !hex
            && matches!(bytes[i - 1], b'e' | b'E')
            && bytes
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_digit() || *n == b'_');
        if b.is_ascii_alphanumeric() || b == b'_' || fraction || exponent_sign {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// The end of the balanced group opening at `start`, string and character literals skipped.
fn group_end(body: &str, start: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => i = skip_string(bytes, i),
            b'\'' => i = skip_char(bytes, i),
            _ => i += 1,
        }
    }
    None
}

/// Words after which an operand starts, so a following `-`, `*`, `&` or `!` is a prefix
/// operator and `(` or `[` opens a group rather than a call or an index.
const LEADING_KEYWORDS: &[&str] = &[
    "if", "elif", "else", "for", "in", "match", "when", "let", "set", "mut", "is", "not", "as",
    "or", "with", "xor", "bitand", "bitor", "filter", "extends", "include", "import", "block",
    "macro", "decl", "declare", "return",
];

fn is_leading_keyword(token: &Token<'_>) -> bool {
    token.kind == Kind::Word && LEADING_KEYWORDS.contains(&token.text)
}

/// Whether `token` ends an operand, so what follows is a suffix or a binary operator.
fn ends_operand(token: &Token<'_>, generic_close: bool) -> bool {
    match token.kind {
        Kind::Word => !is_leading_keyword(token),
        Kind::Literal | Kind::Verbatim => true,
        Kind::Punct => matches!(token.text, ")" | "]" | "}" | "?") || generic_close,
    }
}

/// The tag's tokens with canonical spacing, or `None` when the tag is not understood.
fn format_tokens(body: &str) -> Option<String> {
    let tokens = tokenize(body)?;
    let tag = tokens
        .first()
        .filter(|t| t.kind == Kind::Word)
        .map(|t| t.text);
    let mut out = String::new();
    // Open groups, to tell a `when` arm's `|` alternatives from a filter's `|`.
    let mut depth = 0usize;
    // Generic arguments: after `::<`, or in a type after `:` in `let` and `macro` tags.
    let mut generics = 0usize;
    let mut in_type = false;
    let mut type_depth = 0usize;
    // Whether the previous token was a prefix operator, and whether it closed generics.
    let mut prefix = false;
    let mut generic_close = false;

    for (index, token) in tokens.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| tokens[i]);
        let text = token.text;

        let opens_generics = text == "<"
            && (previous.is_some_and(|p| p.text == "::")
                || (in_type && generics == 0)
                || generics > 0);
        let closes_generics = generics > 0 && matches!(text, ">" | ">>");
        let is_prefix = token.kind == Kind::Punct
            && matches!(text, "-" | "*" | "&" | "&&" | "!")
            && previous.is_none_or(|p| !ends_operand(&p, generic_close));

        let space = match previous {
            None => false,
            Some(previous) => {
                let p = previous.text;
                if prefix {
                    false
                } else if matches!(text, "," | ";" | ")" | "]" | "?" | "." | "::")
                    || matches!(p, "(" | "[" | "." | "::")
                {
                    // `::std` after a keyword keeps its space.
                    text == "::" && is_leading_keyword(&previous)
                } else if matches!(p, "," | ";") || text == "{" {
                    true
                } else if p == "{" {
                    text != "}"
                } else if text == "}" {
                    true
                } else if text == ":"
                    || opens_generics
                    || closes_generics
                    || (generic_close && matches!(text, "(" | "::"))
                    || (p == "<" && generics > 0)
                    || matches!(text, ".." | "..=")
                    || matches!(p, ".." | "..=")
                {
                    // Tight: a type or field's colon, generics, ranges.
                    false
                } else if text == "|" || p == "|" {
                    tag == Some("when") && depth == 0
                } else if text == "(" || text == "[" {
                    !ends_operand(&previous, generic_close) && !(p == "call" && text == "(")
                } else if text == "!"
                    && previous.kind == Kind::Word
                    && tokens
                        .get(index + 1)
                        .is_some_and(|n| n.kind == Kind::Verbatim)
                {
                    false
                } else {
                    !(p == "!" && token.kind == Kind::Verbatim)
                }
            }
        };

        if let Some(gap) = token.kept_gap {
            out.push_str(gap);
        } else if space {
            out.push(' ');
        }
        out.push_str(text);

        match text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth = depth.saturating_sub(1),
            _ => {}
        }
        if text == ":" && matches!(tag, Some("let" | "set" | "macro")) {
            in_type = true;
            type_depth = depth;
        } else if in_type && depth <= type_depth && generics == 0 && matches!(text, "=" | "," | ")")
        {
            in_type = false;
        }
        if opens_generics {
            generics += 1;
        }
        generic_close = closes_generics;
        if closes_generics {
            generics = generics.saturating_sub(if text == ">>" { 2 } else { 1 });
        }
        prefix = is_prefix;
    }
    Some(out)
}

/// `askama-analyzer fmt`: formats the templates at `paths` in place or, with `check`, lists
/// those that are not formatted. A directory means every file under a `templates` directory
/// in it. Templates use askama's default delimiters.
pub fn run(check: bool, paths: &[PathBuf]) -> anyhow::Result<ExitCode> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            templates_under(path, false, &mut files);
        } else {
            files.push(path.clone());
        }
    }
    files.sort();
    let syntax = Syntax::default();
    let mut failed = false;
    for file in &files {
        let source = match fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{}: {error}", file.display());
                failed = true;
                continue;
            }
        };
        match format(&source, &syntax) {
            Ok(formatted) if formatted == source => {}
            Ok(_) if check => {
                println!("{}", file.display());
                failed = true;
            }
            Ok(formatted) => {
                if let Err(error) = fs::write(file, formatted) {
                    eprintln!("{}: {error}", file.display());
                    failed = true;
                }
            }
            Err(error) => {
                eprintln!("{}: {error}", file.display());
                failed = true;
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn templates_under(directory: &Path, inside: bool, found: &mut Vec<PathBuf>) {
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
            templates_under(&path, inside || name == "templates", found);
        } else if inside {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(source: &str) -> String {
        format(source, &Syntax::default())
            .unwrap_or_else(|error| panic!("{source:?} should format: {error}"))
    }

    #[test]
    fn tags_get_one_space_inside_their_delimiters() {
        assert_eq!(
            formatted("{{x}}{%if a%}{%endif%}{#note#}"),
            "{{ x }}{% if a %}{% endif %}{# note #}"
        );
    }

    #[test]
    fn whitespace_control_markers_stay_against_their_delimiters() {
        assert_eq!(
            formatted("{{-x+}}{%~ if a-%}{%endif%}{#-c-#}"),
            "{{- x +}}{%~ if a -%}{% endif %}{#- c -#}"
        );
    }

    #[test]
    fn text_outside_tags_is_never_touched() {
        let source = "  <p class=\"a  b\">\n\t{{ x }}  </p>  {  }} %} ";
        assert_eq!(formatted(source), source);
    }

    #[test]
    fn binary_operators_get_spaces_and_prefix_operators_none() {
        assert_eq!(formatted("{{a+b*-c}}"), "{{ a + b * -c }}");
        assert_eq!(
            formatted("{% if !a&&b==c||d %}{% endif %}"),
            "{% if !a && b == c || d %}{% endif %}"
        );
        assert_eq!(formatted("{{ a   ~   b }}"), "{{ a ~ b }}");
        assert_eq!(formatted("{{ & x }}{{ * y }}"), "{{ &x }}{{ *y }}");
    }

    #[test]
    fn calls_arguments_and_filters_are_spaced_as_rust_writes_them() {
        assert_eq!(
            formatted("{{ f ( a ,b , c = 1 ) }}"),
            "{{ f(a, b, c = 1) }}"
        );
        assert_eq!(
            formatted("{{ x | upper | truncate( 3 ) }}"),
            "{{ x|upper|truncate(3) }}"
        );
        assert_eq!(formatted("{{ a . b [ 0 ] ? . c }}"), "{{ a.b[0]?.c }}");
    }

    #[test]
    fn ranges_paths_and_turbofish_are_tight() {
        assert_eq!(
            formatted("{{ x[ 1 .. ] }}{{ a ..= b }}"),
            "{{ x[1..] }}{{ a..=b }}"
        );
        assert_eq!(
            formatted("{{ Vec :: < u8 > :: new ( ) }}"),
            "{{ Vec::<u8>::new() }}"
        );
    }

    #[test]
    fn struct_literals_and_patterns_get_rusts_spacing() {
        assert_eq!(formatted("{{ P{x:1,y} }}"), "{{ P { x: 1, y } }}");
        assert_eq!(
            formatted("{% match v %}{% when Some with(x)|None %}{% when P{a,..} %}{% endmatch %}"),
            "{% match v %}{% when Some with (x) | None %}{% when P { a, .. } %}{% endmatch %}"
        );
    }

    #[test]
    fn is_defined_and_casts_keep_their_words_apart() {
        assert_eq!(
            formatted("{% if x  is  not   defined %}{% endif %}"),
            "{% if x is not defined %}{% endif %}"
        );
        assert_eq!(formatted("{{ ( a ) as u8 }}"), "{{ (a) as u8 }}");
    }

    #[test]
    fn types_in_let_and_macro_tags_keep_generics_tight() {
        assert_eq!(
            formatted("{% let x : Vec < u8 > = v %}"),
            "{% let x: Vec<u8> = v %}"
        );
        assert_eq!(
            formatted("{% macro m( a : & str , b = 1 ) %}{% endmacro %}"),
            "{% macro m(a: &str, b = 1) %}{% endmacro %}"
        );
    }

    #[test]
    fn rust_macro_arguments_are_kept_as_written() {
        assert_eq!(
            formatted("{{ format !( \"{}\" ,x ) }}"),
            "{{ format!( \"{}\" ,x ) }}"
        );
    }

    #[test]
    fn raw_blocks_and_comment_text_are_kept_as_written() {
        let raw = "{%raw%}{{ a+b }}{%endraw%}";
        assert_eq!(formatted(raw), raw);
        assert_eq!(formatted("{#  a  +  b  #}"), "{# a  +  b #}");
    }

    #[test]
    fn a_tag_laid_out_over_lines_keeps_its_line_breaks() {
        let source = "{% call kit::card(\n    title ,\n    body\n) %}{% endcall %}";
        assert_eq!(
            formatted(source),
            "{% call kit::card(\n    title,\n    body\n) %}{% endcall %}"
        );
    }

    #[test]
    fn a_template_that_does_not_parse_is_refused() {
        let error =
            format("{{ a +", &Syntax::default()).expect_err("an unclosed tag does not parse");
        assert!(
            matches!(error, FormatError::Parse(_)),
            "expected a parse error, got {error:?}"
        );
    }

    #[test]
    fn formatting_twice_changes_nothing() {
        let once = formatted("{%for x in xs if x>0%}{{x|f(1,2)}}{%endfor%}");
        assert_eq!(formatted(&once), once);
    }
}
