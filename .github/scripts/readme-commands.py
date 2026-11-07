#!/usr/bin/env python3
"""Prints the commands of one of the README's install blocks, so install-check.yml runs exactly what the README says.

A block is the fenced code block right after an `<!-- install-check: NAME -->` comment in README.md (GitHub doesn't
show the comment). With --os, a line whose comment names another OS (`# Linux: …`, `# macOS: …`, `# Windows: …`) is
left out, so the checksum block's lines run only where they belong. Exits 1 when the block is missing, so a README
edit that drops a marker fails the check instead of quietly testing nothing.

Usage: readme-commands.py NAME [--os linux|macos|windows]
"""
import argparse
import re
import sys
from pathlib import Path

README = Path(__file__).resolve().parents[2] / "README.md"
OSES = {"linux": "Linux", "macos": "macOS", "windows": "Windows"}


def block(text, name):
    """The lines of the fenced block after NAME's marker, or None."""
    m = re.search(rf"<!-- install-check: {re.escape(name)} -->\n```[^\n]*\n(.*?)\n```", text, re.S)
    return m.group(1).splitlines() if m else None


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("name")
    parser.add_argument("--os", choices=sorted(OSES))
    args = parser.parse_args()
    lines = block(README.read_text(encoding="utf-8"), args.name)
    if not lines:
        sys.exit(f"README.md has no install-check block named {args.name!r}")
    if args.os:
        others = "|".join(v for k, v in OSES.items() if k != args.os)
        lines = [line for line in lines if not re.search(rf"#\s*({others}):", line)]
    print("\n".join(lines))


if __name__ == "__main__":
    main()
