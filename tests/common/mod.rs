// Shared by the integration tests: throwaway repos in the OS temp dir, a revue server per test (the binary cargo
// built, with REVUE_TEST=1 so GET /api/debug works), a small HTTP client and an SSE reader. Tests and servers are
// isolated from the user's setup: git ignores the global and system config (GIT_ENV), each server gets its own
// discovery directory, and GIT_* and REVUE_* variables from the shell are dropped (`isolate`).
#![allow(dead_code)]
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[allow(unused_imports)]
pub use serde_json::{json, Value};

// Change entries from /api/tree without their version tag ("v"), for comparing the rest exactly. Every entry (an
// object with a "status") must carry one, 16 hex digits.
pub fn untagged(mut v: Value) -> Value {
    match &mut v {
        Value::Object(o) => {
            if o.contains_key("status") {
                let tag = o.remove("v").unwrap_or_else(|| panic!("no version tag in {o:?}"));
                let tag = tag.as_str().unwrap_or_default();
                assert!(tag.len() == 16 && tag.bytes().all(|b| b.is_ascii_hexdigit()), "bad version tag {tag:?}");
            }
            for x in o.values_mut() {
                *x = untagged(x.take());
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| *x = untagged(x.take())),
        _ => {}
    }
    v
}

pub fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// Polls `cond` until it holds or `ms` pass; returns whether it held. Use it instead of a fixed sleep before an
// assertion that something happened.
pub fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= Duration::from_millis(ms) {
            return false;
        }
        sleep(20);
    }
}

