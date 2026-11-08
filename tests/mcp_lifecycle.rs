// How `revue mcp` treats servers that run older code than is on disk. The code file is faked with REVUE_CODE_FILE
// (honoured by debug builds with REVUE_TEST=1); every MCP and server here uses its own XDG_RUNTIME_DIR.
#![cfg(unix)]
mod common;
use common::*;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    runtime: std::path::PathBuf,
    next: u64,
}

impl Mcp {
    fn start(runtime: &Path, code: &Path) -> Mcp {
        let mut child = isolate(&mut Command::new(env!("CARGO_BIN_EXE_revue")))
            .arg("mcp")
            .env("XDG_RUNTIME_DIR", runtime)
            .env("REVUE_TEST", "1")
            .env("REVUE_CODE_FILE", code)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap_or(0) == 0 || tx.send(line).is_err() {
                return;
            }
        });
        let stdin = child.stdin.take();
        let mut m = Mcp { child, stdin, lines, runtime: runtime.to_path_buf(), next: 0 };
        m.call("initialize", json!({}));
        m
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        writeln!(self.stdin.as_mut().unwrap(), "{}", json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).unwrap();
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(30)).expect("no reply from the MCP server");
            let m: Value = serde_json::from_str(&line).unwrap();
            if m["id"] == id {
                return m;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> String {
        let r = self.call("tools/call", json!({ "name": name, "arguments": args }));
        r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    fn registered(&self, root: &str) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(self.runtime.join("revue")) else { return Vec::new() };
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|e| serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).ok()?).ok())
            .filter(|e| e["root"] == root)
            .collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let Ok(rd) = std::fs::read_dir(self.runtime.join("revue")) else { return };
        for e in rd.flatten() {
            let v: Option<Value> = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str(&t).ok());
            if let Some(pid) = v.and_then(|v| v["pid"].as_i64()) {
                if pid as u32 != std::process::id() {
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                }
            }
        }
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn alive(pid: u64) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// Replace the file the way an install does: a new file in its place.
fn rewrite(code: &Path) {
    std::fs::remove_file(code).unwrap();
    std::fs::write(code, "v2").unwrap();
}

fn repo() -> TempDir {
    let dir = TempDir::new("life-repo");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    write(dir.path(), "a.txt", "two\n");
    dir
}

#[test]
fn a_stale_detached_server_is_replaced() {
    let (dir, rt, codes) = (repo(), TempDir::new("life-rt"), TempDir::new("life-code"));
    let root = dir.path().to_str().unwrap();
    let code = codes.path().join("revue.bin");
    std::fs::write(&code, "v1").unwrap();
    let mut m = Mcp::start(rt.path(), &code);
    assert!(m.tool("revue_changes", json!({ "repo": root, "persist": true })).contains("file(s) changed"));
    let first = m.registered(root)[0]["pid"].as_u64().unwrap();

    rewrite(&code);
    let text = m.tool("revue_changes", json!({ "repo": root, "persist": true }));
    assert!(text.contains("file(s) changed") && text.contains("replaced a detached revue server whose code changed on disk"), "{text}");
    let now: Vec<u64> = m.registered(root).iter().map(|e| e["pid"].as_u64().unwrap()).collect();
    assert_eq!(now.len(), 1, "{now:?}");
    assert_ne!(now[0], first, "the stale server is still the one in use");
    assert!(!alive(first), "the stale server is still running");
}

#[test]
fn a_stale_server_started_by_hand_is_reused_with_a_note() {
    let (dir, rt, codes) = (repo(), TempDir::new("life-rt"), TempDir::new("life-code"));
    let root = dir.path().to_str().unwrap();
    let code = codes.path().join("revue.bin");
    std::fs::write(&code, "v1").unwrap();
    let rt_s = rt.path().to_str().unwrap();
    let code_s = code.to_str().unwrap();
    let by_hand = Server::try_start(dir.path(), &[], &[("XDG_RUNTIME_DIR", rt_s), ("REVUE_CODE_FILE", code_s)]).ok().unwrap();
    rewrite(&code);

    let mut m = Mcp::start(rt.path(), &code);
    let text = m.tool("revue_changes", json!({ "repo": root }));
    assert!(text.contains("file(s) changed"), "{text}");
    assert!(text.contains(&format!("the code of the revue server on port {} (external) changed on disk", by_hand.port)) && text.contains("restart it"), "{text}");
    assert_eq!(by_hand.get("/api/info", &[]).status, 200, "the server started by hand must keep running");
}
