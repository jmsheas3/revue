// The MCP server's tools over stdio JSON-RPC, against an isolated discovery directory. revue_open and revue_show run
// only with a tab connected for them to move: with none they open a browser.
mod common;
use common::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    _runtime: TempDir,
}

impl Mcp {
    fn start() -> Mcp {
        let runtime = TempDir::new("mcp-tools");
        let mut child = isolate(&mut Command::new(env!("CARGO_BIN_EXE_revue")))
            .arg("mcp")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stdin = child.stdin.take();
        let mut m = Mcp { child, stdin, stdout, next_id: 0, _runtime: runtime };
        let init = m.call("initialize", json!({ "protocolVersion": "2025-06-18", "clientInfo": { "name": "test-client" } }));
        assert_eq!(init["result"]["serverInfo"]["name"], "revue");
        m
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin.as_mut().unwrap(), "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the MCP server closed stdout");
            let m: Value = serde_json::from_str(&line).unwrap();
            if m["id"] == id {
                return m;
            }
        }
    }

    // The text of a tool result, and whether it was an error.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.call("tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        (r["content"][0]["text"].as_str().unwrap_or("").to_string(), r["isError"] == true)
    }

    fn ok(&mut self, name: &str, args: Value) -> String {
        let (text, err) = self.tool(name, args);
        assert!(!err, "{name} failed: {text}");
        text
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.take()); // the session ends when stdin closes, and its servers with it
        if !wait_until(5000, || matches!(self.child.try_wait(), Ok(Some(_)))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn repo() -> TempDir {
    let dir = TempDir::new("mcp-repo");
    let root = dir.path();
    init(root);
    write(root, "lib.js", "export function helper() {}\n");
    write(root, "main.js", "import { helper } from './lib';\nhelper();\n");
    commit_all(root, "first commit");
    write(root, "lib.js", "export function helper() { return 1; }\n");
    dir
}

#[test]
fn tools_list_describes_every_tool() {
    let mut m = Mcp::start();
    let tools = m.call("tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "revue_open", "revue_show", "revue_annotate", "revue_annotations", "revue_changes", "revue_diff", "revue_log",
            "revue_definitions", "revue_status", "revue_stop"
        ]
    );
    for t in tools.as_array().unwrap() {
        assert!(!t["description"].as_str().unwrap().is_empty(), "{}", t["name"]);
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{}", t["name"]);
        // Every tool that may start a server takes persist, and every one that names a folder takes git; arguments
        // outside the schema are refused.
        let props = &t["inputSchema"]["properties"];
        assert_eq!(props["persist"].is_object(), !["revue_status", "revue_stop"].contains(&t["name"].as_str().unwrap()), "{}", t["name"]);
        assert!(props["git"].is_object(), "{}", t["name"]);
    }
    let required = |name: &str| tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["inputSchema"]["required"].clone();
    assert_eq!(required("revue_show"), json!(["path"]));
    assert_eq!(required("revue_annotate"), json!(["path", "line", "text"]));
    assert_eq!(required("revue_definitions"), json!(["name"]));
    let show = tools.as_array().unwrap().iter().find(|t| t["name"] == "revue_show").unwrap();
    assert_eq!(show["inputSchema"]["properties"]["view"]["enum"], json!(["file", "diff", "preview"]));
    // Unknown tools and methods are protocol errors, not tool errors.
    assert_eq!(m.call("tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
    assert_eq!(m.call("bogus", json!({}))["error"]["code"], -32601);
    assert_eq!(m.call("ping", json!({}))["result"], json!({}));
}

#[test]
fn the_read_tools_describe_the_repo() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();

    let changes = m.ok("revue_changes", json!({ "repo": r }));
    assert!(changes.starts_with("1 file(s) changed, +1 -1 (working tree vs HEAD)"), "{changes}");
    assert!(changes.contains("M  lib.js  +1 -1"), "{changes}");

    let diff = m.ok("revue_diff", json!({ "repo": r, "path": "lib.js", "context": "0" }));
    assert!(diff.contains("-export function helper() {}") && diff.contains("+export function helper() { return 1; }"), "{diff}");
    let (text, err) = m.tool("revue_diff", json!({ "repo": r, "context": "lots" }));
    assert!(err && text.contains("context must be a number"), "{text}");
    let (text, err) = m.tool("revue_diff", json!({ "repo": r, "path": "missing.js" }));
    assert!(err && text.contains("missing.js is not in this view"), "{text}");
    assert!(m.ok("revue_diff", json!({ "repo": r, "path": "main.js" })).contains("main.js has no changes"));

    let log = m.ok("revue_log", json!({ "repo": r }));
    let short = rev(dir.path(), "HEAD")[..7].to_string();
    assert!(log.starts_with(&short) && log.contains("t  first commit  (HEAD -> main)"), "{log}");

    let defs = m.ok("revue_definitions", json!({ "repo": r, "name": "helper", "path": "main.js" }));
    assert!(defs.starts_with("* lib.js:1  function  export function helper()"), "{defs}");
    assert!(m.ok("revue_definitions", json!({ "repo": r, "name": "nothingLikeThis" })).contains("No definition found"));

    let status = m.ok("revue_status", json!({ "repo": r }));
    assert!(status.starts_with(r) && status.contains("session, notes in memory"), "{status}");

    // An untracked binary file is listed as binary, as a staged one is, and adds nothing to the totals.
    write(dir.path(), "logo.bin", [0x89u8, b'P', 0, 1]);
    // macOS's watcher can't see the events the OS still holds, so for a moment after an edit the answer remembered from
    // before it can still be served there: wait for the new file.
    let mut changes = String::new();
    assert!(
        wait_until(5000, || {
            changes = m.ok("revue_changes", json!({ "repo": r }));
            changes.starts_with("2 file(s) changed, +1 -1 (working tree vs HEAD)")
        }),
        "{changes}"
    );
    assert!(changes.contains("U  logo.bin  binary"), "{changes}");
}

#[test]
fn an_untracked_file_too_large_to_count_says_so() {
    // The server counts no lines of an untracked text file over 2 MB (add null, del 0), and an untracked binary that
    // big is still binary; neither may read as "+0 -0".
    let dir = repo();
    let root = dir.path();
    write(root, "huge.log", "x\n".repeat(3 << 19));
    write(root, "huge.bin", [0u8].repeat(3 << 20));
    let mut m = Mcp::start();
    let changes = m.ok("revue_changes", json!({ "repo": root.to_str().unwrap() }));
    assert!(changes.starts_with("3 file(s) changed, +1 -1 (working tree vs HEAD)"), "{changes}");
    assert!(changes.contains("\nU  huge.log  not counted (too large or unreadable)\n"), "{changes}");
    assert!(changes.contains("\nU  huge.bin  binary\n"), "{changes}");
    assert!(!changes.contains("+0 -0"), "{changes}");

    // Nor does it get a diff, which would be all of it: revue_diff says so, for the file and among all of them, rather
    // than print it or nothing. The binary one is still git's one line.
    let size = 3 << 20;
    let says = format!("(untracked file huge.log is too large to show: {size} bytes)\n");
    let diff = m.ok("revue_diff", json!({ "repo": root.to_str().unwrap(), "path": "huge.log" }));
    assert_eq!(diff, says);
    let all = m.ok("revue_diff", json!({ "repo": root.to_str().unwrap() }));
    assert!(all.contains(&says), "{all}");
    assert!(all.contains("Binary files /dev/null and b/huge.bin differ"), "{all}");
    assert!(all.contains("+export function helper() { return 1; }"), "{all}");
    assert!(!all.contains("+x\n") && !all.contains("truncated"), "{all}");
}

#[test]
fn an_untracked_nested_repo_is_listed_by_its_commit_or_as_having_none() {
    // git lists a repo inside the work tree as one untracked `dir/`: one line, the commit git would add for it, or,
    // with no commit yet, nothing git could add, which says so rather than "too large or unreadable".
    let dir = repo();
    let root = dir.path();
    for name in ["nest", "empty"] {
        std::fs::create_dir(root.join(name)).unwrap();
        init(&root.join(name));
    }
    write(&root.join("nest"), "x.txt", "x\n");
    commit_all(&root.join("nest"), "inner");
    let mut m = Mcp::start();
    let changes = m.ok("revue_changes", json!({ "repo": root.to_str().unwrap() }));
    assert!(changes.lines().any(|l| l == "U  nest/  +1 -0"), "{changes}");
    assert!(changes.lines().any(|l| l == "U  empty/  nested repo with no commits yet"), "{changes}");
}

#[test]
fn notes_are_added_listed_and_cleared() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    let added = m.ok("revue_annotate", json!({ "repo": r, "path": "lib.js", "line": 1, "text": "why return 1?", "show": false }));
    assert!(added.starts_with("Added note ") && added.contains("on lib.js:1 (working tree vs HEAD; stored in memory)"), "{added}");
    let listed = m.ok("revue_annotations", json!({ "repo": r }));
    assert!(listed.contains("1 note(s) in working tree vs HEAD") && listed.contains("lib.js:1 by test-client"), "{listed}");
    assert!(listed.contains("    why return 1?"), "{listed}");
    let cleared = m.ok("revue_annotations", json!({ "repo": r, "action": "clear", "all": true }));
    assert_eq!(cleared, "Removed 1 note(s) (all views).");
    assert!(m.ok("revue_annotations", json!({ "repo": r })).starts_with("No notes"));
}

