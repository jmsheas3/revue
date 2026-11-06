# 📁 Plain folders

A folder that isn't inside a git repo (or any folder, with `--no-git`) opens as a document viewer for its files:

```sh
revue ~/notes               # not in a repo
revue --no-git ~/code/proj  # a repo, opened as a plain folder
```

There are no modes, branches, commits or diffs, and the header shows the folder's name with a "folder" tag.

## 🌳 The file tree

The tree lists every file, hidden ones and anything under `.git/` included, up to 50,000 files. Past that, it keeps the files nearest the top of the folder (`.git/` last), and a last row says only the first 50,000 are shown.

## 📖 Opening files

- The page opens on the folder's README: `README.md`, then `README`, then any `readme.*`, then `index.md`. It's rendered when it's markdown (`.md` or `.markdown`). With no README, the page asks you to pick a file.
- Opening a folder in the tree (a click, Enter or Space) shows that folder's README too; the arrow keys only open and close folders.
- Files open in the file view (images as pictures), markdown in the preview, and SVG drawn in its Image view.
- Live updates, agent notes, line targets and the narrow-window drawer work as in a repo.

The keys are listed under [Keyboard shortcuts](keyboard-shortcuts.md#in-a-plain-folder).

## ✋ Nothing is written into the folder

With [`--notes file`](options.md), notes go to revue's data directory instead of `.git/revue/notes.json`:

| Platform | Notes directory |
|---|---|
| Linux | `$XDG_DATA_HOME/revue/notes/`, or `~/.local/share/revue/notes/` |
| macOS | `~/Library/Application Support/revue/notes/` |
| Windows | `%LOCALAPPDATA%\revue\notes\` |

A repo's server and a `--no-git` server on the same folder are separate servers.
