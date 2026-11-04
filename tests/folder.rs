// Plain folders: a folder outside any repo, or one opened with --no-git, is shown as its files on disk. The listing
// walks the folder (.git included, capped), files are read as in the working tree, the git endpoints say there is no
// repo, notes are filed under one key, and live updates see every file.
#![cfg(unix)]
mod common;
use common::*;
use serde_json::Value;

fn folder() -> TempDir {
    let dir = TempDir::new("folder");
    let root = dir.path();
    write(root, "README.md", "# Notes\n");
    write(root, "docs/a.txt", "one\ntwo\n");
    write(root, ".hidden/h.txt", "h\n");
    write(root, ".git/HEAD", "ref: refs/heads/main\n");
    dir
}

fn start(root: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Server {
    Server::try_start(root, args, env).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr))
}

fn files(tree: &Value) -> Vec<String> {
    tree["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect()
}

// The variable that puts `--notes file`'s data directory (folder::notes_file) in `data`, and the notes directory that
// gives: XDG_DATA_HOME elsewhere, but macOS uses ~/Library/Application Support, so there HOME. Otherwise a test on a
// Mac would write into the real one.
fn data_home(data: &std::path::Path) -> (&'static str, std::path::PathBuf) {
    if cfg!(target_os = "macos") {
        ("HOME", data.join("Library/Application Support/revue/notes"))
    } else {
        ("XDG_DATA_HOME", data.join("revue/notes"))
    }
}

#[test]
fn a_folder_outside_any_repo_opens_as_a_plain_folder() {
    let dir = folder();
    let s = Server::start(dir.path());
    let info = s.body("/api/info", &[]);
    assert_eq!(info["git"], false);
    assert_eq!(info["root"], dir.path().to_string_lossy().as_ref());
    assert!(info.get("branch").is_none(), "{info}");
    let tree = s.body("/api/tree", &[]);
    assert_eq!(files(&tree), [".git/HEAD", ".hidden/h.txt", "README.md", "docs/a.txt"], "hidden files and .git are listed");
    assert_eq!(tree["changes"], serde_json::json!({}));
    assert_eq!(tree["commit"], Value::Null);
    assert_eq!(tree["truncated"], false);
    // A view's parameters are ignored, not refused: a page asks with a mode before /api/info says there are none.
    let (status, same) = s.json("/api/tree", &[("mode", "commit"), ("ref", "nope")]);
    assert_eq!(status, 200);
    assert_eq!(same, tree);
    let server = s.body("/api/server", &[]);
    assert_eq!(server["git"], false);
    assert_eq!(server["watcher"], "ok");
}

#[test]
fn no_git_opens_a_repo_folder_as_a_plain_folder() {
    let dir = TempDir::new("folder-nogit");
    let root = dir.path();
    init(root);
    write(root, "a.txt", "one\n");
    write(root, "sub/b.txt", "two\n");
    commit_all(root, "init");

    // Without the flag a subfolder opens its repo, at the top level.
    let repo = Server::start(&root.join("sub"));
    let info = repo.body("/api/info", &[]);
    assert_eq!(info["git"], true);
    assert_eq!(info["root"], root.to_string_lossy().as_ref());

    // With it, the folder itself, and none of the repo.
    let sub = start(&root.join("sub"), &["--no-git"], &[]);
    let info = sub.body("/api/info", &[]);
    assert_eq!(info["git"], false);
    assert_eq!(info["root"], root.join("sub").to_string_lossy().as_ref());
    assert_eq!(files(&sub.body("/api/tree", &[])), ["b.txt"]);

    // At the top level the repo's own files are listed too, .git last in the walk but sorted with the rest.
    let top = start(root, &["--no-git"], &[]);
    let listed = files(&top.body("/api/tree", &[]));
    assert!(listed.contains(&".git/HEAD".to_string()), "{listed:?}");
    assert!(listed.iter().any(|f| f.starts_with(".git/objects/")), "{listed:?}");
    assert!(listed.contains(&"sub/b.txt".to_string()));
    // revue writes nothing into .git in a plain folder: no index refresh, no notes.
    let index = std::fs::metadata(root.join(".git/index")).unwrap().modified().unwrap();
    write(root, "a.txt", "changed\n");
    top.body("/api/tree", &[]);
    assert_eq!(std::fs::metadata(root.join(".git/index")).unwrap().modified().unwrap(), index);
}

#[test]
fn git_endpoints_say_there_is_no_repo() {
    let dir = folder();
    let s = Server::start(dir.path());
    for path in ["/api/diff", "/api/diffs", "/api/log"] {
        let (status, body) = s.json(path, &[("path", "docs/a.txt")]);
        assert_eq!(status, 400, "{path}");
        assert!(body["error"].as_str().unwrap().ends_with("is not a git repository"), "{path}: {body}");
    }
    let (status, body) = s.post("/api/switch", "", &[("branch", "main")]);
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().ends_with("is not a git repository"), "{body}");
    // The write guard still comes first.
    assert_eq!(request(s.port, "POST", "/api/switch?branch=main", &[], b"").status, 403);
}