#[test]
fn long_output_is_capped() {
    let dir = repo();
    let root = dir.path();
    let big: String = (0..5000).map(|i| format!("line number {i} of a long file\n")).collect();
    write(root, "big.txt", &big);
    let mut m = Mcp::start();
    let diff = m.ok("revue_diff", json!({ "repo": root.to_str().unwrap(), "path": "big.txt" }));
    assert!(diff.len() < 61_000, "{} characters", diff.len());
    assert!(diff.contains("\n\n[truncated: ") && diff.ends_with("Ask for one path at a time.]"), "{}", &diff[diff.len() - 200..]);
}

fn plain_folder() -> TempDir {
    let dir = TempDir::new("mcp-folder");
    write(dir.path(), "README.md", "# Folder\n");
    write(dir.path(), "docs/a.js", "function helper() {}\nhelper();\n");
    dir
}

// The port of the one server revue_status lists for `r`.
fn port_of(status: &str) -> u16 {
    let at = status.find("  port ").unwrap_or_else(|| panic!("no port in {status}")) + 7;
    status[at..].split(',').next().unwrap().parse().unwrap()
}

// A tab that follows navigation, for revue_show to move instead of opening a browser. The server counts it only after
// the stream's head is out, so this waits for that.
fn follower(port: u16) -> Events {
    let tab = Events::connect(port, "/api/events?nav=1");
    assert!(wait_until(5000, || request(port, "GET", "/api/server", &[], b"").json()["navTabs"] == 1));
    tab
}

