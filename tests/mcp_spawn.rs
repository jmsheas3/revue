// `revue mcp` must still start detached servers after its binary is replaced on disk (as `cargo install` does while
// an agent session runs). Linux then reports the running executable as "<path> (deleted)".
#![cfg(unix)]
mod common;
use common::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    // Send a request and wait for the response with its id (one JSON object per line).
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

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({ "name": name, "arguments": args }))["result"].clone()
    }
}

fn copy_exe(to: &Path) {
    std::fs::copy(env!("CARGO_BIN_EXE_revue"), to).unwrap();
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// Stop every server registered in the test's discovery directory, so a failure leaves nothing running.
fn stop_registered(runtime: &Path) {
    let Ok(rd) = std::fs::read_dir(runtime.join("revue")) else { return };
    for e in rd.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        if let Some(pid) = v["pid"].as_i64() {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }
}

struct Cleanup<'a>(&'a Path);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        stop_registered(self.0);
    }
}

#[test]
fn detached_servers_start_after_the_binary_is_replaced() {
    let bin = TempDir::new("mcp-bin");
    let runtime = TempDir::new("mcp-runtime");
    let repo = TempDir::new("mcp-repo");
    let root = repo.path();
    init(root);
    write(root, "a.txt", "one\n");
    commit_all(root, "init");
    write(root, "a.txt", "two\n");

    let exe = bin.path().join("revue");
    copy_exe(&exe);
    let _cleanup = Cleanup(runtime.path());
    let mut child = isolate(&mut Command::new(&exe))
        .arg("mcp")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.take();
    let mut mcp = Mcp { child, stdin, stdout, next_id: 0 };
    mcp.call("initialize", json!({}));

    // Replace the binary the way cargo install does: a new file renamed over the old one.
    let fresh = bin.path().join("revue.new");
    copy_exe(&fresh);
    std::fs::rename(&fresh, &exe).unwrap();

    let res = mcp.tool("revue_changes", json!({ "repo": root, "persist": true }));
    let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
    assert_ne!(res["isError"], true, "revue_changes failed: {text}");
    assert!(text.contains("1 file(s) changed"), "{text}");

    let res = mcp.tool("revue_stop", json!({ "repo": root }));
    let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
    assert!(text.contains("Stopped"), "{text}");

    drop(mcp.stdin.take()); // the session is over once stdin closes
    let status = mcp.child.wait().unwrap();
    assert!(status.success());
}
