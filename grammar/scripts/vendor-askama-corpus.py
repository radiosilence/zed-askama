#!/usr/bin/env python3
"""Refresh grammar/test/askama-corpus from askama's own test suite.

The differential test (grammar/tests/differential.rs) parses these inputs with
both askama_parser and this grammar. They come from askama
(https://github.com/askama-rs/askama), licensed MIT OR Apache-2.0; both
licence texts are copied beside them.

What is copied, from the given tag:

- every file under testing/templates (symlinks and .gitignore files aside) into
  templates/, keeping its path;
- every `source = "…"` or `source = r#"…"#` string literal in
  testing/tests/**/*.rs into inline/, one file per literal, named after the
  test file and line. Literals inside a `#[template(…)]` attribute that sets
  `syntax` are skipped: they use custom delimiters, which this grammar does not
  parse. Sources built from macro arguments (`source = $source`) are skipped
  because they cannot be extracted without expanding the macro;
- the list of compile-fail templates, those referenced only from
  testing/tests/ui/, into compile-fail.txt. The other templates must parse
  without errors.

Usage: vendor-askama-corpus.py <tag> [<askama checkout>]

Without a checkout, the tag is cloned from GitHub into a temporary directory.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPOSITORY = "https://github.com/askama-rs/askama"
OUT = Path(__file__).resolve().parent.parent / "test" / "askama-corpus"


def git(checkout: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(checkout), *args], check=True, capture_output=True, text=True
    ).stdout


def decode_rust_string(text: str, start: int) -> tuple[str, int] | None:
    """Decodes the Rust string literal at `start`, returning it and its end."""
    raw = re.match(r'r(#*)"', text[start:])
    if raw:
        hashes = raw.group(1)
        body_start = start + raw.end()
        end = text.find('"' + hashes, body_start)
        if end < 0:
            return None
        return text[body_start:end], end + 1 + len(hashes)
    if not text.startswith('"', start):
        return None
    out = []
    i = start + 1
    while i < len(text):
        c = text[i]
        if c == '"':
            return "".join(out), i + 1
        if c != "\\":
            out.append(c)
            i += 1
            continue
        e = text[i + 1]
        if e == "\n":
            i += 2
            while i < len(text) and text[i] in " \t\n\r":
                i += 1
            continue
        simple = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", "0": "\0", "'": "'", '"': '"'}
        if e in simple:
            out.append(simple[e])
            i += 2
        elif e == "x":
            out.append(chr(int(text[i + 2 : i + 4], 16)))
            i += 4
        elif e == "u":
            close = text.index("}", i)
            out.append(chr(int(text[i + 3 : close], 16)))
            i = close + 1
        else:
            return None
    return None


def extract_sources(text: str) -> list[tuple[int, str]]:
    sources = []
    for match in re.finditer(r"\bsource\s*=\s*", text):
        decoded = decode_rust_string(text, match.end())
        if decoded is None:
            continue
        source, end = decoded
        attribute_start = text.rfind("#[template(", 0, match.start())
        attribute_end = text.find(")]", end)
        attribute = text[max(attribute_start, 0) : match.start()] + text[end : max(attribute_end, end)]
        if re.search(r"\bsyntax\s*=", attribute):
            continue
        line = text.count("\n", 0, match.start()) + 1
        sources.append((line, source))
    return sources


def main() -> None:
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    tag = sys.argv[1]
    with tempfile.TemporaryDirectory() as temporary:
        if len(sys.argv) == 3:
            checkout = Path(sys.argv[2])
        else:
            checkout = Path(temporary) / "askama"
            subprocess.run(
                ["git", "clone", "--quiet", "--depth", "1", "--branch", tag, REPOSITORY, str(checkout)],
                check=True,
            )
        vendor(checkout, tag)


def vendor(checkout: Path, tag: str) -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    (OUT / "templates").mkdir(parents=True)
    (OUT / "inline").mkdir()

    for licence in ("LICENSE-MIT", "LICENSE-APACHE"):
        (OUT / licence).write_text(git(checkout, "show", f"{tag}:{licence}"))
    (OUT / "TAG").write_text(tag + "\n")

    templates = []
    for line in git(checkout, "ls-tree", "-r", tag, "testing/templates").splitlines():
        meta, path = line.split("\t", 1)
        mode = meta.split()[0]
        if mode != "100644" or path.endswith(".gitignore"):
            continue
        relative = path.removeprefix("testing/templates/")
        destination = OUT / "templates" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(
            subprocess.run(
                ["git", "-C", str(checkout), "show", f"{tag}:{path}"], check=True, capture_output=True
            ).stdout
        )
        templates.append(relative)

    referenced_outside_ui: set[str] = set()
    referenced_from_ui: set[str] = set()
    test_files = [
        path
        for path in git(checkout, "ls-tree", "-r", "--name-only", tag, "testing/tests").splitlines()
        if path.endswith(".rs")
    ]
    for path in test_files:
        text = git(checkout, "show", f"{tag}:{path}")
        names = set(re.findall(r'\bpath\s*=\s*"([^"]+)"', text))
        names |= set(re.findall(r'\{%-?\s*(?:extends|include|import)\s+"([^"]+)"', text))
        if path.startswith("testing/tests/ui/"):
            referenced_from_ui |= names
        else:
            referenced_outside_ui |= names
        stem = path.removeprefix("testing/tests/").removesuffix(".rs").replace("/", "__")
        for line, source in extract_sources(text):
            (OUT / "inline" / f"{stem}__L{line}.askama").write_text(source)

    # A template another valid template includes or extends is valid too.
    for template in templates:
        if template in referenced_outside_ui:
            text = (OUT / "templates" / template).read_text(errors="replace")
            referenced_outside_ui |= set(
                re.findall(r'\{%-?\s*(?:extends|include|import)\s+"([^"]+)"', text)
            )

    compile_fail = sorted(t for t in templates if t in referenced_from_ui and t not in referenced_outside_ui)
    (OUT / "compile-fail.txt").write_text("".join(f"{t}\n" for t in compile_fail))


if __name__ == "__main__":
    main()
