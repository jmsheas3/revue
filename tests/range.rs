// Range resolution, change parsing and input checks, tested through the HTTP API on throwaway repos.
mod common;
use common::*;

fn new_repo(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    init(dir.path());
    dir
}

// No commits yet: one staged file and one untracked file.
fn empty() -> (TempDir, Server) {
    let dir = new_repo("empty");
    let root = dir.path();
    write(root, "staged.txt", "staged\n");
    git(root, &["add", "staged.txt"]);
    write(root, "new.txt", "one\ntwo\n");
    let s = Server::start(root);
    (dir, s)
}

struct Renames {
    dir: TempDir,
    s: Server,
    root_sha: String,
    renames: String,
}

// Renames in one commit, with awkward names.
fn renames() -> Renames {
    let dir = new_repo("renames");
    let root = dir.path();
    for f in ["old name.txt", "ünï.txt", "tab\there.txt", "keep\ttab.txt"] {
        write(root, f, lines(10, None));
    }
    commit_all(root, "root");
    let root_sha = rev(root, "HEAD");
    git(root, &["mv", "old name.txt", "new name.txt"]);
    std::fs::create_dir(root.join("dír")).unwrap();
    git(root, &["mv", "ünï.txt", "dír/ünï moved.txt"]);
    write(root, "dír/ünï moved.txt", lines(10, Some(4)));
    git(root, &["mv", "tab\there.txt", "tab\tmoved.txt"]);
    write(root, "keep\ttab.txt", lines(10, Some(2)));
    commit_all(root, "renames");
    let renames = rev(root, "HEAD");
    let s = Server::start(root);
    Renames { dir, s, root_sha, renames }
}

// main and a feature branch that forked from it; main moves on afterwards.
fn branches() -> (TempDir, Server) {
    let dir = new_repo("branches");
    let root = dir.path();
    write(root, "shared.txt", lines(5, None));
    commit_all(root, "root");
    git(root, &["switch", "-q", "-c", "feature"]);
    write(root, "feature.txt", "feature\n");
    commit_all(root, "feature work");
    git(root, &["switch", "-q", "main"]);
    write(root, "main-only.txt", "main\n");
    commit_all(root, "main moves on");
    let s = Server::start(root);
    (dir, s)
}

// A repo whose committed blob for lost.txt is missing from the object store.
fn corrupt() -> (TempDir, Server) {
    let dir = new_repo("corrupt");
    let root = dir.path();
    write(root, "lost.txt", "about to go\n");
    write(root, "fine.txt", "still here\n");
    commit_all(root, "root");
    let blob = rev(root, "HEAD:lost.txt");
    std::fs::remove_file(root.join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    let s = Server::start(root);
    (dir, s)
}

fn tree_changes(s: &Server, params: &[(&str, &str)]) -> Value {
    untagged(s.body("/api/tree", params)["changes"].clone())
}

#[test]
fn changes_parses_a_pure_rename_with_spaces_in_the_path() {
    let r = renames();
    let (status, body) = r.s.json("/api/tree", &[("mode", "commit"), ("ref", &r.renames)]);
    assert_eq!(status, 200);
    assert_eq!(untagged(body["changes"]["new name.txt"].clone()), json!({ "status": "R", "oldPath": "old name.txt", "add": 0, "del": 0 }));
    assert!(body["changes"].get("old name.txt").is_none());
}

#[test]
fn changes_parses_a_rename_with_edits_and_unicode_paths() {
    let r = renames();
    let body = r.s.body("/api/tree", &[("mode", "commit"), ("ref", &r.renames)]);
    assert_eq!(untagged(body["changes"]["dír/ünï moved.txt"].clone()), json!({ "status": "R", "oldPath": "ünï.txt", "add": 1, "del": 1 }));
    assert!(body["files"].as_array().unwrap().contains(&json!("dír/ünï moved.txt")));
}

#[test]
fn changes_handles_a_tab_in_renamed_and_modified_paths() {
    let r = renames();
    let ch = tree_changes(&r.s, &[("mode", "commit"), ("ref", &r.renames)]);
    assert_eq!(ch["tab\tmoved.txt"], json!({ "status": "R", "oldPath": "tab\there.txt", "add": 0, "del": 0 }));
    assert_eq!(ch["keep\ttab.txt"], json!({ "status": "M", "add": 1, "del": 1 }));
    assert_eq!(ch.as_object().unwrap().len(), 4);
    // The rename's diff is found through its old path.
    let (status, d) = r.s.json("/api/diff", &[("mode", "commit"), ("ref", &r.renames), ("path", "dír/ünï moved.txt"), ("old", "ünï.txt")]);
    assert_eq!(status, 200);
    let diff = d["diff"].as_str().unwrap();
    assert!(diff.lines().any(|l| l == "-line 5"), "{diff}");
    assert!(diff.lines().any(|l| l == "+edited 5"), "{diff}");
    drop(r.dir);
}

#[test]
fn check_ref_rejects_unsafe_characters_and_a_leading_dash() {
    let (_dir, s) = branches();
    for r in ["-p", "--all", "--output=/tmp/x", "a b", "a;b", "$(id)", "main:file", "a\\b", "a'b"] {
        let (status, body) = s.json("/api/tree", &[("mode", "commit"), ("ref", r)]);
        assert_eq!(status, 400, "{r}");
        assert!(body["error"].as_str().unwrap().starts_with("bad ref"), "{r}");
    }
    for params in [[("mode", "branch"), ("base", "-x")], [("mode", "branch"), ("head", "--all")]] {
        let (status, body) = s.json("/api/tree", &params);
        assert_eq!(status, 400, "{params:?}");
        assert!(body["error"].as_str().unwrap().starts_with("bad ref"));
    }
    let (status, body) = s.json("/api/log", &[("branch", "--all")]);
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().starts_with("bad ref"));
    // Ordinary ref syntax still passes.
    for r in ["HEAD~1", "main^", "HEAD@{0}", "feature"] {
        assert_eq!(s.get("/api/tree", &[("mode", "commit"), ("ref", r)]).status, 200, "{r}");
    }
}

