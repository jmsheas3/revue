// The MCP server's JSON-RPC handling and its guard on revue_stop. Each test runs `revue mcp` with its own
// XDG_RUNTIME_DIR, so it never sees (or stops) the user's real servers.
#![cfg(unix)]
mod common;
use common::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    runtime: TempDir,
}

impl Mcp {
    fn start(env: &[(&str, &str)]) -> Mcp {
        let runtime = TempDir::new("mcp-rt");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_revue"));
        isolate(&mut cmd).arg("mcp").env("XDG_RUNTIME_DIR", runtime.path()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        // Lines arrive on a channel, so a reply that never comes fails the test instead of hanging it.
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || loop {
            let mut line = String::new();
            if stdout.read_line(&mut line).unwrap_or(0) == 0 || tx.send(line).is_err() {
                return;
            }
        });
        let stdin = child.stdin.take();
        Mcp { child, stdin, lines, runtime }
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let w = self.stdin.as_mut().unwrap();
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    // The next message on stdout (one JSON value per line).
    fn read(&mut self) -> Value {
        let line = self.lines.recv_timeout(Duration::from_secs(20)).expect("no reply from the MCP server (it closed stdout or never answered)");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send_raw(format!("{msg}\n").as_bytes());
        loop {
            let m = self.read();
            if m["id"] == id {
                return m;
            }
        }
    }

    fn tool(&mut self, id: u64, name: &str, args: Value) -> String {
        let r = self.call(id, "tools/call", json!({ "name": name, "arguments": args }));
        r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    // Servers registered in this MCP's discovery directory.
    fn registered(&self) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(self.runtime.path().join("revue")) else { return Vec::new() };
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
            .filter_map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
            .collect()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Stop anything this test registered, then the MCP itself.
        for e in self.registered() {
            if let Some(pid) = e["pid"].as_i64() {
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        }
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn repo() -> TempDir {
    let dir = TempDir::new("mcp-repo");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    write(dir.path(), "a.txt", "two\n");
    dir
}

#[test]
fn bad_lines_get_a_parse_error_and_the_session_goes_on() {
    let mut m = Mcp::start(&[]);
    m.send_raw(b"\xff\xfe not utf-8\n");
    let r = m.read();
    assert_eq!(r["error"]["code"], -32700, "{r}");
    m.send_raw(b"{not json\n");
    assert_eq!(m.read()["error"]["code"], -32700);
    assert_eq!(m.call(1, "ping", json!({}))["result"], json!({}));
}

#[test]
fn batches_and_requests_without_a_method_are_invalid_requests() {
    let mut m = Mcp::start(&[]);
    m.send_raw(b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n");
    let r = m.read();
    assert_eq!(r["error"]["code"], -32600, "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("batch"), "{r}");
    m.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":7}\n");
    let r = m.read();
    assert_eq!((r["id"].clone(), r["error"]["code"].clone()), (json!(7), json!(-32600)), "{r}");
    // A response to something we never asked (no method, no id) is still ignored.
    m.send_raw(b"{\"jsonrpc\":\"2.0\",\"result\":{}}\n");
    assert_eq!(m.call(2, "ping", json!({}))["result"], json!({}));
}

#[test]
fn initialize_answers_with_a_protocol_version_it_supports() {
    let mut m = Mcp::start(&[]);
    let v = |m: &mut Mcp, id, want: &str| m.call(id, "initialize", json!({ "protocolVersion": want }))["result"]["protocolVersion"].clone();
    assert_eq!(v(&mut m, 1, "2025-03-26"), "2025-03-26");
    assert_eq!(v(&mut m, 2, "2099-01-01"), "2025-06-18");
    assert_eq!(m.call(3, "initialize", json!({}))["result"]["protocolVersion"], "2025-06-18");
}

// Clients pick revue for "show me the changes" only if the instructions and revue_open say to.
#[test]
fn instructions_point_requests_to_see_changes_at_revue_open() {
    let mut m = Mcp::start(&[]);
    let init = m.call(1, "initialize", json!({}));
    let ins = init["result"]["instructions"].as_str().unwrap();
    assert!(ins.contains("show me the changes") && ins.contains("revue_open"), "{ins}");
    let tools = m.call(2, "tools/list", json!({}));
    let desc = |name: &str| tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["description"].as_str().unwrap().to_string();
    assert!(desc("revue_open").contains("asks to see or review code changes"));
    for name in ["revue_changes", "revue_diff"] {
        assert!(desc(name).contains("to show changes to the user, use revue_open"), "{name}");
    }
}

// Something else answering on a dead detached server's port, claiming to be it, must not get a pid of its choosing
// killed by revue_stop.
#[test]
fn revue_stop_only_stops_a_registered_server() {
    let dir = repo();
    let root = dir.path().to_str().unwrap().to_string();
    let mut m = Mcp::start(&[]);
    m.call(1, "initialize", json!({}));
    let text = m.tool(2, "revue_changes", json!({ "repo": root, "persist": true }));
    assert!(text.contains("file(s) changed"), "{text}");
    let e = m.registered().into_iter().find(|e| e["root"] == root.as_str()).expect("the detached server registered");
    let (port, pid) = (e["port"].as_u64().unwrap() as u16, e["pid"].as_u64().unwrap() as u32);

    // The detached server dies without cleaning up, and an impostor takes over its port.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let start = Instant::now();
    while alive(pid) && start.elapsed() < Duration::from_secs(5) {
        sleep(20);
    }
    let mut bystander = Command::new("sleep").arg("30").spawn().unwrap();
    let victim = bystander.id();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("the port is free again");
    let claim = json!({ "root": root, "port": port, "pid": victim, "lifetime": "detached", "notes": "memory", "tabs": 0, "navTabs": 0, "annotations": 0 }).to_string();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{claim}", claim.len());
        }
    });

    let text = m.tool(3, "revue_stop", json!({ "repo": root }));
    sleep(200);
    let survived = alive(victim);
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(survived, "revue_stop killed an unrelated process: {text}");
    assert!(text.contains("not stopping anything"), "{text}");
    let _ = Path::new(&root);
}