#[test]
fn files_are_read_from_disk_and_kept_inside_the_folder() {
    let dir = folder();
    let root = dir.path();
    let outside = TempDir::new("folder-outside");
    write(outside.path(), "secret.txt", "secret\n");
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("leak.txt")).unwrap();
    std::os::unix::fs::symlink("docs/a.txt", root.join("inside.txt")).unwrap();
    let s = Server::start(root);
    assert_eq!(s.body("/api/file", &[("path", "docs/a.txt")])["content"], "one\ntwo\n");
    // There is one side: `old` reads the file too.
    assert_eq!(s.body("/api/file", &[("path", "docs/a.txt"), ("side", "old")])["content"], "one\ntwo\n");
    assert_eq!(s.body("/api/file", &[("path", ".git/HEAD")])["content"], "ref: refs/heads/main\n");
    assert_eq!(s.body("/api/file", &[("path", "inside.txt")])["content"], "one\ntwo\n");
    assert_eq!(s.body("/api/file", &[("path", "leak.txt")]), serde_json::json!({ "outside": true }));
    assert_eq!(s.json("/api/file", &[("path", "../x")]).0, 400);
    assert_eq!(s.json("/api/file", &[("path", "docs")]).0, 400);
    assert_eq!(s.json("/api/file", &[("path", "missing.txt")]).0, 404);
}