#[test]
fn revue_show_previews_markdown_and_svg_files_only() {
    let dir = repo();
    let root = dir.path();
    write(root, "README.md", "# Title\n\nSome text.\n");
    write(root, "docs/Guide.MARKDOWN", "# Guide\n");
    write(root, "logo.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>\n");
    write(root, "src/lib.rs", "pub fn f() {}\n");
    let r = root.to_str().unwrap();
    let mut m = Mcp::start();
    m.ok("revue_changes", json!({ "repo": r })); // starts the server
    let port = port_of(&m.ok("revue_status", json!({ "repo": r })));
    let tab = follower(port);
    let url = |file: &str, rest: &str| format!("URL: http://127.0.0.1:{port}/#mode=working&file={}{rest}\n", enc(file));
    let navigated = || tab.named("navigate", 0).iter().map(|d| serde_json::from_str::<Value>(d).unwrap()).collect::<Vec<_>>();

    // A markdown file's rendered preview, which the tab is sent to.
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "README.md", "view": "preview" }));
    assert!(shown.contains(&url("README.md", "&view=preview")), "{shown}");
    assert!(shown.contains("Browser: moved the open revue tab"), "{shown}");
    assert!(wait_until(5000, || navigated().len() == 1));
    let v = &navigated()[0];
    assert_eq!((v["file"].as_str(), v["view"].as_str(), v["line"].clone()), (Some("README.md"), Some("preview"), Value::Null), "{v}");

    // An SVG drawn as an image, and markdown named in any case, as the page tells them.
    for path in ["logo.svg", "docs/Guide.MARKDOWN"] {
        let shown = m.ok("revue_show", json!({ "repo": r, "path": path, "view": "preview" }));
        assert!(shown.contains(&url(path, "&view=preview")), "{shown}");
    }

    // Any other file has no preview, and the preview has no lines: each is refused, rather than shown in another view
    // (the page would show the file or the diff instead).
    let (text, err) = m.tool("revue_show", json!({ "repo": r, "path": "src/lib.rs", "view": "preview" }));
    assert!(err, "{text}");
    assert_eq!(text, "view \"preview\" is only for markdown (.md, .markdown) and SVG (.svg) files; show src/lib.rs with view \"file\" or \"diff\"");
    for lines in [json!({ "line": 2 }), json!({ "line": 1, "endLine": 2 })] {
        let mut args = json!({ "repo": r, "path": "README.md", "view": "preview" });
        args.as_object_mut().unwrap().extend(lines.as_object().unwrap().clone());
        let (text, err) = m.tool("revue_show", args);
        assert!(err && text.starts_with("the preview has no line numbers; "), "{text}");
    }

    // None of those moved the tab: the next navigation it gets is this file view of line 2.
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "README.md", "line": 2 }));
    assert!(shown.contains(&url("README.md", "&view=file&line=2")), "{shown}");
    assert!(wait_until(5000, || navigated().last().is_some_and(|v| v["view"] == "file")));
    let views: Vec<Value> = navigated().iter().map(|v| json!([v["file"], v["view"], v["line"]])).collect();
    assert_eq!(
        views,
        [
            json!(["README.md", "preview", null]),
            json!(["logo.svg", "preview", null]),
            json!(["docs/Guide.MARKDOWN", "preview", null]),
            json!(["README.md", "file", 2])
        ]
    );
}

