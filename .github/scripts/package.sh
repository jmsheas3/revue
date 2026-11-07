#!/usr/bin/env bash
# Packages a built revue binary for a release as <out-dir>/revue-<name>.tar.gz, holding a folder of the same name
# with the binary, LICENSE, THIRD_PARTY_LICENSES, THIRD_PARTY_LICENSES_RUST (made by licenses.sh, in the checkout's
# root), README.md and the files it links to (SECURITY.md, docs/*.md). (Windows's .zip is made in release.yml, the same way.) The
# names carry no version, so the README's releases/latest/download/ links always reach the newest release; the
# binary's --version says which it is. Prints the archive's path.
# Usage: package.sh <name> <binary> <out-dir>    e.g. package.sh linux-x86_64 out/revue dist
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 <name> <binary> <out-dir>" >&2; exit 2; }
name=$1 bin=$2 out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
base="revue-$name"
[ -f "$root/THIRD_PARTY_LICENSES_RUST" ] \
  || { echo "::error title=Package::$root/THIRD_PARTY_LICENSES_RUST is missing: run .github/scripts/licenses.sh"; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$base"
cp "$bin" "$stage/$base/revue"
chmod 755 "$stage/$base/revue"
cp "$root/LICENSE" "$root/THIRD_PARTY_LICENSES" "$root/THIRD_PARTY_LICENSES_RUST" "$root/README.md" "$root/SECURITY.md" "$stage/$base/"
mkdir "$stage/$base/docs"
cp "$root"/docs/*.md "$stage/$base/docs/"
mkdir -p "$out"
# COPYFILE_DISABLE keeps macOS's tar from adding ._ files for extended attributes.
COPYFILE_DISABLE=1 tar -C "$stage" -czf "$out/$base.tar.gz" "$base"
echo "$out/$base.tar.gz"
