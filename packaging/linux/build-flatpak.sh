#!/usr/bin/env bash
# Build the desktop app as a Flatpak for <arch> (x86_64, aarch64) and write
# the single-file bundle dist/capycal-app-v<version>-<arch>.flatpak. The
# bundle names no repository: it carries no key, so updates from there would
# go unverified. Updates come through the signed .flatpakref.
# Needs flatpak and flatpak-builder with the flathub remote (--user).
# Usage: packaging/linux/build-flatpak.sh <arch>
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
arch=$1
id=app.capycal.Capycal
dist="$root/dist"
stage="$dist/pkg-flatpak"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
date=$(git -C "$root" log -1 --format=%cs 2>/dev/null || date -u +%F)
rm -rf "$stage"
mkdir -p "$stage"
# The Flatpak's ID; it provides the one of the native packages
sed -e "s/@VERSION@/$version/" -e "s/@DATE@/$date/" \
    -e "s|<id>app.capycal</id>|<id>$id</id>|" \
    -e "s|app.capycal.desktop|$id.desktop|" \
    -e "s|<provides>|&\n    <id>app.capycal</id>|" \
    "$root/packaging/linux/app.capycal.metainfo.xml" >"$stage/$id.metainfo.xml"
if command -v appstreamcli >/dev/null; then
    # See build-app-packages.sh for the rdns override
    appstreamcli validate --no-net --override=cid-desktopapp-is-not-rdns=info "$stage/$id.metainfo.xml"
fi

repo="$dist/flatpak-repo"
bundle="$dist/capycal-app-v$version-$arch.flatpak"
# Without rofiles-fuse: in a container (distrobox, toolbox) fusermount may not
# mount it, and the build fails at its very end
flatpak-builder --user --arch="$arch" --default-branch=stable --force-clean --install-deps-from=flathub \
    --disable-rofiles-fuse \
    --state-dir="$root/.flatpak-builder" --repo="$repo" \
    "$dist/flatpak-build-$arch" "$root/packaging/linux/flatpak/$id.yml"
flatpak build-bundle --arch="$arch" "$repo" "$bundle" "$id" stable \
    --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
cd "$dist"
sha256sum "$(basename "$bundle")" >"$(basename "$bundle").sha256"
echo "built $bundle"
