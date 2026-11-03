// /api/tree and /api/info answers are remembered until the watcher reports a change, so asking again costs no git
// call, and every kind of change still shows up: an edit, a commit (in a linked worktree too, whose git directories
// are watched where they are), and changes the watcher can't see (once it has failed) within the ref cache's 3 s.
#![cfg(target_os = "linux")]
mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// A `git` first on PATH that logs each call's arguments, then runs the real one.
struct GitLog {
    _bin: TempDir,
    log: PathBuf,
    path: String,
}

impl GitLog {
    fn new() -> GitLog {
        let real = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap().trim().to_string();
        assert!(!real.is_empty(), "git not found");
        let bin = TempDir::new("cache-bin");
        let log = bin.path().join("git.log");
        let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexec '{real}' \"$@\"\n", log.display());
        std::fs::write(bin.path().join("git"), script).unwrap();
        std::fs::set_permissions(bin.path().join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
        GitLog { _bin: bin, log, path }
    }

    // The calls logged since the last take (and clears the log).
    fn take(&self) -> Vec<String> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let _ = std::fs::remove_file(&self.log);
        text.lines().map(String::from).collect()
    }

    fn start(&self, root: &Path) -> Server {
        Server::try_start(root, &[], &[("PATH", &self.path)]).ok().expect("server did not start")
    }
}

fn repo() -> TempDir {
    let dir = TempDir::new("cache");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "first");
    write(dir.path(), "b.txt", "two\n");
    commit_all(dir.path(), "second");
    dir
}

// Poll `f` until it holds or `ms` pass.
fn eventually(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        if f() {
            return true;
        }
        sleep(100);
    }
    f()
}

#[test]
fn asking_for_the_same_view_twice_runs_no_git_the_second_time() {
    let dir = repo();
    let git = GitLog::new();
    let s = git.start(dir.path());
    let first = s.get("/api/tree", &[("mode", "commit"), ("ref", "HEAD")]);
    assert_eq!(first.status, 200);
    git.take();
    let second = s.get("/api/tree", &[("mode", "commit"), ("ref", "HEAD")]);
    assert_eq!(git.take(), Vec::<String>::new(), "the second identical request ran git");
    assert_eq!(second.body, first.body);
    // A different view is its own entry.
    s.get("/api/tree", &[("mode", "commit"), ("ref", "HEAD~1")]);
    assert!(!git.take().is_empty(), "a different view was served from the cache");
}

#[test]
fn an_edit_shows_up_and_the_settled_view_is_then_remembered() {
    let dir = repo();
    let git = GitLog::new();
    let s = git.start(dir.path());
    let tree = || s.body("/api/tree", &[("mode", "working")]);
    assert_eq!(tree()["changes"], json!({}));
    let ev = s.events();
    write(dir.path(), "a.txt", "edited\n");
    assert!(ev.next(2000).is_some(), "no change event");
    assert_eq!(tree()["changes"]["a.txt"]["status"], "M");
    // Refreshing the index rewrites it, which the watcher reports, so the next request computes again. git keeps
    // rewriting it while the edit is in the same second as the index ("racily clean"), but that settles, and from
    // then on asking again is free. A loop would never get there. Each try asks once the watcher has reported every
    // write so far, as a tab refetching on its message does.
    let settled = eventually(5000, || {
        s.settle(&ev);
        git.take();
        let fresh = tree()["changes"]["a.txt"]["status"] == "M";
        fresh && git.take().is_empty()
    });
    assert!(settled, "the working view never settled into the cache");
}

// What an agent does: change a file and ask straight away, before the debounced change event has gone out.
#[test]
fn asking_right_after_an_edit_sees_the_edit() {
    let dir = repo();
    let s = Server::start(dir.path());
    let tree = || s.body("/api/tree", &[("mode", "working")]);
    tree();
    for i in 1..=20u64 {
        write(dir.path(), "u.txt", "x\n".repeat(i as usize));
        let add = tree()["changes"]["u.txt"]["add"].clone();
        assert_eq!(add, json!(i), "round {i}: an answer from before the edit");
        sleep(30);
    }
}

