# ⚙️ Command-line options

```sh
revue [dir] [--port N] [--no-open] [--no-git] [--notes memory|file] [--idle-timeout MIN]
revue mcp [--lifetime session|detached] [--notes memory|file] [--idle-timeout MIN]
```

| Option | Environment variable | Default | What it does |
|---|---|---|---|
| `dir` | | the current directory | The repo (any folder inside it) or [plain folder](plain-folders.md) to open |
| `--port N` | | 4777, or a free port if that one is taken | The port to listen on. An explicit port that's in use is an error, and `0` picks any free port. |
| `--no-open` | | | Don't open the browser |
| `--no-git` | | | Open `dir` as a plain folder, even inside a repo |
| `--notes memory\|file` | `REVUE_NOTES` | `memory` | Where agent notes are kept (see [note storage](mcp.md#-how-it-behaves)) |
| `--idle-timeout MIN` | `REVUE_IDLE_TIMEOUT` | never (60 for detached servers) | Exit after MIN minutes with no open tabs and no requests. On `revue mcp`, it sets the timeout of detached servers only; servers that live with the session stop when it ends. |
| `--lifetime session\|detached` | `REVUE_LIFETIME` | `session` | `revue mcp` only: whether the servers it starts stop with the agent session or keep running |
| `-V`, `--version` | | | Print the version and the commit it was built from |
| `-h`, `--help` | | | Print the usage |
