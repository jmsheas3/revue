// MCP server over stdio (newline-delimited JSON-RPC 2.0), so agents can open revue and point the user at files and
// lines. It finds or starts one revue server per repo and drives it over HTTP. stdout carries only protocol messages.
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::server::{self, Server};
use crate::{discovery, http, repo, Opts};

const DETACHED_IDLE_MINUTES: f64 = 60.0;
// Tool output goes into the agent's context, so keep it bounded.
const MAX_OUTPUT: usize = 60000;

type R<T> = Result<T, String>;

#[derive(Clone)]
struct Rec {
    port: u16,
    lifetime: String,
    server: Option<Arc<Server>>, // in-process (session) servers only
}

struct State {
    opts: Opts,
    exe: R<PathBuf>, // this binary, for detached servers (see own_exe)
    servers: Mutex<HashMap<(String, bool), Rec>>, // (root, git) -> server: a repo, or a plain folder
    client_name: Mutex<Option<String>>,   // from initialize; the default author of notes
    remarks: Mutex<Vec<String>>,          // things the current tool call should tell the agent (see remark)
}

// Add a line to the current tool call's output, after its result.
fn remark(st: &State, text: String) {
    st.remarks.lock().unwrap().push(text);
}

// ---------------------------------------------------------------- servers
// The path of this binary, resolved once at startup. When it is replaced on disk (as `cargo install` does), Linux
// reports the running executable as "<path> (deleted)"; the path itself then names the new binary, which is the one
// detached servers should run.
fn own_exe() -> R<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let s = exe.to_string_lossy();
    Ok(match s.strip_suffix(" (deleted)") {
        Some(live) => PathBuf::from(live),
        None => exe,
    })
}

