#!/usr/bin/env bash
# Signs and notarizes the macOS release, from the unsigned universal .tar.gz the build job made:
#   <out-dir>/revue-v<version>-macos-universal.tar.gz  the binary signed with Developer ID Application (hardened
#                                                       runtime, secure timestamp) and notarized
#   <out-dir>/revue-v<version>-macos-universal.pkg     an installer that puts it in /usr/local/bin, signed with
#                                                       Developer ID Installer, notarized, with its ticket stapled
# Apple can't staple a ticket to a bare binary, so macOS checks the .tar.gz's binary with Apple online on its first
# run; the .pkg carries its ticket and installs offline.
# Needs MACOS_APP_P12, MACOS_INSTALLER_P12, MACOS_P12_PASSWORD, APPLE_API_KEY, APPLE_API_KEY_ID and
# APPLE_API_ISSUER_ID: see the release environment's secrets.
# Usage: macos-sign.sh <unsigned .tar.gz> <out-dir>
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <unsigned .tar.gz> <out-dir>" >&2; exit 2; }
in=$1 out=$2
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
# The installer's ID. Upgrades and `pkgutil --forget` go by it, so it must never change.
pkg_id=io.github.jmsheas3.revue

missing=""
for v in MACOS_APP_P12 MACOS_INSTALLER_P12 MACOS_P12_PASSWORD APPLE_API_KEY APPLE_API_KEY_ID APPLE_API_ISSUER_ID; do
  [ -n "${!v:-}" ] || missing="$missing $v"
done
if [ -n "$missing" ]; then
  echo "::error title=Signing::The release environment is missing these secrets:$missing"
  exit 1
fi

work=$(mktemp -d)
keychain="$work/signing.keychain-db"
cleanup() {
  security delete-keychain "$keychain" 2> /dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# A keychain of its own, holding both identities, that codesign and pkgbuild may use without asking.
keychain_password=$(openssl rand -base64 24)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
for kind in APP INSTALLER; do
  var="MACOS_${kind}_P12"
  printf '%s' "${!var}" | base64 --decode > "$work/$kind.p12"
  security import "$work/$kind.p12" -k "$keychain" -P "$MACOS_P12_PASSWORD" \
    -T /usr/bin/codesign -T /usr/bin/pkgbuild -T /usr/bin/productsign > /dev/null \
    || { echo "::error title=Signing::$var couldn't be imported: check MACOS_P12_PASSWORD, and that it was exported as the setup README says"; exit 1; }
  rm "$work/$kind.p12"
done
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" > /dev/null
# Search it as well as the keychains already listed.
existing=$(security list-keychains -d user | sed 's/^ *"//; s/"$//')
# shellcheck disable=SC2086 # one keychain path per line, none with spaces
security list-keychains -d user -s "$keychain" $existing

identity() { # "Application" or "Installer"
  security find-identity -v "$keychain" | sed -n "s/.*\"\(Developer ID $1: [^\"]*\)\".*/\1/p" | head -n 1
}
app_identity=$(identity Application)
installer_identity=$(identity Installer)
if [ -z "$app_identity" ] || [ -z "$installer_identity" ]; then
  echo "::error title=Signing::Expected a Developer ID Application identity in MACOS_APP_P12 and a Developer ID Installer identity in MACOS_INSTALLER_P12; found:"
  security find-identity -v "$keychain"
  exit 1
fi
echo "Signing as $app_identity"
echo "Packaging as $installer_identity"

key="$work/AuthKey.p8"
(umask 077 && printf '%s\n' "$APPLE_API_KEY" > "$key")
notarize() { # file
  local result id status
  echo "Notarizing $(basename "$1") (this usually takes a few minutes)"
  result=$(xcrun notarytool submit "$1" --key "$key" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" \
    --wait --timeout 45m --output-format json) || true
  echo "$result"
  id=$(printf '%s' "$result" | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get("id", ""))' 2> /dev/null || true)
  status=$(printf '%s' "$result" | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get("status", ""))' 2> /dev/null || true)
  if [ "$status" != Accepted ]; then
    echo "::error title=Notarization::Apple's answer for $(basename "$1"): ${status:-none (see the log above)}"
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --key "$key" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID" || true
    fi
    exit 1
  fi
}

# The binary
tar -xzf "$in" -C "$work"
bin="$work/revue-v$version-macos-universal/revue"
codesign --force --options runtime --timestamp --identifier "$pkg_id" --sign "$app_identity" --keychain "$keychain" "$bin"
codesign --verify --strict --verbose=2 "$bin"
codesign --display --verbose=2 "$bin"
ditto -c -k "$bin" "$work/revue.zip"
notarize "$work/revue.zip"
mkdir -p "$out"
"$root/.github/scripts/package.sh" macos-universal "$bin" "$out"

# The installer
mkdir "$work/pkgroot"
cp "$bin" "$work/pkgroot/revue"
pkg="$out/revue-v$version-macos-universal.pkg"
pkgbuild --root "$work/pkgroot" --identifier "$pkg_id" --version "$version" --install-location /usr/local/bin \
  --sign "$installer_identity" --keychain "$keychain" --timestamp "$pkg"
pkgutil --check-signature "$pkg"
notarize "$pkg"
xcrun stapler staple "$pkg"
xcrun stapler validate "$pkg"
spctl --assess --type install --verbose=2 "$pkg"
