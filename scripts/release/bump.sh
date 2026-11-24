#!/usr/bin/env bash
# Bump the version, commit, tag. Usage: scripts/release/bump.sh X.Y.Z[-pre] [--push]
# The two version strings: [workspace.package] in Cargo.toml and [package] in desktop/Cargo.toml.
# Pushing the tag starts .github/workflows/release.yml, which waits for CI on that commit.
set -euo pipefail
new="${1:?usage: bump.sh X.Y.Z[-pre] [--push]}"
push="${2:-}"
[[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]] || { echo "version must be X.Y.Z[-pre]" >&2; exit 2; }
[[ -z "$(git status --porcelain)" ]] || { echo "working tree is not clean" >&2; exit 1; }
[[ "$(git branch --show-current)" == main ]] || { echo "not on main" >&2; exit 1; }
git fetch -q origin main --tags
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || { echo "main is not level with origin/main" >&2; exit 1; }
if git rev-parse -q --verify "refs/tags/v$new" >/dev/null; then echo "v$new exists (tags are immutable)" >&2; exit 1; fi

perl -0pi -e 's/(\[workspace\.package\]\nversion = )"[^"]*"/${1}"'"$new"'"/' Cargo.toml
perl -0pi -e 's/(\[package\][^\[]*?\nversion = )"[^"]*"/${1}"'"$new"'"/' desktop/Cargo.toml
for f in Cargo.toml desktop/Cargo.toml; do
  got=$(python3 -c 'import sys,tomllib; d=tomllib.load(open(sys.argv[1],"rb")); print(d.get("workspace",{}).get("package",{}).get("version") or d["package"]["version"])' "$f")
  [[ "$got" == "$new" ]] || { echo "failed to bump $f (got $got)" >&2; exit 1; }
done
# Refresh both lockfiles (workspace and path-crate versions) without upgrading anything.
cargo metadata --format-version 1 >/dev/null
(cd desktop && cargo metadata --format-version 1 >/dev/null)
if [[ -f CHANGELOG.md ]] && grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  perl -0pi -e 's/^## \[Unreleased\]\n/## [Unreleased]\n\n## ['"$new"'] - '"$(date -u +%F)"'\n/m' CHANGELOG.md
fi

cargo c && cargo lint && cargo t
(cd desktop && cargo check --locked)

git commit -qam "Release v$new"
git tag -a "v$new" -m "rust-build v$new"
if [[ "$push" == "--push" ]]; then
  git push --atomic origin main "v$new"
else
  echo "Tagged v$new. Publish with: git push --atomic origin main v$new"
fi