// The same with the timing of a loaded machine: the watcher's reader is late, and takes and counts the edit while the
// request is between its looks at the queue and at the count. Asking for the count first would see it unmoved with
// nothing pending, the version of the answer from before the edit.
#[test]
fn asking_right_after_an_edit_sees_the_edit_while_the_reader_takes_it() {
    let dir = repo();
    let git = GitLog::new();
    let env = [("PATH", git.path.as_str()), ("REVUE_TEST_READER_DELAY_MS", "10"), ("REVUE_TEST_VERSION_GAP_MS", "30")];
    let s = Server::try_start(dir.path(), &[], &env).ok().expect("server did not start");
    let tree = || s.body("/api/tree", &[("mode", "working")]);
    let ev = s.events();
    for i in 1..=3u64 {
        write(dir.path(), "u.txt", "x\n".repeat(i as usize));
        // Settled: the answer is remembered, so a stale version would be served from it.
        let settled = eventually(5000, || {
            s.settle(&ev);
            tree();
            git.take();
            tree();
            git.take().is_empty()
        });
        assert!(settled, "round {i}: the view never settled into the cache");
        write(dir.path(), "u.txt", "x\n".repeat(i as usize + 100));
        assert_eq!(tree()["changes"]["u.txt"]["add"], json!(i + 100), "round {i}: an answer from before the edit");
    }
}

#[test]
fn a_commit_updates_info() {
    let dir = TempDir::new("cache-info");
    init(dir.path());
    let s = Server::start(dir.path());
    assert_eq!(s.body("/api/info", &[])["hasHead"], false);
    assert_eq!(s.body("/api/info", &[])["hasHead"], false);
    let ev = s.events();
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "first");
    assert!(ev.next(2000).is_some(), "no change event");
    let info = s.body("/api/info", &[]);
    assert_eq!(info["hasHead"], true);
    assert_eq!(info["branch"], "main");
    assert_eq!(info["stale"], false);
}

#[test]
fn without_live_updates_answers_expire_after_3_seconds() {
    let dir = repo();
    let s = Server::start(dir.path());
    assert_eq!(s.debug(&[("watchError", "off")])["watched"], json!([]));
    let branches = || s.body("/api/info", &[])["branches"].clone();
    assert_eq!(branches(), json!(["main"]));
    git(dir.path(), &["branch", "other"]);
    assert!(eventually(4000, || branches() == json!(["main", "other"])), "a new branch never showed up");
}

// The tree is remembered by the commits it lists, which each request resolves first: a branch that has moved is another
// tree at once, even with live updates off, when answers otherwise last 3 s. The commit the page shows for the branch
// (looked up through /api/log, which isn't remembered) and the files it shows then agree.
#[test]
fn a_branch_that_moved_gets_its_new_tree_at_once_without_live_updates() {
    let dir = repo();
    let s = Server::start(dir.path());
    assert_eq!(s.debug(&[("watchError", "off")])["watched"], json!([]));
    let tree = || s.body("/api/tree", &[("mode", "commit"), ("ref", "main")]);
    assert_eq!(tree()["files"], json!(["a.txt", "b.txt"]));
    write(dir.path(), "c.txt", "three\n");
    commit_all(dir.path(), "third");
    let after = tree();
    assert_eq!(after["commit"], rev(dir.path(), "main"));
    assert_eq!(after["files"], json!(["a.txt", "b.txt", "c.txt"]));
    assert_eq!(after["changes"], json!({ "c.txt": after["changes"]["c.txt"] }));
    assert_eq!(after["changes"]["c.txt"]["status"], "A");
}

// A linked worktree's git directories are outside its root, and watched there (watcher_live.rs), so a git change
// drops what is remembered as in any repo: a new branch, written to the shared git directory, is in /api/info by the
// time its change message arrives. With the watcher seeing everything ("ok"), nothing expires after 3 s instead, so it
// is the watcher that dropped the answer. A commit shows up in the tree too, which is also remembered by the commits
// it lists.
#[test]
fn in_a_linked_worktree_git_changes_drop_what_is_remembered() {
    let dir = repo();
    let wt_dir = TempDir::new("cache-wt");
    let wt = wt_dir.path().join("wt");
    git(dir.path(), &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    {
        let s = Server::start(&wt);
        assert_eq!(s.body("/api/server", &[])["watcher"], "ok");
        let ev = s.events();
        let branches = || s.body("/api/info", &[])["branches"].clone();
        let changes = || untagged(s.body("/api/tree", &[("mode", "commit"), ("ref", "HEAD")])["changes"].clone());
        assert_eq!(branches(), json!(["main", "side"]));
        assert_eq!(changes(), json!({ "b.txt": { "status": "A", "add": 1, "del": 0 } }));
        git(&wt, &["branch", "other"]);
        assert!(ev.wait_changes(1, 5000), "no change message for the new branch");
        assert_eq!(branches(), json!(["main", "other", "side"]));
        s.settle(&ev);
        ev.clear();
        write(&wt, "c.txt", "three\n");
        commit_all(&wt, "third");
        assert!(ev.wait_changes(1, 5000), "no change message for the commit");
        assert_eq!(changes(), json!({ "c.txt": { "status": "A", "add": 1, "del": 0 } }));
    }
    git(dir.path(), &["worktree", "remove", "--force", wt.to_str().unwrap()]);
}
