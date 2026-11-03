// The working-tree view refreshes the index. Files modified in the same second the index was written are "racily
// clean": git can't trust their stat info, so every `git diff` re-reads them, and git diff never rewrites the index
// to settle it. Right after a checkout, a pull or a big commit that can be every file, on every request.
mod common;
use common::*;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILES: usize = 50;

fn set_mtime(p: &Path, t: SystemTime) {
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

// A committed repo whose files and index share one mtime, which is exactly git's racy condition (a file not older
// than the index that recorded it). Returns that mtime.
fn racy_repo(dir: &TempDir) -> SystemTime {
    let root = dir.path();
    init(root);
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - 10;
    let t = UNIX_EPOCH + Duration::from_secs(secs);
    for i in 0..FILES {
        let rel = format!("f/{i}.txt");
        write(root, &rel, format!("file {i}\n"));
        set_mtime(&root.join(rel), t);
    }
    commit_all(root, "files");
    set_mtime(&root.join(".git/index"), t);
    t
}

#[test]
fn the_working_tree_view_settles_racily_clean_files() {
    let dir = TempDir::new("index");
    let t = racy_repo(&dir);
    let s = Server::start(dir.path());
    assert_eq!(s.body("/api/tree", &[("mode", "working")])["changes"], json!({}));
    // Rewritten after the files, so git can trust their stat info from now on.
    let index = mtime(&dir.path().join(".git/index"));
    assert!(index > t, "the index was not rewritten, so every diff keeps re-reading the files");
    assert_eq!(git(dir.path(), &["diff-files", "--name-only"]), "");
}

#[test]
fn the_index_write_does_not_cause_an_update_loop() {
    let dir = TempDir::new("index-loop");
    racy_repo(&dir);
    let s = Server::start(dir.path());
    let ev = s.events();
    s.body("/api/tree", &[("mode", "working")]);
    // The index write may be reported once (as "worktree"); a tab refetching on it must not keep it going.
    s.settle(&ev);
    s.body("/api/tree", &[("mode", "working")]);
    s.settle(&ev);
    let settled = ev.changes();
    assert!(settled <= 2, "{:?}", ev.data());
    assert!(ev.data().iter().all(|d| d == "worktree"), "{:?}", ev.data());
    s.body("/api/tree", &[("mode", "working")]);
    sleep(1000);
    assert_eq!(ev.changes(), settled, "updates kept coming: {:?}", ev.data());
    // The watcher is alive: a real edit is still reported, so the quiet above meant something.
    write(dir.path(), "f/1.txt", "edited\n");
    assert!(ev.wait_changes(settled + 1, 3000), "the watcher reports nothing at all");
}

#[test]
fn a_real_modification_is_still_reported() {
    let dir = TempDir::new("index-mod");
    racy_repo(&dir);
    write(dir.path(), "f/7.txt", "changed\n");
    let s = Server::start(dir.path());
    let ch = untagged(s.body("/api/tree", &[("mode", "working")])["changes"].clone());
    assert_eq!(ch, json!({ "f/7.txt": { "status": "M", "add": 1, "del": 1 } }));
}
