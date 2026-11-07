# 🤖 Use revue from a coding agent (MCP)

`revue mcp` runs an [MCP](https://modelcontextprotocol.io) server over stdio, built into the same binary. It lets an agent open revue, point you at specific lines, and leave notes on them.

## 🔌 Set it up

For Claude Code:

```sh
claude mcp add revue -- revue mcp            # this project only (revue on PATH, or give its full path)
claude mcp add -s user revue -- revue mcp    # every project
```

Other MCP clients take the same command. In a client that's configured with JSON, that's usually:

```json
{ "mcpServers": { "revue": { "command": "revue", "args": ["mcp"] } } }
```

To pass [options](options.md) with Claude Code, put them after `mcp`:

```sh
claude mcp add revue -- revue mcp --notes file
```

## 💬 Try it

Ask things like:

- *"Show me the code changes."*
- *"Show me the lines you just changed."*
- *"Leave notes in revue on anything I should double-check."*

## 🧰 Tools

| Tool | What it does |
|---|---|
| `revue_open` | Open the overview of all changes |
| `revue_show` | Show a file, optionally scrolled to a line or range (`line`, `endLine`), in the diff or whole-file view, or (`view: preview`, without a line) a markdown file rendered or an SVG drawn |
| `revue_annotate` | Leave a note on a line or range, and show it |
| `revue_annotations` | List or clear notes |
| `revue_changes` | Changed files with status and line counts, as text |
| `revue_diff` | Unified diff of one file or all changed files, as text |
| `revue_log` | Recent commits |
| `revue_definitions` | Where an identifier is defined (the same search as Ctrl+click) |
| `revue_status` | Running revue servers and their open tabs |
| `revue_stop` | Stop a server the MCP started |

### Arguments

- **The view.** Every view-dependent tool takes the same `mode` (`working`, `commit` or `branch`) and `ref`/`base`/`head` arguments as the page.
- **The repo.** A `repo` argument picks the repository (any folder inside it). Without it, tools use the folder `revue mcp` was started in. Claude Code starts it in your project, but a client that starts it somewhere else, such as your home folder, should pass `repo`, or that folder opens as a plain folder.
- **New tabs.** `revue_open`, `revue_show` and `revue_annotate` take `newTab: true` to open a new tab even when one is open.
- **Plain folders.** A folder outside any repo, or any folder with `git: false`, opens as a [plain folder](plain-folders.md): `revue_open`, `revue_show`, the notes tools and `revue_definitions` work on its files, and `revue_changes`, `revue_diff` and `revue_log` say it isn't a git repository. A repo's server and a `git: false` server on the same folder are separate servers.

## 🔧 How it behaves

- **One server per repo.** The MCP server reuses a revue server that's already running for the repo, including one you started yourself with `revue`. Otherwise it starts one, on any free port rather than 4777. Running servers register themselves in `$XDG_RUNTIME_DIR/revue/` (or a per-user temp directory) so they can be found.
- **Your open tab follows.** If a revue tab is open, it moves to what the agent wants to show you, and Back returns to where you were. With several tabs open, only the one you used last moves. If no tab is open, a new one opens. A background tab updates but can't bring itself to the front.
- **Server lifetime.** By default, a server the MCP starts stops when the agent session ends. With `revue mcp --lifetime detached` (or `persist: true` on a tool call), it keeps running and exits after 60 minutes with no open tabs and no requests. `revue_stop` stops it sooner. Servers you started by hand are never stopped by the agent.
- **Note storage.** Notes are kept in memory and lost when the server stops. With `--notes file` (on `revue mcp` or `revue`), they're saved per worktree in `.git/revue/notes.json` (a linked worktree's in its own git directory), which git never commits; for a plain folder, they go to [revue's data directory](plain-folders.md#-nothing-is-written-into-the-folder). On `revue mcp` it applies to the servers it starts: a server you started yourself keeps its own setting.
