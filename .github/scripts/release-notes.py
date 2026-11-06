#!/usr/bin/env python3
"""Writes a release's notes, in Markdown, to stdout: the pull requests merged since the previous release, how to
install, and a link to every change.

The pull requests are found from the commits (git rev-list <previous>..HEAD, so the checkout needs its history):
each commit's merged PRs into develop or main, through GitHub's API. That lists what was merged into develop even
though only develop's PR merged into main, and skips that develop → main PR itself; a hotfix, merged into main and
then into develop from the same branch, is listed once.

Environment: VERSION, PREVIOUS (the previous release's tag, empty for the first), GITHUB_REPOSITORY, and GH_TOKEN
for gh.
"""
import json
import os
import subprocess

MAX_COMMITS = 500


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def merged_prs(previous, repo):
    commits = run("git", "rev-list", f"{previous}..HEAD" if previous else "HEAD").split()
    prs = {}
    for sha in commits[:MAX_COMMITS]:
        for pr in json.loads(run("gh", "api", f"repos/{repo}/commits/{sha}/pulls")):
            if pr.get("merged_at") and pr["base"]["ref"] in ("main", "develop") and pr["head"]["ref"] != "develop":
                prs[pr["number"]] = pr
    listed = {}
    for pr in sorted(prs.values(), key=lambda p: p["merged_at"]):
        branch = pr["head"]["ref"]
        key = branch if branch.startswith("hotfix/") else pr["number"]
        if key not in listed or pr["base"]["ref"] == "main":
            listed[key] = pr
    return sorted(listed.values(), key=lambda p: p["merged_at"]), len(commits) > MAX_COMMITS


def main():
    version, previous = os.environ["VERSION"], os.environ.get("PREVIOUS", "")
    repo = os.environ["GITHUB_REPOSITORY"]
    tag = f"v{version}"
    prs, cut = merged_prs(previous, repo)
    out = ["## What's changed", ""]
    if prs:
        out += [f"- {' '.join(pr['title'].split())} (#{pr['number']})" for pr in prs]
        if cut:
            out.append(f"- …and the changes in commits before the last {MAX_COMMITS} (see the full list below)")
    else:
        out.append("No pull requests since the last release." if previous else "The first release.")
    out += [
        "",
        "## Install",
        "",
        f"revue needs only git. Pick the file for your platform below; the README has"
        f" [copy-paste install commands](https://github.com/{repo}/blob/{tag}/README.md#-install).",
        "",
        "- **macOS** (Apple Silicon and Intel): `revue-macos-universal.pkg`, which installs `revue` in"
        " `/usr/local/bin`, or `revue-macos-universal.tar.gz`. Both are signed and notarized by Apple.",
        "- **Linux**: `revue-linux-x86_64.tar.gz` or `revue-linux-arm64.tar.gz`, static binaries for any distribution.",
        "- **Windows** (experimental): `revue-windows-x86_64.zip`.",
        "",
        "`SHA256SUMS` lists every file's SHA-256. Each archive also holds the README, its docs and the license"
        " notices; `THIRD_PARTY_LICENSES_RUST` is attached on its own too, since the `.pkg` installs only the binary.",
        "",
    ]
    compare = f"compare/{previous}...{tag}" if previous else f"commits/{tag}"
    out.append(f"**Every change**: https://github.com/{repo}/{compare}")
    print("\n".join(out))


if __name__ == "__main__":
    main()
