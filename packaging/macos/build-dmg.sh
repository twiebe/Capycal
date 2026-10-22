#!/usr/bin/env bash
# Pack Capycal.app into a compressed disk image with an /Applications link.
# Usage: packaging/macos/build-dmg.sh [Capycal.app] [output.dmg]
#   (defaults: target/macos/Capycal.app, target/macos/capycal-v<version>-macos.dmg)
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
app=${1:-$root/target/macos/Capycal.app}
dmg=${2:-$root/target/macos/capycal-v$version-macos.dmg}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R "$app" "$stage/Capycal.app"
ln -s /Applications "$stage/Applications"

mkdir -p "$(dirname "$dmg")"
hdiutil create -volname Capycal -srcfolder "$stage" -ov -format UDZO "$dmg"
echo "built $dmg"
