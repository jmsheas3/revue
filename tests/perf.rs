// Caches and shortcuts that keep requests cheap must not change what the API returns. Each test starts from a
// throwaway repo with no commits yet.
mod common;
use common::*;

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("perf");
    init(dir.path());
    let s = Server::start(dir.path());
    (dir, s)
}

#[test]
fn the_cached_head_and_default_base_are_forgotten_when_the_repo_changes() {
    let (dir, s) = repo();
    let body = s.body("/api/info", &[]);
    assert_eq!(body["hasHead"], false);
    assert_eq!(body["defaultBase"], Value::Null);
    let ev = s.events();
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "first");
    assert!(ev.next(2000).is_some(), "no change event");
    let body = s.body("/api/info", &[]); // well inside the cache TTL, so only the watcher can have cleared it
    assert_eq!(body["hasHead"], true);
    assert_eq!(body["defaultBase"], "main");
}

#[test]
fn untracked_line_counts_are_unchanged_cached_and_follow_edits() {
    let (dir, s) = repo();
    let root = dir.path();
    write(root, "u/empty.txt", "");
    write(root, "u/two.txt", "a\nb\n");
    write(root, "u/no-eol.txt", "a\nb\nc");
    write(root, "u/bin.dat", [1u8, 0, 2]);
    write(root, "u/big.txt", "x\n".repeat(3 * 512 * 1024));
    // Over the size limit too, and quick to make: a PNG's first bytes (binary: it has NULs), then a hole to 3 MB, and
    // text whose first NUL comes after the 8000 bytes git looks at, so git counts it as text.
    let sparse = |rel: &str, start: &[u8]| {
        write(root, rel, start);
        std::fs::File::options().write(true).open(root.join(rel)).unwrap().set_len(3 << 20).unwrap();
    };
    sparse("u/big.png", b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR");
    sparse("u/late-nul.txt", "x\n".repeat(4500).as_bytes());
    for i in 0..40 {
        write(root, &format!("u/many/{i}.txt"), "l\n".repeat(i));
    }
    // The server logs the files it reads (REVUE_TEST); each reads() call returns and clears that log.
    s.reads();
    let ch = untagged(s.body("/api/tree", &[("mode", "working")])["changes"].clone());
    // A binary file has no counts, as git's numstat gives a tracked one, also when it is too big to read in full; a
    // text file that big is added, but not counted.
    let counts = [
        ("empty.txt", json!(0), json!(0)),
        ("two.txt", json!(2), json!(0)),
        ("no-eol.txt", json!(3), json!(0)),
        ("bin.dat", Value::Null, Value::Null),
        ("big.txt", Value::Null, json!(0)),
        ("big.png", Value::Null, Value::Null),
        ("late-nul.txt", Value::Null, json!(0)),
    ];
    for (f, add, del) in counts {
        assert_eq!(ch[format!("u/{f}")], json!({ "status": "?", "add": add, "del": del }), "{f}");
    }
    for i in 0..40 {
        assert_eq!(ch[format!("u/many/{i}.txt")]["add"], i, "{i}");
    }
    // Of a file over the size limit only the start is read, the 8000 bytes that tell binary from text.
    let sizes = s.read_sizes();
    for f in ["big.txt", "big.png", "late-nul.txt"] {
        let reads: Vec<u64> = sizes.iter().filter(|(p, _)| p.ends_with(&format!("/u/{f}"))).map(|(_, n)| *n).collect();
        assert_eq!(reads, [8000], "bytes read of {f}");
    }

    s.body("/api/tree", &[("mode", "working")]);
    assert_eq!(s.reads(), Vec::<String>::new(), "unchanged untracked files were read again");

    // Same size, different content: the mtime alone has to give it away.
    write(root, "u/two.txt", "a\n\n\n");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(root.join("u/two.txt")).unwrap().set_modified(later).unwrap();
    let ch = s.body("/api/tree", &[("mode", "working")])["changes"].clone();
    assert_eq!(ch["u/two.txt"]["add"], 3);
    let names: Vec<String> = s.reads().iter().map(|p| p.rsplit('/').next().unwrap().to_string()).collect();
    assert_eq!(names, ["two.txt"]);
    // The counts reused from the cache are the same as the ones read.
    assert_eq!((&ch["u/bin.dat"]["add"], &ch["u/bin.dat"]["del"]), (&Value::Null, &Value::Null));
    assert_eq!((&ch["u/big.txt"]["add"], &ch["u/big.txt"]["del"]), (&Value::Null, &json!(0)));
    assert_eq!((&ch["u/big.png"]["add"], &ch["u/big.png"]["del"]), (&Value::Null, &Value::Null));
    assert_eq!(ch["u/no-eol.txt"]["add"], 3);
}

#[test]
fn files_over_the_size_limit_are_reported_without_being_read() {
    let (dir, s) = repo();
    let root = dir.path();
    write(root, "a.txt", "one\n");
    commit_all(root, "first"); // so the next commit has a parent without small.txt
    let committed = "y\n".repeat(5 * 256 * 1024); // 2.5 MB
    write(root, "big.txt", &committed);
    write(root, "small.txt", "small\n");
    commit_all(root, "big");
    write(root, "big.txt", format!("{committed}more\n"));
    s.reads();
    let f = |params: &[(&str, &str)]| {
        let mut p = vec![("path", "big.txt")];
        p.extend_from_slice(params);
        s.body("/api/file", &p)
    };
    assert_eq!(f(&[("mode", "working")]), json!({ "tooLarge": true, "size": committed.len() + 5 }));
    // Only its start is read, to tell a binary file from text.
    let reads: Vec<_> = s.read_sizes().into_iter().filter(|(p, _)| p.ends_with("big.txt")).collect();
    assert!(reads.iter().all(|&(_, n)| n <= 8000), "read past the start of a working-tree file over the size limit: {reads:?}");
    assert_eq!(f(&[("mode", "working"), ("side", "old")]), json!({ "tooLarge": true, "size": committed.len() }));
    assert_eq!(f(&[("mode", "commit"), ("ref", "HEAD")]), json!({ "tooLarge": true, "size": committed.len() }));
    // Everything else behaves as before.
    assert_eq!(s.body("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "small.txt")]), json!({ "content": "small\n" }));
    let (status, body) = s.json("/api/file", &[("mode", "commit"), ("ref", "HEAD"), ("path", "small.txt"), ("side", "old")]);
    assert_eq!(status, 404);
    let e = body["error"].as_str().unwrap();
    assert!(e.contains("path 'small.txt' exists on disk, but not in '") && e.ends_with('\''), "{e}");
    assert_eq!(s.body("/api/file", &[("mode", "working"), ("path", "gone.txt")])["error"], "file not found: gone.txt");
}

#[test]
fn binary_files_over_the_size_limit_are_reported_as_binary() {
    // As git decides it, by a NUL in the first 8000 bytes: the file is too large to show either way, but a big image
    // should say it is binary, in the working tree and in a commit, as a small one does.
    let (dir, s) = repo();
    let root = dir.path();
    let mut bin = vec![b'x'; 3 * 1024 * 1024];
    bin[7999] = 0;
    let mut late = vec![b'y'; 3 * 1024 * 1024];
    late[8000] = 0; // past the start: text, as git counts it
    std::fs::write(root.join("big.bin"), &bin).unwrap();
    std::fs::write(root.join("late.dat"), &late).unwrap();
    commit_all(root, "big files");
    let f = |path: &str, params: &[(&str, &str)]| {
        let mut p = vec![("path", path)];
        p.extend_from_slice(params);
        s.body("/api/file", &p)
    };
    s.reads();
    assert_eq!(f("big.bin", &[("mode", "working")]), json!({ "binary": true, "size": bin.len() }));
    assert_eq!(f("late.dat", &[("mode", "working")]), json!({ "tooLarge": true, "size": late.len() }));
    let reads = s.read_sizes();
    assert!(reads.iter().any(|(p, _)| p.ends_with("big.bin")), "{reads:?}");
    assert!(reads.iter().all(|&(_, n)| n <= 8000), "read past the start of a file over the size limit: {reads:?}");
    for params in [&[("mode", "commit"), ("ref", "HEAD")][..], &[("mode", "working"), ("side", "old")]] {
        assert_eq!(f("big.bin", params), json!({ "binary": true, "size": bin.len() }), "{params:?}");
        assert_eq!(f("late.dat", params), json!({ "tooLarge": true, "size": late.len() }), "{params:?}");
    }
}

#[test]
fn untracked_text_files_over_the_size_limit_get_no_diff() {
    // An untracked text file's diff is all of it, which neither view shows past 2 MB: the server doesn't make one
    // (git would read the whole file and write every line), and says why, as /api/file does. /api/diffs says the same
    // among the other files' text. A binary file that big is still diffed (git writes one line saying so), and a
    // tracked file is diffed whatever its size, as git diffs it.
    let (dir, s) = repo();
    let root = dir.path();
    write(root, "tracked.txt", "a\n");
    commit_all(root, "first");
    let big = "y\n".repeat(3 * 512 * 1024); // 3 MB
    write(root, "tracked.txt", &big);
    write(root, "big.log", &big);
    let mut bin = vec![b'x'; 3 << 20];
    bin[7999] = 0;
    std::fs::write(root.join("big.bin"), &bin).unwrap();
    write(root, "small.txt", "small\n");
    let diff = |p: &str, status: &str| s.body("/api/diff", &[("mode", "working"), ("path", p), ("old", ""), ("status", status)]);

    s.reads();
    assert_eq!(diff("big.log", "?"), json!({ "diff": null, "tooLarge": true, "size": big.len() }));
    // Only its start is read, to tell a binary file from text.
    let reads: Vec<_> = s.read_sizes().into_iter().filter(|(p, _)| p.ends_with("big.log")).collect();
    assert_eq!(reads.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [8000], "{reads:?}");

    let bin_diff = diff("big.bin", "?")["diff"].as_str().unwrap().to_string();
    assert!(bin_diff.ends_with("Binary files /dev/null and b/big.bin differ\n"), "{bin_diff}");
    let tracked = diff("tracked.txt", "M")["diff"].as_str().unwrap().to_string();
    assert!(tracked.len() > big.len() && tracked.contains("\n+y\n"), "a tracked file's diff is cut short");
    assert!(diff("small.txt", "?")["diff"].as_str().unwrap().ends_with("\n+small\n"));

    let mut batch: Vec<(&str, &str)> = vec![("mode", "working")];
    for (p, st) in [("big.log", "?"), ("big.bin", "?"), ("small.txt", "?"), ("tracked.txt", "M")] {
        batch.extend([("path", p), ("old", ""), ("status", st)]);
    }
    let diffs = s.body("/api/diffs", &batch)["diffs"].clone();
    assert_eq!(diffs["big.log"], json!({ "tooLarge": true, "size": big.len() }));
    for (p, st) in [("big.bin", "?"), ("small.txt", "?"), ("tracked.txt", "M")] {
        assert_eq!(diffs[p], diff(p, st)["diff"], "{p}: /api/diffs and /api/diff differ");
    }
}

#[test]
fn change_events_say_whether_only_working_files_or_git_state_changed() {
    let (dir, s) = repo();
    let root = dir.path();
    write(root, "a.txt", "one\n");
    commit_all(root, "first");
    let ev = s.events();
    write(root, "a.txt", "edited\n");
    assert_eq!(ev.next(2000).as_deref(), Some("worktree"));
    // Refetching the tree, as a tab does on that event, may rewrite the index; that is no git change. Once the stream
    // is settled, every message for that write has come.
    s.body("/api/tree", &[("mode", "working")]);
    s.body("/api/diff", &[("mode", "working"), ("path", "a.txt")]);
    s.settle(&ev);
    assert!(ev.data().iter().all(|d| d == "worktree"), "{:?}", ev.data());
    ev.clear();
    commit_all(root, "edit");
    assert_eq!(ev.next(2000).as_deref(), Some("git"));
    git(root, &["branch", "other"]);
    assert_eq!(ev.next(2000).as_deref(), Some("git"));
}

#[test]
fn in_a_linked_worktree_a_working_file_edit_is_a_worktree_change() {
    // Its git directory lives outside the worktree and is watched too, so edits needn't be reported as git changes
    // to be safe (tests/watcher_live.rs checks that commits and branches there are).
    let (dir, _s) = repo();
    let root = dir.path();
    write(root, "a.txt", "one\n");
    commit_all(root, "first");
    git(root, &["branch", "other"]);
    let wt_dir = TempDir::new("perf-wt");
    let wt = wt_dir.path().join("wt");
    git(root, &["worktree", "add", "-q", wt.to_str().unwrap(), "other"]);
    {
        let wt_server = Server::start(&wt);
        let ev = wt_server.events();
        std::fs::write(wt.join("a.txt"), "edited in the worktree\n").unwrap();
        assert_eq!(ev.next(2000).as_deref(), Some("worktree"));
    }
    git(root, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
}
