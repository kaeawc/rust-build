#!/usr/bin/env bash
# Render Formula/rust-build.rb from dist/SHA256SUMS and push it to kaeawc/homebrew-tap.
# Env: TAG (vX.Y.Z), REPO (owner/name), GH_TOKEN (PAT, Contents: read/write on homebrew-tap),
#      REQUIRE_TOKEN=1 to fail instead of skipping when GH_TOKEN is empty, RENDER_ONLY=1 to just
#      write ./rust-build.rb (used by the dry run to check the formula renders).
set -euo pipefail
: "${TAG:?}" "${REPO:?}"
if [[ "${RENDER_ONLY:-0}" != 1 && -z "${GH_TOKEN:-}" ]]; then
  [[ "${REQUIRE_TOKEN:-0}" == 1 ]] && { echo "::error::HOMEBREW_TAP_TOKEN is not set in the release environment" >&2; exit 1; }
  echo "HOMEBREW_TAP_TOKEN is not set; skipping"; exit 0
fi
version="${TAG#v}"
label="${LABEL:-$version}"
url="https://github.com/${REPO}/releases/download/${TAG}"
sha_for() {
  local s; s=$(awk -v f="rb-${label}-$1.tar.gz" '$2 == f {print $1}' dist/SHA256SUMS)
  [[ -n "$s" ]] || { echo "no checksum for rb-${label}-$1.tar.gz" >&2; exit 1; }
  printf '%s' "$s"
}
mac_arm=$(sha_for aarch64-apple-darwin)
mac_x86=$(sha_for x86_64-apple-darwin)
lin_arm=$(sha_for aarch64-unknown-linux-gnu)
lin_x86=$(sha_for x86_64-unknown-linux-gnu)

render() {
cat <<RUBY
class RustBuild < Formula
  desc "HTTP/1.1+2 server, client, load generator and TLS tunnel (rb)"
  homepage "https://github.com/${REPO}"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
      url "${url}/rb-${version}-aarch64-apple-darwin.tar.gz"
      sha256 "${mac_arm}"
    end
    on_intel do
      url "${url}/rb-${version}-x86_64-apple-darwin.tar.gz"
      sha256 "${mac_x86}"
    end
  end

  on_linux do
    on_arm do
      url "${url}/rb-${version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${lin_arm}"
    end
    on_intel do
      url "${url}/rb-${version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${lin_x86}"
    end
  end

  livecheck do
    url :stable
    strategy :github_latest
  end

  def install
    bin.install "rb"
    doc.install "README.md"
  end

  test do
    assert_match "rb #{version}", shell_output("#{bin}/rb --version")
  end
end
RUBY
}

if [[ "${RENDER_ONLY:-0}" == 1 ]]; then render > rust-build.rb; exit 0; fi
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
# The token goes in an auth header, not the clone URL, so it never lands in .git/config or logs.
auth="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')"
git -c http.extraHeader="$auth" clone -q https://github.com/kaeawc/homebrew-tap.git "$work"
mkdir -p "$work/Formula"; render > "$work/Formula/rust-build.rb"
cd "$work"
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Formula/rust-build.rb
if git diff --cached --quiet; then echo "formula unchanged"; exit 0; fi
git commit -qm "rust-build ${TAG}"
git -c http.extraHeader="$auth" push -q origin HEAD
