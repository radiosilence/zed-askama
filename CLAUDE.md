# zed-askama

Askama support for Zed, backed by rust-analyzer. The repository ships three things at one version:

- `grammar/`: a tree-sitter grammar for askama templates;
- `crates/`: `askama-analyzer`, a language server that answers template requests by running askama's own code generator and asking the user's rust-analyzer about its output;
- `editors/zed/`: the Zed extension, which provides the languages and queries and finds or downloads `askama-analyzer`.

Its users are Zed users, through Zed's extension registry; the server speaks plain LSP, so other editors can run it. Issue #1 holds the plan and the reasons behind it. The [kōan standard](https://github.com/radiosilence/koan/blob/main/STANDARD.md) applies throughout.

## Git

Every change is a squash-merged pull request with signed commits. `main` is merged into a branch; a pushed branch is never rebased or force-pushed. Titles say what the change does, in sentence case.

## Changelog

`CHANGELOG.md` is generated from `changelog.d/` by `just changelog`; never edit it by hand. Each pull request adds one fragment, `changelog.d/unreleased/<section>/<pr>-<slug>.md`, holding one `- **Lead.** Detail.` line. `scripts/changelog.py` and `scripts/test_changelog.py` are koan's, copied verbatim: fix them in koan.

## Releasing

Not set up yet; it is step 6 of issue #1. Pull requests to zed-industries/extensions, the first submission included, are opened as drafts and stay drafts until the release has been tried in Zed at the submitted commit. Marking one ready is a person's decision.

## CI/CD

One workflow, `.github/workflows/ci-cd.yml`, ending in `Checks`, the only required status. `changes` decides which domains a diff touches (`rust`, `grammar`, `vendor`, `changelog`, `workflows`, `deps`), each an allowlist of paths; a new top-level source directory must be added there.

## Check

- `just check`: everything CI checks but the advisory audit.
- `just fast`: the checks that take seconds, which lefthook runs before a push.

## Invariants

- **askama's code generation is never reimplemented.** The analyser runs askama's generator, vendored in `vendor/askama_derive`, so what rust-analyzer analyses is what `cargo build` compiles.
- **`vendor/askama_derive` is upstream's release plus `vendor/askama_derive-tooling.patch`, and nothing else.** Edit the vendored files, then run `just vendor-askama --write-patch`; CI fails when `just vendor-askama --check` does. ARCHITECTURE.md lists what the patch changes and why.
- **`askama_parser` is pinned exactly to the vendored generator's version.** A new askama release is taken by changing the pin and running `just vendor-askama`, which moves both.
- **Every thread that parses with proc-macro2 frees its source map afterwards** (`proc_macro2::extra::invalidate_current_thread_spans`), since `span-locations` keeps every parsed text for the life of the thread. `askama_analyzer_codegen::generate` does this itself.
- **The extension's grammar `rev` names a pushed commit whose `grammar/src` matches `HEAD`'s.** Zed builds the grammar from that commit, never from the extension's own checkout, so a grammar change is only live once `rev` moves; CI fails while they differ.
- **`grammar/src` is generated** by `tree-sitter generate` at the version `mise.toml` pins, and committed because Zed compiles it without generating; CI fails when it is stale.
- **`vendor/askama-testing` is askama's test suite at the pinned release**, written by `scripts/vendor-askama-testing.py`; the grammar's differential test and the generator's tests read it.
- **`askama-analyzer-codegen` does no I/O.** Files reach it through its `Files` trait, so unsaved buffers are what askama reads.

## Where things live

| Path | What |
|---|---|
| `grammar/` | The tree-sitter grammar (`grammar.js`, the external scanner in `src/scanner.c`), its corpus, and `tests/askama_corpus.rs`, which holds it to `askama_parser`; `test/divergences.txt` lists what the parser rejects beyond syntax |
| `editors/zed/` | The Zed extension: `extension.toml`, `languages/askama-{html,markdown,text}` (the Markdown and Text queries are symlinks to the HTML ones) and snippets |
| `vendor/askama-testing`, `scripts/vendor-askama-testing.py` | askama's test suite and the script that vendors it |
| `crates/askama-analyzer-codegen` | askama's generator run on one item, and the map from each generated token to its template range |
| `vendor/askama_derive`, `vendor/askama_derive-tooling.patch` | askama's generator with the tooling patch |
| `scripts/vendor-askama.py` | Downloads, verifies and patches the vendored generator |
| `ARCHITECTURE.md` | How the pieces fit and the rules they keep |
