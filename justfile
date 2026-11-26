# zed-askama: Askama support for Zed, backed by rust-analyzer

# Everything CI checks short of the audit: formatting, clippy with warnings as
# errors, the tests, the vendored generator, the changelog and the workflows.
check: grammar
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --locked
    python3 scripts/vendor-askama.py --check
    python3 scripts/vendor-askama-testing.py --check
    python3 -m unittest scripts/test_changelog.py
    python3 scripts/changelog.py --lint
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
