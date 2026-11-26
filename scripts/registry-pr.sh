#!/usr/bin/env bash
# Open the version bump for the askama extension on zed-industries/extensions,
# as a draft, from a fork in the token's account: the submodule
# extensions/askama moved to the release commit and its `version` in
# extensions.toml set. A draft stays a draft until a person has tried the
# release in Zed at that commit; nothing here marks it ready.
#
# Safe to re-run: a branch already pushed is replaced, and an open pull
# request from it is left as it is.
#
#   GH_TOKEN=<token> scripts/registry-pr.sh X.Y.Z <commit>
#
# GH_TOKEN needs `repo` (or `public_repo`) scope on the account that holds the
# fork; the fork is created if it does not exist. The first submission, which
# adds the submodule and the extensions.toml entry, is made by hand
# (docs/releasing.md): this script refuses a registry without them.
set -euo pipefail

usage() { echo "usage: scripts/registry-pr.sh X.Y.Z <commit>" >&2; exit 2; }
[ $# -eq 2 ] || usage
version=$1 commit=$2
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "commit must be a full SHA: $commit" >&2; exit 2; }

upstream=zed-industries/extensions
submodule=extensions/askama
branch="askama-v$version"
title="Update askama to v$version"

owner=$(gh api user --jq .login)
open_pr=$(gh pr list --repo "$upstream" --head "$owner:$branch" --state open --json number --jq '.[0].number // empty')
if [ -n "$open_pr" ]; then
  echo "pull request #$open_pr already proposes v$version: https://github.com/$upstream/pull/$open_pr"
  exit 0
fi

# Forking an existing fork returns it.
gh repo fork "$upstream" --clone=false --default-branch-only >/dev/null 2>&1 || true
gh api "repos/$owner/extensions" --jq .full_name >/dev/null

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# A sparse checkout of the top level only: extensions.toml is the one file
# edited, and the index still holds every path, so the commit changes nothing
# else. Submodules are never cloned; the bump only moves a gitlink.
git clone --quiet --depth 1 --filter=blob:none --sparse "https://github.com/$upstream.git" "$work/extensions"
cd "$work/extensions"

if ! git ls-tree HEAD "$submodule" | grep -q '^160000 '; then
  echo "$upstream has no $submodule submodule; make the first submission by hand (docs/releasing.md)" >&2
  exit 1
fi
if ! grep -qx '\[askama\]' extensions.toml; then
  echo "$upstream's extensions.toml has no [askama] entry; make the first submission by hand" >&2
  exit 1
fi

git switch --quiet -c "$branch"
git update-index --add --cacheinfo "160000,$commit,$submodule"
python3 - "$version" <<'PY'
import re
import sys

version = sys.argv[1]
text = open("extensions.toml").read()
entry = re.compile(r'(^\[askama\]\n(?:(?!^\[).*\n)*?^version = ")[^"]*(")', re.MULTILINE)
text, count = entry.subn(lambda m: m.group(1) + version + m.group(2), text, count=1)
if count != 1:
    sys.exit("extensions.toml's [askama] entry has no version")
open("extensions.toml", "w").write(text)
PY
git add extensions.toml
git -c user.name="$owner" -c user.email="$owner@users.noreply.github.com" commit --quiet -m "$title"

gh auth setup-git >/dev/null
git push --quiet --force "https://github.com/$owner/extensions.git" "$branch"

body="Updates the askama extension to [v$version](https://github.com/radiosilence/zed-askama/releases/tag/v$version), at radiosilence/zed-askama@$commit.

Opened as a draft by the release pipeline; it is marked ready once the release has been tried in Zed at this commit."
gh pr create --repo "$upstream" --head "$owner:$branch" --base main --draft --title "$title" --body "$body"
