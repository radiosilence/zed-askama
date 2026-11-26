#!/usr/bin/env python3
"""Vendors askama_derive from crates.io with this repository's tooling patch.

askama-analyzer runs askama's own code generator, which discards template
locations outside a compiler and caches files for the life of the process.
`vendor/askama_derive-tooling.patch` makes it usable from a long-running
server; ARCHITECTURE.md says what it changes and why. The same change is
proposed upstream, and this vendoring goes once it lands.

Usage:
    python3 scripts/vendor-askama.py               write vendor/askama_derive
    python3 scripts/vendor-askama.py --check       fail if vendor/askama_derive is stale
    python3 scripts/vendor-askama.py --write-patch save vendor/askama_derive's edits as the patch

The version is the one Cargo.toml pins `askama_parser` to, since askama_derive
requires that exact parser. The download is verified against the checksum in
the crates.io index before anything is extracted.
"""

import argparse
import hashlib
import io
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor" / "askama_derive"
PATCH = ROOT / "vendor" / "askama_derive-tooling.patch"
CRATE = "askama_derive"
# crates.io asks automated clients to identify themselves.
USER_AGENT = "zed-askama-vendor-script (github.com/radiosilence/zed-askama)"
# Generous for a crate of about 200 KB, and a bound on what a bad mirror can make us hold.
MAX_CRATE_BYTES = 16 * 1024 * 1024


def pinned_version() -> str:
    manifest = (ROOT / "Cargo.toml").read_text()
    found = re.search(r'^askama_parser\s*=\s*\{[^}]*version\s*=\s*"=([0-9.]+)"', manifest, re.M)
    if not found:
        sys.exit("vendor-askama: Cargo.toml must pin askama_parser as `version = \"=X.Y.Z\"`")
    return found.group(1)


def fetch(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read(MAX_CRATE_BYTES + 1)
    if len(body) > MAX_CRATE_BYTES:
        sys.exit(f"vendor-askama: {url} is larger than {MAX_CRATE_BYTES} bytes")
    return body


def expected_checksum(version: str) -> str:
    index = fetch(f"https://index.crates.io/as/ka/{CRATE}").decode()
    for line in index.splitlines():
        entry = json.loads(line)
        if entry["vers"] == version:
            if entry.get("yanked"):
                sys.exit(f"vendor-askama: {CRATE} {version} is yanked")
            return entry["cksum"]
    sys.exit(f"vendor-askama: {CRATE} {version} is not in the crates.io index")


def extract_pristine(version: str, into: Path) -> None:
    """Writes the published crate's files into `into`, which must not exist."""
    data = fetch(f"https://static.crates.io/crates/{CRATE}/{CRATE}-{version}.crate")
    digest = hashlib.sha256(data).hexdigest()
    want = expected_checksum(version)
    if digest != want:
        sys.exit(f"vendor-askama: checksum mismatch for {CRATE} {version}: got {digest}, index says {want}")
    prefix = f"{CRATE}-{version}/"
    into.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            # Only regular files and directories, each inside the crate's own
            # directory: an archive may name anything, and nothing here needs a link.
            name = member.name
            if not name.startswith(prefix) or ".." in Path(name).parts:
                sys.exit(f"vendor-askama: refusing archive entry {name!r}")
            relative = name[len(prefix):]
            if member.isdir() or not relative:
                continue
            if not member.isfile():
                sys.exit(f"vendor-askama: refusing non-file archive entry {name!r}")
            target = into / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                sys.exit(f"vendor-askama: cannot read archive entry {name!r}")
            target.write_bytes(source.read())


def apply_patch(tree: Path) -> None:
    if not PATCH.exists():
        return
    subprocess.run(
        ["git", "apply", "--whitespace=nowarn", "-p1", "--directory", str(tree.relative_to(ROOT)), str(PATCH)],
        cwd=ROOT,
        check=True,
    )


def files(tree: Path) -> dict[str, bytes]:
    return {str(p.relative_to(tree)): p.read_bytes() for p in sorted(tree.rglob("*")) if p.is_file()}


def build(into: Path, version: str, patched: bool) -> None:
    if into.exists():
        shutil.rmtree(into)
    extract_pristine(version, into)
    if patched:
        apply_patch(into)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail if vendor/askama_derive is stale")
    mode.add_argument("--write-patch", action="store_true", help="save vendor/askama_derive's edits as the patch")
    args = parser.parse_args()
    version = pinned_version()

    if args.write_patch:
        # Paths inside the repository, so `git apply --directory` takes the patch as written.
        pristine = ROOT / "target" / "vendor-askama" / "pristine"
        build(pristine, version, patched=False)
        diff = subprocess.run(
            ["git", "diff", "--no-index", "--no-color",
             str(pristine.relative_to(ROOT)), str(VENDOR.relative_to(ROOT))],
            cwd=ROOT, capture_output=True, text=True,
        )
        if diff.returncode not in (0, 1):
            sys.exit(diff.stderr)
        # git names each side by its full path, and a new file by its new path on both sides;
        # the patch is relative to the crate root.
        text = diff.stdout
        for tree in (pristine, VENDOR):
            for side in ("a", "b"):
                text = text.replace(f"{side}/{tree.relative_to(ROOT)}/", f"{side}/")
        PATCH.write_text(text)
        print(f"wrote {PATCH.relative_to(ROOT)} ({text.count(chr(10))} lines)")
        return

    if args.check:
        fresh = ROOT / "target" / "vendor-askama" / "fresh"
        build(fresh, version, patched=True)
        if files(fresh) != files(VENDOR):
            sys.exit("vendor-askama: vendor/askama_derive does not match askama_derive "
                     f"{version} with the patch applied; run `just vendor-askama`")
        print(f"vendor/askama_derive is askama_derive {version} with the patch applied")
        return

    build(VENDOR, version, patched=True)
    print(f"vendored askama_derive {version} into {VENDOR.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
