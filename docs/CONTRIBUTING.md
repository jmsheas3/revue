# 🤝 Contributing to revue

Bug reports and ideas are welcome as [issues](https://github.com/jmsheas3/revue/issues). To report a vulnerability, follow [SECURITY.md](../SECURITY.md) instead.

## 🗺️ How it's built

- **Server:** `src/`. `server.rs` is the HTTP API, `image.rs` decides what counts as an image, `definitions.rs` does go to definition, and `mcp.rs` is the MCP server.
- **UI:** `public/`. It's plain JavaScript with no build step, embedded in the binary at build time, so a change there needs a rebuild.
- **Git access:** plain reads (refs, trees, blobs, history) happen in process through [gitoxide](https://github.com/GitoxideLabs/gitoxide). Diffs and grep come from the git CLI, so they match git exactly.

## 🦀 Build and test

You need Rust 1.88 or later, and Node only for the browser tests.

```sh
cargo run -- --no-open    # run against this repo without opening a browser
cargo test                # builds the binary and runs the API, live-update and go-to-definition tests against it
cd ui-tests
npm ci && npm run install-browsers   # once
npm test                  # browser tests (Playwright, Chromium and Firefox)
npm run screenshots       # remake the README's demo GIF and the social preview card (needs ffmpeg)
```

`npm run screenshots` writes `docs/demo-*.gif` (the README's demo) and `docs/social-preview.png`, the card GitHub shows when the repo is linked elsewhere. GitHub doesn't read that card from the repo: upload it again in **Settings → General → Social preview** after it changes.

To install your build: `cargo install --locked --path .` puts `revue` in `~/.cargo/bin`, and `cargo uninstall revue` removes it. `--locked` builds with the dependency versions revue is tested with (`Cargo.lock`); without it, cargo picks the newest ones, which may need a newer Rust.

## 📚 Docs

The README stays short. Details live in `docs/`, which is packed into every release archive next to the README, so keep links between them relative. The README's install commands are tested: [`install-check.yml`](../.github/workflows/install-check.yml) runs the code block after each `<!-- install-check: … -->` comment on fresh Linux, macOS and Windows machines against the latest release, after every release, every Monday and on pull requests that change the README. Keep those comments above their blocks when you edit them. When you change a shortcut, update the `?` panel in `public/index.html`, the README's Quick start table and `docs/keyboard-shortcuts.md`.

## 📥 Pull requests

Work goes into `develop` through pull requests, and `develop` goes into `main`. Every pull request into either runs the checks in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml):

- `cargo test` on Linux (x86_64 and ARM64) and macOS
- the browser tests
- clippy for the Linux, macOS and Windows code
- a build with the oldest Rust revue supports
- `cargo audit`
- a check that every crate's license is one [`.github/licenses/about.toml`](../.github/licenses/about.toml) accepts
- a Windows build that has to start and serve a repo
- a check of the version in `Cargo.toml`

The one check both branches require, **CI passed**, passes only when all of those did. Only `develop` and `hotfix/…` branches can merge into `main`. A pull request also needs an approval from a code owner ([`.github/CODEOWNERS`](../.github/CODEOWNERS)), which a repository admin can bypass; the checks can't be bypassed. The full `cargo test` also runs on Windows, but isn't required to pass there yet.

## 🚢 Releases

1. Raise `version` in `Cargo.toml`, run `cargo check` so `Cargo.lock` follows, and merge that into `develop`.
2. Open a pull request from `develop` into `main`, and merge it with **Create a merge commit**.

The merge to `main` runs every check again on the merged commit, builds the binaries, signs and notarizes the macOS ones, tests the installer on an Apple Silicon and an Intel Mac, and publishes everything as the release `v<version>`, which tags that commit. The files' names carry no version (`revue-linux-x86_64.tar.gz`, `revue-macos-universal.pkg`, …), so the README's `releases/latest/download/` links always fetch the newest release.

A release can't be changed afterwards, and its version can't be used again. A merge to `main` that doesn't raise the version builds the binaries (kept with the workflow run for 90 days) and releases nothing. `cargo audit` also runs every Monday on both branches, and Dependabot updates the workflows' Actions once a month.
