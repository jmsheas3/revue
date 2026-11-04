// HTTP API and live-update regressions. Each test builds a throwaway repo and starts a server on a free port.
mod common;
use common::*;
use std::path::Path;

// A committed a.txt, a tracked build/ folder, and ignored node_modules/ and target/ folders.
fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("server");
    let root = dir.path();
    init(root);
    write(root, "a.txt", lines(20, None));
    write(root, ".gitignore", "node_modules/\ntarget/\n");
    write(root, "Cargo.lock", "version = 3\n");
    write(root, "build/keep.txt", "tracked build output\n");
    write(root, "node_modules/pkg/index.js", "ignored\n");
    write(root, "target/debug/app", "ignored\n");
    commit_all(root, "init");
    let server = Server::start(root);
    (dir, server)
}

#[test]
fn an_empty_untracked_file_counts_as_0_added_lines() {
    let (dir, s) = repo();
    write(dir.path(), "empty.txt", "");
    write(dir.path(), "two.txt", "a\nb\n");
    let body = s.body("/api/tree", &[("mode", "working")]);
    assert_eq!(body["changes"]["empty.txt"]["add"], 0);
    assert_eq!(body["changes"]["two.txt"]["add"], 2);
}

// git's numstat counts no lines in a binary file ("-\t-"), so a staged one has null counts, which the page marks
// "bin". An untracked one gets the same. An untracked text file too big to count isn't binary: it keeps del 0, as a
// file that is only added. git calls a file binary for a NUL in its first 8000 bytes only, and counts the lines of
// one with a NUL further on, staged or not.
#[test]
fn an_untracked_binary_file_has_null_counts_like_a_staged_one() {
    let (dir, s) = repo();
    let root = dir.path();
    let late_nul = format!("{}\0tail\n", "x\n".repeat(5000));
    for p in ["staged.bin", "untracked.bin"] {
        write(root, p, [0x89u8, b'P', 0, 1, 0, 2]);
    }
    for p in ["staged-late.txt", "untracked-late.txt"] {
        write(root, p, &late_nul);
    }
    git(root, &["add", "staged.bin", "staged-late.txt"]);
    write(root, "huge.txt", "x\n".repeat(3 * 512 * 1024)); // 3 MB
    let ch = untagged(s.body("/api/tree", &[("mode", "working")])["changes"].clone());
    assert_eq!(ch["staged.bin"], json!({ "status": "A", "add": null, "del": null }));
    assert_eq!(ch["untracked.bin"], json!({ "status": "?", "add": null, "del": null }));
    assert_eq!(ch["staged-late.txt"], json!({ "status": "A", "add": 5001, "del": 0 }));
    assert_eq!(ch["untracked-late.txt"], json!({ "status": "?", "add": 5001, "del": 0 }));
    assert_eq!(ch["huge.txt"], json!({ "status": "?", "add": null, "del": 0 }));
}

#[test]
fn context_0_gives_a_diff_with_no_context_lines() {
    let (dir, s) = repo();
    write(dir.path(), "a.txt", read(dir.path(), "a.txt").replace("line 10\n", "line ten\n"));
    let zero = s.body("/api/diff", &[("mode", "working"), ("path", "a.txt"), ("context", "0")])["diff"].as_str().unwrap().to_string();
    assert!(zero.lines().any(|l| l.starts_with("@@ -10 +10 @@")), "{zero}");
    assert!(!zero.lines().any(|l| l.starts_with(" line")), "{zero}");
    let dflt = s.body("/api/diff", &[("mode", "working"), ("path", "a.txt")])["diff"].as_str().unwrap().to_string();
    assert!(dflt.lines().any(|l| l.starts_with("@@ -7,7 +7,7 @@")), "{dflt}");
}

// A name with a quote and a tab in it, which git quotes in its output. Windows can't make it.
const QUOTED: &str = "k/q\"uote\there.txt";
const QUOTED_MOVED: &str = "k/q\"uote\tmoved.txt";

