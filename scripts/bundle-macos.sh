#!/bin/zsh
set -euo pipefail

project_root="${0:A:h:h}"
if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  print -u2 "AproBook v0.5.1 bundling supports Apple Silicon macOS only."
  exit 1
fi

app="$project_root/dist/AproBook-v0.5.1.app"
if [[ -e "$app" ]]; then
  print -u2 "Bundle already exists: $app. Move it aside before rebuilding."
  exit 1
fi
staging="$project_root/dist/.AproBook-v0.5.1-building.app"
if [[ -e "$staging" ]]; then
  print -u2 "Incomplete staging bundle exists: $staging. Move it aside before rebuilding."
  exit 1
fi

cd "$project_root"
cargo build --release --locked

mkdir -p "$staging/Contents/MacOS" "$staging/Contents/Resources/assets"
cp "$project_root/target/release/AproBook" "$staging/Contents/MacOS/"
cp "$project_root/crates/ebook-desktop/macos/Info.plist" "$staging/Contents/"
cp -R "$project_root/crates/ebook-desktop/assets/pdfium" "$staging/Contents/Resources/assets/"

iconset="$project_root/dist/AproBook.iconset"
mkdir -p "$iconset"
module_cache="$project_root/target/swift-module-cache"
mkdir -p "$module_cache"
CLANG_MODULE_CACHE_PATH="$module_cache" SWIFT_MODULECACHE_PATH="$module_cache" swift "$project_root/scripts/make-icon.swift" "$project_root/crates/ebook-desktop/assets/aprobook-icon.svg" "$iconset/icon_512x512@2x.png"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$iconset/icon_512x512@2x.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128 256; do
  doubled=$((size * 2))
  sips -z "$doubled" "$doubled" "$iconset/icon_512x512@2x.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
CLANG_MODULE_CACHE_PATH="$module_cache" SWIFT_MODULECACHE_PATH="$module_cache" swift "$project_root/scripts/pack-icns.swift" "$iconset" "$staging/Contents/Resources/AproBook.icns"
mv "$staging" "$app"
codesign --force --deep --sign - "$app"
print "Built $app"
