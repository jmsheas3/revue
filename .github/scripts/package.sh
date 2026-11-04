#!/usr/bin/env bash
# Packages a built revue binary for a release as <out-dir>/revue-v<version>-<name>.tar.gz, holding a folder of the
# same name with the binary, LICENSE, THIRD_PARTY_LICENSES and README.md. (Windows's .zip is made in release.yml.)
# Prints the archive's path.
# Usage: package.sh <name> <binary> <out-dir>    e.g. package.sh linux-x86_64 out/revue dist
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 <name> <binary> <out-dir>" >&2; exit 2; }
name=$1 bin=$2 out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
base="revue-v$version-$name"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$base"
cp "$bin" "$stage/$base/revue"
chmod 755 "$stage/$base/revue"
cp "$root/LICENSE" "$root/THIRD_PARTY_LICENSES" "$root/README.md" "$stage/$base/"
mkdir -p "$out"
# COPYFILE_DISABLE keeps macOS's tar from adding ._ files for extended attributes.
COPYFILE_DISABLE=1 tar -C "$stage" -czf "$out/$base.tar.gz" "$base"
echo "$out/$base.tar.gz"
