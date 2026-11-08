<h1 align="center">👀 revue</h1>

<p align="center">
  <b>Let your coding agent show you what it changed.</b><br>
  A git change viewer that runs in your browser: your agent opens its changes for you, scrolls you to the lines that
  matter and leaves notes on them. Or read over your own work before you commit. Live, next to the full file tree.
</p>

<p align="center">
  <a href="https://github.com/jmsheas3/revue/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/jmsheas3/revue?logo=github&label=release"></a>
  <a href="https://github.com/jmsheas3/revue/actions/workflows/release.yml?query=branch%3Amain"><img alt="Release build status" src="https://github.com/jmsheas3/revue/actions/workflows/release.yml/badge.svg?branch=main&event=push"></a>
  <a href="docs/mcp.md"><img alt="MCP server built in" src="https://img.shields.io/badge/MCP-built%20in-8A2BE2"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/jmsheas3/revue"></a>
</p>

<p align="center">
  <a href="#-install">📦 Install</a> ·
  <a href="#-quick-start">🚀 Quick start</a> ·
  <a href="#-use-it-with-coding-agents">🤖 Agents</a> ·
  <a href="#-documentation">📚 Docs</a>
</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/demo-dark.gif">
  <img alt="A 17-second loop in revue: the overview shows five files a coding agent changed; the agent opens src/pricing.ts with its coupon change highlighted, and leaves a note on the tax line asking for a check; then the user edits that line, the diff updates live and the note is marked outdated." src="docs/demo-light.gif">
</picture>

## ✨ Highlights

- 🤖 **Made for coding agents**: over MCP, an agent opens its changes for you, points you at lines and leaves notes on them.
- ⚡ **Live**: the page updates as you edit, commit or switch branches.
- 🔀 **Three modes**: **Changes** (your uncommitted work), **History** (any commit) and **Branch** (a branch against its base, like a pull request).
- 🌳 **The whole file tree** with changes marked, for any local or remote branch, without checking it out.
- 🆚 **Split or unified diffs** with syntax highlighting and the changed part of each line marked.
- 🧭 **Go to definition**: Ctrl+click (⌘+click) a name to jump to where it's defined.
- 📷 **Images and markdown**: changed images before and after, side by side, and a sanitized markdown preview.
- 📁 **Plain folders**: point it at any folder to browse its files and docs.
- 🔒 **Stays local**: it listens on 127.0.0.1 only, and changes your files only when you check out a branch ([security](docs/security.md)).

👉 [See every feature](docs/features.md)

## 📦 Install

