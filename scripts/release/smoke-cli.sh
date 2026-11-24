#!/usr/bin/env bash
# smoke-cli.sh <dist> <label> <version> <target>: verify, extract and exercise one rb archive.
# Runs under bash on Linux, macOS, Windows (Git Bash) and Alpine.
set -euo pipefail
dist="$1" label="$2" version="$3" target="$4"
name="rb-${label}-${target}"
case "$target" in
  *windows*) archive="$name.zip" exe=rb.exe ;;
  *) archive="$name.tar.gz" exe=rb ;;
esac
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd "$dist" && grep "  ${archive}\$" SHA256SUMS | sum -c -)

work="$(mktemp -d)"
case "$archive" in
  *.zip) 7z x -bso0 -o"$work" "$dist/$archive" ;;
  *) tar -xzf "$dist/$archive" -C "$work" ;;
esac
rb="$work/$name/$exe"

got="$("$rb" --version | tr -d '\r')"
[[ "$got" == "rb $version" ]] || { echo "rb --version printed '$got', expected 'rb $version'" >&2; exit 1; }

port=$((20000 + RANDOM % 20000)); tls=$((port + 1))
pids=()
cleanup() { local p; for p in ${pids[@]+"${pids[@]}"}; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
wait_up() { for _ in $(seq 1 50); do "$rb" get -k "$1" >/dev/null 2>&1 && return 0; sleep 0.2; done; echo "server at $1 never came up" >&2; return 1; }

"$rb" serve --addr "127.0.0.1:$port" & pids+=($!)
wait_up "http://127.0.0.1:$port/healthz"
"$rb" get "http://127.0.0.1:$port/healthz"
"$rb" get -i "http://127.0.0.1:$port/api/info"
"$rb" loadgen -c 4 -d 2s "http://127.0.0.1:$port/healthz"
# The embedded web UI (--features full includes web).
"$rb" get "http://127.0.0.1:$port/" | tee "$work/index.html" | head -c 200; echo
grep -q '<div id="root"></div>' "$work/index.html" || { echo "web UI is not embedded" >&2; exit 1; }

"$rb" serve --addr "127.0.0.1:$tls" --self-signed & pids+=($!)
wait_up "https://127.0.0.1:$tls/healthz"
"$rb" get -k -i "https://127.0.0.1:$tls/api/info" | tee "$work/info.txt"
grep -q 'HTTP/2.0' "$work/info.txt" || { echo "TLS + ALPN did not negotiate HTTP/2" >&2; exit 1; }

"$rb" cert --out "$work/certs"
test -s "$work/certs/cert.pem" && test -s "$work/certs/key.pem"
echo "smoke OK: $archive"
