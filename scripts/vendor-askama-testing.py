#!/usr/bin/env python3
"""Vendor askama's test suite into vendor/askama-testing.

The grammar's differential test and the generator's corpus test both read
askama's own test suite: its templates, its tests (including the compile-fail
tests in tests/ui/) and their configuration. This script copies the
`testing/` directory of one askama release, from the release's GitHub tarball
checked against a pinned SHA-256, along with askama's licence texts. askama is
licensed MIT OR Apache-2.0 (https://github.com/askama-rs/askama).

Only regular files are extracted. Symbolic links (askama's tests include one
that points outside the templates directory on purpose) and any path that
would leave the target directory are skipped.

Usage:
    scripts/vendor-askama-testing.py           refresh vendor/askama-testing
    scripts/vendor-askama-testing.py --check   fail if it differs from the tag

To move to another release, change TAG and SHA256 together.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import shutil
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath

TAG = "v0.16.1"
SHA256 = "fb0de206524ec23143d3327022f17969847004ab7426bfccb33df1522e4e5f07"
URL = f"https://github.com/askama-rs/askama/archive/refs/tags/{TAG}.tar.gz"
LICENCES = ("LICENSE-MIT", "LICENSE-APACHE")

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / "vendor" / "askama-testing"


def download() -> bytes:
    with urllib.request.urlopen(URL) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256:
        sys.exit(f"{URL} has SHA-256 {digest}, expected {SHA256}")
    return data


def extract(data: bytes, destination: Path) -> None:
    """Writes testing/ and the licences from the tarball into `destination`."""
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            if not member.isfile():
                continue
            parts = PurePosixPath(member.name).parts
            if len(parts) < 2 or any(part in ("", ".", "..") for part in parts):
                continue
            inner = parts[1:]
            if inner[0] == "testing" and len(inner) > 1:
                relative = PurePosixPath(*inner[1:])
            elif len(inner) == 1 and inner[0] in LICENCES:
                relative = PurePosixPath(inner[0])
            else:
                continue
            path = destination.joinpath(*relative.parts)
            if not path.resolve().is_relative_to(destination.resolve()):
                continue
            source = archive.extractfile(member)
            if source is None:
                continue
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.read())


def files(directory: Path) -> dict[str, bytes]:
    return {
        path.relative_to(directory).as_posix(): path.read_bytes()
        for path in sorted(directory.rglob("*"))
        if path.is_file()
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if vendor/askama-testing differs")
    arguments = parser.parse_args()

    data = download()
    with tempfile.TemporaryDirectory() as temporary:
        fresh = Path(temporary) / "askama-testing"
        fresh.mkdir()
        extract(data, fresh)

        if arguments.check:
            expected = files(fresh)
            actual = files(TARGET) if TARGET.is_dir() else {}
            changed = sorted(
                name for name in expected.keys() | actual.keys() if expected.get(name) != actual.get(name)
            )
            if changed:
                listing = "\n".join(f"  {name}" for name in changed[:20])
                sys.exit(
                    f"vendor/askama-testing differs from askama {TAG} in {len(changed)} files:\n{listing}\n"
                    "Run scripts/vendor-askama-testing.py to refresh it."
                )
            print(f"vendor/askama-testing matches askama {TAG}")
            return

        if TARGET.exists():
            shutil.rmtree(TARGET)
        TARGET.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(fresh, TARGET)
        print(f"vendored askama {TAG} testing/ into {TARGET.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