// Committed files, then one of each kind of working-tree change.
fn change_kinds(root: &Path) {
    let ten = (1..=10).map(|i| format!("row {i}\n")).collect::<String>();
    for f in ["k/mod.txt", "k/old name.txt", "k/gone.txt", QUOTED].into_iter().filter(|f| can_name(f)) {
        write(root, f, format!("{ten}{f}\n"));
    }
    write(root, "k/bin.dat", "a\0b");
    git(root, &["add", "k"]);
    commit(root, "kinds");
    write(root, "k/mod.txt", format!("{}k/mod.txt\n", ten.replace("row 5", "row five")));
    git(root, &["mv", "k/old name.txt", "k/new näme.txt"]);
    if can_name(QUOTED) {
        git(root, &["mv", QUOTED, QUOTED_MOVED]);
    }
    std::fs::remove_file(root.join("k/gone.txt")).unwrap();
    write(root, "k/bin.dat", "c\0d");
    write(root, "k/added.txt", "staged\n");
    git(root, &["add", "k/added.txt"]);
    write(root, "k/new.txt", "u1\nu2\n");
}

#[test]
fn tree_reports_every_kind_of_change() {
    let (dir, s) = repo();
    change_kinds(dir.path());
    let body = s.body("/api/tree", &[("mode", "working")]);
    let changes = untagged(body["changes"].clone());
    let k: serde_json::Map<String, Value> =
        changes.as_object().unwrap().iter().filter(|(p, _)| p.starts_with("k/")).map(|(p, v)| (p.clone(), v.clone())).collect();
    let mut want = json!({
        "k/mod.txt": { "status": "M", "add": 1, "del": 1 },
        "k/new näme.txt": { "status": "R", "oldPath": "k/old name.txt", "add": 0, "del": 0 },
        QUOTED_MOVED: { "status": "R", "oldPath": QUOTED, "add": 0, "del": 0 },
        "k/gone.txt": { "status": "D", "add": 0, "del": 11 },
        "k/bin.dat": { "status": "M", "add": null, "del": null },
        "k/added.txt": { "status": "A", "add": 1, "del": 0 },
        "k/new.txt": { "status": "?", "add": 2, "del": 0 },
    });
    if !can_name(QUOTED) {
        want.as_object_mut().unwrap().remove(QUOTED_MOVED);
    }
    assert_eq!(Value::Object(k.clone()), want);
    let files: Vec<&str> = body["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    for f in k.keys().map(|s| s.as_str()).chain(["a.txt"]) {
        assert!(files.contains(&f), "{f}");
    }
    assert!(!files.contains(&"k/old name.txt"));
    let mut sorted = files.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(files, sorted);
}

#[test]
fn api_diffs_gives_each_file_what_api_diff_gives_it() {
    let (dir, s) = repo();
    change_kinds(dir.path());
    let changes = s.body("/api/tree", &[("mode", "working")])["changes"].clone();
    let files: Vec<String> = changes.as_object().unwrap().keys().filter(|p| p.starts_with("k/")).cloned().collect();
    for context in ["3", "0", "all"] {
        let mut params: Vec<(&str, &str)> = vec![("mode", "working"), ("context", context)];
        for p in &files {
            params.push(("path", p));
            params.push(("old", changes[p]["oldPath"].as_str().unwrap_or("")));
            params.push(("status", changes[p]["status"].as_str().unwrap()));
        }
        params.extend([("path", "a.txt"), ("old", ""), ("status", "M")]); // unchanged
        let (status, body) = s.json("/api/diffs", &params);
        assert_eq!(status, 200);
        let diffs = body["diffs"].as_object().unwrap();
        let mut keys: Vec<&String> = diffs.keys().collect();
        keys.sort();
        let mut want: Vec<String> = files.iter().cloned().chain(["a.txt".to_string()]).collect();
        want.sort();
        assert_eq!(keys, want.iter().collect::<Vec<_>>());
        assert_eq!(diffs["a.txt"], "");
        for p in &files {
            let one = s.body(
                "/api/diff",
                &[
                    ("mode", "working"),
                    ("context", context),
                    ("path", p),
                    ("old", changes[p]["oldPath"].as_str().unwrap_or("")),
                    ("status", changes[p]["status"].as_str().unwrap()),
                ],
            )["diff"]
                .clone();
            assert_ne!(one, "", "{p}");
            assert_eq!(diffs[p], one, "{p} context={context}");
        }
    }
}

