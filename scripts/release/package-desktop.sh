#!/usr/bin/env bash
# package-desktop.sh <label> <version> <target>: Rune app → macOS .app zip / Linux tar.gz / Windows zip
set -euo pipefail
label="$1" version="$2" target="$3"
name="rb-desktop-${label}-${target}"
bin="desktop/target/$target/release/rb-desktop"
stage="dist/stage/$name"
rm -rf dist/stage && mkdir -p "$stage"
cp README.md LICENSE "$stage/"
case "$target" in
  *apple-darwin)
    app="$stage/rust-build.app"
    mkdir -p "$app/Contents/MacOS"
    cp "$bin" "$app/Contents/MacOS/rb-desktop"
    short="${version%%-*}"
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>rb-desktop</string>
  <key>CFBundleIdentifier</key><string>com.kaeawc.rust-build.desktop</string>
  <key>CFBundleName</key><string>rust-build</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${short}</string>
  <key>CFBundleVersion</key><string>${short}</string>
  <key>LSMinimumSystemVersion</key><string>${MACOSX_DEPLOYMENT_TARGET:-11.0}</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
    codesign --force --sign - "$app" # ad-hoc; Developer ID + notarization is phase 2
    (cd dist/stage && ditto -c -k --keepParent "$name" "../$name.zip")
    ;;
  *windows*)
    cp "$bin.exe" "$stage/"
    (cd dist/stage && 7z a -tzip -bso0 "../$name.zip" "$name")
    ;;
  *)
    cp "$bin" "$stage/"
    (cd dist/stage && tar -czf "../$name.tar.gz" "$name")
    ;;
esac
rm -rf dist/stage
