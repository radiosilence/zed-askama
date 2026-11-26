# Releasing

The grammar and the Zed extension ship at one version. The language server is released from [radiosilence/askama-analyzer](https://github.com/radiosilence/askama-analyzer), and installed extensions take its newest release on their next start, so a server change needs no release here. A release is a merged pull request; nothing is tagged or uploaded by hand.

## How a release goes

1. **Open the release pull request.** Run `just release patch` (or `minor`, `major`, or an exact `X.Y.Z`) at a terminal, or the **Release** workflow from the Actions tab, which does the same with `RELEASE_TOKEN`. `scripts/release.sh` branches `release-vX.Y.Z` from `origin/main` and:
   - sets every version file to the new version with `scripts/versions.py --set` (the workspace `Cargo.toml` and `extension.toml`; not `grammar/tree-sitter.json`, whose version is compiled into the generated parser);
   - pins `extension.toml`'s grammar `rev` to the `origin/main` commit the branch starts from, which is pushed and holds the release's `grammar/src`, since Zed builds the grammar from that commit;
   - refreshes the lockfile's own entries;
   - turns `changelog.d/unreleased` into the release's `CHANGELOG.md` section;
   - commits `release: vX.Y.Z`, pushes, and opens the pull request with that section as its body.

   It refuses a dirty tree, a version that already has a tag, a version not above `main`'s, and a release branch that already has an open pull request.
2. **CI checks the release pull request** as it checks any other.
3. **Merge it.** The push to `main` changes the workspace version, which `changes` in `ci-cd.yml` reads as a release when `vX.Y.Z` has no GitHub release yet.
4. **The GitHub release** is published once `Checks` has passed, with the changelog section as notes.
5. **The registry pull request** is opened on zed-industries/extensions as a draft, moving `extensions/askama` to the release commit, setting its `version` and dropping any `path`, since the extension sits at the repository's root (`scripts/registry-pr.sh`).
6. **Try the release in Zed**, installing the extension as a dev extension from the release commit, then mark the registry pull request ready. Nothing marks it ready automatically: the registry's prerequisites ask that a release is tried before it is submitted, and that is a person's decision.

A release that failed part way is retried by re-running the failed jobs, or by running **CI/CD** on `main` from the Actions tab, which releases whatever version has no GitHub release yet. Every step repeats safely: the release action fills in a release that exists, and the registry script leaves an open pull request alone.

## Secrets

| Secret | Used by | What it needs |
|---|---|---|
| `RELEASE_TOKEN` | the **Release** workflow, and publishing the GitHub release | A fine-grained token on this repository with contents and pull requests read and write. A pull request opened with the workflow's own token starts no workflows, so the release pull request could never pass CI; a release fails rather than going out without it. |
| `ZED_EXTENSIONS_TOKEN` | opening the registry pull request | A classic token with `public_repo` scope on the account that holds the fork of zed-industries/extensions, since a fine-grained token cannot open a pull request on another owner's repository. Without it the release still publishes, and the job says the registry pull request must be opened by hand. |

## The first registry submission

The registry script only moves an extension that is already listed, so the first submission is made by hand, after the first GitHub release, following Zed's [publishing guide](https://zed.dev/docs/extensions/publishing/publishing-guide):

1. Install the extension as a dev extension from the release commit and try it: the languages, the queries, and the server downloading and starting.
2. Fork zed-industries/extensions into the account `ZED_EXTENSIONS_TOKEN` belongs to, and clone the fork.
3. Add this repository as a submodule over HTTPS, at the release commit, which is on `main`:

   ```sh
   git submodule add https://github.com/radiosilence/zed-askama.git extensions/askama
   git -C extensions/askama checkout <release commit>
   ```

4. Add the entry to `extensions.toml`:

   ```toml
   [askama]
   submodule = "extensions/askama"
   version = "X.Y.Z"
   ```

5. Run `pnpm sort-extensions`, commit, push, and open the pull request as a draft. Mark it ready once the release has been tried at that commit.

Every later release opens its registry pull request itself.
