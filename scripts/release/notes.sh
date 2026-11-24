#!/usr/bin/env bash
# notes.sh <tag>: release notes = CHANGELOG section (if any) + commits since the previous tag +
# install/verify instructions. rust-build pushes straight to main, so `git log` is the change list
# (GitHub's --generate-notes would list almost nothing but Dependabot PRs).
set -euo pipefail
tag="$1" version="${1#v}" repo="${GITHUB_REPOSITORY:-kaeawc/rust-build}"
prev="$(git describe --tags --abbrev=0 --match 'v*' "${tag}^" 2>/dev/null || true)"
url="https://github.com/${repo}/releases/download/${tag}"
if [[ -f CHANGELOG.md ]]; then
  awk -v v="$version" 'index($0, "## [" v "]") == 1 {f=1; next} /^## \[/ {f=0} f' CHANGELOG.md
fi
echo "## Changes${prev:+ since $prev}"
echo
git log --no-merges --format='- %s (%h)' "${prev:+$prev..}$tag"
cat <<NOTES

## Install

\`\`\`sh
brew install kaeawc/tap/rust-build                     # macOS / Linux (stable releases)
docker pull ghcr.io/${repo}:${version}
curl -LO ${url}/rb-${version}-x86_64-unknown-linux-gnu.tar.gz   # or -musl, aarch64-*, *-apple-darwin, .zip on Windows
\`\`\`

## Verify

\`\`\`sh
curl -LO ${url}/SHA256SUMS && sha256sum -c --ignore-missing SHA256SUMS
gh attestation verify rb-${version}-x86_64-unknown-linux-gnu.tar.gz -R ${repo}
gh attestation verify oci://ghcr.io/${repo}:${version} -R ${repo}
\`\`\`

The desktop app (\`rb-desktop-*\`) is ad-hoc signed on macOS: after unzipping, run
\`xattr -dr com.apple.quarantine rust-build.app\`. On Linux it needs \`libxkbcommon-x11-0\`,
\`libwayland-client0\` and a Vulkan driver. musl builds are static but use musl's slower allocator;
prefer the gnu builds for \`loadgen\` and \`serve\` under load.
NOTES
