#!/usr/bin/env bash
# Write the PKGBUILDs of the AUR packages capycal-bin (from the musl
# tarballs) and capycal-app-bin (from the desktop app's .deb) for a release,
# with the checksums in <dir>.
# Usage: packaging/linux/aur/make-pkgbuild.sh <dir with the .sha256 files> <output dir>
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
dir=$1
out=$2
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../../Cargo.toml" | head -n1)
sum() { cut -d' ' -f1 "$dir/$1.sha256"; }
fill() {
    sed -e "s/@VERSION@/$version/" \
        -e "s/@SHA256_X86_64@/$(sum "$2")/" \
        -e "s/@SHA256_AARCH64@/$(sum "$3")/" \
        "$here/$1.PKGBUILD.in" >"$out/$1.PKGBUILD"
    echo "wrote $out/$1.PKGBUILD"
}
fill capycal-bin "capycal-v$version-x86_64-unknown-linux-musl.tar.gz" "capycal-v$version-aarch64-unknown-linux-musl.tar.gz"
fill capycal-app-bin "capycal-app_$version-1_amd64.deb" "capycal-app_$version-1_arm64.deb"
