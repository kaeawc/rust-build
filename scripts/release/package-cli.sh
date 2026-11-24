#!/usr/bin/env bash
# package-cli.sh <label> <target>: target/<target>/dist/rb → dist/rb-<label>-<target>.{tar.gz,zip}
# with one top-level directory holding rb, README.md and LICENSE.
set -euo pipefail
label="$1" target="$2"
name="rb-${label}-${target}"
exe=rb; [[ "$target" == *windows* ]] && exe=rb.exe
stage="dist/stage/$name"
rm -rf dist/stage && mkdir -p "$stage"
cp "target/$target/dist/$exe" README.md LICENSE "$stage/"
cd dist/stage
if [[ "$target" == *windows* ]]; then
  7z a -tzip -bso0 "../$name.zip" "$name"
else
  COPYFILE_DISABLE=1 tar -czf "../$name.tar.gz" "$name" # no AppleDouble ._ files from macOS tar
fi
cd .. && rm -rf stage