Download, unpack, run. revue is a single binary that **needs git** and a **browser from 2024 or later** (Chrome or Edge 123, Firefox 121, Safari 17.5; it's tested in Chromium and Firefox), and no Rust, Node or other runtime.

| Platform | Download | Notes |
|---|---|---|
| 🍎 **macOS** (Apple Silicon and Intel) | [`.pkg` installer](https://github.com/jmsheas3/revue/releases/latest/download/revue-macos-universal.pkg) · [`.tar.gz`](https://github.com/jmsheas3/revue/releases/latest/download/revue-macos-universal.tar.gz) | Signed and notarized by Apple |
| 🐧 **Linux** x86_64 | [`revue-linux-x86_64.tar.gz`](https://github.com/jmsheas3/revue/releases/latest/download/revue-linux-x86_64.tar.gz) | Static, runs on any distribution |
| 🐧 **Linux** ARM64 | [`revue-linux-arm64.tar.gz`](https://github.com/jmsheas3/revue/releases/latest/download/revue-linux-arm64.tar.gz) | Static, runs on any distribution |
| 🪟 **Windows** x86_64 | [`revue-windows-x86_64.zip`](https://github.com/jmsheas3/revue/releases/latest/download/revue-windows-x86_64.zip) | Experimental |

<details>
<summary>🍎 <b>macOS</b> from the terminal</summary>

<!-- install-check: macos -->
```sh
curl -fsSLO https://github.com/jmsheas3/revue/releases/latest/download/revue-macos-universal.pkg
sudo installer -pkg revue-macos-universal.pkg -target /
```

This installs `revue` in `/usr/local/bin`, as double-clicking the `.pkg` does. A newer `.pkg` upgrades it in place. To uninstall:

<!-- install-check: macos-uninstall -->
```sh
sudo rm /usr/local/bin/revue && sudo pkgutil --forget io.github.jmsheas3.revue
```

The `.tar.gz` holds the same signed binary, to put anywhere on your `PATH` without an installer.
</details>

<details>
<summary>🐧 <b>Linux</b> from the terminal</summary>

<!-- install-check: linux -->
```sh
curl -fsSL https://github.com/jmsheas3/revue/releases/latest/download/revue-linux-x86_64.tar.gz | tar -xz
sudo install revue-linux-x86_64/revue /usr/local/bin/
```

On ARM64 (`uname -m` says `aarch64`), use `revue-linux-arm64` in both lines. Without `sudo`, `install -D revue-linux-x86_64/revue ~/.local/bin/revue` puts it in your home folder instead; make sure `~/.local/bin` is on your `PATH`.
</details>

<details>
<summary>🪟 <b>Windows</b> from PowerShell</summary>

<!-- install-check: windows -->
```powershell
Invoke-WebRequest https://github.com/jmsheas3/revue/releases/latest/download/revue-windows-x86_64.zip -OutFile revue-windows-x86_64.zip
Expand-Archive revue-windows-x86_64.zip -DestinationPath . -Force
.\revue-windows-x86_64\revue.exe --version
```

Move `revue.exe` to a folder on your `PATH` to run it as `revue`. To update, stop any running revue first: Windows can't replace a `revue.exe` that is running.
</details>

<details>
<summary>🔐 <b>Verify a download</b></summary>

Every release lists each file's SHA-256 in `SHA256SUMS`:

<!-- install-check: verify -->
```sh
curl -fsSLO https://github.com/jmsheas3/revue/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing    # Linux: checks the files you downloaded
shasum -a 256 revue-macos-universal.pkg     # macOS: prints the hash to compare with SHA256SUMS
```

<!-- install-check: verify-windows -->
```powershell
(Get-FileHash revue-windows-x86_64.zip).Hash   # Windows: compare with SHA256SUMS (case doesn't matter)
```
</details>

🔄 **Updating:** download again and replace the binary. A revue that is still running shows a **Server out of date** badge until you restart it.

### 🦀 From source

With Rust 1.88 or later:

```sh
cargo install --locked --git https://github.com/jmsheas3/revue
```

That puts `revue` in `~/.cargo/bin`, and `cargo uninstall revue` removes it. ⚠️ `cargo install revue` installs an unrelated crate that has the same name.

## 🚀 Quick start

```sh
revue                 # in a git repo: starts a server and opens your browser
revue ~/code/proj     # point it at a repo (any folder inside one works)
revue ~/notes         # a folder that isn't in a repo opens as a plain folder
```

The page opens on your uncommitted changes: every changed file's diff, one after another, next to the file tree.

| Press | To |
|---|---|
| `j` `k` | step to the next / previous changed file |
| `Esc` | go back to all of them |
| `1` `2` `3` | switch between Changes, History and Branch |
| Ctrl+click (⌘+click) | go to a definition |
| `?` | list every shortcut |

revue prints the page's address and listens on port 4777, or on a free port if that one is taken; Ctrl+C stops it. See every [option](docs/options.md) and [shortcut](docs/keyboard-shortcuts.md), and [plain folders](docs/plain-folders.md) for folders outside a repo.

## 🤖 Use it with coding agents

`revue mcp` is an [MCP](https://modelcontextprotocol.io) server, built into the same binary. With it, an agent can open its changes in your browser, scroll you to the lines it means and leave notes on them. For Claude Code:

```sh
claude mcp add -s user revue -- revue mcp
```

Using Antigravity CLI, Cursor CLI or GitHub Copilot CLI? [Here's how to add it](docs/mcp.md#-set-it-up). Other MCP clients take the same command, usually as `{ "mcpServers": { "revue": { "command": "revue", "args": ["mcp"] } } }`.

Then ask things like 💬 *"show me the code changes"*, *"show me the lines you just changed"* or *"leave notes in revue on anything I should double-check"*.

👉 [Tools, arguments and how agents share servers](docs/mcp.md)

## 📚 Documentation

- ✨ [Features](docs/features.md): coding agents, live updates, modes, the file tree, diffs, go to definition, images, markdown
- ⌨️ [Keyboard shortcuts](docs/keyboard-shortcuts.md): every key, in a repo and in a plain folder
- 📁 [Plain folders](docs/plain-folders.md): a document viewer for any folder
- 🤖 [Coding agents (MCP)](docs/mcp.md): setup, tools and server lifetimes
- ⚙️ [Command-line options](docs/options.md): every flag and environment variable
- 🔒 [Security](docs/security.md): what revue writes, who can reach it, and how it treats repo content
- 🤝 [Contributing](docs/CONTRIBUTING.md): building, testing, pull requests and releases

## 🤝 Contributing

Bug reports and ideas are welcome as [issues](https://github.com/jmsheas3/revue/issues). Pull requests go into `develop`; [CONTRIBUTING](docs/CONTRIBUTING.md) explains how to build, test and release. To report a vulnerability, please follow [SECURITY.md](SECURITY.md) instead of opening an issue.

## 📄 License

MIT; see [LICENSE](LICENSE). The UI includes highlight.js (BSD-3-Clause), marked (MIT) and DOMPurify (Apache-2.0 or MPL-2.0), unmodified in `public/vendor/`; their license texts are in [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES). The binaries are also built from Rust crates under their own licenses (MIT, Apache-2.0 and a few others); each release lists them, with their license texts, in `THIRD_PARTY_LICENSES_RUST`.