#[test]
fn the_listing_stops_at_the_cap_and_watches_only_what_it_reached() {
    let dir = folder();
    let root = dir.path();
    write(root, "docs/b.txt", "b\n");
    write(root, "docs/deep/er/x.txt", "x\n");
    // The cap is reached in docs/, so docs/deep is never read.
    let s = start(root, &[], &[("REVUE_TEST_MAX_FILES", "3")]);
    let tree = s.body("/api/tree", &[]);
    assert_eq!(tree["truncated"], true);
    // Breadth first, .git last: the top level, then the next one down, until the cap.
    assert_eq!(files(&tree), [".hidden/h.txt", "README.md", "docs/a.txt"]);
    // Linux watches folder by folder; macOS's one recursive watch lists only the root, and filters events instead.
    if cfg!(target_os = "linux") {
        let watched = s.watched();
        assert!(watched.contains(&"docs".to_string()), "{watched:?}");
        assert!(!watched.iter().any(|w| w == "docs/deep" || w == "docs/deep/er" || w == ".git"), "{watched:?}");
    }
    // Changes outside the reached folders go unseen, so remembered answers expire instead.
    assert_eq!(s.body("/api/server", &[])["watcher"], "partial");
    // Under the cap, every folder is watched.
    let all = Server::start(root);
    assert_eq!(all.body("/api/tree", &[])["truncated"], false);
    if cfg!(target_os = "linux") {
        assert!(wait_until(3000, || all.watched().iter().any(|w| w == "docs/deep/er")), "{:?}", all.watched());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn every_change_is_a_worktree_change_and_updates_the_listing() {
    let dir = folder();
    let root = dir.path();
    let s = Server::start(root);
    assert!(!files(&s.body("/api/tree", &[])).contains(&"new.txt".to_string()));
    let ev = s.events();
    write(root, "new.txt", "new\n");
    assert!(ev.wait_changes(1, 5000), "no change message");
    assert!(files(&s.body("/api/tree", &[])).contains(&"new.txt".to_string()));
    // Changes under .git are only files in a plain folder. Every message for new.txt has come once the stream is
    // settled, so none can pass for the .git change's.
    s.settle(&ev);
    ev.clear();
    write(root, ".git/refs/heads/main", "1234\n");
    assert!(ev.wait_changes(1, 5000), "no change message for .git");
    assert!(ev.data().iter().all(|d| d == "worktree"), "{:?}", ev.data());
    assert!(wait_until(3000, || files(&s.body("/api/tree", &[])).contains(&".git/refs/heads/main".to_string())));
}

#[test]
fn notes_are_filed_under_the_folder() {
    let dir = folder();
    let s = Server::start(dir.path());
    let (status, nav) = s.post("/api/navigate", r#"{"mode":"commit","ref":"nope","file":"docs/a.txt","line":2,"view":"preview"}"#, &[]);
    assert_eq!(status, 200, "{nav}");
    assert_eq!(nav["view"], serde_json::json!({ "mode": "folder", "view": "preview", "file": "docs/a.txt", "line": 2, "endLine": null }));
    let (status, added) = s.post("/api/annotations", r#"{"action":"add","file":"docs/a.txt","line":2,"text":"about two","side":"old"}"#, &[]);
    assert_eq!(status, 200, "{added}");
    assert_eq!(added["note"]["key"], "files");
    assert_eq!(added["note"]["side"], "new", "a folder's files have one side");
    assert_eq!(added["note"]["snippet"], "two");
    let listed = s.body("/api/annotations", &[("mode", "branch")]);
    assert_eq!(listed["key"], "files");
    assert_eq!(listed["notes"][0]["stale"], false);
    write(dir.path(), "docs/a.txt", "one\nTWO\n");
    assert_eq!(s.body("/api/annotations", &[("all", "1")])["notes"][0]["stale"], true);
    assert_eq!(s.body("/api/annotations", &[("all", "1")])["others"][0]["view"], serde_json::json!({ "mode": "folder" }));
}

#[test]
fn notes_in_a_file_go_to_the_data_directory() {
    let dir = folder();
    let data = TempDir::new("folder-data");
    let (var, notes_dir) = data_home(data.path());
    let env = [(var, data.path().to_str().unwrap())];
    let before = files(&Server::start(dir.path()).body("/api/tree", &[]));
    {
        let s = start(dir.path(), &["--notes", "file"], &env);
        let (status, body) = s.post("/api/annotations", r#"{"action":"add","file":"README.md","line":1,"text":"kept"}"#, &[]);
        assert_eq!(status, 200, "{body}");
        assert_eq!(s.body("/api/server", &[])["notes"], "file");
    }
    let saved: Vec<_> = std::fs::read_dir(&notes_dir).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(saved.len(), 1, "{saved:?}");
    assert!(std::fs::read_to_string(&saved[0]).unwrap().contains("\"kept\""));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&notes_dir).unwrap().permissions().mode() & 0o777, 0o700);
    // Nothing was written into the folder, and a new server reads the notes back.
    let s = start(dir.path(), &["--notes", "file"], &env);
    assert_eq!(files(&s.body("/api/tree", &[])), before);
    assert_eq!(s.body("/api/annotations", &[])["notes"][0]["text"], "kept");
}

#[test]
fn a_notes_directory_others_can_use_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder();
    let data = TempDir::new("folder-data-open");
    let (var, notes_dir) = data_home(data.path());
    std::fs::create_dir_all(&notes_dir).unwrap();
    std::fs::set_permissions(&notes_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = Server::try_start(dir.path(), &["--notes", "file"], &[(var, data.path().to_str().unwrap())]).err().unwrap();
    assert_eq!(err.code, Some(1));
    assert!(err.stderr.contains("other users can use it"), "{}", err.stderr);
}

#[test]
fn a_repo_server_and_a_folder_server_on_the_same_root_both_register() {
    let dir = TempDir::new("folder-both");
    let root = dir.path();
    init(root);
    write(root, "a.txt", "one\n");
    commit_all(root, "init");
    let runtime = TempDir::new("folder-runtime");
    let env = [("XDG_RUNTIME_DIR", runtime.path().to_str().unwrap())];
    let _repo = start(root, &[], &env);
    let _folder = start(root, &["--no-git"], &env);
    let mut entries: Vec<Value> = std::fs::read_dir(runtime.path().join("revue"))
        .unwrap()
        .flatten()
        .map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect();
    entries.sort_by_key(|e| e["git"].as_bool());
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[0]["git"], false);
    assert_eq!(entries[1]["git"], true);
    assert!(entries.iter().all(|e| e["root"] == root.to_string_lossy().as_ref()));
}
