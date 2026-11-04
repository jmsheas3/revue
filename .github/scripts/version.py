#!/usr/bin/env python3
"""Compares the version in Cargo.toml with the released versions (the v* tags) and says what merging does.

  version.py check    for CI: fails on a version that can't be released (lower than the latest release, not a
                      semantic version, or out of step with Cargo.lock); otherwise notes what a merge will do
  version.py release  for release.yml: the same checks, then writes version, tag, previous (the latest release
                      before it, or nothing), release (true when the version isn't released yet) and prerelease to
                      $GITHUB_OUTPUT

A release is only ever made of a version higher than every earlier one, so a merge that doesn't raise the version
releases nothing. Tags are read from the remote with git ls-remote, so a shallow checkout is enough; set
VERSION_TAGS to a space-separated list of tags to use instead (for trying it out locally).
"""
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NUM = r"0|[1-9]\d*"
IDENT = r"0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*"
# Semantic Versioning 2.0.0, without build metadata (+...), which a tag can't be ordered by.
SEMVER = re.compile(rf"^({NUM})\.({NUM})\.({NUM})(?:-((?:{IDENT})(?:\.(?:{IDENT}))*))?$")


def precedence(version):
    """A sortable key for a version (SemVer section 11), or None if it isn't one."""
    m = SEMVER.match(version)
    if not m:
        return None
    core = tuple(int(x) for x in m.group(1, 2, 3))
    if m.group(4) is None:
        return (core, 1, ())  # a release comes after all of its pre-releases
    # Numeric identifiers sort numerically and before alphanumeric ones; more identifiers sort later.
    pre = tuple((0, int(i), "") if i.isdigit() else (1, 0, i) for i in m.group(4).split("."))
    return (core, 0, pre)


def annotate(level, message):
    print(f"::{level} title=Version::{message}")


def summary(text):
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"### Version\n\n{text}\n")


def released():
    """Every v<semver> tag, by name, with its precedence."""
    names = os.environ.get("VERSION_TAGS")
    if names is not None:
        names = names.split()
    else:
        out = subprocess.run(["git", "ls-remote", "--tags", "--refs", "origin"], cwd=ROOT, check=True,
                             capture_output=True, text=True).stdout
        names = [line.split("\trefs/tags/", 1)[1] for line in out.splitlines() if "\trefs/tags/" in line]
    tags = {}
    for name in names:
        if not name.startswith("v"):
            continue
        key = precedence(name[1:])
        if key is None:
            annotate("warning", f"Ignoring the tag {name}: it isn't v followed by a semantic version.")
        else:
            tags[name] = key
    return tags


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "check"
    if mode not in ("check", "release"):
        sys.exit(f"usage: {sys.argv[0]} check|release")
    package = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    version = package["version"]
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    # The crate itself is the one entry with its name and no source (a registry or git dependency has one).
    locked = [p["version"] for p in lock.get("package", []) if p.get("name") == package["name"] and "source" not in p]
    tag = f"v{version}"
    key = precedence(version)
    tags = released()
    latest = max(tags, key=tags.get) if tags else None

    errors = []
    if key is None:
        errors.append(f"Cargo.toml's version, {version}, isn't a semantic version such as 1.2.3 or 1.2.3-rc.1 "
                      "(build metadata, +..., isn't allowed).")
    if locked != [version]:
        errors.append(f"Cargo.lock has {package['name']} {', '.join(locked) or '(missing)'} but Cargo.toml says "
                      f"{version}: run cargo check and commit Cargo.lock.")
    if key is not None and latest is not None and key < tags[latest]:
        errors.append(f"{tag} is lower than the latest release, {latest}: raise the version in Cargo.toml above it.")
    if errors:
        for e in errors:
            annotate("error", e)
        summary("\n\n".join(errors))
        return 1

    new = latest is None or key > tags[latest]
    event, base = os.environ.get("GITHUB_EVENT_NAME", ""), os.environ.get("GITHUB_BASE_REF", "")
    raise_hint = "To release, raise the version in Cargo.toml and run cargo check so Cargo.lock follows."
    if event == "pull_request" and base == "main":
        message = f"Merging this releases {tag}." if new else f"Merging this won't release: {tag} is already released. {raise_hint}"
    elif event == "pull_request":
        message = (f"{tag} isn't released yet: it will be when {base} next merges into main." if new
                   else f"{tag} is the latest release, so merging this into {base} doesn't change what main would release.")
    else:
        message = f"{tag} isn't released yet, so a merge to main releases it." if new else f"{tag} is already released: this builds but releases nothing."
    annotate("notice", message)
    summary(message)

    if mode == "release":
        outputs = {
            "version": version,
            "tag": tag,
            "previous": latest or "",
            "release": str(new).lower(),
            "prerelease": str(key[1] == 0).lower(),
        }
        lines = "".join(f"{k}={v}\n" for k, v in outputs.items())
        path = os.environ.get("GITHUB_OUTPUT")
        if path:
            with open(path, "a", encoding="utf-8") as f:
                f.write(lines)
        else:
            sys.stdout.write(lines)
    return 0


if __name__ == "__main__":
    sys.exit(main())
