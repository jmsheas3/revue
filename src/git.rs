// Running the git CLI, the error type every request handler returns, and starting threads. Diffs, grep, the untracked
// file list and `git switch` go through the CLI so their output is exactly git's; everything else reads the repo in
// process (repo.rs).
use std::ffi::OsStr;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{JoinHandle, Scope, ScopedJoinHandle};

pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

// An error the request handler answers with `status` (bad input, missing file) instead of 500.
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn http_error(status: u16, message: impl Into<String>) -> Error {
    Error { status, message: message.into() }
}

pub fn internal(message: impl Into<String>) -> Error {
    http_error(500, message)
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        internal(e.to_string())
    }
}

// Threads. The OS may refuse one (EAGAIN near the process's or the user's limit), and std::thread::spawn panics then,
// taking down the thread that asked (the accept loop, say); these return the error instead. A request that can't get
// a thread fails with 503 (`no_thread`) and the server goes on.
pub fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> std::io::Result<JoinHandle<T>> {
    refuse_spawn()?;
    std::thread::Builder::new().spawn(f)
}

pub fn spawn_scoped<'s, T: Send + 's>(s: &'s Scope<'s, '_>, f: impl FnOnce() -> T + Send + 's) -> Result<ScopedJoinHandle<'s, T>> {
    refuse_spawn().and_then(|_| std::thread::Builder::new().spawn_scoped(s, f)).map_err(no_thread)
}

pub fn no_thread(e: std::io::Error) -> Error {
    http_error(503, format!("could not start a thread, try again: {e}"))
}

// Test hook (/api/debug?failSpawns=n, debug builds only): the next n spawns above fail as if the OS refused them.
static FAIL_SPAWNS: AtomicUsize = AtomicUsize::new(0);

pub fn fail_spawns(n: usize) {
    FAIL_SPAWNS.store(n, Ordering::SeqCst);
}

// Newer Rust deprecates fetch_update for try_update, which is newer than our rust-version (1.88).
#[allow(deprecated)]
fn refuse_spawn() -> std::io::Result<()> {
    if cfg!(debug_assertions) && FAIL_SPAWNS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "Resource temporarily unavailable (failSpawns)"));
    }
    Ok(())
}

// git's messages for refs and paths that don't exist describe the request, not a server fault.
pub fn git_error(msg: String) -> Error {
    let lower = msg.to_lowercase();
    if ["bad revision", "unknown revision", "not a valid object name", "invalid object name"].iter().any(|s| lower.contains(s)) {
        return http_error(400, msg);
    }
    if msg.contains("does not exist in '") || msg.contains("exists on disk, but not in '") {
        return http_error(404, msg);
    }
    internal(msg)
}

// Run git with core.quotepath=off and colors off whatever the user's config says (color.ui=always would put escape
// codes in diffs and in grep output). Exit codes outside `allow` are errors carrying git's stderr.
// A git command in `root` whose output doesn't depend on the user's config: paths unquoted, no colors. Every git call
// goes through this, including ones that stream their output instead of using git_bytes.
pub fn command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.quotepath=off", "-c", "color.ui=never", "-c", "color.diff=never", "-c", "color.grep=never"]);
    cmd.current_dir(root);
    cmd
}

pub fn git_bytes<S: AsRef<OsStr>>(root: &Path, args: &[S], allow: &[i32], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut cmd = command(root);
    cmd.args(args);
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| internal(format!("git: {e}")))?;
    let out = std::thread::scope(|s| {
        if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
            // Written from another thread so a large output can't deadlock against a full stdin pipe.
            if let Err(e) = spawn_scoped(s, move || {
                let _ = stdin.write_all(data);
            }) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        child.wait_with_output().map_err(|e| internal(format!("git: {e}")))
    })?;
    let code = out.status.code().unwrap_or(-1);
    if allow.contains(&code) {
        return Ok(out.stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if stderr.is_empty() {
        let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string_lossy().into_owned()).collect();
        return Err(git_error(format!("Command failed: git {}", args.join(" "))));
    }
    Err(git_error(stderr))
}

// The first `n` bytes of a git command's output (all of it when shorter). git is killed once they are in, so a big
// object is never read through just to see how it starts.
pub fn git_head<S: AsRef<OsStr>>(root: &Path, args: &[S], n: usize) -> Result<Vec<u8>> {
    let mut cmd = command(root);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| internal(format!("git: {e}")))?;
    let mut buf = Vec::with_capacity(n);
    let read = child.stdout.take().unwrap().take(n as u64).read_to_end(&mut buf);
    if buf.len() == n {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(buf);
    }
    let out = child.wait_with_output().map_err(|e| internal(format!("git: {e}")))?;
    if out.status.success() {
        read?;
        return Ok(buf);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(git_error(if stderr.is_empty() { "Command failed: git".into() } else { stderr }))
}

pub fn git<S: AsRef<OsStr>>(root: &Path, args: &[S]) -> Result<String> {
    git_allow(root, args, &[0])
}

pub fn git_allow<S: AsRef<OsStr>>(root: &Path, args: &[S], allow: &[i32]) -> Result<String> {
    git_bytes(root, args, allow, None).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn split_z(s: &str) -> Vec<String> {
    s.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}
