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
        "revue needs git, and nothing else: no Rust, no Node. Each archive below holds the `revue` binary, the README"
        " and its docs (`docs/`), revue's license (`LICENSE`) and the licenses of what it's built from"
        " (`THIRD_PARTY_LICENSES` for the UI's libraries, `THIRD_PARTY_LICENSES_RUST` for the Rust crates, also"
        " attached on its own).",
        "",
        "- **macOS** (Apple Silicon and Intel): open `revue-macos-universal.pkg`, which installs `revue` in"
        " `/usr/local/bin`; or unpack `revue-macos-universal.tar.gz` and put `revue` on your `PATH`. Both are"
        " signed and notarized by Apple. To uninstall the `.pkg`:"
        " `sudo rm /usr/local/bin/revue && sudo pkgutil --forget io.github.jmsheas3.revue`.",
        "- **Linux** (x86_64 or ARM64): unpack `revue-linux-x86_64.tar.gz` or `revue-linux-arm64.tar.gz` and put"
        " `revue` on your `PATH`. It's a static binary, so it runs on any distribution.",
        "- **Windows** (x86_64), **experimental**: unzip `revue-windows-x86_64.zip` and run `revue.exe`. It passes a"
        " smoke test, but most of revue's tests don't run on Windows yet.",
        "",
        "To check a download, compare its SHA-256 with its line in `SHA256SUMS`:"
        " `sha256sum -c SHA256SUMS --ignore-missing` on Linux, `shasum -a 256 <file>` on macOS.",
        "",
        f"The README has [copy-paste install commands](https://github.com/{repo}/blob/{tag}/README.md#-install).",
        "",
    ]
    compare = f"compare/{previous}...{tag}" if previous else f"commits/{tag}"
    out.append(f"**Every change**: https://github.com/{repo}/{compare}")
    print("\n".join(out))


if __name__ == "__main__":
    main()