#[test]
fn check_path_rejects_paths_outside_the_repo() {
    let (dir, s) = branches();
    let outside = dir.path().parent().unwrap().join("elsewhere").join("new name.txt");
    for p in ["../outside.txt", "a/../../outside.txt", "/etc/passwd", outside.to_str().unwrap(), ""] {
        for endpoint in ["/api/file", "/api/diff"] {
            let (status, body) = s.json(endpoint, &[("mode", "working"), ("path", p)]);
            assert_eq!(status, 400, "{endpoint} {p}");
            let e = body["error"].as_str().unwrap();
            assert!(e == "path escapes repo" || e == "bad path", "{endpoint} {p}: {e}");
        }
    }
    // A path that only passes through a parent and comes back in is fine.
    assert_eq!(s.get("/api/file", &[("mode", "working"), ("path", "x/../shared.txt")]).status, 200);
}

#[test]
fn working_mode_in_a_repo_with_no_commits_compares_against_the_empty_tree() {
    let (_dir, s) = empty();
    let (status, body) = s.json("/api/tree", &[("mode", "working")]);
    assert_eq!(status, 200);
    assert_eq!(body["files"], json!(["new.txt", "staged.txt"]));
    assert_eq!(untagged(body["changes"]["staged.txt"].clone()), json!({ "status": "A", "add": 1, "del": 0 }));
    assert_eq!(untagged(body["changes"]["new.txt"].clone()), json!({ "status": "?", "add": 2, "del": 0 }));
    assert_eq!(s.body("/api/file", &[("mode", "working"), ("path", "staged.txt"), ("side", "old")]), json!({ "content": "" }));
    assert_eq!(s.body("/api/log", &[]), json!([]));
    assert_eq!(s.body("/api/info", &[])["hasHead"], false);
}

#[test]
fn commit_mode_on_a_root_commit_compares_against_the_empty_tree() {
    let r = renames();
    let (status, body) = r.s.json("/api/tree", &[("mode", "commit"), ("ref", &r.root_sha)]);
    assert_eq!(status, 200);
    let ch = untagged(body["changes"].clone());
    let ch = ch.as_object().unwrap();
    let mut keys: Vec<&String> = ch.keys().collect();
    keys.sort();
    assert_eq!(keys, ["keep\ttab.txt", "old name.txt", "tab\there.txt", "ünï.txt"]);
    for c in ch.values() {
        assert_eq!(c, &json!({ "status": "A", "add": 10, "del": 0 }));
    }
    let old = r.s.body("/api/file", &[("mode", "commit"), ("ref", &r.root_sha), ("path", "old name.txt"), ("side", "old")]);
    assert_eq!(old, json!({ "content": "" }));
}

