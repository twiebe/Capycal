#!/usr/bin/env bash
# Write the Homebrew formula capycal and the cask capycal-app for a release from
# the checksums of its macOS and musl tarballs and of the disk image.
# Usage: packaging/homebrew/make-homebrew.sh <dir with the .sha256 files> <output dir>
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
dir=$1
out=$2
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../Cargo.toml" | head -n1)
sum() { cut -d' ' -f1 "$dir/capycal-v$version-$1.sha256"; }
mkdir -p "$out"
sed -e "s/@VERSION@/$version/" \
    -e "s/@SHA256_MACOS@/$(sum aarch64-apple-darwin.tar.gz)/" \
    -e "s/@SHA256_X86_64@/$(sum x86_64-unknown-linux-musl.tar.gz)/" \
    -e "s/@SHA256_AARCH64@/$(sum aarch64-unknown-linux-musl.tar.gz)/" \
    "$here/capycal.rb.in" >"$out/capycal.rb"
sed -e "s/@VERSION@/$version/" \
    -e "s/@SHA256_DMG@/$(sum macos.dmg)/" \
    "$here/capycal-app.rb.in" >"$out/capycal-app.rb"
echo "wrote $out/capycal.rb and $out/capycal-app.rb"