#[test]
fn api_diffs_rejects_too_many_paths_and_paths_outside_the_repo() {
    let (_dir, s) = repo();
    let names: Vec<String> = (0..51).map(|i| format!("f{i}.txt")).collect();
    let mut many: Vec<(&str, &str)> = vec![("mode", "working")];
    many.extend(names.iter().map(|n| ("path", n.as_str())));
    let (status, body) = s.json("/api/diffs", &many);
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("at most 50 paths"));
    for params in [vec![("path", "../x.txt")], vec![("path", "a.txt"), ("old", "../../etc/passwd")]] {
        let mut p = vec![("mode", "working")];
        p.extend(params);
        let (status, body) = s.json("/api/diffs", &p);
        assert_eq!(status, 400);
        assert_eq!(body["error"], "path escapes repo");
    }
    assert_eq!(s.body("/api/diffs", &[("mode", "working")]), json!({ "diffs": {} }));
}

#[test]
fn static_files_carry_an_etag_and_revalidate_with_304() {
    let (_dir, s) = repo();
    let first = s.get("/app.js", &[]);
    assert_eq!(first.status, 200);
    assert_eq!(first.header("cache-control"), Some("no-cache"));
    // The files are built into the binary, so the tag is their size and a content hash.
    let size = std::fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("public/app.js")).unwrap().len();
    let etag = first.header("etag").unwrap().to_string();
    let prefix = format!("W/\"{size}-");
    assert!(etag.starts_with(&prefix) && etag.len() == prefix.len() + 17 && etag.ends_with('"'), "{etag}");
    assert_eq!(first.body.len() as u64, size);
    let again = request(s.port, "GET", "/app.js", &[("if-none-match", &etag)], b"");
    assert_eq!(again.status, 304);
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    assert!(again.body.is_empty());
    let stale = request(s.port, "GET", "/app.js", &[("if-none-match", "W/\"1-2\"")], b"");
    assert_eq!(stale.status, 200);
    assert_eq!(stale.body.len() as u64, size);
}

#[test]
fn reading_a_file_missing_from_disk_gives_a_clean_error() {
    let (dir, s) = repo();
    let (status, body) = s.json("/api/file", &[("mode", "working"), ("path", "nope/missing.txt")]);
    assert_eq!(status, 404);
    assert_eq!(body["error"], "file not found: nope/missing.txt");
    assert!(!body["error"].as_str().unwrap().contains(dir.path().to_str().unwrap()));
}

// ---------------------------------------------------------------- live updates
#[cfg(target_os = "linux")] // one native recursive watch elsewhere
#[test]
fn ignored_folders_get_no_watchers_tracked_ones_do() {
    let (_dir, s) = repo();
    let watched = s.watched();
    for d in ["", ".git", "build"] {
        assert!(watched.iter().any(|w| w == d), "{d} not watched: {watched:?}");
    }
    for d in ["node_modules", "node_modules/pkg", "target", ".git/objects", ".git/logs"] {
        assert!(!watched.iter().any(|w| w == d), "{d} watched");
    }
}

#[test]
fn a_tracked_cargo_lock_change_triggers_an_update() {
    let (dir, s) = repo();
    let ev = s.events();
    write(dir.path(), "Cargo.lock", "version = 4\n");
    assert!(ev.wait_changes(1, 3000), "no change event");
}

#[test]
fn a_tracked_file_in_a_build_folder_git_does_not_ignore_triggers_an_update() {
    let (dir, s) = repo();
    let ev = s.events();
    write(dir.path(), "build/keep.txt", "changed\n");
    assert!(ev.wait_changes(1, 3000), "no change event");
}

