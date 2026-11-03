#![cfg(unix)]
// The command line: bad arguments print usage and exit 1, --version prints the version, an idle server exits on its own, and the default port
// falls back to a free one when 4777 is taken.
mod common;
use common::*;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn revue() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_revue"));
    isolate(&mut c);
    c
}

fn repo() -> TempDir {
    let dir = TempDir::new("cli");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    dir
}

#[test]
fn bad_arguments_print_usage_and_exit_1() {
    let dir = repo();
    for (args, msg) in [
        (vec!["--port", "abc"], "--port needs a port number from 0 to 65535, not abc"),
        (vec!["--port", "70000"], "--port needs a port number from 0 to 65535, not 70000"),
        (vec!["--port"], "--port needs a port number from 0 to 65535"),
        (vec!["--notes", "disk"], "--notes must be memory or file, not disk"),
        (vec!["--idle-timeout", "soon"], "--idle-timeout must be a number of minutes"),
        (vec!["mcp", "--lifetime", "forever"], "--lifetime must be session or detached, not forever"),
    ] {
        let out = revue().arg(dir.path()).args(&args).stdin(Stdio::null()).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {err}");
        assert!(err.contains(msg), "{args:?}: {err}");
        assert!(err.contains("Usage: revue [dir]") && err.contains("Run revue --help for details."), "{args:?}: {err}");
    }
}

#[test]
fn help_prints_usage_and_exits_0() {
    let out = revue().arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("revue mcp [--lifetime session|detached]"));
}

#[test]
fn version_prints_the_version_and_commit_and_exits_0() {
    // build.rs names the commit the binary was built from, or none outside a git checkout.
    let mut git = Command::new("git");
    isolate(&mut git);
    let commit = git.args(["rev-parse", "--short", "HEAD"]).current_dir(env!("CARGO_MANIFEST_DIR")).output().unwrap();
    let commit = String::from_utf8_lossy(&commit.stdout).trim().to_string();
    let want = format!("revue {}", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        // Outside any repo, and before the other arguments are checked.
        let out = revue().args([flag, "--port", "abc"]).current_dir(std::env::temp_dir()).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let text = text.trim_end();
        assert_eq!(out.status.code(), Some(0), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
        if commit.is_empty() {
            assert_eq!(text, want, "{flag}");
        } else {
            assert!(
                text == format!("{want} ({commit})") || text == format!("{want} ({commit}, modified)"),
                "{flag}: {text:?}, want {want} ({commit})"
            );
        }
    }
}

// A folder outside any repo opens as a plain folder (tests/folder.rs); a path that isn't a folder is an error.
#[test]
fn a_path_that_is_not_a_folder_is_an_error() {
    let dir = TempDir::new("cli-norepo");
    write(dir.path(), "a.txt", "one\n");
    let out = revue().arg(dir.path().join("a.txt")).arg("--no-open").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("a.txt is not a folder"), "{}", String::from_utf8_lossy(&out.stderr));
    let out = revue().arg(dir.path().join("missing")).arg("--no-open").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing: No such file"), "{}", String::from_utf8_lossy(&out.stderr));
}

// The banner says when a folder is shown as a plain folder.
#[test]
fn a_plain_folder_says_so_when_it_starts() {
    let dir = TempDir::new("cli-folder");
    let mut child = revue()
        .arg(dir.path())
        .args(["--no-open", "--port", "0", "--idle-timeout", "5"])
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(line.trim_end(), format!("revue: {} (folder)", dir.path().display()));
}

#[test]
fn an_idle_server_exits_and_removes_its_discovery_file() {
    let dir = repo();
    // 0.02 minutes is 1.2 s with no tabs and no requests.
    let mut s = Server::try_start(dir.path(), &["--idle-timeout", "0.02"], &[]).ok().unwrap();
    let discovery = s.discovery_dir().unwrap();
    assert_eq!(std::fs::read_dir(&discovery).unwrap().count(), 1);
    assert_eq!(s.wait_exit(8000), Some(0), "the idle server didn't exit");
    assert_eq!(std::fs::read_dir(&discovery).unwrap().count(), 0, "its discovery file was left behind");
}

#[test]
fn the_default_port_falls_back_to_a_free_one_when_4777_is_taken() {
    // Hold 4777 if it's free; if something else already holds it (a revue the user runs, say), that works as well.
    // Either way this test only binds, it never connects to whatever is there.
    let _hold = std::net::TcpListener::bind("127.0.0.1:4777").ok();
    let dir = repo();
    let runtime = TempDir::new("cli-runtime");
    let mut child = revue()
        .arg(dir.path())
        .arg("--no-open")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    while !line.contains("http://127.0.0.1:") {
        if out.read_line(&mut line).unwrap() == 0 {
            break;
        }
    }
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let _ = child.wait();
    let port: u16 = line.split("http://127.0.0.1:").nth(1).expect("no URL").chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap();
    assert_ne!(port, 4777);
}
