// Server-core edge cases: the repo boundary for untracked diffs, malformed HTTP, the user's git
// config, a git hook's environment, shallow clones, odd files, the cost of listing notes, the index refresh,
// navigate's view, and connection limits. Every server here gets its own discovery dir, so none of them shows up next
// to the user's real ones.
mod common;
use common::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn start(root: &Path, env: &[(&str, &str)]) -> (TempDir, Server) {
    let runtime = TempDir::new("core-run");
    let mut e: Vec<(&str, &str)> = vec![("XDG_RUNTIME_DIR", runtime.path().to_str().unwrap())];
    e.extend_from_slice(env);
    let s = Server::try_start(root, &[], &e).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr));
    (runtime, s)
}

fn repo(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    init(dir.path());
    write(dir.path(), "a.txt", lines(5, None));
    commit_all(dir.path(), "one");
    dir
}

// Writes raw bytes and returns the status line (None if the server closed the connection without answering).
fn raw(port: u16, bytes: &[u8]) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(bytes).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    text.lines().next().filter(|l| l.starts_with("HTTP/")).map(String::from)
}

// ---------------------------------------------------------------- the repo boundary
#[cfg(unix)]
#[test]
fn untracked_diffs_do_not_read_through_a_symlinked_directory() {
    let tmp = TempDir::new("core-link");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "TOPSECRET-91c2\n").unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    init(&root);
    write(&root, "a.txt", "a\n");
    commit_all(&root, "one");
    std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("leak.txt")).unwrap();
    let (_rt, s) = start(&root, &[]);
    let p = [("mode", "working"), ("path", "linkdir/secret.txt"), ("status", "?")];
    let res = s.get("/api/diff", &p);
    assert!(!res.text().contains("TOPSECRET"), "leaked: {}", res.text());
    assert_eq!(res.status, 400, "{}", res.text());
    let res = s.get("/api/diffs", &[("mode", "working"), ("path", "linkdir/secret.txt"), ("old", ""), ("status", "?")]);
    assert!(!res.text().contains("TOPSECRET"), "leaked: {}", res.text());
    assert_eq!(res.status, 400, "{}", res.text());
    // An untracked link itself still shows as a link, not as its target.
    let res = s.get("/api/diff", &[("mode", "working"), ("path", "leak.txt"), ("status", "?")]);
    assert_eq!(res.status, 200);
    assert!(res.text().contains("new file mode 120000") && !res.text().contains("TOPSECRET"), "{}", res.text());
}

// ---------------------------------------------------------------- malformed HTTP
#[test]
fn a_huge_chunk_size_is_refused_without_a_panic_or_a_leaked_connection() {
    let dir = repo("core-chunk");
    let (_rt, s) = start(dir.path(), &[]);
    let req = format!("POST /api/navigate HTTP/1.1\r\nhost: 127.0.0.1:{}\r\ntransfer-encoding: chunked\r\n\r\n1\r\nx\r\nffffffffffffffff\r\n", s.port);
    let pid = s.body("/api/server", &[])["pid"].as_u64().unwrap();
    let fds = || std::fs::read_dir(format!("/proc/{pid}/fd")).map(|d| d.count()).unwrap_or(0);
    let status = raw(s.port, req.as_bytes());
    assert!(status.as_deref().is_some_and(|l| l.contains(" 413 ") || l.contains(" 400 ")), "got {status:?}");
    assert!(!s.stderr().contains("panicked"), "{}", s.stderr());
    if cfg!(target_os = "linux") {
        let before = fds();
        for _ in 0..20 {
            raw(s.port, req.as_bytes());
        }
        sleep(200);
        assert!(fds() <= before + 2, "{before} fds before, {} after", fds());
    }
    assert_eq!(s.get("/api/info", &[]).status, 200);
}

