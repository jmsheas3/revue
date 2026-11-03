// Signals: the server exits cleanly on SIGTERM and SIGINT (removing its discovery file), and the processes it starts
// (git, the browser opener, detached servers) don't inherit a blocked signal mask, so they can still be interrupted.
#![cfg(target_os = "linux")]
mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn repo() -> TempDir {
    let dir = TempDir::new("signals");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    write(dir.path(), "a.txt", "two\n");
    dir
}

// A process inherits the blocked set of the thread that starts it. A shell clears its own mask on startup, so the fake
// git below can't report what it inherited; instead it records the set of every thread of its parent, the server,
// spawning thread included, when it runs. None may block anything. glibc's posix_spawn blocks every signal in the
// thread that calls it until the child has exec'd, so a thread caught spawning another git at that moment shows a full
// mask for a moment; a thread is logged with its mask only once it has blocked something on 10 reads 50 ms apart.
#[test]
fn processes_the_server_starts_have_no_signals_blocked() {
    let dir = repo();
    let real_git = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap().trim().to_string();
    assert!(!real_git.is_empty(), "git not found");
    // A `git` first on PATH that logs its parent's blocked-signal sets, then runs the real one.
    let bin = TempDir::new("signals-bin");
    let log = bin.path().join("sigblk.log");
    let script = format!(
        r#"#!/bin/sh
for t in /proc/$PPID/task/*; do
  i=0
  while m=$(grep SigBlk "$t/status" 2>/dev/null); do
    case "${{m#*:}}" in *[1-9a-f]*) ;; *) break ;; esac
    i=$((i + 1))
    [ "$i" -ge 10 ] && break
    sleep 0.05
  done
  [ -n "$m" ] && echo "$m" >> '{}'
done
exec '{real_git}' "$@"
"#,
        log.display()
    );
    std::fs::write(bin.path().join("git"), script).unwrap();
    std::fs::set_permissions(bin.path().join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    let s = Server::try_start(dir.path(), &[], &[("PATH", &path)]).ok().expect("server did not start");
    assert_eq!(s.get("/api/tree", &[("mode", "working")]).status, 200);
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = logged.lines().collect();
    assert!(!lines.is_empty(), "the fake git was never run");
    for l in lines {
        let mask = l.trim_start_matches("SigBlk:").trim();
        assert!(mask.chars().all(|c| c == '0'), "a server thread blocks signals, so its children inherit them: {l}");
    }
}

// Starts the binary directly (not through Server, whose Drop would signal it again) with its own discovery dir.
fn exits_cleanly_on(sig: libc::c_int) {
    let dir = repo();
    let runtime = TempDir::new("signals-run");
    let mut child = isolate(&mut Command::new(env!("CARGO_BIN_EXE_revue")))
        .arg(dir.path())
        .args(["--no-open", "--port", "0"])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut seen = String::new();
    while !seen.contains("http://127.0.0.1:") {
        let mut line = String::new();
        assert!(std::io::BufRead::read_line(&mut out, &mut line).unwrap() > 0, "server exited early");
        seen.push_str(&line);
    }
    let discovery = runtime.path().join("revue");
    let entries = || std::fs::read_dir(&discovery).map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count()).unwrap_or(0);
    assert_eq!(entries(), 1, "no discovery file");
    unsafe { libc::kill(child.id() as libc::pid_t, sig) };
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("server did not exit on signal {sig}");
        }
        sleep(20);
    };
    assert_eq!(status.code(), Some(0), "exit status {status:?} (signal {:?})", status.signal());
    assert_eq!(entries(), 0, "discovery file left behind");
}

#[test]
fn sigterm_exits_cleanly_and_removes_the_discovery_file() {
    exits_cleanly_on(libc::SIGTERM);
}

#[test]
fn sigint_exits_cleanly_and_removes_the_discovery_file() {
    exits_cleanly_on(libc::SIGINT);
}
