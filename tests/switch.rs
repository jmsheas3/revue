// POST /api/switch, the one endpoint that changes the repo: it runs `git switch`, refuses with uncommitted tracked
// changes, and turns a remote-only branch into a local tracking branch.
mod common;
use common::*;
use std::path::Path;

// main with a committed a.txt, plus a feature branch.
fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("switch");
    let root = dir.path();
    init(root);
    write(root, "a.txt", "one\n");
    commit_all(root, "init");
    git(root, &["branch", "feature"]);
    let s = Server::start(root);
    (dir, s)
}

fn current(root: &Path) -> String {
    git(root, &["branch", "--show-current"]).trim().to_string()
}

#[test]
fn switching_to_a_local_branch_checks_it_out() {
    let (dir, s) = repo();
    let (status, body) = s.post("/api/switch", "", &[("branch", "feature")]);
    assert_eq!((status, body), (200, json!({ "ok": true })));
    assert_eq!(current(dir.path()), "feature");
    // The cached HEAD is forgotten, so the page sees the switch at once.
    assert_eq!(s.body("/api/info", &[])["branch"], "feature");
}

#[test]
fn uncommitted_tracked_changes_refuse_the_switch() {
    let (dir, s) = repo();
    write(dir.path(), "a.txt", "edited\n");
    let (status, body) = s.post("/api/switch", "", &[("branch", "feature")]);
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("uncommitted changes"), "{body}");
    assert_eq!(current(dir.path()), "main");
    // Untracked files don't count.
    git(dir.path(), &["checkout", "-q", "--", "a.txt"]);
    write(dir.path(), "new.txt", "untracked\n");
    assert_eq!(s.post("/api/switch", "", &[("branch", "feature")]).0, 200);
}

#[test]
fn a_remote_only_branch_becomes_a_local_tracking_branch() {
    let tmp = TempDir::new("switch-remote");
    let remote = tmp.path().join("remote.git");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(tmp.path(), &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    init(&root);
    write(&root, "a.txt", "one\n");
    commit_all(&root, "init");
    git(&root, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&root, &["switch", "-q", "-c", "topic"]);
    commit(&root, "topic work");
    git(&root, &["push", "-q", "origin", "main", "topic"]);
    git(&root, &["switch", "-q", "main"]);
    git(&root, &["branch", "-q", "-D", "topic"]);
    git(&root, &["fetch", "-q", "origin"]);
    let s = Server::start(&root);

    let (status, body) = s.post("/api/switch", "", &[("branch", "origin/topic")]);
    assert_eq!((status, body), (200, json!({ "ok": true })));
    assert_eq!(current(&root), "topic");
    assert_eq!(git(&root, &["rev-parse", "--abbrev-ref", "topic@{upstream}"]).trim(), "origin/topic");

    // A second remote-only pick of the same branch now finds the local one in the way.
    git(&root, &["switch", "-q", "main"]);
    let (status, body) = s.post("/api/switch", "", &[("branch", "origin/topic")]);
    assert_eq!(status, 409);
    assert!(body["error"].as_str().unwrap().contains("a local branch \"topic\" already exists"), "{body}");
}

#[test]
fn a_switch_needs_a_post_with_the_revue_header() {
    let (dir, s) = repo();
    assert_eq!(s.get("/api/switch", &[("branch", "feature")]).status, 403);
    assert_eq!(request(s.port, "POST", "/api/switch?branch=feature", &[], b"").status, 403);
    assert_eq!(current(dir.path()), "main");
}
