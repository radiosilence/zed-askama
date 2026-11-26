# zed-askama: Askama support for Zed, backed by rust-analyzer

# Everything CI checks short of the audit and the release builds: formatting,
# clippy with warnings as errors, the tests, the extension, the vendored
# generator, the changelog, the version files and the workflows.
check: grammar extension
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    python3 scripts/vendor-askama.py --check
    python3 scripts/vendor-askama-testing.py --check
    python3 -m unittest scripts/test_changelog.py
    python3 scripts/changelog.py --lint
    python3 scripts/versions.py
    actionlint

# The grammar as Zed builds it: the generated parser is what grammar.js makes,
# the corpus passes, and every query the extension ships compiles against it.
grammar:
    cd grammar && tree-sitter generate && git diff --exit-code -- src
    cd grammar && tree-sitter test
    cd grammar && for query in ../editors/zed/languages/*/*.scm; do tree-sitter query "$query" ../vendor/askama-testing/templates/attr.html > /dev/null; done

# The checks that take seconds: the lockfile matches the manifests, and formatting.
fast:
    cargo metadata --locked --format-version 1 > /dev/null
    cargo fmt --all -- --check

# Format the Rust code.
fmt:
    cargo fmt --all

# The Zed extension as Zed's builder compiles it, with its lints and unit
# tests. A workspace of its own, so the root's commands do not reach it.
extension:
    cd editors/zed && cargo fmt -- --check
    cd editors/zed && cargo clippy --locked --all-targets -- -D warnings
    cd editors/zed && cargo clippy --locked --target wasm32-wasip2 -- -D warnings
    cd editors/zed && cargo test --locked
    cd editors/zed && cargo build --locked --release --target wasm32-wasip2

# Fail unless every version file states the one version the repository ships at.
versions:
    python3 scripts/versions.py

# Open the release PR: `just release patch|minor|major`, or an exact version.
# Merging it publishes the GitHub release and opens the registry bump as a
# draft; tags are never pushed by hand (docs/releasing.md).
release version *args:
    scripts/release.sh {{version}} {{args}}

# Install the git hooks.
hooks:
    lefthook install

# Write CHANGELOG.md from the fragments in changelog.d/. `--stdout` prints it instead.
changelog *args:
    python3 scripts/changelog.py {{args}}

# Vendor askama_derive with the tooling patch. `--check` fails if vendor/ is
# stale; `--write-patch` saves edits made in vendor/askama_derive as the patch.
vendor-askama *args:
    python3 scripts/vendor-askama.py {{args}}
