# ✨ Features

Everything revue shows you, and how it decides what to show. For the keys, see [Keyboard shortcuts](keyboard-shortcuts.md).

- [Coding agents](#-coding-agents)
- [Live updates](#-live-updates)
- [Three modes](#-three-modes)
- [The whole file tree](#-the-whole-file-tree)
- [Any branch, without checking it out](#-any-branch-without-checking-it-out)
- [Diffs](#-diffs)
- [Go to definition](#-go-to-definition)
- [Images](#-images)
- [Markdown preview](#-markdown-preview)
- [Plain folders](#-plain-folders)
- [Stays local](#-stays-local)
- [Themes and narrow windows](#-themes-and-narrow-windows)

## 🤖 Coding agents

Over [MCP](mcp.md), an agent can open its changes in your browser, scroll you to the lines it means and leave notes on them. [How your tab follows it](mcp.md#-how-it-behaves).

Notes appear under the lines they're about, in diffs and in the file view. They're marked *outdated* when those lines change afterwards.

- Each note has a **Dismiss** button.
- The toolbar counts them, steps through them, opens those left in other views (another commit, say) and clears this view's.
- The file tree counts them per file.

## ⚡ Live updates

A file watcher pushes changes to the page as you edit, commit or switch branches. The **Live** dot in the header says whether updates are connected; hover over it for the server's version, port, uptime and open tabs. If the file watcher stops, that card says so, and `r` refreshes by hand.

## 🔀 Three modes

| Mode | Key | What it shows |
|---|---|---|
| **Changes** | `1` | Uncommitted work compared with `HEAD`: staged, unstaged and untracked changes together, one diff per file. |
| **History** | `2` | The branch's commits (or every branch's), and each one's diff; a merge's against its first parent. |
| **Branch** | `3` | A branch compared with its base from their merge-base, like a pull request. |

Branch mode's base is the first of `main`, `master`, `origin/main`, `origin/master` and `develop` that exists. **Compared with** picks another.

## 🌳 The whole file tree

Every file, with changed ones marked:

| Mark | Meaning |
|---|---|
| `M` | modified |
| `A` | added |
| `D` | deleted |
| `R` | renamed |
| `T` | type changed, such as a file turned into a symlink |
| `U` | untracked |

Folders that contain changes get a dot. **Changed only** hides the rest.

## 🌿 Any branch, without checking it out

The branch picker in the header shows any local or remote branch. History and Branch follow it; picking one in Changes switches to Branch, since uncommitted work exists only on the checked-out branch.

While the picker shows another branch, a **Check out** button runs a real `git switch` after you confirm. See [what revue changes](security.md#-what-revue-changes).

## 🆚 Diffs

- Split or unified, with syntax highlighting and the changed part of each line marked.
- Context can be 3, 10 or 50 lines, or the whole file.
- The **File** tab shows the whole file with its changed lines marked in the gutter, before or after the change.
- Split needs about 60 characters a side: diffs are unified when the pane is narrower than that, and while line wrapping is off (`w`).

## 🧭 Go to definition

Ctrl+click (⌘+click on macOS) a function, class or variable in a diff or file to jump to where it's defined, at the revision you're viewing. Browser Back returns you.

When several definitions share the name, revue follows the file's imports (JS/TS, Python, Go packages, Rust `use` paths and workspace crates) and the call site (`this.`/`self.`, `Type::`, the enclosing class or `impl`, local shadowing) to pick one. It opens a picker only when that still leaves a tie.

It's a `git grep` and regex heuristic for JS/TS, Python, Go, Rust, C-family languages and shell, with a looser fallback for other languages, so it's quick but not compiler-accurate.

## 📷 Images

PNG, JPEG, GIF, WebP and ICO files are shown as pictures: a changed image before and after, side by side, in its diff and in the overview, and the File tab shows either side. SVG files keep their text views and get an **Image** tab that draws them (an unchanged one opens there). revue tells an image by its contents, not its name (an SVG needs its `.svg` name as well).

revue doesn't draw these, and says why instead:

- an image over 20 MB or 50 million pixels;
- an SVG over 2 MB, one that would draw over 50,000 elements (counting the copies its `<use>`s make), or one that embeds images (as `data:` URLs) that revue can't check or that come to over 50 million pixels in all;
- an SVG that isn't well-formed XML, which browsers don't draw at all. Its Image tab says what is wrong and on which line, such as `</h> doesn't close <g> (line 3)` or an `xlink:href` without its `xmlns:xlink`. Only what both Chromium and Firefox refuse to draw counts as broken.

Why these limits exist is under [Security](security.md#-repo-content-is-untrusted).

## 📝 Markdown preview

- `.md` and `.markdown` files have a **Preview** view (GitHub-flavoured, with `marked`), and an unchanged one opens there.
- The output is sanitized with DOMPurify.
- The repo's own images are shown, from the version you're viewing. `<picture>` sources and GitHub's `#gh-dark-mode-only` / `#gh-light-mode-only` images follow revue's theme.
- An image from anywhere else is never loaded; a remote one becomes a link to it instead.
- Relative links open the file in revue, and outside links open in a new tab.

## 📁 Plain folders

Point revue at a folder that isn't in a git repo (or pass `--no-git`) and it opens as a document viewer for that folder's files: its README rendered, markdown previewed and images drawn, with no modes or diffs. See [Plain folders](plain-folders.md).

## 🔒 Stays local

revue listens on 127.0.0.1 only, and changes your files only when you check out a branch. [Security](security.md) covers exactly what it writes, who can reach it, and how it treats repo content.

## 🌗 Themes and narrow windows

**Auto** follows your system, or pick **Light** or **Dark** in the header. Narrow windows (760 px or less) get a one-row header and the sidebar as a drawer (☰).
