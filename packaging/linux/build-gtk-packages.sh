#!/usr/bin/env bash
# Package the Linux desktop app built for <target> (cargo build --release
# --features gtk --bin capycal-gtk, a glibc target): .deb and .rpm named
# capycal-gtk with the desktop entry and the icons. Needs cargo-deb and
# cargo-generate-rpm.
# Usage: packaging/linux/build-gtk-packages.sh <target>
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
target=$1
dist="$root/dist"
# Cargo.toml's gtk variants take the icons from here
stage="$dist/pkg-gtk"

rm -rf "$stage"
mkdir -p "$stage"
iconset="$root/packaging/macos/icon/AppIcon.iconset"
for size in 16 32 128 256 512; do
    cp "$iconset/icon_${size}x${size}.png" "$stage/icon-$size.png"
done

(cd "$root" && cargo deb --no-build --no-strip --variant gtk --target "$target" --output "$dist/")
(cd "$root" && cargo generate-rpm --variant gtk --target "$target" --output "$dist/")

cd "$dist"
for file in capycal-gtk_*.deb capycal-gtk-*.rpm; do
    shasum -a 256 "$file" >"$file.sha256"
done
echo "packaged capycal-gtk for $target"
