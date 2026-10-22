#!/usr/bin/env bash
# Build Capycal.app for Apple silicon: the capycal-ui menu bar app as main
# executable plus the capycal CLI next to it, signed ad hoc.
# Usage: packaging/macos/build-app.sh [output dir]   (default: target/macos)
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/macos}
target=aarch64-apple-darwin
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)

cargo build --release --locked --features ui --target "$target" --manifest-path "$root/Cargo.toml"
bin_dir="${CARGO_TARGET_DIR:-$root/target}/$target/release"

app="$out/Capycal.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" >"$app/Contents/Info.plist"
cp "$bin_dir/capycal-ui" "$bin_dir/capycal" "$app/Contents/MacOS/"
cp "$root/LICENSE" "$root/CONTRIBUTORS.md" "$root/THIRD-PARTY-NOTICES.txt" "$root/src/bin/capycal-ui/assets/OCTICONS-LICENSE" "$root/src/bin/capycal-ui/assets/SIMPLE-ICONS-LICENSE" "$app/Contents/Resources/"
iconutil -c icns "$root/packaging/macos/icon/AppIcon.iconset" -o "$app/Contents/Resources/AppIcon.icns"
codesign --force --deep --sign - "$app"
echo "built $app ($version)"
