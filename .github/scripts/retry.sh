#!/usr/bin/env bash
# Runs a command up to three times, RETRY_WAIT seconds apart (default 30), and exits with its last status. For the
# release steps that ask Apple's servers something: `xcrun stapler` looks the notarization ticket up online, and a
# dropped connection fails it outright (error 68, "The network connection was lost"), which shouldn't fail a release.
# Usage: retry.sh <command> [args...]    e.g. retry.sh xcrun stapler validate revue-macos-universal.pkg
# (Written for bash 3.2 too, macOS's own.)
set -uo pipefail

[ $# -ge 1 ] || { echo "usage: $0 <command> [args...]" >&2; exit 2; }
wait=${RETRY_WAIT:-30}
status=0
for try in 1 2 3; do
  "$@" && exit 0
  status=$?
  [ "$try" -lt 3 ] || break
  echo "::warning title=Retry::$* failed with exit code $status (try $try of 3); trying again in $wait s"
  sleep "$wait"
done
echo "::error title=Retry::$* failed three times"
exit "$status"