// ---------------------------------------------------------------- the user's git config
#[cfg(unix)]
#[test]
fn diffs_are_the_same_whatever_the_users_git_config_says() {
    let dir = repo("core-config");
    let root = dir.path();
    write(root, "b.txt", "b\n");
    write(root, "lib.js", "function helper() {}\n");
    commit_all(root, "two");
    let ext = root.join(".git/ext.sh");
    std::fs::write(&ext, "#!/bin/sh\necho EXTERNAL \"$@\"\n").unwrap();
    std::fs::set_permissions(&ext, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for (k, v) in [("color.ui", "always"), ("color.diff", "always"), ("color.grep", "always"), ("diff.mnemonicPrefix", "true"), ("diff.noprefix", "true"), ("diff.external", ext.to_str().unwrap())] {
        git(root, &["config", k, v]);
    }
    write(root, "a.txt", lines(5, Some(2)));
    write(root, "b.txt", "B\n");
    write(root, "new.txt", "u\n");
    let (_rt, s) = start(root, &[]);
    for (path, status) in [("a.txt", "M"), ("b.txt", "M"), ("new.txt", "?")] {
        let d = s.body("/api/diff", &[("mode", "working"), ("path", path), ("status", status)])["diff"].as_str().unwrap().to_string();
        assert!(!d.contains('\u{1b}'), "{path}: color codes in {d:?}");
        assert!(!d.contains("EXTERNAL"), "{path}: external diff ran: {d}");
        assert!(d.starts_with(&format!("diff --git a/{path} b/{path}")), "{path}: {d}");
    }
    // The batch matches its sections to `a/… b/…` headers, so a changed prefix would send every file to its own diff.
    let one = s.body("/api/diff", &[("mode", "working"), ("path", "a.txt")])["diff"].clone();
    let batch = s.body("/api/diffs", &[("mode", "working"), ("path", "a.txt"), ("old", ""), ("status", "M"), ("path", "b.txt"), ("old", ""), ("status", "M")]);
    assert_eq!(batch["diffs"]["a.txt"], one);
    assert!(!batch["diffs"]["b.txt"].as_str().unwrap().contains('\u{1b}'));
    let tree = s.body("/api/tree", &[("mode", "working")]);
    assert_eq!(untagged(tree["changes"]["a.txt"].clone()), json!({ "status": "M", "add": 1, "del": 1 }));
    // git grep (go to definition) must not see colors either.
    let def = s.body("/api/def", &[("mode", "working"), ("name", "helper")]);
    assert!(def["candidates"].as_array().unwrap().iter().any(|c| c["path"] == "lib.js"), "{def}");
}

// ---------------------------------------------------------------- a git hook's environment
// Git exports GIT_DIR, GIT_INDEX_FILE and GIT_WORK_TREE to the hooks it runs. `cargo test` started from one must not
// let them reach the fixtures' git or the servers (`isolate`), or `git add -A` and `commit` land in the user's repo.
// The fixtures run in a child copy of this test binary whose environment names a victim repo, so this process's own
// environment, which the tests running in parallel share, is never changed.
#[test]
fn git_variables_from_a_hook_reach_neither_the_fixtures_nor_the_servers() {
    let victim = TempDir::new("core-victim");
    let v = victim.path();
    init(v);
    write(v, "real.txt", "real\n");
    commit_all(v, "victim");
    write(v, "staged.txt", "staged\n");
    git(v, &["add", "staged.txt"]);
    write(v, "unstaged.txt", "not added\n");
    let index = || std::fs::read(v.join(".git/index")).unwrap();
    let (head, before) = (rev(v, "HEAD"), index());

    let (git_dir, index_file) = (v.join(".git"), v.join(".git/index"));
    // A pre-commit hook gets GIT_INDEX_FILE; GIT_DIR and GIT_WORK_TREE come with other hooks and with worktrees.
    let hooks: [&[(&str, &Path)]; 2] =
        [&[("GIT_INDEX_FILE", &index_file)], &[("GIT_DIR", &git_dir), ("GIT_INDEX_FILE", &index_file), ("GIT_WORK_TREE", v)]];
    for vars in hooks {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fixtures_and_a_server_under_a_hooks_environment", "--ignored", "--test-threads=1"])
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let report = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert_eq!(rev(v, "HEAD"), head, "{vars:?}: a commit landed in the victim repo");
        assert!(index() == before, "{vars:?}: the victim's index was rewritten");
        assert!(out.status.success() && report.contains("1 passed"), "{vars:?}: the fixtures failed:\n{report}");
    }
    assert_eq!(git(v, &["ls-files"]), "real.txt\nstaged.txt\n");
}

// Run by the test above, in a child process whose GIT_DIR, GIT_INDEX_FILE and GIT_WORK_TREE name another repo. On
// its own (`--ignored`) it is an ordinary fixture test.
#[test]
#[ignore = "run by git_variables_from_a_hook_reach_neither_the_fixtures_nor_the_servers"]
fn fixtures_and_a_server_under_a_hooks_environment() {
    let dir = TempDir::new("core-hook");
    let root = dir.path();
    init(root);
    write(root, "fixture.txt", "fixture\n");
    commit_all(root, "fixture");
    assert!(root.join(".git/HEAD").is_file(), "git init went elsewhere");
    assert_eq!(git(root, &["log", "--format=%s"]), "fixture\n");
    assert_eq!(git(root, &["ls-files"]), "fixture.txt\n");
    write(root, "fixture.txt", "changed\n");
    let (_rt, s) = start(root, &[]);
    let tree = s.body("/api/tree", &[("mode", "working")]);
    assert_eq!(tree["files"], json!(["fixture.txt"]), "{tree}");
    assert_eq!(untagged(tree["changes"].clone()), json!({ "fixture.txt": { "status": "M", "add": 1, "del": 1 } }));
}

// ---------------------------------------------------------------- shallow clones
#[test]
fn a_shallow_clone_shows_its_history_and_its_root_commit() {
    let src = repo("core-shallow-src");
    write(src.path(), "b.txt", "b\n");
    commit_all(src.path(), "two");
    let tmp = TempDir::new("core-shallow");
    let clone = tmp.path().join("clone");
    let url = format!("file://{}", src.path().display());
    git(tmp.path(), &["clone", "-q", "--depth", "1", &url, clone.to_str().unwrap()]);
    let (_rt, s) = start(&clone, &[]);
    let (status, log) = s.json("/api/log", &[]);
    assert_eq!(status, 200, "{log}");
    assert_eq!(log.as_array().unwrap().len(), 1, "{log}");
    let (status, tree) = s.json("/api/tree", &[("mode", "commit"), ("ref", "HEAD")]);
    assert_eq!(status, 200, "{tree}");
    assert_eq!(tree["changes"]["a.txt"]["status"], "A", "{tree}");
    assert_eq!(tree["changes"]["b.txt"]["status"], "A", "{tree}");
    let (status, d) = s.json("/api/diff", &[("mode", "commit"), ("ref", "HEAD"), ("path", "b.txt")]);
    assert_eq!(status, 200, "{d}");
    assert!(d["diff"].as_str().unwrap().contains("+b"), "{d}");
    assert_eq!(s.body("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "b.txt"), ("side", "old")]), json!({ "content": "" }));
    assert_eq!(s.get("/api/log", &[("all", "1")]).status, 200);
}

// ---------------------------------------------------------------- odd files
#[test]
fn reading_a_directory_or_a_fifo_is_a_clean_error() {
    let dir = repo("core-odd");
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    write(dir.path(), "sub/x.txt", "x\n");
    let (_rt, s) = start(dir.path(), &[]);
    let (status, body) = s.json("/api/file", &[("mode", "working"), ("path", "sub")]);
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "sub is not a file");
    #[cfg(unix)]
    {
        let fifo = std::ffi::CString::new(dir.path().join("pipe").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        let t = Instant::now();
        let (status, body) = s.json("/api/file", &[("mode", "working"), ("path", "pipe")]);
        assert_eq!(status, 400, "{body}");
        assert!(t.elapsed() < Duration::from_secs(2), "reading a fifo blocked");
        // The tree still lists untracked files around it without hanging.
        assert_eq!(s.get("/api/tree", &[("mode", "working")]).status, 200);
    }
    assert!(!s.stderr().contains("revue: GET"), "logged as a server fault: {}", s.stderr());
}

// ---------------------------------------------------------------- notes
#[test]
fn listing_notes_reads_each_file_once() {
    let dir = repo("core-notes");
    let (_rt, s) = start(dir.path(), &[]);
    for line in 1..=5 {
        let body = format!(r#"{{"action":"add","file":"a.txt","line":{line},"text":"note {line}"}}"#);
        let (status, res) = s.post("/api/annotations", &body, &[]);
        assert_eq!(status, 200, "{res}");
    }
    s.reads();
    let (status, res) = s.json("/api/annotations", &[("mode", "working")]);
    assert_eq!(status, 200);
    let notes = res["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 5);
    assert!(notes.iter().all(|n| n["stale"] == false), "{res}");
    let reads = s.reads().into_iter().filter(|r| r.ends_with("a.txt")).count();
    assert_eq!(reads, 1, "a.txt read {reads} times for 5 notes");
    // Still per side and file: an edit makes exactly the notes on changed lines stale.
    write(dir.path(), "a.txt", lines(5, Some(2)));
    let res = s.body("/api/annotations", &[("mode", "working")]);
    let stale: Vec<i64> = res["notes"].as_array().unwrap().iter().filter(|n| n["stale"] == true).map(|n| n["line"].as_i64().unwrap()).collect();
    assert_eq!(stale, [3]);
}

// ---------------------------------------------------------------- the index refresh
// A `git` first on PATH that logs each `update-index` before running the real one.
#[cfg(unix)]
fn logging_git() -> (TempDir, String, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let real = String::from_utf8(std::process::Command::new("which").arg("git").output().unwrap().stdout).unwrap().trim().to_string();
    let bin = TempDir::new("core-git");
    let log = bin.path().join("git.log");
    let script = format!("#!/bin/sh\ncase \"$*\" in *update-index*) echo update-index >> '{}';; esac\nexec '{real}' \"$@\"\n", log.display());
    std::fs::write(bin.path().join("git"), script).unwrap();
    std::fs::set_permissions(bin.path().join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    (bin, path, log)
}

#[cfg(unix)]
fn refreshes(log: &Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
}

#[cfg(unix)]
#[test]
fn the_index_is_refreshed_once_per_change_and_never_under_a_held_lock() {
    let dir = repo("core-refresh");
    let (_bin, path, log) = logging_git();
    let (_rt, s) = start(dir.path(), &[("PATH", &path)]);
    // With the watcher off, answers expire after 3 s, but nothing says the repo changed, so no second refresh.
    s.debug(&[("watchError", "off for this test")]);
    assert_eq!(s.get("/api/tree", &[("mode", "working")]).status, 200);
    assert_eq!(refreshes(&log), 1);
    sleep(3200);
    assert_eq!(s.get("/api/tree", &[("mode", "working")]).status, 200);
    assert_eq!(refreshes(&log), 1, "refreshed again with nothing changed");

    // A held index.lock (a commit in progress) is left alone.
    let dir = repo("core-refresh-lock");
    let (_bin, path, log) = logging_git();
    let (_rt, s) = start(dir.path(), &[("PATH", &path)]);
    std::fs::write(dir.path().join(".git/index.lock"), "").unwrap();
    write(dir.path(), "a.txt", lines(5, Some(1)));
    let (status, tree) = s.json("/api/tree", &[("mode", "working")]);
    assert_eq!(status, 200);
    assert_eq!(tree["changes"]["a.txt"]["status"], "M");
    assert_eq!(refreshes(&log), 0, "refreshed while index.lock was held");
}

#[test]
fn touching_one_file_settles_quickly() {
    let dir = repo("core-settle");
    let (_rt, s) = start(dir.path(), &[]);
    s.get("/api/tree", &[("mode", "working")]);
    let ev = s.events();
    write(dir.path(), "a.txt", lines(5, Some(4)));
    // Behave like a tab: refetch the tree on every change message.
    let deadline = Instant::now() + Duration::from_millis(2500);
    let mut seen = 0;
    while Instant::now() < deadline {
        if ev.next(100).is_some() {
            seen += 1;
            s.get("/api/tree", &[("mode", "working")]);
        }
    }
    assert!((1..=2).contains(&seen), "{seen} change messages for one edit");
}

// ---------------------------------------------------------------- navigate
#[test]
fn navigate_broadcasts_validated_strings() {
    let dir = repo("core-nav");
    git(dir.path(), &["branch", "123"]);
    let (_rt, s) = start(dir.path(), &[]);
    let (status, res) = s.post("/api/navigate", r#"{"mode":"branch","head":123,"base":"main"}"#, &[]);
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["view"]["head"], json!("123"), "{res}");
    assert_eq!(res["view"]["base"], json!("main"), "{res}");
}

// ---------------------------------------------------------------- connection limits
#[test]
fn too_many_connections_get_503() {
    let dir = repo("core-conns");
    let (_rt, s) = start(dir.path(), &[]);
    let mut idle = Vec::new();
    for _ in 0..256 {
        idle.push(TcpStream::connect(("127.0.0.1", s.port)).unwrap());
    }
    sleep(300);
    let req = format!("GET /api/info HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nconnection: close\r\n\r\n", s.port);
    let status = raw(s.port, req.as_bytes());
    assert!(status.as_deref().is_some_and(|l| l.contains(" 503 ")), "got {status:?}");
    drop(idle);
    sleep(300);
    assert_eq!(s.get("/api/info", &[]).status, 200);
}

#[test]
fn a_client_sending_its_headers_slowly_is_cut_off() {
    let dir = repo("core-slow");
    let (_rt, s) = start(dir.path(), &[]);
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    let start = Instant::now();
    let head = format!("GET /api/info HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nx-slow: ", s.port);
    c.write_all(head.as_bytes()).unwrap();
    // One byte a second: each read is well inside the 5 s idle timeout, but the whole head never arrives.
    let cut = loop {
        if c.write_all(b"a").is_err() {
            break start.elapsed();
        }
        if start.elapsed() > Duration::from_secs(16) {
            panic!("still connected after 16 s");
        }
        std::thread::sleep(Duration::from_secs(1));
        // A closed connection shows up as a read of 0 bytes (or an error) once the server gives up.
        c.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
        let mut b = [0u8; 64];
        match c.read(&mut b) {
            Ok(0) => break start.elapsed(),
            Ok(_) => break start.elapsed(), // an error response
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break start.elapsed(),
        }
    };
    assert!(cut < Duration::from_secs(13), "cut off after {cut:?}");
}