#[test]
fn enum_arguments_outside_their_schema_are_refused() {
    // A value the schema doesn't list, or one of the wrong type, would otherwise mean the default (view "bogus" showed
    // the diff) or reach the server. Each is refused before anything is shown, stored or started.
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    m.ok("revue_changes", json!({ "repo": r })); // starts the server
    let port = port_of(&m.ok("revue_status", json!({ "repo": r })));
    let tab = follower(port);
    let navigated = || tab.named("navigate", 0).iter().map(|d| serde_json::from_str::<Value>(d).unwrap()).collect::<Vec<_>>();

    const VIEW: &str = r#"view must be "file", "diff" or "preview""#;
    const SIDE: &str = r#"side must be "new" or "old""#;
    const MODE: &str = r#"mode must be "working", "commit" or "branch""#;
    for (tool, bad, want) in [
        ("revue_show", json!({ "path": "lib.js", "view": "bogus" }), format!(r#"{VIEW}, not "bogus""#)),
        ("revue_show", json!({ "path": "lib.js", "side": "sideways" }), format!(r#"{SIDE}, not "sideways""#)),
        ("revue_show", json!({ "path": "lib.js", "mode": "bogus" }), format!(r#"{MODE}, not "bogus""#)),
        // The wrong type: a view of 2 was read as no view at all, so the diff.
        ("revue_show", json!({ "path": "lib.js", "view": 2 }), "view must be a string, not 2".to_string()),
        ("revue_show", json!({ "path": "lib.js", "side": true }), "side must be a string, not true".to_string()),
        ("revue_show", json!({ "path": "lib.js", "mode": ["commit"] }), r#"mode must be a string, not ["commit"]"#.to_string()),
        ("revue_open", json!({ "mode": "Working" }), format!(r#"{MODE}, not "Working""#)),
        ("revue_annotate", json!({ "path": "lib.js", "line": 1, "text": "x", "side": "before" }), format!(r#"{SIDE}, not "before""#)),
        ("revue_annotations", json!({ "action": "delete" }), r#"action must be "list" or "clear", not "delete""#.to_string()),
        ("revue_definitions", json!({ "name": "helper", "side": 0 }), "side must be a string, not 0".to_string()),
    ] {
        let mut args = json!({ "repo": r });
        args.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        let (text, err) = m.tool(tool, args);
        assert!(err, "{tool} {bad}: {text}");
        assert_eq!(text, want, "{tool} {bad}");
    }
    assert!(m.ok("revue_annotations", json!({ "repo": r })).starts_with("No notes"), "a refused note was stored");

    // None of those moved the tab: the only navigation it gets is this one, with each enum given one of its values.
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "lib.js", "line": 1, "view": "file", "side": "old", "mode": "working" }));
    assert!(shown.contains("Browser: moved the open revue tab"), "{shown}");
    assert!(wait_until(5000, || navigated().iter().any(|v| v["view"] == "file" && v["side"] == "old")));
    let views: Vec<Value> = navigated().iter().map(|v| json!([v["file"], v["view"], v["side"], v["mode"]])).collect();
    assert_eq!(views, [json!(["lib.js", "file", "old", "working"])]);
    // Null and "" still mean the default, as they did.
    m.ok("revue_annotate", json!({ "repo": r, "path": "lib.js", "line": 1, "text": "kept", "side": "", "mode": null, "show": false }));
    let listed = m.ok("revue_annotations", json!({ "repo": r, "action": "list" }));
    assert!(listed.contains("1 note(s) in working tree vs HEAD") && listed.contains("    kept"), "{listed}");
    assert!(m.ok("revue_definitions", json!({ "repo": r, "name": "helper", "side": "new" })).starts_with("* lib.js:1"));
}

#[test]
fn arguments_of_the_wrong_type_or_name_are_refused() {
    // Booleans, numbers and strings of the wrong type quietly meant the default (git "false" opened the repo, show
    // "false" moved the browser, limit "50" listed 30), and a misspelled name was ignored. Each is refused before
    // anything is shown, stored or started.
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    m.ok("revue_changes", json!({ "repo": r })); // starts the repo's server
    let port = port_of(&m.ok("revue_status", json!({ "repo": r })));
    let tab = follower(port);

    let long = "x".repeat(5000);
    let show_takes = "path, line, endLine, view, side, repo, git, mode, ref, base, head, persist, newTab";
    for (tool, bad, want) in [
        ("revue_show", json!({ "path": "lib.js", "git": "false" }), r#"git must be true or false, not "false""#.to_string()),
        ("revue_open", json!({ "newTab": "true" }), r#"newTab must be true or false, not "true""#.to_string()),
        ("revue_open", json!({ "persist": 1 }), "persist must be true or false, not 1".to_string()),
        ("revue_annotate", json!({ "path": "lib.js", "line": 1, "text": "x", "show": "false" }), r#"show must be true or false, not "false""#.to_string()),
        ("revue_annotate", json!({ "path": "lib.js", "line": "1", "text": "x" }), r#"line must be an integer, not "1""#.to_string()),
        ("revue_annotate", json!({ "path": "lib.js", "line": 1.5, "text": "x" }), "line must be an integer, not 1.5".to_string()),
        ("revue_annotate", json!({ "path": "lib.js", "line": 0, "text": "x" }), "line must be at least 1, not 0".to_string()),
        ("revue_annotate", json!({ "path": "lib.js", "line": 1, "text": 5 }), "text must be a string, not 5".to_string()),
        ("revue_show", json!({ "path": "lib.js", "endLine": "" }), r#"endLine must be an integer, not """#.to_string()),
        ("revue_log", json!({ "limit": "50" }), r#"limit must be an integer, not "50""#.to_string()),
        ("revue_log", json!({ "limit": 501 }), "limit must be at most 500, not 501".to_string()),
        ("revue_log", json!({ "skip": -1 }), "skip must be at least 0, not -1".to_string()),
        ("revue_log", json!({ "all": "true" }), r#"all must be true or false, not "true""#.to_string()),
        ("revue_annotations", json!({ "all": 1 }), "all must be true or false, not 1".to_string()),
        ("revue_diff", json!({ "context": true }), "context must be a string or an integer, not true".to_string()),
        ("revue_diff", json!({ "context": 2.5 }), "context must be a string or an integer, not 2.5".to_string()),
        // A misspelled or unknown name, with the names the tool takes.
        ("revue_show", json!({ "path": "lib.js", "veiw": "file" }), format!(r#"unknown argument "veiw" (revue_show takes: {show_takes})"#)),
        ("revue_show", json!({ "path": "lib.js", "Side": "old" }), format!(r#"unknown argument "Side" (revue_show takes: {show_takes})"#)),
        ("revue_status", json!({ "port": 4777 }), r#"unknown argument "port" (revue_status takes: repo, git)"#.to_string()),
        // A huge value is cut short in the message.
        ("revue_show", json!({ "path": "lib.js", "view": long }), format!(r#"view must be "file", "diff" or "preview", not "{}…"#, "x".repeat(99))),
        // "" with clear would mean no id, so every note of the view.
        ("revue_annotations", json!({ "action": "clear", "id": "" }), r#"id must name a note, not ""; leave it out to clear every note of the view"#.to_string()),
    ] {
        let mut args = json!({ "repo": r });
        args.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        let (text, err) = m.tool(tool, args);
        assert!(err, "{tool} {bad}: {text}");
        assert_eq!(text, want, "{tool} {bad}");
    }
    // Only the repo's server was started: git "false" refused, no plain-folder one.
    let status = m.ok("revue_status", json!({ "repo": r }));
    assert!(!status.contains("plain folder"), "{status}");
    assert!(m.ok("revue_annotations", json!({ "repo": r })).starts_with("No notes"), "a refused note was stored");

    // Valid values of each type still work: a note kept from the tab, an integer written 2.0, the log's limit.
    m.ok("revue_annotate", json!({ "repo": r, "path": "lib.js", "line": 1.0, "text": "kept", "show": false, "side": "", "mode": "" }));
    assert!(m.ok("revue_annotations", json!({ "repo": r, "all": true })).contains("    kept"));
    assert_eq!(m.ok("revue_log", json!({ "repo": r, "limit": 1, "skip": 0, "all": false })).lines().count(), 1);
    // "" means the default, and isn't sent on: the tab is moved to the working view's new side.
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "lib.js", "line": 1, "side": "", "mode": "", "ref": "", "view": "" }));
    assert!(shown.contains(&format!("URL: http://127.0.0.1:{port}/#mode=working&file=lib.js&view=file&line=1\n")), "{shown}");
    // Wait for this call's own navigation, so one a refused call made first would be counted too.
    let navigated = || tab.named("navigate", 0).iter().map(|d| serde_json::from_str::<Value>(d).unwrap()).collect::<Vec<_>>();
    assert!(wait_until(5000, || navigated().iter().any(|v| v["view"] == "file" && v["line"] == 1)));
    let views: Vec<Value> = navigated().iter().map(|v| json!([v["file"], v["view"], v["line"], v["side"], v["mode"]])).collect();
    assert_eq!(views, [json!(["lib.js", "file", 1, "new", "working"])], "only the valid revue_show moved the tab");

    // A number of lines of context, as clients send it, and a limit of 0, which means the default.
    let diff = m.ok("revue_diff", json!({ "repo": r, "path": "lib.js", "context": 0 }));
    assert!(diff.contains("@@ -1 +1 @@"), "{diff}");
    assert!(m.ok("revue_diff", json!({ "repo": r, "context": 10.0 })).contains("+export function helper() { return 1; }"));
    assert!(m.ok("revue_log", json!({ "repo": r, "limit": 0 })).contains("first commit"));
}

#[test]
fn mode_folder_is_taken_as_revue_names_a_plain_folder_view() {
    // revue calls a plain folder's view {mode: "folder"}, so an agent may pass it back: a plain folder takes it, and a
    // repo refuses it before a server is started for it.
    let dir = plain_folder();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    let added = m.ok("revue_annotate", json!({ "repo": r, "path": "docs/a.js", "line": 1, "text": "here", "mode": "folder", "show": false }));
    assert!(added.contains("(the folder's files; stored in memory)"), "{added}");
    assert!(m.ok("revue_annotations", json!({ "repo": r, "mode": "folder" })).contains("    here"));
    assert!(m.ok("revue_definitions", json!({ "repo": r, "name": "helper", "mode": "folder" })).starts_with("* docs/a.js:1"));
    let port = port_of(&m.ok("revue_status", json!({ "repo": r })));
    let tab = follower(port);
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "docs/a.js", "line": 1, "mode": "folder" }));
    assert!(shown.contains(&format!("URL: http://127.0.0.1:{port}/#file=docs%2Fa.js&view=file&line=1\n")), "{shown}");
    assert!(tab.named("navigate", 5000).iter().any(|v| v.contains(r#""mode":"folder""#)));

    let repo = repo();
    let rr = repo.path().to_str().unwrap();
    let want = format!(r#"mode "folder" is only for a plain folder, and {rr} is a git repository; mode must be "working", "commit" or "branch" there"#);
    for (tool, args) in [
        ("revue_changes", json!({})),
        ("revue_diff", json!({})),
        ("revue_open", json!({})),
        ("revue_show", json!({ "path": "lib.js" })),
        ("revue_annotate", json!({ "path": "lib.js", "line": 1, "text": "x" })),
        ("revue_annotations", json!({})),
        ("revue_definitions", json!({ "name": "helper" })),
    ] {
        let mut a = json!({ "repo": rr, "mode": "folder" });
        a.as_object_mut().unwrap().extend(args.as_object().unwrap().clone());
        let (text, err) = m.tool(tool, a);
        assert!(err && text == want, "{tool}: {text}");
    }
    let none = m.ok("revue_status", json!({ "repo": rr }));
    assert_eq!(none, format!("No revue server is running for {rr}."), "a server was started for a refused mode");
    // The advertised values stay the three a repo has.
    let tools = m.call("tools/list", json!({}))["result"]["tools"].clone();
    let open = tools.as_array().unwrap().iter().find(|t| t["name"] == "revue_open").unwrap();
    assert_eq!(open["inputSchema"]["properties"]["mode"]["enum"], json!(["working", "commit", "branch"]));
}

#[test]
fn a_plain_folder_has_files_and_notes_but_no_changes_or_history() {
    let dir = plain_folder();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    let added = m.ok("revue_annotate", json!({ "repo": r, "path": "docs/a.js", "line": 1, "text": "the helper", "show": false }));
    assert!(added.contains("on docs/a.js:1 (the folder's files; stored in memory)"), "{added}");
    let listed = m.ok("revue_annotations", json!({ "repo": r }));
    assert!(listed.contains("1 note(s) in the folder's files") && listed.contains("    the helper"), "{listed}");
    let defs = m.ok("revue_definitions", json!({ "repo": r, "name": "helper", "path": "docs/a.js" }));
    assert!(defs.starts_with("* docs/a.js:1  function  function helper()"), "{defs}");
    for tool in ["revue_changes", "revue_diff", "revue_log"] {
        let (text, err) = m.tool(tool, json!({ "repo": r }));
        assert!(err && text.starts_with(&format!("{r} is not a git repository")), "{tool}: {text}");
    }
    let status = m.ok("revue_status", json!({ "repo": r }));
    assert!(status.starts_with(&format!("{r} (plain folder)\n")), "{status}");

    // revue_show moves an open tab to the file, with a link that names only the file.
    let port = port_of(&status);
    let tab = follower(port);
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "docs/a.js", "line": 2 }));
    assert!(shown.starts_with(&format!("revue for {r} (a plain folder: its files only)\n")), "{shown}");
    assert!(shown.contains(&format!("URL: http://127.0.0.1:{port}/#file=docs%2Fa.js&view=file&line=2\n")), "{shown}");
    assert!(shown.contains("Browser: moved the open revue tab"), "{shown}");
    let nav = tab.named("navigate", 5000);
    assert!(nav.iter().any(|v| v.contains(r#""mode":"folder""#) && v.contains(r#""file":"docs/a.js""#)), "{nav:?}");

    // Its README in the preview, which a file that isn't markdown or SVG has none of here either.
    let shown = m.ok("revue_show", json!({ "repo": r, "path": "README.md", "view": "preview" }));
    assert!(shown.contains(&format!("URL: http://127.0.0.1:{port}/#file=README.md&view=preview\n")), "{shown}");
    let (text, err) = m.tool("revue_show", json!({ "repo": r, "path": "docs/a.js", "view": "preview" }));
    assert!(err && text.starts_with("view \"preview\" is only for markdown"), "{text}");
}

#[test]
fn git_false_opens_a_repo_as_a_plain_folder_beside_its_repo_server() {
    let dir = repo();
    let r = dir.path().to_str().unwrap();
    let mut m = Mcp::start();
    assert!(m.ok("revue_changes", json!({ "repo": r })).starts_with("1 file(s) changed"));
    // Its own server, detached here, so it is spawned with --no-git.
    let added = m.ok("revue_annotate", json!({ "repo": r, "git": false, "persist": true, "path": "lib.js", "line": 1, "text": "plain", "show": false }));
    assert!(added.contains("(the folder's files; stored in memory)"), "{added}");
    let (text, err) = m.tool("revue_changes", json!({ "repo": r, "git": false }));
    assert!(err && text.contains("is not a git repository"), "{text}");
    // The repo's notes and the folder's are apart.
    assert!(m.ok("revue_annotations", json!({ "repo": r })).starts_with("No notes"));
    assert!(m.ok("revue_annotations", json!({ "repo": r, "git": false })).contains("    plain"));
    let status = m.ok("revue_status", json!({ "repo": r }));
    assert_eq!(status.matches(&format!("{r}\n")).count() + status.matches(&format!("{r} (plain folder)\n")).count(), 2, "{status}");
    assert!(status.contains(" (plain folder)\n  port ") && status.contains("detached"), "{status}");
    let stopped = m.ok("revue_stop", json!({ "repo": r, "git": false }));
    assert!(stopped.starts_with(&format!("Stopped the revue server for {r} (pid ")), "{stopped}");
    assert!(wait_until(5000, || !m.ok("revue_status", json!({ "repo": r })).contains("plain folder")));
    assert!(m.ok("revue_changes", json!({ "repo": r })).starts_with("1 file(s) changed"), "the repo's server is still there");
}