// The user's ~/.gitconfig (signing, hooks, default branch, diff settings) must not change what tests see.
pub const GIT_ENV: &[(&str, &str)] = &[("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

// Every git and revue process a test starts goes through this: it drops each GIT_* and REVUE_* variable this process
// inherited, then sets GIT_ENV. Git exports GIT_DIR, GIT_INDEX_FILE, GIT_WORK_TREE and more to the hooks it runs, so
// `cargo test` run from a hook would otherwise point the fixtures' `git add -A`/`commit` and every server at the
// user's own repo; REVUE_* settings (REVUE_NOTES and the like) would change how a server behaves. Set any variable a
// test needs after calling this.
pub fn isolate(cmd: &mut Command) -> &mut Command {
    for (k, _) in std::env::vars_os() {
        let b = k.as_encoded_bytes();
        if b.starts_with(b"GIT_") || b.starts_with(b"REVUE_") {
            cmd.env_remove(&k);
        }
    }
    cmd.envs(GIT_ENV.iter().copied())
}

// ---------------------------------------------------------------- repos
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> TempDir {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
        let p = std::env::temp_dir().join(format!("revue-{prefix}-{}-{}-{nanos}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(std::fs::canonicalize(&p).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn try_git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = isolate(&mut Command::new("git"))
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn git(root: &Path, args: &[&str]) -> String {
    try_git(root, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

pub fn init(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
}

pub fn commit(root: &Path, msg: &str) {
    git(root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", msg]);
}

pub fn commit_all(root: &Path, msg: &str) {
    git(root, &["add", "-A"]);
    commit(root, msg);
}

pub fn rev(root: &Path, r: &str) -> String {
    git(root, &["rev-parse", r]).trim().to_string()
}

pub fn write(root: &Path, rel: &str, data: impl AsRef<[u8]>) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

pub fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

// "line 1\n" … "line n\n", with line `edit` (0-based) changed.
pub fn lines(n: usize, edit: Option<usize>) -> String {
    (0..n).map(|i| if Some(i) == edit { format!("edited {}\n", i + 1) } else { format!("line {}\n", i + 1) }).collect()
}

// ---------------------------------------------------------------- query strings
pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for &c in s.as_bytes() {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => out.push(c as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}

pub fn qs(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

// ---------------------------------------------------------------- HTTP
pub struct Res {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Res {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }
}

fn read_line(r: &mut impl BufRead) -> String {
    let mut s = String::new();
    r.read_line(&mut s).unwrap();
    s.trim_end_matches(['\r', '\n']).to_string()
}

fn read_head(r: &mut impl BufRead) -> (u16, Vec<(String, String)>) {
    let status_line = read_line(r);
    let status = status_line.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("bad status line {status_line:?}"));
    let mut headers = Vec::new();
    loop {
        let l = read_line(r);
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    (status, headers)
}

fn read_chunk(r: &mut impl BufRead) -> Option<Vec<u8>> {
    let size = usize::from_str_radix(read_line(r).trim(), 16).ok()?;
    if size == 0 {
        return None;
    }
    let mut buf = vec![0; size];
    r.read_exact(&mut buf).ok()?;
    let mut crlf = [0; 2];
    r.read_exact(&mut crlf).ok()?;
    Some(buf)
}

fn send(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> BufReader<TcpStream> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nconnection: close\r\ncontent-length: {}\r\n", body.len());
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("host: 127.0.0.1:{port}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    BufReader::new(s)
}

pub fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Res {
    let mut r = send(port, method, path, headers, body);
    let (status, headers) = read_head(&mut r);
    let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    if get("transfer-encoding").is_some_and(|t| t.contains("chunked")) {
        while let Some(c) = read_chunk(&mut r) {
            body.extend(c);
        }
    } else if let Some(n) = get("content-length").and_then(|n| n.parse::<usize>().ok()) {
        body.resize(n, 0);
        r.read_exact(&mut body).unwrap();
    } else if status != 304 {
        r.read_to_end(&mut body).unwrap();
    }
    Res { status, headers, body }
}

// Only the status and the (lowercased) headers of a GET, then the connection is dropped (which also ends an event
// stream).
pub fn head_only(port: u16, path: &str, headers: &[(&str, &str)]) -> (u16, Vec<(String, String)>) {
    let mut r = send(port, "GET", path, headers, b"");
    let head = read_head(&mut r);
    let _ = r.get_ref().shutdown(Shutdown::Both);
    head
}

pub fn status_only(port: u16, path: &str, host: &str) -> u16 {
    head_only(port, path, &[("host", host)]).0
}

// ---------------------------------------------------------------- servers
pub struct Server {
    child: Child,
    pub port: u16,
    stderr: Arc<Mutex<String>>,
    // Its own discovery directory, unless the test passed XDG_RUNTIME_DIR; removed after the server stops.
    _runtime: Option<TempDir>,
}

// Kills a child that never became a Server (it exited, or printed no usable URL).
struct Starting(Option<Child>);

impl Drop for Starting {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub struct StartError {
    pub code: Option<i32>,
    pub stderr: String,
}

impl Server {
    pub fn start(root: &Path) -> Server {
        Server::try_start(root, &[], &[]).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr))
    }

    // Spawns `revue <root> --no-open --port 0 --idle-timeout 5 <args>` and waits for the URL it prints. The idle
    // timeout means a server orphaned by a killed test run exits on its own a few minutes later.
    pub fn try_start(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Server, StartError> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_revue"));
        cmd.arg(root).args(["--no-open"]);
        if !args.contains(&"--port") {
            cmd.args(["--port", "0"]);
        }
        if !args.contains(&"--idle-timeout") {
            cmd.args(["--idle-timeout", "5"]);
        }
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        isolate(&mut cmd).env("REVUE_TEST", "1");
        let runtime = if env.iter().any(|(k, _)| *k == "XDG_RUNTIME_DIR") {
            None
        } else {
            let rt = TempDir::new("runtime");
            cmd.env("XDG_RUNTIME_DIR", rt.path());
            Some(rt)
        };
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut err_pipe = child.stderr.take().unwrap();
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err_pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut starting = Starting(Some(child));
        let mut seen = String::new();
        loop {
            let mut line = String::new();
            if out.read_line(&mut line).unwrap_or(0) == 0 {
                let code = starting.0.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
                sleep(50); // let the stderr thread catch up
                return Err(StartError { code, stderr: stderr.lock().unwrap().clone() });
            }
            seen.push_str(&line);
            if let Some(i) = seen.find("http://127.0.0.1:") {
                let digits: String = seen[i + 17..].chars().take_while(|c| c.is_ascii_digit()).collect();
                let Ok(port) = digits.parse::<u16>() else {
                    // `starting` kills the child on the way out.
                    return Err(StartError { code: None, stderr: format!("no port in {seen:?}") });
                };
                // Keep draining stdout so the server never blocks on a full pipe.
                std::thread::spawn(move || {
                    let mut sink = Vec::new();
                    let _ = out.read_to_end(&mut sink);
                });
                let child = starting.0.take().unwrap();
                return Ok(Server { child, port, stderr, _runtime: runtime });
            }
        }
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    // The discovery directory this server registers in (XDG_RUNTIME_DIR/revue), when the helper made one.
    pub fn discovery_dir(&self) -> Option<PathBuf> {
        self._runtime.as_ref().map(|d| d.path().join("revue"))
    }

    // Waits up to `ms` for the server to exit on its own; its exit code, or None if it's still running.
    pub fn wait_exit(&mut self, ms: u64) -> Option<i32> {
        let mut code = None;
        wait_until(ms, || match self.child.try_wait() {
            Ok(Some(st)) => {
                code = Some(st.code().unwrap_or(-1));
                true
            }
            _ => false,
        });
        code
    }

    pub fn get(&self, path: &str, params: &[(&str, &str)]) -> Res {
        let p = if params.is_empty() { path.to_string() } else { format!("{path}?{}", qs(params)) };
        request(self.port, "GET", &p, &[], b"")
    }

    // GET returning (status, JSON body).
    pub fn json(&self, path: &str, params: &[(&str, &str)]) -> (u16, Value) {
        let r = self.get(path, params);
        (r.status, r.json())
    }

    pub fn body(&self, path: &str, params: &[(&str, &str)]) -> Value {
        self.json(path, params).1
    }

    // A write: POST with the x-revue header.
    pub fn post(&self, path: &str, body: &str, params: &[(&str, &str)]) -> (u16, Value) {
        let p = if params.is_empty() { path.to_string() } else { format!("{path}?{}", qs(params)) };
        let r = request(self.port, "POST", &p, &[("x-revue", "1")], body.as_bytes());
        (r.status, r.json())
    }

    pub fn debug(&self, params: &[(&str, &str)]) -> Value {
        self.body("/api/debug", params)
    }

    pub fn watched(&self) -> Vec<String> {
        self.debug(&[])["watched"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
    }

    // The files read since the last debug() call (and clears the log).
    pub fn reads(&self) -> Vec<String> {
        self.read_sizes().into_iter().map(|(p, _)| p).collect()
    }

    // The same, with how many bytes each read took of its file.
    pub fn read_sizes(&self) -> Vec<(String, u64)> {
        let reads = self.debug(&[])["reads"].clone();
        reads.as_array().unwrap().iter().map(|r| (r["path"].as_str().unwrap().to_string(), r["bytes"].as_u64().unwrap())).collect()
    }

    // An event stream, settled: registered, and with the messages for changes made before it connected (a commit
    // the test just made, say, still in the 250 ms debounce) received and cleared, so they can't pass for later ones.
    pub fn events(&self) -> Events {
        let ev = Events::connect(self.port, "/api/events");
        self.settle(&ev);
        ev.clear();
        ev
    }

    // Waits until `ev` has every message for the changes made so far. /api/debug says whether the watcher still has
    // changes to send (`unsent`: queued in the kernel, read, or held for the debounce), and with ?mark=n then sends
    // `: mark n` to every open stream, after any message sent before. A stream that gets a mark sent while nothing
    // was unsent has had them all; one that isn't registered yet gets none, so marks are sent until one comes. Marks
    // only grow, so one meant for another stream of the same server never passes for this one's. On macOS and
    // Windows a change the OS hasn't handed to the watcher yet can't be seen, so the stream also waits the 400 ms it
    // always did there.
    pub fn settle(&self, ev: &Events) {
        let start = Instant::now();
        let floor = Duration::from_millis(if cfg!(target_os = "linux") { 0 } else { 400 });
        let mut mark = 0;
        let settled = wait_until(10_000, || {
            if start.elapsed() < floor {
                return false;
            }
            let unsent;
            (mark, unsent) = self.send_mark();
            !unsent && ev.wait_mark(mark, 500)
        });
        assert!(settled, "the event stream never settled: unsent {}, last mark {mark}, seen {}", self.debug(&[])["unsent"], ev.mark());
    }

    // Sends the next mark to every open event stream (see settle): the mark, and whether the watcher had changes to
    // send just before it.
    pub fn send_mark(&self) -> (u64, bool) {
        static MARK: AtomicU64 = AtomicU64::new(0);
        let mark = MARK.fetch_add(1, Ordering::SeqCst) + 1;
        let unsent = self.debug(&[("mark", &mark.to_string())])["unsent"].as_bool().expect("unsent");
        (mark, unsent)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SIGTERM, so the server exits normally and removes its discovery file.
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------- server-sent events
pub struct Events {
    stream: TcpStream,
    // the data of each unnamed message, in order
    data: Arc<Mutex<Vec<String>>>,
    // (event name, data) of each named message (navigate, annotations), in order
    named: Arc<Mutex<Vec<(String, String)>>>,
    // the last `: mark n` comment received (see Server::settle)
    mark: Arc<AtomicU64>,
}

impl Events {
    pub fn connect(port: u16, path: &str) -> Events {
        let mut r = send(port, "GET", path, &[], b"");
        let (status, _) = read_head(&mut r);
        assert_eq!(status, 200);
        let stream = r.get_ref().try_clone().unwrap();
        stream.set_read_timeout(None).unwrap();
        let _ = r.get_ref().set_read_timeout(None);
        let data = Arc::new(Mutex::new(Vec::new()));
        let named = Arc::new(Mutex::new(Vec::new()));
        let mark = Arc::new(AtomicU64::new(0));
        let (sink, named_sink, mark_sink) = (data.clone(), named.clone(), mark.clone());
        std::thread::spawn(move || {
            let mut pending = String::new();
            while let Some(chunk) = read_chunk(&mut r) {
                pending.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = pending.find("\n\n") {
                    let msg: String = pending.drain(..end + 2).collect();
                    let event = msg.lines().find_map(|l| l.strip_prefix("event: ")).map(String::from);
                    for l in msg.lines() {
                        if let Some(n) = l.strip_prefix(": mark ").and_then(|n| n.parse().ok()) {
                            mark_sink.store(n, Ordering::SeqCst);
                        }
                        if let Some(d) = l.strip_prefix("data: ") {
                            match &event {
                                Some(e) => named_sink.lock().unwrap().push((e.clone(), d.to_string())),
                                None => sink.lock().unwrap().push(d.to_string()),
                            }
                        }
                    }
                }
            }
        });
        Events { stream, data, named, mark }
    }

    // The last mark received (see Server::settle).
    pub fn mark(&self) -> u64 {
        self.mark.load(Ordering::SeqCst)
    }

    // Waits up to `ms` for mark `n` (or a later one); returns whether it came.
    pub fn wait_mark(&self, n: u64, ms: u64) -> bool {
        wait_until(ms, || self.mark() >= n)
    }

    // The data of the named messages called `event` received so far, waiting up to `ms` for at least one.
    pub fn named(&self, event: &str, ms: u64) -> Vec<String> {
        let get = || self.named.lock().unwrap().iter().filter(|(e, _)| e == event).map(|(_, d)| d.clone()).collect::<Vec<_>>();
        wait_until(ms, || !get().is_empty());
        get()
    }

    pub fn clear(&self) {
        self.data.lock().unwrap().clear();
    }

    pub fn data(&self) -> Vec<String> {
        self.data.lock().unwrap().clone()
    }

    // How many change messages ("worktree" or "git") arrived.
    pub fn changes(&self) -> usize {
        self.data().iter().filter(|d| *d == "worktree" || *d == "git").count()
    }

    // Waits up to `ms` for at least `n` change messages in total; returns whether they came.
    pub fn wait_changes(&self, n: usize, ms: u64) -> bool {
        wait_until(ms, || self.changes() >= n)
    }

    // The next unnamed message, waiting up to `ms`.
    pub fn next(&self, ms: u64) -> Option<String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(ms) {
            {
                let mut d = self.data.lock().unwrap();
                if !d.is_empty() {
                    return Some(d.remove(0));
                }
            }
            sleep(50);
        }
        None
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
