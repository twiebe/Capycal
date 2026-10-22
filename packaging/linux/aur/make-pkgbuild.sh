#!/usr/bin/env bash
# Write the PKGBUILD of the AUR package capycal-bin for a release from the
# checksums of its musl tarballs.
# Usage: packaging/linux/aur/make-pkgbuild.sh <dir with the .tar.gz.sha256 files> <output>
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
dir=$1
out=$2
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../../Cargo.toml" | head -n1)
sum() { cut -d' ' -f1 "$dir/capycal-v$version-$1-unknown-linux-musl.tar.gz.sha256"; }
sed -e "s/@VERSION@/$version/" \
    -e "s/@SHA256_X86_64@/$(sum x86_64)/" \
    -e "s/@SHA256_AARCH64@/$(sum aarch64)/" \
    "$here/PKGBUILD.in" >"$out"
echo "wrote $out"