#[test]
fn changes_inside_an_ignored_node_modules_do_not_trigger_updates() {
    let (dir, s) = repo();
    let ev = s.events();
    write(dir.path(), "node_modules/pkg/index.js", "changed\n");
    write(dir.path(), "target/debug/app", "changed\n");
    sleep(600); // longer than the debounce, so an event would have arrived
    assert_eq!(ev.changes(), 0, "{:?}", ev.data());
    // The watcher is alive: a tracked file still triggers one, so the silence above meant something.
    write(dir.path(), "a.txt", "edited\n");
    assert!(ev.wait_changes(1, 3000), "the watcher reports nothing at all");
}

#[test]
fn folders_created_after_startup_are_watched() {
    let (dir, s) = repo();
    let ev = s.events();
    std::fs::create_dir_all(dir.path().join("later/deeper")).unwrap();
    if cfg!(target_os = "linux") {
        assert!(wait_until(3000, || s.watched().iter().any(|w| w == "later/deeper")), "later/deeper never watched");
    }
    assert!(ev.wait_changes(1, 3000), "no event for the new folder");
    s.settle(&ev); // every message for the new folder has come, so none passes for the file's
    ev.clear();
    write(dir.path(), "later/deeper/x.txt", "x\n");
    assert!(ev.wait_changes(1, 3000), "no event for a file in the new folder");
    std::fs::remove_dir_all(dir.path().join("later")).unwrap();
    if cfg!(target_os = "linux") {
        wait_until(3000, || !s.watched().iter().any(|w| w.starts_with("later")));
        let watched = s.watched();
        assert!(!watched.iter().any(|w| w == "later" || w == "later/deeper"), "{watched:?}");
    }
}

#[test]
fn a_port_in_use_is_an_error_and_a_watcher_error_keeps_the_server_up() {
    let (dir, s) = repo();
    // An explicit port that is taken fails instead of falling back to a free one.
    let port = s.port.to_string();
    let Err(e) = Server::try_start(dir.path(), &["--port", &port], &[]) else { panic!("started on a port in use") };
    assert_eq!(e.code, Some(1));
    assert!(e.stderr.contains(&format!("port {port} is already in use")), "{}", e.stderr);

    let other = Server::start(dir.path());
    assert!(!other.watched().is_empty());
    let watched = other.debug(&[("watchError", "boom")])["watched"].clone();
    assert!(other.stderr().contains("live updates disabled: boom"), "{}", other.stderr());
    assert_eq!(watched, json!([]));
    assert_eq!(other.get("/api/info", &[]).status, 200);
}

#[test]
fn an_unknown_api_path_is_a_json_404_and_a_server_whose_code_changed_on_disk_says_it_is_stale() {
    let (dir, s) = repo();
    let (status, body) = s.json("/api/nope", &[]);
    assert_eq!(status, 404);
    assert!(body["error"].as_str().unwrap().contains("unknown endpoint /api/nope"));
    assert_eq!(s.body("/api/info", &[])["stale"], false);

    // A stand-in for the server's binary (REVUE_CODE_FILE, honored with REVUE_TEST), rewritten while it runs.
    let code_dir = TempDir::new("code");
    let code = code_dir.path().join("revue.bin");
    std::fs::write(&code, "v1").unwrap();
    let other = Server::try_start(dir.path(), &[], &[("REVUE_CODE_FILE", code.to_str().unwrap())]).ok().unwrap();
    assert_eq!(other.body("/api/info", &[])["stale"], false);
    std::fs::remove_file(&code).unwrap();
    std::fs::write(&code, "v2").unwrap();
    assert_eq!(other.body("/api/info", &[])["stale"], true);
    assert_eq!(other.body("/api/server", &[])["stale"], true);
    let (status, body) = other.json("/api/added-later", &[]);
    assert_eq!(status, 404);
    assert!(body["error"].as_str().unwrap().contains("older code than is on disk; restart it"));
}