#[test]
fn commit_mode_on_a_later_commit_compares_against_its_parent() {
    let (_dir, s) = branches();
    assert_eq!(tree_changes(&s, &[("mode", "commit"), ("ref", "main")]), json!({ "main-only.txt": { "status": "A", "add": 1, "del": 0 } }));
}

#[test]
fn branch_mode_compares_the_head_against_the_merge_base() {
    // main moved on after feature forked; its new file must not show up as deleted on feature.
    let (_dir, s) = branches();
    let (status, body) = s.json("/api/tree", &[("mode", "branch"), ("base", "main"), ("head", "feature")]);
    assert_eq!(status, 200);
    assert_eq!(untagged(body["changes"].clone()), json!({ "feature.txt": { "status": "A", "add": 1, "del": 0 } }));
    assert!(!body["files"].as_array().unwrap().contains(&json!("main-only.txt")));
}

#[test]
fn branch_mode_defaults_the_base_to_main() {
    let (_dir, s) = branches();
    assert_eq!(s.body("/api/info", &[])["defaultBase"], "main");
    assert_eq!(tree_changes(&s, &[("mode", "branch"), ("head", "feature")]), json!({ "feature.txt": { "status": "A", "add": 1, "del": 0 } }));
    // The old side of the comparison is the merge base: shared.txt as it was when feature forked.
    let mb = s.body("/api/file", &[("mode", "branch"), ("head", "feature"), ("path", "shared.txt"), ("side", "old")]);
    assert_eq!(mb, json!({ "content": lines(5, None) }));
    // Head defaults to HEAD (main here), so main against itself has no changes.
    assert_eq!(tree_changes(&s, &[("mode", "branch")]), json!({}));
}

#[test]
fn the_default_base_is_the_first_of_main_master_origin_main_origin_master_and_develop() {
    // Every candidate exists at first; each round deletes the one that won, so the next in line must win. HEAD is on
    // another branch, so main can go too. The origin/* refs are made by hand, with no remote to fetch from.
    let dir = new_repo("default-base");
    let root = dir.path();
    write(root, "a.txt", "one\n");
    commit_all(root, "root");
    git(root, &["switch", "-q", "-c", "work"]);
    git(root, &["branch", "master"]);
    git(root, &["branch", "develop"]);
    let sha = rev(root, "HEAD");
    git(root, &["update-ref", "refs/remotes/origin/main", &sha]);
    git(root, &["update-ref", "refs/remotes/origin/master", &sha]);
    let order = [
        ("main", "refs/heads/main"),
        ("master", "refs/heads/master"),
        ("origin/main", "refs/remotes/origin/main"),
        ("origin/master", "refs/remotes/origin/master"),
        ("develop", "refs/heads/develop"),
    ];
    for (name, full) in order {
        // A fresh server each round: the default base is cached for a few seconds.
        let s = Server::start(root);
        assert_eq!(s.body("/api/info", &[])["defaultBase"], name);
        drop(s);
        git(root, &["update-ref", "-d", full]);
    }
    let s = Server::start(root);
    assert_eq!(s.body("/api/info", &[])["defaultBase"], Value::Null, "work is never a default base");
}

// The tree names the commit it listed, by its full sha, however the view named it; the working tree is no commit.
// History opens at the first commit of its list with the tree it fetched for the branch's name, and checks the two.
#[test]
fn the_tree_reports_the_commit_it_listed() {
    let (dir, s) = branches();
    let root = dir.path();
    let commit = |params: &[(&str, &str)]| s.body("/api/tree", params)["commit"].clone();
    assert_eq!(commit(&[("mode", "commit"), ("ref", "main")]), rev(root, "main"));
    assert_eq!(commit(&[("mode", "commit"), ("ref", &rev(root, "feature")[..7])]), rev(root, "feature"));
    assert_eq!(commit(&[("mode", "commit")]), rev(root, "HEAD"));
    assert_eq!(commit(&[("mode", "branch"), ("base", "main"), ("head", "feature")]), rev(root, "feature"));
    assert_eq!(commit(&[("mode", "working")]), Value::Null);
}

