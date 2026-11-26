#!/usr/bin/env python3
"""Check, or set, the one version the repository ships at.

The grammar and the Zed extension release together, so every file
that states a version must state the same one:

- Cargo.toml, `[workspace.package] version` (the crates inherit it);
- extension.toml, the version Zed's registry shows.

grammar/tree-sitter.json's `metadata.version` is left alone: `tree-sitter
generate` writes it into grammar/src/parser.c, so moving it would change the
generated parser, which the extension's grammar `rev` (a commit on main from
before the release) must match.

The lockfiles' entries for these crates follow the manifests; `cargo update
--workspace` (run by scripts/release.sh) writes them, and `cargo build
--locked` fails when they lag.

Usage:
    scripts/versions.py            fail unless every file states one version
    scripts/versions.py --set X.Y.Z  write X.Y.Z into every file
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def toml_version(path: Path, table: str) -> tuple[str, re.Pattern[str]]:
    """The `version = "…"` line inside `[table]`, and a pattern matching it."""
    pattern = re.compile(
        r'(^\[' + re.escape(table) + r'\]\n(?:(?!^\[).*\n)*?^version = ")([^"]*)(")', re.MULTILINE
    )
    found = pattern.search(path.read_text())
    if found is None:
        sys.exit(f"{path.relative_to(ROOT)} has no version in [{table}]")
    return found.group(2), pattern


def extension_manifest_version(path: Path) -> tuple[str, re.Pattern[str]]:
    """extension.toml's top-level `version`, before its first table."""
    pattern = re.compile(r'(\A(?:(?!^\[).*\n)*?^version = ")([^"]*)(")', re.MULTILINE)
    found = pattern.search(path.read_text())
    if found is None:
        sys.exit(f"{path.relative_to(ROOT)} has no top-level version")
    return found.group(2), pattern


TOML_FILES = {
    "Cargo.toml": lambda path: toml_version(path, "workspace.package"),
    "extension.toml": extension_manifest_version,
}


def versions() -> dict[str, str]:
    found = {name: read(ROOT / name)[0] for name, read in TOML_FILES.items()}
    return found


def set_version(version: str) -> None:
    for name, read in TOML_FILES.items():
        path = ROOT / name
        _, pattern = read(path)
        path.write_text(pattern.sub(lambda m: m.group(1) + version + m.group(3), path.read_text(), count=1))


def main() -> None:
    arguments = sys.argv[1:]
    if arguments[:1] == ["--set"] and len(arguments) == 2:
        if not VERSION.match(arguments[1]):
            sys.exit(f"{arguments[1]} is not a version (X.Y.Z)")
        set_version(arguments[1])
    elif arguments:
        sys.exit(__doc__)

    found = versions()
    if len(set(found.values())) != 1:
        listing = "\n".join(f"  {name}: {version}" for name, version in found.items())
        sys.exit(f"the version files disagree:\n{listing}\nset them together with scripts/versions.py --set X.Y.Z")
    print(f"every version file states {next(iter(found.values()))}")


if __name__ == "__main__":
    main()
