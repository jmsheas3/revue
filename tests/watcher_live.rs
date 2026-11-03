// Live updates in the situations the first watcher missed: steady activity, linked worktrees, the user's inotify limit
// and folders that stop (or start) being ignored; and how the tests know every change has been sent.
#![cfg(target_os = "linux")]
mod common;
use common::*;
use std::time::{Duration, Instant};

fn repo(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    dir
}

fn until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        if f() {
            return true;
        }
        sleep(50);
    }
    f()
}

#[test]
fn steady_writes_still_produce_updates_while_they_last() {
    let dir = repo("steady");
    let s = Server::start(dir.path());
    let ev = s.events();
    for i in 0..30 {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.path().join("log.txt"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{i}\n").as_bytes()))
            .unwrap();
        sleep(100);
    }
    // Counted right as the writes stop, before the final debounce could fire.
    let during = ev.changes();
    assert!(during >= 2, "only {during} change events during 3 s of writes every 100 ms");
}

#[test]
fn commits_and_branches_in_a_linked_worktree_are_git_changes() {
    let dir = repo("wt-main");
    git(dir.path(), &["branch", "other"]);
    let wt_dir = TempDir::new("wt-linked");
    let wt = wt_dir.path().join("wt");
    git(dir.path(), &["worktree", "add", "-q", wt.to_str().unwrap(), "other"]);
    {
        let s = Server::start(&wt);
        let ev = s.events();
        commit(&wt, "in the worktree");
        assert_eq!(ev.next(2000).as_deref(), Some("git"), "no event for a commit in the worktree");
        s.settle(&ev);
        ev.clear();
        git(&wt, &["branch", "extra"]);
        assert_eq!(ev.next(2000).as_deref(), Some("git"), "no event for a new branch");
        s.settle(&ev);
        ev.clear();
        // A working file is still a working-tree change.
        std::fs::write(wt.join("a.txt"), "edited\n").unwrap();
        assert_eq!(ev.next(2000).as_deref(), Some("worktree"));
    }
    git(dir.path(), &["worktree", "remove", "--force", wt.to_str().unwrap()]);
}

#[test]
fn running_out_of_watches_keeps_the_top_folder_and_git_state_live() {
    let dir = repo("limit");
    for i in 0..10 {
        write(dir.path(), &format!("d{i}/sub/x.txt"), "x\n");
    }
    commit_all(dir.path(), "dirs");
    // A debug-only hook: watches past this many fail with ENOSPC, as when the user's inotify limit is reached.
    let s = Server::try_start(dir.path(), &[], &[("REVUE_TEST_MAX_WATCHES", "8")]).ok().unwrap();
    assert!(until(2000, || s.stderr().contains("out of inotify watches")), "stderr: {}", s.stderr());
    assert!(!s.stderr().contains("live updates disabled"), "stderr: {}", s.stderr());
    let watched = s.watched();
    assert!(watched.iter().any(|w| w.is_empty()) && watched.iter().any(|w| w == ".git"), "{watched:?}");
    assert!(!watched.iter().any(|w| w.starts_with('d')), "per-folder watches were kept: {watched:?}");
    let ev = s.events();
    write(dir.path(), "top.txt", "top\n");
    assert_eq!(ev.next(2000).as_deref(), Some("worktree"));
    s.settle(&ev);
    ev.clear();
    commit_all(dir.path(), "more");
    assert_eq!(ev.next(2000).as_deref(), Some("git"));
}

#[test]
fn a_folder_that_stops_being_ignored_gets_watched() {
    let dir = repo("unignore");
    write(dir.path(), ".gitignore", "target/\n");
    commit_all(dir.path(), "ignore target");
    write(dir.path(), "target/x.txt", "x\n");
    let s = Server::start(dir.path());
    assert!(until(1000, || s.watched().iter().any(|w| w.is_empty())));
    assert!(!s.watched().iter().any(|w| w == "target"), "an ignored folder was watched");
    // Force-adding a file inside makes the folder tracked, so it must be watched from now on.
    git(dir.path(), &["add", "-f", "target/x.txt"]);
    assert!(until(3000, || s.watched().iter().any(|w| w == "target")), "target never watched: {:?}", s.watched());
    let ev = s.events();
    write(dir.path(), "target/x.txt", "changed\n");
    assert!(ev.next(2000).is_some(), "no event for a change in the now-tracked folder");
}

// What Server::events waits on: GET /api/debug says the watcher has changes to send from the moment the kernel queues
// one until its message has gone out, and ?mark=n then puts a comment in every open event stream after it. So a mark
// sent while nothing is unsent never comes before the message for a change made earlier.
#[test]
fn the_watcher_says_when_every_change_has_been_sent() {
    let dir = repo("unsent");
    // A debug-only hook: the reader waits 300 ms before each read, so a change sits in the kernel's queue first.
    let s = Server::try_start(dir.path(), &[], &[("REVUE_TEST_READER_DELAY_MS", "300")]).ok().unwrap();
    let ev = s.events();
    assert_eq!(s.debug(&[])["unsent"], false);
    write(dir.path(), "a.txt", "edited\n");
    assert_eq!(s.debug(&[])["unsent"], true, "a change in the kernel's queue counts as unsent");
    // Then read and held for the debounce: still unsent, until the message has gone out.
    let start = Instant::now();
    loop {
        let (mark, unsent) = s.send_mark();
        assert!(ev.wait_mark(mark, 2000), "mark {mark} never came");
        if !unsent {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "still unsent after 5 s");
        sleep(20);
    }
    assert_eq!(ev.data(), ["worktree"], "a mark sent with nothing unsent came before the change's message");
}