// A detached HEAD has no branch: /api/info still names it, by its short sha, and says it is detached, so the page
// doesn't call it the checked-out branch.
#[test]
fn info_says_whether_head_is_detached() {
    let (dir, s) = repo();
    let info = s.body("/api/info", &[]);
    assert_eq!((&info["branch"], &info["detached"]), (&json!("main"), &json!(false)), "{info}");

    git(dir.path(), &["switch", "-q", "--detach"]);
    let short = &rev(dir.path(), "HEAD")[..7];
    assert!(wait_until(5000, || s.body("/api/info", &[])["detached"] == true), "{}", s.body("/api/info", &[]));
    assert_eq!(s.body("/api/info", &[])["branch"], short);

    git(dir.path(), &["switch", "-q", "main"]);
    assert!(wait_until(5000, || s.body("/api/info", &[])["detached"] == false), "{}", s.body("/api/info", &[]));
    assert_eq!(s.body("/api/info", &[])["branch"], "main");
}

// /api/server says which build is running, since when, and whether live updates see every change.
#[test]
fn server_reports_build_and_watcher() {
    let (_dir, s) = repo();
    let b = s.body("/api/server", &[]);
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
    assert!(b["commit"].is_string() && b["modified"].is_boolean(), "{b}");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let started = b["started"].as_u64().unwrap();
    assert!(started <= now && now - started < 60_000, "started {started}, now {now}");
    // The watcher starts after the server answers, so it can briefly be "off".
    assert!(wait_until(5000, || s.body("/api/server", &[])["watcher"] == "ok"));
    s.debug(&[("watchError", "off for this test")]);
    assert_eq!(s.body("/api/server", &[])["watcher"], "off");
}

