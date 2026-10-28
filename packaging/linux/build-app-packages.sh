#!/usr/bin/env bash
# Package the Linux desktop app built for <target> (cargo build --release
# --features gtk --bin capycal-app, a glibc target): .deb and .rpm named
# capycal-app with the desktop entry, the AppStream metadata and the icons.
# Needs cargo-deb and cargo-generate-rpm; checks the metadata if appstreamcli
# is installed.
# Usage: packaging/linux/build-app-packages.sh <target>
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
target=$1
dist="$root/dist"
# Cargo.toml's gtk variants take the icons and the metadata from here
stage="$dist/pkg-app"

rm -rf "$stage"
mkdir -p "$stage"
iconset="$root/packaging/macos/icon/AppIcon.iconset"
for size in 16 32 128 256 512; do
    cp "$iconset/icon_${size}x${size}.png" "$stage/icon-$size.png"
done

# The release software centers show: the version and the date of the commit
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
date=$(git -C "$root" log -1 --format=%cs 2>/dev/null || date -u +%F)
sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$date/" \
    "$root/packaging/linux/app.capycal.metainfo.xml" >"$stage/app.capycal.metainfo.xml"
if command -v appstreamcli >/dev/null; then
    # appstreamcli does not count .app as a top-level domain, so it takes
    # app.capycal (capycal.app reversed) for a name that is not reverse DNS
    appstreamcli validate --no-net --override=cid-desktopapp-is-not-rdns=info \
        "$stage/app.capycal.metainfo.xml"
fi

(cd "$root" && cargo deb --no-build --no-strip --variant gtk --target "$target" --output "$dist/")
(cd "$root" && cargo generate-rpm --variant gtk --target "$target" --output "$dist/")

cd "$dist"
for file in capycal-app_*.deb capycal-app-*.rpm; do
    shasum -a 256 "$file" >"$file.sha256"
done
echo "packaged capycal-app for $target"
