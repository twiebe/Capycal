#!/usr/bin/env bash
# Sign the .rpm files in <dir> with the key of the package repositories and
# rewrite their .sha256 files. The key comes ASCII armored, without a
# passphrase, in $PACKAGES_GPG_KEY. Needs gpg and rpmsign.
# Usage: packaging/linux/sign-rpms.sh <dir>
set -euo pipefail

dir=$1
GNUPGHOME=$(mktemp -d)
export GNUPGHOME
trap 'rm -rf "$GNUPGHOME"' EXIT
gpg --batch --quiet --import <<<"$PACKAGES_GPG_KEY"
fingerprint=$(gpg --batch --with-colons --list-secret-keys | awk -F: '/^fpr/ { print $10; exit }')

cd "$dir"
for rpm in *.rpm; do
    # Debian's rpm looks for gpg2
    rpmsign --addsign --define "_gpg_name $fingerprint" --define "__gpg $(command -v gpg)" "$rpm" >/dev/null
    shasum -a 256 "$rpm" >"$rpm.sha256"
done
echo "signed $(ls -- *.rpm | wc -l) rpms with $fingerprint"
