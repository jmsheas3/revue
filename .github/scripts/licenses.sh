#!/usr/bin/env bash
# Writes the license notices of every Rust crate compiled into the release binaries, with cargo-about
# (.github/licenses/about.toml and about.hbs), to THIRD_PARTY_LICENSES_RUST or the file given. Fails when a crate's
# license isn't one about.toml accepts. The texts come from the crates' own sources only: cargo fetch downloads them,
# then cargo-about runs offline. Needs cargo-about (CI installs it).
# Usage: licenses.sh [out-file]
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/THIRD_PARTY_LICENSES_RUST}
cd "$root"
cargo fetch --locked
cargo about generate --frozen --fail --config .github/licenses/about.toml --output-file "$out" .github/licenses/about.hbs
crates=$(grep -c '^  [A-Za-z0-9_-]* [0-9]' "$out" || true)
[ "$crates" -gt 50 ] || { echo "::error title=Licenses::Only $crates crates listed in $out; expected well over 50"; exit 1; }
echo "Wrote $out: $crates crates."