#[test]
fn an_unknown_mode_is_rejected() {
    let (_dir, s) = branches();
    let (status, body) = s.json("/api/tree", &[("mode", "bogus")]);
    assert_eq!(status, 400);
    assert_eq!(body["error"], "unknown mode bogus");
}

type Params = &'static [(&'static str, &'static str)];

#[test]
fn bad_input_gets_400_a_missing_file_404() {
    let (dir, s) = branches();
    // The view's refs are resolved once, so a file missing from the commit is reported for the commit read.
    let missing = format!("does not exist in '{}'", rev(dir.path(), "main"));
    let cases: &[(u16, &str, Params, &str)] = &[
        (400, "/api/tree", &[("mode", "commit"), ("ref", "nosuch")], "Not a valid object name nosuch"),
        (400, "/api/log", &[("branch", "nosuch")], "bad revision 'nosuch'"),
        (400, "/api/tree", &[("mode", "branch"), ("base", "main"), ("head", "nosuch")], "nosuch"),
        (404, "/api/file", &[("mode", "working"), ("path", "nope.txt")], "file not found: nope.txt"),
        (404, "/api/file", &[("mode", "commit"), ("ref", "main"), ("path", "feature.txt")], &missing),
        (404, "/api/file", &[("mode", "branch"), ("head", "feature"), ("path", "feature.txt"), ("side", "old")], "does not exist"),
    ];
    for (want, path, params, msg) in cases {
        let (status, body) = s.json(path, params);
        let e = body["error"].as_str().unwrap();
        assert_eq!(status, *want, "{path} {params:?}: {e}");
        assert!(e.contains(msg), "{path} {params:?}: {e}");
    }
    assert_eq!(s.body("/api/file", &[("mode", "working"), ("path", "nope.txt")])["error"], "file not found: nope.txt");
    let posts: &[(&str, &str, Params, &str)] = &[
        ("/api/navigate", r#"{"mode":"commit","ref":"nosuch"}"#, &[], "unknown revision: nosuch"),
        ("/api/navigate", r#"{"line":3}"#, &[], "line needs a file"),
        ("/api/navigate", "not json", &[], "bad JSON body"),
        ("/api/annotations", r#"{"action":"nope"}"#, &[], "action must be add or clear"),
        ("/api/switch", "", &[("branch", "nosuch")], "no branch named nosuch"),
    ];
    for (path, body, params, msg) in posts {
        let (status, res) = s.post(path, body, params);
        assert_eq!(status, 400, "{path} {body}");
        assert_eq!(res["error"], *msg);
    }
}

#[test]
fn an_internal_git_failure_gets_500_and_is_logged_to_stderr() {
    let (_dir, s) = corrupt();
    let before = s.stderr().len();
    // Whole lines only: stderr is unbuffered, so eprintln! writes a line in pieces and one can be read half written.
    let logged = || {
        let text = s.stderr()[before..].to_string();
        let whole = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
        whole.lines().filter(|l| !l.is_empty()).map(String::from).collect::<Vec<_>>()
    };
    let (status, body) = s.json("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "lost.txt")]);
    assert_eq!(status, 500);
    let e = body["error"].as_str().unwrap();
    assert!(["bad object", "unable to read", "could not get object info"].iter().any(|m| e.contains(m)), "{e}");
    assert!(wait_until(2000, || !logged().is_empty()), "nothing logged");
    assert_eq!(logged().len(), 1, "{:?}", logged());
    assert!(logged()[0].starts_with("revue: GET /api/file?") && logged()[0].contains("lost.txt"), "{}", logged()[0]);
    // Bad input next to it is still a 4xx and isn't logged.
    assert_eq!(s.get("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "../x")]).status, 400);
    sleep(100); // a log line would have arrived by now
    assert_eq!(logged().len(), 1);
    // The rest of the repo is still readable.
    assert_eq!(s.body("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "fine.txt")]), json!({ "content": "still here\n" }));
}
