#!/usr/bin/env bash
# The smallest proof that a revue binary works: it reports the version in Cargo.toml and the commit it was built
# from (and not "modified"), starts on a repo with one commit and one edit, and answers /api/info, /api/tree and the
# page. CI runs it on the test build; release.yml runs it on every release binary, and on the installed macOS .pkg.
# Usage: smoke-test.sh path/to/revue    (written for bash 3.2 too, macOS's own)
set -euo pipefail

fail() { echo "::error title=Smoke test::$*"; exit 1; }

[ $# -eq 1 ] || { echo "usage: $0 path/to/revue" >&2; exit 2; }
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
[ -x "$bin" ] || fail "$1 isn't an executable file"
root=$(cd "$(dirname "$0")/../.." && pwd)
expected=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
head=$(git -C "$root" rev-parse HEAD)

work=$(mktemp -d)
pid=""
cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2> /dev/null || true; wait "$pid" 2> /dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

# Nothing from the runner's or the user's git config, and a discovery directory of its own.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 XDG_RUNTIME_DIR="$work/run"
mkdir -m 700 "$XDG_RUNTIME_DIR"
repo="$work/smoke"
git init -q -b main "$repo"
printf 'hello\n' > "$repo/hello.txt"
git -C "$repo" add hello.txt
git -C "$repo" -c user.name=smoke -c user.email=smoke@example.com commit -qm first
printf 'hello, world\n' > "$repo/hello.txt"

version=$("$bin" --version)
echo "$version"
[[ $version =~ ^revue\ ([^ ]+)\ \(([0-9a-f]+)\)$ ]] || fail "--version printed '$version', not 'revue <version> (<commit>)'"
[ "${BASH_REMATCH[1]}" = "$expected" ] || fail "--version says ${BASH_REMATCH[1]}, Cargo.toml says $expected"
case $head in "${BASH_REMATCH[2]}"*) ;; *) fail "--version says commit ${BASH_REMATCH[2]}, this checkout is $head" ;; esac

"$bin" "$repo" --no-open --port 0 > "$work/out.log" 2>&1 &
pid=$!
url=""
for _ in $(seq 150); do
  url=$(grep -o 'http://127\.0\.0\.1:[0-9]*/' "$work/out.log" | head -n 1 || true)
  [ -n "$url" ] && break
  kill -0 "$pid" 2> /dev/null || fail "revue exited: $(cat "$work/out.log")"
  sleep 0.2
done
[ -n "$url" ] || fail "revue didn't print its address within 30 s: $(cat "$work/out.log")"
echo "listening at $url"

check() { # name, URL path, Python expression on the parsed JSON answer `d`
  local answer
  answer=$(curl -fsS --max-time 30 "$url$2") || fail "GET /$2 failed"
  printf '%s' "$answer" | python3 -I -c "import json, sys; d = json.load(sys.stdin); sys.exit(0 if ($3) else 1)" \
    || fail "GET /$2 answered $answer"
  echo "ok: $1"
}
check "the repo" "api/info" 'd["git"] is True and d["name"] == "smoke" and d["branch"] == "main" and d["hasHead"]'
check "the edit" "api/tree?mode=working" 'd["files"] == ["hello.txt"] and d["changes"]["hello.txt"]["status"] == "M"'
page=$(curl -fsS --max-time 30 "$url") || fail "GET / failed"
case $page in *'<html'*) echo "ok: the page" ;; *) fail "GET / didn't answer the page" ;; esac
echo "Smoke test passed."
