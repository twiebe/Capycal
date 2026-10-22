#!/usr/bin/env bash
# Push the PKGBUILDs written by make-pkgbuild.sh to the AUR, each with its
# .SRCINFO (made by makepkg in an Arch container, so it needs Docker). Needs
# an SSH key registered with the AUR account in AUR_SSH_KEY.
# Usage: packaging/linux/aur/publish.sh <dir with the .PKGBUILD files>
set -euo pipefail

dir=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
key="$work/key"
printf '%s\n' "$AUR_SSH_KEY" >"$key"
chmod 600 "$key"
ssh-keyscan -t ed25519 aur.archlinux.org >"$work/known_hosts" 2>/dev/null
export GIT_SSH_COMMAND="ssh -i $key -o IdentitiesOnly=yes -o UserKnownHostsFile=$work/known_hosts"
version=$(sed -n 's/^pkgver=//p' "$dir/capycal-bin.PKGBUILD")

for pkg in capycal-bin capycal-app-bin; do
    repo="$work/$pkg"
    git clone -q "ssh://aur@aur.archlinux.org/$pkg.git" "$repo"
    cp "$dir/$pkg.PKGBUILD" "$repo/PKGBUILD"
    docker run --rm --platform linux/amd64 -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$repo:/pkg" -w /pkg \
        archlinux:base-devel makepkg --printsrcinfo >"$repo/.SRCINFO"
    git -C "$repo" add PKGBUILD .SRCINFO
    if git -C "$repo" diff --cached --quiet; then
        echo "$pkg is up to date"
        continue
    fi
    git -C "$repo" -c user.name="github-actions[bot]" \
        -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
        commit -q -m "capycal $version"
    git -C "$repo" push -q origin HEAD:master
    echo "pushed $pkg $version"
done
