#!/usr/bin/env bash
# Publishes the files in <dir> as the GitHub release v<VERSION> of this commit: first a draft holding every file,
# then published, which creates the tag on GITHUB_SHA. With immutable releases on, publishing locks the tag and the
# files. A draft left by an earlier attempt is replaced; a published release is never touched.
# Needs GH_TOKEN, GH_REPO, VERSION, PRERELEASE (true or false), GITHUB_SHA, and the notes in notes.md.
# Usage: publish.sh <dir>
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <dir>" >&2; exit 2; }
dir=$1
tag="v$VERSION"

if draft=$(gh release view "$tag" --json isDraft --jq .isDraft 2> /dev/null); then
  if [ "$draft" != true ]; then
    echo "::error title=Publish::$tag is already published."
    exit 1
  fi
  echo "Replacing the draft of $tag that an earlier attempt left"
  gh release delete "$tag" --yes
fi

flags=(--draft --target "$GITHUB_SHA" --title "revue $tag" --notes-file notes.md)
if [ "$PRERELEASE" = true ]; then flags+=(--prerelease); else flags+=(--latest); fi
gh release create "$tag" "${flags[@]}" "$dir"/*
gh release edit "$tag" --draft=false
url=$(gh release view "$tag" --json url --jq .url)
echo "Published $url"
echo "Published [revue $tag]($url)." >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