// Each change carries a version tag ("v") that moves whenever the file's diff could change, and only then, so the page
// can skip refetching diffs that are still the same.
#[test]
fn change_tags_move_with_the_diff_and_only_with_it() {
    let (dir, s) = repo();
    let root = dir.path();
    let tag = |params: &[(&str, &str)], p: &str| -> String {
        s.body("/api/tree", params)["changes"][p]["v"].as_str().map(String::from).unwrap_or_default()
    };
    let working = |p: &str| tag(&[("mode", "working")], p);
    // Waits for the remembered /api/tree answer to give way to one with a different tag.
    let moved = |p: &str, from: &str| {
        let mut now = String::new();
        assert!(wait_until(5000, || { now = working(p); !now.is_empty() && now != from }), "the tag of {p} never moved from {from}");
        now
    };

    write(root, "a.txt", lines(20, Some(3)));
    write(root, "u.txt", "untracked\n");
    let a1 = moved("a.txt", "");
    let u1 = moved("u.txt", "");
    assert_ne!(a1, u1);
    // Nothing changed: the same tags, also in an answer computed again (another file's edit moves the watcher on).
    write(root, "other.txt", "x\n");
    moved("other.txt", "");
    assert_eq!(working("a.txt"), a1);
    assert_eq!(working("u.txt"), u1);

    // An edit that keeps the size and the line counts, a mode change, and an edit to an untracked file.
    write(root, "a.txt", lines(20, Some(4)));
    let a2 = moved("a.txt", &a1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("a.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
        moved("a.txt", &a2);
    }
    #[cfg(not(unix))]
    let _ = a2;
    write(root, "u.txt", "untracked, edited\n");
    moved("u.txt", &u1);

    // Committed sides are tagged by their blobs: the same commit always gives the same tag, another commit that
    // changes the file another one, and an edit in the working tree leaves both alone.
    git(root, &["add", "a.txt"]);
    commit(root, "edit 4");
    let first = rev(root, "HEAD");
    write(root, "a.txt", lines(20, Some(5)));
    git(root, &["add", "a.txt"]);
    commit(root, "edit 5");
    let second = rev(root, "HEAD");
    let in_commit = |r: &str| tag(&[("mode", "commit"), ("ref", r)], "a.txt");
    let (c1, c2) = (in_commit(&first), in_commit(&second));
    assert!(!c1.is_empty() && !c2.is_empty());
    assert_ne!(c1, c2);
    write(root, "a.txt", lines(20, Some(6)));
    let w = moved("a.txt", "");
    assert_ne!(w, c2);
    assert_eq!(in_commit(&first), c1);
    assert_eq!(in_commit(&second), c2);

    // A rename is tagged with its old path too.
    git(root, &["checkout", "-q", "--", "a.txt"]);
    git(root, &["mv", "a.txt", "b.txt"]);
    commit(root, "rename");
    let renamed = s.body("/api/tree", &[("mode", "commit"), ("ref", "HEAD")])["changes"]["b.txt"].clone();
    assert_eq!(renamed["oldPath"], "a.txt");
    assert!(renamed["v"].as_str().is_some_and(|v| v != c2));
}

#[test]
fn an_untracked_nested_repo_shows_the_commit_git_would_add_for_it() {
    // git lists a repo inside the work tree as one untracked entry, `nest/`, and `git diff --no-index` fails on it,
    // which left its diff empty. Once added it is a gitlink, so it shows as git shows it then: the commit checked out
    // there, one line. One with no commit yet can't be added and shows just its header.
    let dir = TempDir::new("nested");
    let root = dir.path();
    init(root);
    write(root, "a.txt", "a\n");
    commit_all(root, "outer");
    let nest = root.join("nest");
    std::fs::create_dir(&nest).unwrap();
    init(&nest);
    write(&nest, "x.txt", "x\n");
    commit_all(&nest, "inner");
    std::fs::create_dir(root.join("empty")).unwrap();
    init(&root.join("empty"));
    let s = Server::start(root);
    let changes = || s.body("/api/tree", &[("mode", "working")])["changes"].clone();
    let diff = |p: &str| s.body("/api/diff", &[("mode", "working"), ("path", p), ("old", ""), ("status", "?")])["diff"].clone();
    let ch = changes();
    assert_eq!((&ch["nest/"]["status"], &ch["nest/"]["add"], &ch["nest/"]["del"]), (&json!("?"), &json!(1), &json!(0)));
    assert_eq!((&ch["empty/"]["add"], &ch["empty/"]["del"]), (&Value::Null, &json!(0)));
    assert_eq!(diff("empty/"), "diff --git a/empty b/empty\n");
    let batch = s.body(
        "/api/diffs",
        &[("mode", "working"), ("path", "nest/"), ("old", ""), ("status", "?"), ("path", "empty/"), ("old", ""), ("status", "?")],
    );
    assert_eq!(batch["diffs"]["nest/"], diff("nest/"));
    assert_eq!(batch["diffs"]["empty/"], diff("empty/"));

    // A commit in the nested repo moves its tag (the folder itself doesn't change) and its diff.
    let before = (ch["nest/"]["v"].clone(), diff("nest/"));
    write(&nest, "x.txt", "y\n");
    commit_all(&nest, "inner 2");
    let ch = changes();
    assert_ne!(ch["nest/"]["v"], before.0);
    let now = diff("nest/");
    assert_ne!(now, before.1);
    let head = git(&nest, &["rev-parse", "HEAD"]);
    assert!(now.as_str().unwrap().ends_with(&format!("+Subproject commit {head}")), "{now}");

    // The same diff and counts git gives once it is added.
    git(root, &["add", "nest"]);
    let flags = ["--no-ext-diff", "--no-color", "--src-prefix=a/", "--dst-prefix=b/"];
    let mut args = vec!["-c", "core.quotepath=off", "diff", "--cached"];
    args.extend(flags);
    args.extend(["--", "nest"]);
    assert_eq!(now, git(root, &args));
    assert!(git(root, &["diff", "--cached", "--numstat", "--", "nest"]).starts_with("1\t0\tnest"));
}