// The root a tool call is about, and whether it is a repo: the work tree's top level of the repo `repo` is in, or the
// folder itself when it is in none or `git` is false (a plain folder, as `revue --no-git` opens it).
fn repo_root(a: &Value) -> R<(String, bool)> {
    let dir = match a.get("repo").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(r) => std::path::absolute(r).unwrap_or_else(|_| PathBuf::from(r)),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    if a.get("git") != Some(&Value::Bool(false)) {
        if let Some(top) = repo::toplevel(&dir) {
            return Ok((top.to_string_lossy().into_owned(), true));
        }
    }
    match std::fs::canonicalize(&dir) {
        Ok(d) if d.is_dir() => Ok((d.to_string_lossy().into_owned(), false)),
        Ok(_) => Err(format!("{} is not a folder", dir.display())),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

// What the git-only tools say about a plain folder.
fn not_a_repo(root: &str) -> String {
    format!("{root} is not a git repository (it is open as a plain folder); use revue_show to show its files, or revue_definitions to search them")
}

// A discovery entry or /api/server answer for this root and kind. Servers from before plain folders have no `git`.
fn is_server_for(e: &Value, root: &str, git: bool) -> bool {
    e["root"].as_str() == Some(root) && e["git"].as_bool().unwrap_or(true) == git
}

fn request(port: u16, path: &str, body: Option<&Value>) -> R<Value> {
    let timeout = Duration::from_secs(if body.is_some() { 15 } else { 5 });
    let body = body.map(|b| b.to_string());
    let res = http::request(port, path, body.as_deref(), timeout).map_err(|e| e.to_string())?;
    let data: Value = serde_json::from_str(&res.body)
        .map_err(|_| format!("revue returned {}: {}", res.status, res.body.chars().take(200).collect::<String>()))?;
    if let Some(e) = data.get("error").filter(|e| !e.is_null()) {
        return Err(e.as_str().map(String::from).unwrap_or_else(|| e.to_string()));
    }
    Ok(data)
}

// The server's self-description, or None if nothing (or a different repo or kind) answers on that port.
fn probe(port: u16, root: &str, git: bool) -> Option<Value> {
    let s = request(port, "/api/server", None).ok()?;
    is_server_for(&s, root, git).then_some(s)
}

fn spawn_detached(st: &State, root: &str, git: bool) -> R<Rec> {
    let idle = if st.opts.idle_timeout > 0.0 { st.opts.idle_timeout } else { DETACHED_IDLE_MINUTES };
    let exe = st.exe.clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args([root, "--no-open", "--port", "0", "--notes", &st.opts.notes, "--idle-timeout", &idle.to_string()]);
    if !git {
        cmd.arg("--no-git");
    }
    cmd
        .env("REVUE_SPAWNED_BY", "mcp")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        // Its own session, so it outlives us and our terminal.
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        // No console and its own process group, so it outlives us and Ctrl+C in our console doesn't reach it.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    // The child picks a free port; it shows up in the discovery file once it's listening.
    for _ in 0..50 {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("revue server exited with code {}", status.code().map(|c| c.to_string()).unwrap_or_else(|| "null".into())));
        }
        let found = discovery::list_servers().into_iter().find(|e| is_server_for(e, root, git) && e["pid"].as_u64() == Some(pid as u64));
        if let Some(e) = found {
            // Only reaps it: without a thread nobody waits for it, and once it exits it stays a zombie until this MCP
            // process exits.
            let _ = crate::git::spawn(move || {
                let _ = child.wait();
            });
            return Ok(Rec { port: e["port"].as_u64().unwrap_or(0) as u16, lifetime: "detached".into(), server: None });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Don't leave a server running that nobody knows about (or a zombie once it exits).
    let _ = child.kill();
    let _ = child.wait();
    Err("timed out waiting for the revue server to start".into())
}

// Stop a detached server we found registered and wait (briefly) for it to go, so a fresh one can take its place.
fn stop_detached(root: &str, port: u16, pid: u32) {
    if registered(port, root, pid).is_none() {
        return;
    }
    terminate(pid);
    for _ in 0..30 {
        if registered(port, root, pid).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

const STALE: &str = "changed on disk after it started, so the page may be out of date";

// Reuse a server for this repo (ours, or one found through discovery), or start one. `persist` overrides --lifetime
// for this call: true = detached, false = session. Returns the server and whether it was started now.
//
// A server running older code than is on disk (`stale`) is replaced when it's a detached one, which MCP servers own.
// An in-process session server is this MCP's own code, so only reconnecting the MCP helps; a server started by hand or
// by another session isn't ours to stop. Those are reused, and the tool output says so.
fn ensure_server(st: &State, root: &str, git: bool, persist: Option<bool>) -> R<(Rec, bool)> {
    let key = (root.to_string(), git);
    let known = st.servers.lock().unwrap().get(&key).cloned();
    let mut replace_detached = false;
    if let Some(k) = known {
        match probe(k.port, root, git) {
            Some(s) if s["stale"] == true && k.server.is_none() && k.lifetime == "detached" => {
                stop_detached(root, k.port, discovery::pid_of(&s["pid"]).unwrap_or(0));
                replace_detached = true;
            }
            Some(s) => {
                if s["stale"] == true {
                    remark(st, if k.server.is_some() {
                        format!("this MCP server's code {STALE}; reconnect it (for example /mcp in Claude Code) to load the new code.")
                    } else {
                        format!("the code of the revue server on port {} {STALE}; restart it to load the new code.", k.port)
                    });
                }
                return Ok((k, false));
            }
            None => {
                // Gone or unresponsive. An in-process server we're about to forget must not keep running unseen.
                if let Some(srv) = &k.server {
                    srv.close();
                }
            }
        }
    }
    st.servers.lock().unwrap().remove(&key);
    if !replace_detached {
        for e in discovery::list_servers().into_iter().filter(|e| is_server_for(e, root, git)) {
            let port = e["port"].as_u64().unwrap_or(0) as u16;
            let pid = e["pid"].as_u64().unwrap_or(0);
            // Whatever answers must be the process that registered, not something else that took over the port.
            let Some(s) = probe(port, root, git).filter(|s| s["pid"].as_u64() == Some(pid)) else { continue };
            let lifetime = e["lifetime"].as_str().unwrap_or("").to_string();
            if s["stale"] == true {
                if lifetime == "detached" {
                    stop_detached(root, port, pid as u32);
                    replace_detached = true;
                    break;
                }
                remark(st, format!("the code of the revue server on port {port} ({lifetime}) {STALE}; restart it to load the new code."));
            }
            let rec = Rec { port, lifetime, server: None };
            st.servers.lock().unwrap().insert(key, rec.clone());
            return Ok((rec, false));
        }
    }
    let detached = replace_detached || persist.unwrap_or(st.opts.lifetime == "detached");
    let rec = if detached {
        spawn_detached(st, root, git)?
    } else {
        let opts = server::Options { port: 0, notes: st.opts.notes.clone(), idle_timeout: 0.0, lifetime: "session".into(), git, on_idle: None };
        let srv = server::start(std::path::Path::new(root), opts).map_err(|e| e.to_string())?;
        Rec { port: srv.port(), lifetime: "session".into(), server: Some(Arc::new(srv)) }
    };
    if replace_detached {
        remark(st, format!("replaced a detached revue server whose code {STALE}."));
    }
    st.servers.lock().unwrap().insert(key, rec.clone());
    Ok((rec, true))
}

// This user's live discovery entry for exactly this port, root and pid: the only proof that a pid is a revue server
// we may stop (list_servers reads only files we own and drops dead pids).
fn registered(port: u16, root: &str, pid: u32) -> Option<Value> {
    discovery::list_servers().into_iter().find(|e| {
        e["port"].as_u64() == Some(port as u64) && e["root"].as_str() == Some(root) && e["pid"].as_u64() == Some(pid as u64)
    })
}

// Ask a detached server to exit. A detached Windows process has no console to send Ctrl+C to, so it is ended outright;
// the next list_servers() sees the pid is gone and removes its discovery file.
fn terminate(pid: u32) {
    // kill() would take 0 as this process's group, and a pid past pid_t as another group or every process.
    if !discovery::valid_pid(pid as u64) {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 0);
            CloseHandle(h);
        }
    }
}

// String(v), for argument values.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

// An argument as given, or None when it is missing, null or "", which all mean the default (check_args lets "" through
// only for string arguments).
fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null() && v.as_str() != Some(""))
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn flag(a: &Value, k: &str) -> bool {
    a.get(k) == Some(&Value::Bool(true))
}

// Same encoding as writeHash()/readHash() in public/app.js.
fn view_hash(v: &Value) -> String {
    let mut p = http::Query::default();
    let mode = v["mode"].as_str().unwrap_or("working");
    // A plain folder's hash names only the file: it has one view.
    if mode != "folder" {
        p.set("mode", mode);
    }
    let s = |k: &str| v.get(k).filter(|x| !x.is_null() && x.as_str() != Some("") && x != &&Value::Bool(false)).map(js_string);
    if mode == "commit" {
        if let Some(r) = s("ref") {
            p.set("ref", r);
        }
    }
    if mode == "branch" {
        if let Some(b) = s("base") {
            p.set("base", b);
        }
    }
    if let Some(h) = s("head").filter(|h| h != "HEAD") {
        p.set("head", h);
    }
    if let Some(f) = s("file") {
        p.set("file", f);
        if let Some(view @ ("file" | "preview")) = v["view"].as_str() {
            p.set("view", view);
        }
        if v["side"].as_str() == Some("old") {
            p.set("side", "old");
        }
        if let Some(l) = s("line") {
            p.set("line", l);
        }
        if let Some(l) = s("endLine") {
            p.set("endLine", l);
        }
    }
    p.encode()
}

// Forward a value unless it means the default (null or "", as arg reads them), so the server never sees one.
fn put(m: &mut Map<String, Value>, k: &str, v: Option<&Value>) {
    if let Some(v) = v.filter(|v| !v.is_null() && v.as_str() != Some("")) {
        m.insert(k.into(), v.clone());
    }
}

// Move the most recently focused tab to the view, or open a browser tab if none can follow.
fn show(root: &str, srv: &(Rec, bool), a: &Value, file: Option<Map<String, Value>>) -> R<Vec<String>> {
    let mut view = Map::new();
    for k in ["mode", "ref", "base", "head"] {
        put(&mut view, k, a.get(k));
    }
    if let Some(f) = file {
        for (k, v) in f {
            put(&mut view, &k, Some(&v));
        }
    }
    let (rec, started) = srv;
    let res = request(rec.port, "/api/navigate", Some(&Value::Object(view)))?;
    let url = format!("http://127.0.0.1:{}/#{}", rec.port, view_hash(&res["view"]));
    let reused = res["tabs"].as_u64().unwrap_or(0) > 0 && !flag(a, "newTab");
    if !reused {
        server::open_browser(&url);
    }
    let kind = if res["view"]["mode"] == "folder" { " (a plain folder: its files only)" } else { "" };
    Ok(vec![
        format!("revue for {root}{kind}"),
        format!("URL: {url}"),
        format!("Server: {} ({}, port {})", if *started { "started" } else { "reused" }, rec.lifetime, rec.port),
        format!("Browser: {}", if reused { "moved the open revue tab" } else { "opened a new tab" }),
    ])
}

fn describe(s: &Value) -> String {
    format!(
        "{}{}\n  port {}, pid {}, {}, notes in {}, {} tab(s) open ({} can follow navigation), {} note(s)",
        s["root"].as_str().unwrap_or(""),
        if s["git"] == false { " (plain folder)" } else { "" },
        s["port"],
        s["pid"],
        s["lifetime"].as_str().unwrap_or(""),
        s["notes"].as_str().unwrap_or(""),
        s["tabs"],
        s["navTabs"],
        s["annotations"]
    ) + if s["stale"] == Value::Bool(true) { "\n  OUT OF DATE: its code changed on disk after it started; restart it before relying on the page" } else { "" }
}

// GET an API endpoint with the range arguments (mode/ref/base/head) plus extra params.
fn get(rec: &Rec, pathname: &str, a: &Value, extra: &[(&str, Option<Value>)]) -> R<Value> {
    let mut q = http::Query::default();
    let range: Vec<(&str, Option<Value>)> = ["mode", "ref", "base", "head"].iter().map(|k| (*k, a.get(*k).cloned())).collect();
    for (k, v) in range.iter().chain(extra.iter()) {
        if let Some(v) = v.as_ref().filter(|v| !v.is_null() && v.as_str() != Some("")) {
            q.set(k, js_string(v));
        }
    }
    request(rec.port, &format!("{pathname}?{}", q.encode()), None)
}

// The root, whether it is a repo, and its server (started now or not).
fn server_for(st: &State, a: &Value) -> R<(String, bool, (Rec, bool))> {
    let (root, git) = repo_root(a)?;
    folder_mode_in_repo(a, &root, git)?;
    let persist = a.get("persist").and_then(Value::as_bool);
    let srv = ensure_server(st, &root, git, persist)?;
    Ok((root, git, srv))
}

// check_args lets mode "folder" through, as revue names a plain folder's view; a repo has no such view, and its server
// would refuse it only once started, so this says so before one is.
fn folder_mode_in_repo(a: &Value, root: &str, git: bool) -> R<()> {
    if git && arg_str(a, "mode") == Some("folder") {
        return Err(format!(r#"mode "folder" is only for a plain folder, and {root} is a git repository; mode must be "working", "commit" or "branch" there"#));
    }
    Ok(())
}

// For the tools that read changes or history: a plain folder has neither, and no server is started for it.
fn repo_server_for(st: &State, a: &Value) -> R<(String, (Rec, bool))> {
    let (root, git) = repo_root(a)?;
    if !git {
        return Err(not_a_repo(&root));
    }
    folder_mode_in_repo(a, &root, git)?;
    let srv = ensure_server(st, &root, git, a.get("persist").and_then(Value::as_bool))?;
    Ok((root, srv))
}

// A file in this view, with its change entry (null if unchanged). Paths are relative to the repo root.
fn file_in(rec: &Rec, root: &str, a: &Value) -> R<(String, Value, Value)> {
    let tree = get(rec, "/api/tree", a, &[])?;
    let raw = a.get("path").map(js_string).unwrap_or_else(|| "undefined".into());
    let file = raw.strip_prefix("./").unwrap_or(&raw).to_string();
    let listed = tree["files"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(&file)));
    if !listed {
        return Err(format!("{file} is not in this view of the repo (paths are relative to {root})"));
    }
    let change = tree["changes"].get(&file).cloned().unwrap_or(Value::Null);
    Ok((file, change, tree))
}

fn range_label(a: &Value, git: bool) -> String {
    if !git {
        return "the folder's files".into();
    }
    match arg_str(a, "mode").unwrap_or("working") {
        "commit" => format!("commit {} vs its parent", arg_str(a, "ref").unwrap_or("HEAD")),
        "branch" => format!("{} vs its merge-base with {}", arg_str(a, "head").unwrap_or("HEAD"), arg_str(a, "base").unwrap_or("the default base")),
        _ => "working tree vs HEAD".into(),
    }
}

fn lines(n: &Value) -> String {
    match n.get("endLine").filter(|e| !e.is_null() && e.as_u64() != Some(0)) {
        Some(e) => format!("{}-{}", n["line"], e),
        None => n["line"].to_string(),
    }
}

fn note_line(n: &Value, with_range: bool) -> String {
    format!(
        "[{}] {}:{}{} by {}{}{}\n    {}",
        n["id"].as_str().unwrap_or(""),
        n["file"].as_str().unwrap_or(""),
        lines(n),
        if n["side"].as_str() == Some("old") { " (old side)" } else { "" },
        n["author"].as_str().unwrap_or(""),
        if n["stale"] == Value::Bool(true) { " [outdated: the lines changed since]" } else { "" },
        if with_range { format!(" {{{}}}", n["key"].as_str().unwrap_or("")) } else { String::new() },
        n["text"].as_str().unwrap_or("").replace('\n', "\n    ")
    )
}

fn capped(text: String, hint: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX_OUTPUT {
        return text;
    }
    let mut at = chars[..=MAX_OUTPUT].iter().rposition(|&c| c == '\n').unwrap_or(0);
    if at == 0 {
        at = MAX_OUTPUT;
    }
    let head: String = chars[..at].iter().collect();
    format!("{head}\n\n[truncated: {} more characters. {hint}]", chars.len() - at)
}

fn ymd(secs: i64) -> String {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------- tools
fn range_props() -> Map<String, Value> {
    let v = json!({
        "repo": { "type": "string", "description": "Any folder inside the git repo. Defaults to the current working directory. A folder outside any repo opens as a plain folder: its files only, with no changes, history or modes." },
        "git": { "type": "boolean", "description": "false opens the folder as a plain folder (its files only, including .git) even inside a git repo, like revue --no-git. Default true." },
        "mode": { "type": "string", "enum": ["working", "commit", "branch"], "description": "working = uncommitted changes vs HEAD (default); commit = one commit vs its parent; branch = head vs its merge-base with base, like a PR." },
        "ref": { "type": "string", "description": "Commit to show in commit mode (default HEAD)." },
        "base": { "type": "string", "description": "Base branch in branch mode (default: main, master, origin/main, origin/master or develop, whichever exists)." },
        "head": { "type": "string", "description": "Branch to view in commit/branch mode without checking it out (default: the checked-out branch)." },
    });
    v.as_object().unwrap().clone()
}

fn open_props() -> Map<String, Value> {
    let v = json!({
        "newTab": { "type": "boolean", "description": "Open a new browser tab even if a revue tab is already open." },
        "persist": { "type": "boolean", "description": "If a server has to be started: true keeps it running after this agent session ends, false stops it with the session. Defaults to the --lifetime setting." },
    });
    v.as_object().unwrap().clone()
}

fn schema(own: Value, range: bool, open: bool, required: &[&str]) -> Value {
    let mut props = own.as_object().cloned().unwrap_or_default();
    if range {
        props.extend(range_props());
        // Every tool that takes a view may start a server for it (server_for, repo_server_for), which persist decides.
        props.insert("persist".into(), open_props()["persist"].clone());
    }
    if open {
        props.extend(open_props());
    }
    let mut s = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        s["required"] = json!(required);
    }
    s["additionalProperties"] = json!(false);
    s
}

// The tool definitions, built once: tools/list sends them and check_args reads each call's schema from them.
fn tools() -> &'static [Value] {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS.get_or_init(build_tools)
}

fn build_tools() -> Vec<Value> {
    let repo_only = || json!({ "repo": range_props()["repo"], "git": range_props()["git"] });
    vec![
        json!({ "name": "revue_open",
            "description": "Open revue, the browser-based git change viewer, on the overview of all changed files. Use this when the user asks to see or review code changes, rather than printing a diff. Starts a revue server for the repo if needed and reuses an open revue tab when there is one.",
            "inputSchema": schema(json!({}), true, true, &[]) }),
        json!({ "name": "revue_show",
            "description": "Show the user a file in revue, optionally scrolled to a line or line range. Use this to point at specific code: \"look at server.js lines 120-140\". With a line it defaults to the whole-file view; without one, to the diff. view \"preview\" shows a markdown file rendered or an SVG drawn as an image. In a plain folder (not a git repo) it is the file or its preview.",
            "inputSchema": schema(json!({
                "path": { "type": "string", "description": "File path relative to the repo root." },
                "line": { "type": "integer", "minimum": 1, "description": "Line to scroll to and highlight. Not with view preview, which has no line numbers." },
                "endLine": { "type": "integer", "minimum": 1, "description": "Last line of a range starting at line." },
                "view": { "type": "string", "enum": ["file", "diff", "preview"], "description": "Whole file, the diff, or the preview: the rendered markdown of a .md or .markdown file, or an .svg file drawn as an image (only for those files, and without line). Default: file if line is given, else diff." },
                "side": { "type": "string", "enum": ["new", "old"], "description": "Which version: new (after the change, default) or old (before it). Line numbers refer to this side." },
            }), true, true, &["path"]) }),
        json!({ "name": "revue_annotate",
            "description": "Leave a note on a line or line range, shown inline in revue under those lines (in the diff and the file view), and show it to the user. Use it to explain a change, flag something to review, or ask a question about specific code. Notes belong to the current view (mode/ref/base/head) and are marked outdated if the lines change later.",
            "inputSchema": schema(json!({
                "path": { "type": "string", "description": "File path relative to the repo root." },
                "line": { "type": "integer", "minimum": 1, "description": "First line the note is about." },
                "endLine": { "type": "integer", "minimum": 1, "description": "Last line, for a range." },
                "text": { "type": "string", "description": "The note. Plain text; newlines are kept." },
                "side": { "type": "string", "enum": ["new", "old"], "description": "new (after the change, default) or old (before it, e.g. for deleted lines). Line numbers refer to this side." },
                "author": { "type": "string", "description": "Shown on the note. Defaults to the MCP client's name." },
                "show": { "type": "boolean", "description": "Also move the browser to the note (default true). Set false when leaving several notes, then call revue_show or revue_open once." },
            }), true, true, &["path", "line", "text"]) }),
        json!({ "name": "revue_annotations",
            "description": "List or clear notes left with revue_annotate. By default this acts on the notes of one view (mode/ref/base/head); all=true covers every view.",
            "inputSchema": schema(json!({
                "action": { "type": "string", "enum": ["list", "clear"], "description": "list (default) or clear." },
                "id": { "type": "string", "description": "With clear: remove only this note." },
                "all": { "type": "boolean", "description": "Every view, not just the given one." },
            }), true, false, &[]) }),
        json!({ "name": "revue_changes",
            "description": "List the files changed in a view of the repo (uncommitted work, a commit, or a branch vs its base), with status and added/deleted line counts. Returns text to you; to show changes to the user, use revue_open or revue_show.",
            "inputSchema": schema(json!({}), true, false, &[]) }),
        json!({ "name": "revue_diff",
            "description": "Unified diff of one changed file, or of every changed file, in a view of the repo. Includes untracked files in the working tree view. Returns text to you; to show changes to the user, use revue_open or revue_show.",
            "inputSchema": schema(json!({
                "path": { "type": "string", "description": "File path relative to the repo root. Omit for all changed files." },
                "context": { "type": ["string", "integer"], "minimum": 0, "description": "Lines of context around changes: a number such as 10 (default 3), or \"all\" for the whole file." },
            }), true, false, &[]) }),
        json!({ "name": "revue_log",
            "description": "Recent commits on a branch (default: the checked-out one), or on all branches.",
            "inputSchema": schema(json!({
                "repo": range_props()["repo"],
                "git": range_props()["git"],
                "persist": open_props()["persist"],
                "branch": { "type": "string", "description": "Branch or ref to list (default: HEAD)." },
                "all": { "type": "boolean", "description": "Commits from all branches." },
                "limit": { "type": "integer", "minimum": 0, "maximum": 500, "description": "How many commits (default 30, which 0 means too)." },
                "skip": { "type": "integer", "minimum": 0, "description": "Skip this many first, for paging." },
            }), false, false, &[]) }),
        json!({ "name": "revue_definitions",
            "description": "Find where an identifier (function, class, type, variable...) is defined, at the revision a view shows (in a plain folder, in its files). A fast git-grep heuristic, not a compiler: JS/TS, Python, Go, Rust, C-family and shell, with a loose fallback for other languages. The best match comes first, marked with * when it is a clear winner.",
            "inputSchema": schema(json!({
                "name": { "type": "string", "description": "The identifier." },
                "path": { "type": "string", "description": "File the identifier was seen in; ranks nearby definitions first." },
                "side": { "type": "string", "enum": ["new", "old"], "description": "Search the new (default) or old side of the view." },
            }), true, false, &["name"]) }),
        json!({ "name": "revue_status",
            "description": "List running revue servers (all repos, or one), with their port, lifetime, note storage and open tabs.",
            "inputSchema": schema(repo_only(), false, false, &[]) }),
        json!({ "name": "revue_stop",
            "description": "Stop the revue server for a repo, if this MCP server or an earlier one started it. Servers the user started by hand are left alone.",
            "inputSchema": schema(repo_only(), false, false, &[]) }),
    ]
}

fn file_view(a: &Value, file: &str, view: &str) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert("file".into(), json!(file));
    for k in ["line", "endLine", "side"] {
        put(&mut f, k, a.get(k));
    }
    f.insert("view".into(), json!(view));
    f
}

