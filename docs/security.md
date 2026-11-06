# 🔒 What revue changes, and security

revue is a local server that reads your code, so here is exactly what it touches and who can reach it. To report a vulnerability, see [SECURITY.md](../SECURITY.md).

## 💾 What revue changes

- **It reads your repo; it changes your files only when you check out a branch.** **Check out** runs `git switch` after you confirm. It refuses while tracked files have uncommitted changes, and a remote-only branch gets a local tracking branch.
- **Two other writes stay inside `.git/`.**
  - The Changes view refreshes the index's cached file timestamps (`git update-index --refresh`, as `git status` does) so diffs stay fast.
  - With `--notes file`, agent notes are saved in `.git/revue/notes.json`.
- **In a [plain folder](plain-folders.md), nothing is written into the folder at all.**

## 🏠 Only your machine can reach it

- The server listens on 127.0.0.1 and answers only requests addressed to `127.0.0.1:<port>` or `localhost:<port>`, so a website can't read your code through it with DNS rebinding.
- Nor can a page on another site, or one served by another server on your machine, use it. Before doing any work, the API refuses every request the browser marks as sent by another page (`Sec-Fetch-Site`), and every answer is marked for revue's own page only (`Cross-Origin-Resource-Policy`).
- Requests that check out a branch, move your tab, or add or clear notes must be POSTs carrying a header that a cross-site request can't send.
- Other users logged in to the same machine can reach it too, as they can any server on 127.0.0.1, so on a shared machine they could read what revue shows you.

## 🔍 Repo content is untrusted

- Paths and refs from the page are checked so nothing outside the repo is read, and symlinks that lead out of it aren't followed.
- The markdown preview is sanitized and loads nothing but the repo's own images, through revue, and the page runs under a strict Content Security Policy.
- Images are served only as images, and sandboxed when opened in a tab of their own. An SVG, which can hold script, is drawn only as an image, where browsers run none of it, and opening one on its own downloads it.
- Images that could freeze or crash the tab are refused rather than handed to your browser ([the limits](features.md#-images)). A small file's header can claim a huge number of pixels, and an SVG's `<use>` draws a copy of a group that can hold more of them, so a file of a kilobyte can draw millions of elements. Images an SVG embeds as `data:` URLs, which an image does load, count toward the same limits.
- An SVG within every limit can still be slow to draw: filters (a large blur, say), patterns and markers take time without adding elements, and nothing measures that short of drawing it. Such a file can hold its tab for a while; closing the tab recovers.
