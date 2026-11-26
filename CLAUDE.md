# zed-askama

Askama support for Zed. The repository ships two things at one version:

- `grammar/`: a tree-sitter grammar for askama templates;
- `editors/zed/`: the Zed extension, which provides the languages and queries and finds or downloads the language server.

The language server, `askama-analyzer`, lives in [radiosilence/askama-analyzer](https://github.com/radiosilence/askama-analyzer) and is released there; the extension downloads its latest release. Its users are Zed users, through Zed's extension registry. Issue #1 holds the plan and the reasons behind it. The [kōan standard](https://github.com/radiosilence/koan/blob/main/STANDARD.md) applies throughout.

## Git

Every change is a squash-merged pull request with signed commits. `main` is merged into a branch; a pushed branch is never rebased or force-pushed. Titles say what the change does, in sentence case.

## Changelog

`CHANGELOG.md` is generated from `changelog.d/` by `just changelog`; never edit it by hand. Each pull request adds one fragment, `changelog.d/unreleased/<section>/<pr>-<slug>.md`, holding one `- **Lead.** Detail.` line. `scripts/changelog.py` and `scripts/test_changelog.py` are koan's, copied verbatim: fix them in koan.

## Releasing

`just release patch|minor|major|X.Y.Z` (or the **Release** workflow) opens a `release-vX.Y.Z` pull request that sets every version file, pins the extension's grammar `rev` to the `origin/main` commit it branches from, and cuts the changelog. Merging it publishes the GitHub release and opens the version bump on zed-industries/extensions as a draft. [docs/releasing.md](docs/releasing.md) has the steps, the secrets and the first registry submission, which is made by hand.

Pull requests to zed-industries/extensions, the first submission included, are opened as drafts and stay drafts until the release has been tried in Zed at the submitted commit. Marking one ready is a person's decision.

## CI/CD

One workflow, `.github/workflows/ci-cd.yml`, ending in `Checks`, the only required status. `changes` decides which domains a diff touches (`rust`, `grammar`, `extension`, `changelog`, `workflows`, `deps`), each an allowlist of paths; a new top-level source directory must be added there. It also decides whether the run releases: a push to `main` that changes the workspace version publishes it once `Checks` has passed. `versions` runs on every run. `release.yml` only opens the release pull request.

## Check

- `just check`: everything CI checks but the advisory audit.
- `just fast`: the checks that take seconds, which lefthook runs before a push.

## Invariants

- **The extension's grammar `rev` names a pushed commit whose `grammar/src` matches `HEAD`'s.** Zed builds the grammar from that commit, never from the extension's own checkout, so a grammar change is only live once `rev` moves; CI fails while they differ.
- **`grammar/src` is generated** by `tree-sitter generate` at the version `mise.toml` pins, and committed because Zed compiles it without generating; CI fails when it is stale.
- **Every version file states one version.** The workspace `Cargo.toml`, `editors/zed/Cargo.toml` and `editors/zed/extension.toml` move together through `scripts/versions.py --set`; CI fails when they disagree. askama-analyzer has its own version. `grammar/tree-sitter.json`'s version is not one of them: `tree-sitter generate` writes it into the parser, so a release that moved it would no longer match the grammar `rev` it pins.
- **The server's release archive names are a contract.** `server_asset` in `editors/zed/src/lib.rs` downloads `askama-analyzer-<target>.tar.gz` (or `.zip` on Windows) from radiosilence/askama-analyzer's latest release by name.
- **The initialization options are a contract with the server.** `rustAnalyzer: { path, args, env, initializationOptions, settings }` is what askama-analyzer reads; the server reads it leniently, so the two release independently, and a field is added, never renamed.
- **The extension is a workspace of its own.** `editors/zed` has its own `[workspace]` and `Cargo.lock` and is excluded from the root workspace, because Zed's builder runs cargo inside it and the registry may package it alone; `just extension` checks it.
- **`vendor/askama-testing` is askama's test suite at the pinned release**, written by `scripts/vendor-askama-testing.py`; the grammar's differential test reads it, and `askama_parser` is pinned exactly to the same release.

## Where things live

| Path | What |
|---|---|
| `grammar/` | The tree-sitter grammar (`grammar.js`, the external scanner in `src/scanner.c`), its corpus, and `tests/askama_corpus.rs`, which holds it to `askama_parser`; `test/divergences.txt` lists what the parser rejects beyond syntax |
| `editors/zed/` | The Zed extension: `extension.toml`, `languages/askama-{html,markdown,text}` (the Markdown and Text queries are symlinks to the HTML ones), snippets, and `src/lib.rs`, which finds or downloads `askama-analyzer` and rust-analyzer and passes the user's settings to the server |
| `scripts/release.sh`, `scripts/versions.py`, `scripts/registry-pr.sh`, `docs/releasing.md` | The release pull request, the one-version check, the registry draft, and how they fit |
| `vendor/askama-testing`, `scripts/vendor-askama-testing.py` | askama's test suite and the script that vendors it |
