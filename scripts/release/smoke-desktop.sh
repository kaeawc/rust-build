#!/usr/bin/env bash
# smoke-desktop.sh <dist> <label> <target>: launch the packaged app and wait for Rune's ready
# receipt ($RUNE_READY_DIR/<bundle id or exe name>.<pid>, written after the first on_ready).
set -euo pipefail
dist="$1" label="$2" target="$3"
name="rb-desktop-${label}-${target}"
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
work="$(mktemp -d)"
launch=()
case "$target" in
  *apple-darwin)
    archive="$name.zip"; ditto -x -k "$dist/$archive" "$work"
    exe="$work/$name/rust-build.app/Contents/MacOS/rb-desktop" ;;
  *windows*)
    archive="$name.zip"; 7z x -bso0 -o"$work" "$dist/$archive"
    exe="$work/$name/rb-desktop.exe" ;;
  *)
    archive="$name.tar.gz"; tar -xzf "$dist/$archive" -C "$work"
    exe="$work/$name/rb-desktop"
    launch=(xvfb-run -a)
    icds=(/usr/share/vulkan/icd.d/lvp_icd*.json) # lavapipe: software Vulkan
    VK_ICD_FILENAMES="${icds[0]}"; export VK_ICD_FILENAMES ;;
esac
(cd "$dist" && grep "  ${archive}\$" SHA256SUMS | sum -c -)

ready="$work/ready"; mkdir -p "$ready"
RUNE_READY_DIR="$ready"
[[ "$target" == *windows* ]] && RUNE_READY_DIR="$(cygpath -w "$ready")"
export RUNE_READY_DIR
${launch[@]+"${launch[@]}"} "$exe" & pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT
for _ in $(seq 1 120); do
  if compgen -G "$ready/*" >/dev/null; then cat "$ready"/*; echo; echo "desktop ready: $archive"; exit 0; fi
  kill -0 "$pid" 2>/dev/null || { echo "app exited before it was ready" >&2; wait "$pid" || true; exit 1; }
  sleep 0.5
done
echo "no ready receipt within 60 s" >&2; exit 1
