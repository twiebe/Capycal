#!/usr/bin/env bash
# Package the Linux CLI built for <target> (cargo build --release --target):
# a tarball with man page, shell completions and systemd units, plus .deb
# and .rpm packages (needs cargo-deb and cargo-generate-rpm). The binary
# must run on this machine, since it generates the man page and completions.
# Usage: packaging/linux/build-packages.sh <target> [archive name]
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
target=$1
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
name=${2:-capycal-v$version-$target}
bin="${CARGO_TARGET_DIR:-$root/target}/$target/release/capycal"
dist="$root/dist"
# Cargo.toml's deb and rpm metadata take the generated files from here
stage="$dist/pkg"

rm -rf "$stage"
mkdir -p "$stage"
"$bin" --man >"$stage/capycal.1"
"$bin" --completions bash >"$stage/capycal.bash"
"$bin" --completions zsh >"$stage/_capycal"
"$bin" --completions fish >"$stage/capycal.fish"
# Packages install the binary to /usr/bin
for unit in capycal capycal-user; do
    sed 's|/usr/local/bin/capycal|/usr/bin/capycal|' \
        "$root/contrib/systemd/$unit.service" >"$stage/$unit.service"
done

dir="$dist/$name"
rm -rf "$dir"
mkdir -p "$dir/completions" "$dir/systemd"
cp "$bin" "$root/README.md" "$root/LICENSE" "$root/CONTRIBUTORS.md" "$root/THIRD-PARTY-NOTICES.txt" "$root/capycal.example.toml" "$stage/capycal.1" "$dir/"
cp "$stage/capycal.bash" "$stage/_capycal" "$stage/capycal.fish" "$dir/completions/"
cp "$root/contrib/systemd/capycal.service" "$root/contrib/systemd/capycal-user.service" "$root/contrib/systemd/capycal.sysusers" "$dir/systemd/"
tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"

(cd "$root" && cargo deb --no-build --no-strip --target "$target" --output "$dist/")
(cd "$root" && cargo generate-rpm --target "$target" --output "$dist/")

cd "$dist"
for file in "$name.tar.gz" ./*.deb ./*.rpm; do
    shasum -a 256 "${file#./}" >"${file#./}.sha256"
done
echo "packaged $name"