// The files the page has a preview view of: hasPreview() in public/app.js (rendered markdown, or an SVG drawn).
fn has_preview(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [".md", ".markdown", ".svg"].iter().any(|ext| path.ends_with(ext))
}

// A value as an error message shows it: JSON, cut short so a huge string or array can't flood the caller's context.
fn shown(v: &Value) -> String {
    const MAX: usize = 100;
    let s = v.to_string();
    match s.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s,
    }
}

// "a", "b" or "c".
fn one_of(values: &[String]) -> String {
    match values.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => values.concat(),
    }
}

// Whether a value is of a JSON Schema type; a type this doesn't know passes.
fn of_type(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        // JSON Schema's integer: any number with no fractional part, 50.0 included.
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => v.is_number(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

fn type_name(t: &str) -> String {
    match t {
        "integer" | "array" | "object" => format!("an {t}"),
        "boolean" => "true or false".into(),
        t => format!("a {t}"),
    }
}

// Every argument must be one the tool's schema names, of the schema's type, within its minimum and maximum and among
// its enum's values. Unchecked, a wrong one quietly meant the default (view "bogus" showed the diff, git "false" opened
// the repo, show "false" moved the browser, limit "50" listed 30) or was ignored (a misspelled "veiw"). Read from the
// schemas, so a new argument is checked too. Missing and null mean the default, and so does "" for a string argument,
// as arg and put read them; "" for a boolean or a number is of the wrong type.
fn check_args(tool: &Value, a: &Value) -> R<()> {
    let empty = Map::new();
    let props = tool["inputSchema"]["properties"].as_object().unwrap_or(&empty);
    let args = a.as_object().unwrap_or(&empty);
    if let Some(k) = args.keys().find(|k| !props.contains_key(*k)) {
        let names: Vec<&str> = props.keys().map(String::as_str).collect();
        let takes = if names.is_empty() { "no arguments".to_string() } else { names.join(", ") };
        return Err(format!("unknown argument {} ({} takes: {takes})", shown(&json!(k)), tool["name"].as_str().unwrap_or("this tool")));
    }
    for (k, p) in props {
        let Some(v) = args.get(k).filter(|v| !v.is_null()) else { continue };
        // A schema's type is one name or a list of them; a value of any of them will do.
        let types: Vec<&str> = match &p["type"] {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !types.is_empty() && !types.iter().any(|t| of_type(t, v)) {
            let names: Vec<String> = types.iter().map(|t| type_name(t)).collect();
            return Err(format!("{k} must be {}, not {}", one_of(&names), shown(v)));
        }
        if v.as_str() == Some("") {
            continue;
        }
        if let Some(n) = v.as_f64() {
            if let Some(min) = p["minimum"].as_f64().filter(|min| n < *min) {
                return Err(format!("{k} must be at least {}, not {}", js_string(&json!(min)), shown(v)));
            }
            if let Some(max) = p["maximum"].as_f64().filter(|max| n > *max) {
                return Err(format!("{k} must be at most {}, not {}", js_string(&json!(max)), shown(v)));
            }
        }
        if let Some(allowed) = p["enum"].as_array() {
            // revue itself calls a plain folder's one view {mode: "folder"} (/api/navigate's answer, the notes' others),
            // so an agent may pass it back; the server takes it in a plain folder and refuses it in a repo.
            if allowed.contains(v) || (k == "mode" && v == "folder") {
                continue;
            }
            debug_assert!(!allowed.is_empty(), "{k} has an empty enum");
            let names: Vec<String> = allowed.iter().map(Value::to_string).collect();
            let list = if names.is_empty() { "nothing".to_string() } else { one_of(&names) };
            return Err(format!("{k} must be {list}, not {}", shown(v)));
        }
    }
    Ok(())
}

fn run_tool(st: &State, name: &str, a: &Value) -> R<String> {
    // Before any server is started or tab moved.
    if let Some(tool) = tools().iter().find(|t| t["name"] == name) {
        check_args(tool, a)?;
    }
    // "" would mean no id, and clearing with no id clears every note of the view; take it for the mistake it likely is.
    if name == "revue_annotations" && arg_str(a, "action") == Some("clear") && a.get("id").and_then(Value::as_str) == Some("") {
        return Err("id must name a note, not \"\"; leave it out to clear every note of the view".into());
    }
    match name {
        "revue_open" => {
            let (root, _, srv) = server_for(st, a)?;
            Ok(show(&root, &srv, a, None)?.join("\n"))
        }
        "revue_show" => {
            let (root, _, srv) = server_for(st, a)?;
            let (file, _, _) = file_in(&srv.0, &root, a)?; // catch a wrong path here; the browser would just show nothing
            let view = arg_str(a, "view").map(String::from).unwrap_or_else(|| if arg(a, "line").is_some() { "file".into() } else { "diff".into() });
            // The page would quietly show another view instead (setFile in app.js), so say why here.
            if view == "preview" {
                if !has_preview(&file) {
                    return Err(format!("view \"preview\" is only for markdown (.md, .markdown) and SVG (.svg) files; show {file} with view \"file\" or \"diff\""));
                }
                if arg(a, "line").is_some() || arg(a, "endLine").is_some() {
                    return Err("the preview has no line numbers; leave out line and endLine to show the preview, or use view \"file\" to show the lines".into());
                }
            }
            Ok(show(&root, &srv, a, Some(file_view(a, &file, &view)))?.join("\n"))
        }
        "revue_annotate" => {
            let (root, git, srv) = server_for(st, a)?;
            let (file, change, _) = file_in(&srv.0, &root, a)?;
            // An old-side note on a renamed file is about the old path.
            let note_path = match (arg_str(a, "side"), change.get("oldPath").and_then(Value::as_str)) {
                (Some("old"), Some(old)) => old.to_string(),
                _ => file.clone(),
            };
            let author = arg_str(a, "author").map(String::from).or_else(|| st.client_name.lock().unwrap().clone()).unwrap_or_else(|| "agent".into());
            let mut body = Map::new();
            body.insert("action".into(), json!("add"));
            body.insert("file".into(), json!(note_path));
            for k in ["line", "endLine", "side", "text"] {
                put(&mut body, k, a.get(k));
            }
            body.insert("author".into(), json!(author));
            for k in ["mode", "ref", "base", "head"] {
                put(&mut body, k, a.get(k));
            }
            let res = request(srv.0.port, "/api/annotations", Some(&Value::Object(body)))?;
            let mut out = vec![format!(
                "Added note {} on {note_path}:{} ({}; stored in {}).",
                res["note"]["id"].as_str().unwrap_or(""),
                lines(&res["note"]),
                range_label(a, git),
                res["storage"].as_str().unwrap_or("")
            )];
            if a.get("show") != Some(&Value::Bool(false)) {
                let view = if change.is_null() { "file" } else { "diff" };
                out.extend(show(&root, &srv, a, Some(file_view(a, &file, view)))?);
            }
            Ok(out.join("\n"))
        }
        "revue_annotations" => {
            let (root, git, srv) = server_for(st, a)?;
            let all = flag(a, "all");
            if arg_str(a, "action") == Some("clear") {
                let mut body = json!({ "action": "clear", "all": all });
                if let Some(id) = arg(a, "id") {
                    body["id"] = id.clone();
                }
                for k in ["mode", "ref", "base", "head"] {
                    if let Some(v) = arg(a, k) {
                        body[k] = v.clone();
                    }
                }
                let res = request(srv.0.port, "/api/annotations", Some(&body))?;
                let scope = match arg_str(a, "id") {
                    Some(id) => format!("note {id}"),
                    None if all => "all views".into(),
                    None => range_label(a, git),
                };
                return Ok(format!("Removed {} note(s) ({scope}).", res["removed"]));
            }
            let res = get(&srv.0, "/api/annotations", a, &[("all", all.then(|| json!("1")))])?;
            let notes = res["notes"].as_array().cloned().unwrap_or_default();
            if notes.is_empty() {
                return Ok(format!("No notes in {} of {root}.", if all { "any view".into() } else { range_label(a, git) }));
            }
            let body = notes.iter().map(|n| note_line(n, all)).collect::<Vec<_>>().join("\n");
            Ok(capped(
                format!("{} note(s) in {}, stored in {}:\n{body}", notes.len(), if all { "all views".into() } else { range_label(a, git) }, res["storage"].as_str().unwrap_or("")),
                "Clear some notes or narrow to one view.",
            ))
        }
        "revue_changes" => {
            let (_, srv) = repo_server_for(st, a)?;
            let tree = get(&srv.0, "/api/tree", a, &[])?;
            let changes = tree["changes"].as_object().cloned().unwrap_or_default();
            let mut paths: Vec<&String> = changes.keys().collect();
            paths.sort();
            if paths.is_empty() {
                return Ok(format!("No changes ({}).", range_label(a, true)));
            }
            let (mut adds, mut dels) = (0u64, 0u64);
            let rows: Vec<String> = paths
                .iter()
                .map(|p| {
                    let c = &changes[*p];
                    adds += c["add"].as_u64().unwrap_or(0);
                    dels += c["del"].as_u64().unwrap_or(0);
                    let status = c["status"].as_str().unwrap_or("");
                    let counts = if c["add"].is_null() && c["del"].is_null() {
                        if status == "D" { String::new() } else { "binary".into() }
                    } else if c["add"].is_null() && p.ends_with('/') {
                        // git lists a nested repo as `dir/`; one the server doesn't count has no commit to add yet.
                        "nested repo with no commits yet".into()
                    } else if c["add"].is_null() {
                        // An untracked text file the server doesn't count: over 2 MB, or unreadable.
                        "not counted (too large or unreadable)".into()
                    } else {
                        format!("+{} -{}", c["add"].as_u64().unwrap_or(0), c["del"].as_u64().unwrap_or(0))
                    };
                    let old = c["oldPath"].as_str().map(|o| format!("{o} -> ")).unwrap_or_default();
                    format!("{}  {old}{p}  {counts}", if status == "?" { "U" } else { status })
                })
                .collect();
            Ok(capped(
                format!(
                    "{} file(s) changed, +{adds} -{dels} ({}). Status: M modified, A added, D deleted, R renamed, C copied, U untracked.\n{}",
                    paths.len(),
                    range_label(a, true),
                    rows.join("\n")
                ),
                "Use revue_diff on specific paths.",
            ))
        }
        "revue_diff" => {
            let (root, srv) = repo_server_for(st, a)?;
            let context = arg(a, "context").map(js_string).unwrap_or_else(|| "3".into());
            let number = !context.is_empty() && context.bytes().all(|b| b.is_ascii_digit());
            if context != "all" && !number {
                return Err("context must be a number or \"all\"".into());
            }
            let targets: Vec<(String, Value)> = if arg_str(a, "path").is_some() {
                let (file, change, _) = file_in(&srv.0, &root, a)?;
                if change.is_null() {
                    return Ok(format!("{file} has no changes ({}).", range_label(a, true)));
                }
                vec![(file, change)]
            } else {
                let tree = get(&srv.0, "/api/tree", a, &[])?;
                let changes = tree["changes"].as_object().cloned().unwrap_or_default();
                let mut t: Vec<(String, Value)> = changes.into_iter().collect();
                t.sort_by(|x, y| x.0.cmp(&y.0));
                if t.is_empty() {
                    return Ok(format!("No changes ({}).", range_label(a, true)));
                }
                t
            };
            let mut out = String::new();
            let mut i = 0;
            // Stop fetching once over the cap; capped() trims the last diff, and the skipped files are named.
            while i < targets.len() && out.chars().count() <= MAX_OUTPUT {
                let (p, c) = &targets[i];
                let extra = [
                    ("path", Some(json!(p))),
                    ("old", c.get("oldPath").cloned()),
                    ("status", c.get("status").cloned()),
                    ("context", Some(json!(context))),
                ];
                let d = get(&srv.0, "/api/diff", a, &extra)?;
                let text = d["diff"].as_str().unwrap_or("");
                if d["tooLarge"] == Value::Bool(true) {
                    // An untracked text file over 2 MB: the server makes no diff of it, which would be all of it.
                    out.push_str(&format!("(untracked file {p} is too large to show: {} bytes)\n", d["size"]));
                } else if text.is_empty() {
                    out.push_str(&format!("(no textual changes in {p})\n"));
                } else {
                    out.push_str(text);
                }
                i += 1;
            }
            let skipped: Vec<&str> = targets[i..].iter().map(|(p, _)| p.as_str()).collect();
            let hint = if skipped.is_empty() {
                "Ask for one path at a time.".to_string()
            } else {
                format!("Ask for one path at a time. Not shown at all: {}.", skipped.join(", "))
            };
            Ok(capped(out, &hint))
        }
        "revue_log" => {
            let (_, srv) = repo_server_for(st, a)?;
            let all = flag(a, "all");
            let limit = arg(a, "limit").filter(|l| l.as_f64().is_some_and(|n| n != 0.0)).cloned().unwrap_or(json!(30));
            let extra = [
                ("branch", if all { None } else { a.get("branch").cloned() }),
                ("all", all.then(|| json!("1"))),
                ("limit", Some(limit)),
                ("skip", a.get("skip").cloned()),
            ];
            let list = get(&srv.0, "/api/log", &json!({}), &extra)?;
            let list = list.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                return Ok("No commits.".into());
            }
            let rows: Vec<String> = list
                .iter()
                .map(|c| {
                    let refs = c["refs"].as_str().filter(|r| !r.is_empty()).map(|r| format!("  ({r})")).unwrap_or_default();
                    format!(
                        "{}  {}  {}  {}{refs}",
                        c["short"].as_str().unwrap_or(""),
                        ymd(c["time"].as_i64().unwrap_or(0)),
                        c["author"].as_str().unwrap_or(""),
                        c["subject"].as_str().unwrap_or("")
                    )
                })
                .collect();
            Ok(capped(rows.join("\n"), "Use a smaller limit."))
        }
        "revue_definitions" => {
            let (_, _, srv) = server_for(st, a)?;
            let extra = [("name", a.get("name").cloned()), ("from", a.get("path").cloned()), ("side", a.get("side").cloned())];
            let res = get(&srv.0, "/api/def", a, &extra)?;
            let candidates = res["candidates"].as_array().cloned().unwrap_or_default();
            if candidates.is_empty() {
                return Ok(format!("No definition found for {}.", a.get("name").map(js_string).unwrap_or_default()));
            }
            let jump = &res["jump"];
            Ok(candidates
                .iter()
                .map(|d| {
                    let star = if !jump.is_null() && d["path"] == jump["path"] && d["line"] == jump["line"] { "* " } else { "" };
                    format!("{star}{}:{}  {}  {}", d["path"].as_str().unwrap_or(""), d["line"], d["kind"].as_str().unwrap_or(""), d["text"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "revue_status" => {
            // A folder's servers of either kind: its repo's, and a plain folder's (--no-git).
            let root = if arg_str(a, "repo").is_some() { Some(repo_root(a)?.0) } else { None };
            let mut ports: Vec<(u16, String, bool)> = Vec::new();
            let mut add = |port: u16, r: String, git: bool| match ports.iter_mut().find(|(p, _, _)| *p == port) {
                Some(e) => (e.1, e.2) = (r, git),
                None => ports.push((port, r, git)),
            };
            for ((r, git), rec) in st.servers.lock().unwrap().iter() {
                add(rec.port, r.clone(), *git);
            }
            for e in discovery::list_servers() {
                add(e["port"].as_u64().unwrap_or(0) as u16, e["root"].as_str().unwrap_or("").into(), e["git"].as_bool().unwrap_or(true));
            }
            let mut out = Vec::new();
            for (port, r, git) in ports {
                if root.as_ref().is_some_and(|root| *root != r) {
                    continue;
                }
                if let Some(s) = probe(port, &r, git) {
                    out.push(describe(&s));
                }
            }
            if out.is_empty() {
                return Ok(format!("No revue server is running{}.", root.map(|r| format!(" for {r}")).unwrap_or_default()));
            }
            Ok(out.join("\n"))
        }
        "revue_stop" => {
            let (root, git) = repo_root(a)?;
            let key = (root.clone(), git);
            let rec = st.servers.lock().unwrap().get(&key).cloned();
            if let Some(Rec { server: Some(srv), port, .. }) = &rec {
                st.servers.lock().unwrap().remove(&key);
                srv.close();
                return Ok(format!("Stopped the revue server for {root} (port {port})."));
            }
            let mut ports: Vec<u16> = rec.iter().map(|r| r.port).collect();
            for e in discovery::list_servers().into_iter().filter(|e| is_server_for(e, &root, git)) {
                let p = e["port"].as_u64().unwrap_or(0) as u16;
                if !ports.contains(&p) {
                    ports.push(p);
                }
            }
            for port in ports.into_iter().filter(|p| *p != 0) {
                let Some(s) = probe(port, &root, git) else { continue };
                let pid = discovery::pid_of(&s["pid"]).unwrap_or(0);
                // Whatever answers on the port only names a pid; only this user's discovery file for the same port and
                // root, written by that very process, vouches for it. A detached server that died without cleaning up
                // leaves its port free for any local process to bind and claim to be it.
                let Some(entry) = registered(port, &root, pid) else {
                    return Ok(format!(
                        "Port {port} answers as the revue server for {root} (pid {pid}), but no server with that pid is registered for it; not stopping anything."
                    ));
                };
                match entry["lifetime"].as_str() {
                    Some("session") => {
                        return Ok(format!("The revue server for {root} belongs to another agent session (pid {pid}); it stops when that session ends."))
                    }
                    Some("detached") => {}
                    _ => return Ok(format!("The revue server for {root} was started by hand (pid {pid}, port {port}); stop it from its terminal.")),
                }
                terminate(pid);
                st.servers.lock().unwrap().remove(&key);
                return Ok(format!("Stopped the revue server for {root} (pid {pid}, port {port})."));
            }
            st.servers.lock().unwrap().remove(&key);
            Ok(format!("No revue server is running for {root}."))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

// ---------------------------------------------------------------- JSON-RPC
const INSTRUCTIONS: &str = "revue is a git change viewer in the user's browser. When the user asks to see, show or review \
code changes (\"show me the changes\", \"what did you change?\", \"let me see the diff\", \"review this branch\"), open them in \
revue with revue_open instead of printing a diff in the chat. Use revue_show to put a file and line range in front of the user \
(for example the code you just changed or want them to review), and revue_annotate to leave a note on specific lines that the \
user sees inline. revue_changes, revue_diff, revue_log and revue_definitions read the same views (uncommitted work, a commit, \
or a branch vs its base) as text for you; they show the user nothing. A folder that isn't in a git repo (or any folder with \
git: false) opens as a plain folder: revue_open, revue_show, revue_annotate and revue_definitions work on its files, and \
revue_changes, revue_diff and revue_log say it isn't a git repository.";

fn send(out: &Mutex<std::io::Stdout>, mut m: Map<String, Value>) {
    let mut msg = Map::new();
    msg.insert("jsonrpc".into(), json!("2.0"));
    msg.append(&mut m);
    let mut o = out.lock().unwrap();
    let _ = writeln!(o, "{}", Value::Object(msg));
    let _ = o.flush();
}

fn reply(out: &Mutex<std::io::Stdout>, id: &Value, res: Result<Value, (i64, String)>) {
    let mut m = Map::new();
    m.insert("id".into(), id.clone());
    match res {
        Ok(r) => m.insert("result".into(), r),
        Err((code, message)) => m.insert("error".into(), json!({ "code": code, "message": message })),
    };
    send(out, m);
}

// Protocol versions this server speaks, newest first. A client asking for one of them gets it; any other request
// gets the newest, and the client decides whether it can work with that.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn negotiate(asked: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS.iter().find(|v| Some(**v) == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0])
}

pub fn run(opts: Opts) -> ! {
    let st = Arc::new(State {
        opts,
        exe: own_exe(),
        servers: Mutex::new(HashMap::new()),
        client_name: Mutex::new(None),
        remarks: Mutex::new(Vec::new()),
    });
    let out = Arc::new(Mutex::new(std::io::stdout()));
    // One tool call at a time, so concurrent calls can't each start a server for the same repo.
    let (tx, rx) = mpsc::channel::<(Option<Value>, String, Value)>();
    let worker = {
        let (st, out) = (st.clone(), out.clone());
        crate::git::spawn(move || {
            for (id, name, args) in rx {
                st.remarks.lock().unwrap().clear();
                let result = match run_tool(&st, &name, &args) {
                    Ok(mut text) => {
                        for r in st.remarks.lock().unwrap().drain(..) {
                            text = format!("{text}\n\nNote: {r}");
                        }
                        json!({ "content": [{ "type": "text", "text": text }] })
                    }
                    Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
                };
                if let Some(id) = id {
                    reply(&out, &id, Ok(result));
                }
            }
        })
        .unwrap_or_else(|e| {
            eprintln!("revue mcp: could not start a thread: {e}");
            discovery::exit(1)
        })
    };
    let names: Vec<String> = tools().iter().map(|t| t["name"].as_str().unwrap_or("").to_string()).collect();
    // Raw lines, so one line that isn't UTF-8 gets a parse error instead of ending the session.
    let mut stdin = std::io::stdin().lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stdin.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            reply(&out, &Value::Null, Err((-32700, "parse error: not UTF-8".into())));
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => {
                reply(&out, &Value::Null, Err((-32700, "parse error".into())));
                continue;
            }
        };
        // MCP 2025-06-18 dropped JSON-RPC batching (2025-03-26 had it), and no client we know of sends batches.
        if msg.is_array() {
            reply(&out, &Value::Null, Err((-32600, "invalid request: batches are not supported; send one message per line".into())));
            continue;
        }
        let id = msg.get("id").cloned(); // present (even null) = a request that wants an answer
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // With an id and no method it's a malformed request; without either, a response to a request we never send.
            if !msg.is_object() || (id.is_some() && msg.get("result").is_none() && msg.get("error").is_none()) {
                reply(&out, id.as_ref().unwrap_or(&Value::Null), Err((-32600, "invalid request: no method".into())));
            }
            continue;
        };
        let params = &msg["params"];
        let res: Result<Value, (i64, String)> = match method {
            "initialize" => {
                *st.client_name.lock().unwrap() = params["clientInfo"]["name"].as_str().map(|n| n.chars().take(60).collect());
                Ok(json!({
                    "protocolVersion": negotiate(params["protocolVersion"].as_str()),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "revue", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                if names.contains(&name) {
                    let args = if params["arguments"].is_object() { params["arguments"].clone() } else { json!({}) };
                    let _ = tx.send((id, name, args));
                    continue;
                }
                Err((-32602, format!("unknown tool {}", params["name"].as_str().unwrap_or("undefined"))))
            }
            m => Err((-32601, format!("method not found: {m}"))),
        };
        if let Some(id) = id {
            reply(&out, &id, res);
        }
    }
    // The client closed stdin: the session is over once in-flight calls finish. Session servers go with us.
    drop(tx);
    let _ = worker.join();
    discovery::exit(0)
}
