// Symlinks in the working tree must not let the server read files outside the repo (#4). Builds a throwaway repo
// next to an "outside" directory holding a secret, then queries a real server.
#![cfg(unix)]
mod common;
use common::*;
use std::os::unix::fs::symlink;

const SECRET: &str = "TOPSECRET-7f3a";

fn repo() -> (TempDir, Server) {
    let tmp = TempDir::new("link");
    let outside = tmp.path().join("outside");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // If the server read this through a link, its import would make go to definition jump to src/a.js.
    std::fs::write(outside.join("secret.js"), format!("import {{ tool }} from './src/a';\ntool();\n// {SECRET}\nfunction secretFn() {{}}\n")).unwrap();
    write(&root, "src/real.js", "function inner() {}\n");
    write(&root, "src/a.js", "export function tool() {}\n");
    write(&root, "src/b.js", "export function tool() {}\n");
    symlink(outside.join("secret.js"), root.join("committed-link.js")).unwrap();
    git(&root, &["init", "-q"]);
    commit_all(&root, "fixtures");
    // Untracked links: one escaping file link, one escaping directory link, one link that stays inside.
    symlink(outside.join("secret.js"), root.join("leak.js")).unwrap();
    symlink(&outside, root.join("dirlink")).unwrap();
    symlink("src/real.js", root.join("inside.js")).unwrap();
    let s = Server::start(&root);
    (tmp, s)
}

fn get(s: &Server, path: &str, params: &[(&str, &str)]) -> (u16, Value) {
    let mut p = vec![("mode", "working")];
    p.extend_from_slice(params);
    let res = s.get(path, &p);
    let text = res.text();
    assert!(!text.contains(SECRET), "{path} {params:?} leaked the secret: {text}");
    (res.status, res.json())
}

#[test]
fn api_file_does_not_follow_links_out_of_the_repo() {
    let (_tmp, s) = repo();
    for p in ["leak.js", "committed-link.js", "dirlink/secret.js"] {
        let (_, body) = get(&s, "/api/file", &[("path", p)]);
        assert!(body.get("content").is_none(), "{p} returned content");
    }
    assert_eq!(get(&s, "/api/file", &[("path", "leak.js")]).1, json!({ "outside": true }));
}

#[test]
fn api_file_still_follows_links_inside_the_repo() {
    let (_tmp, s) = repo();
    assert_eq!(get(&s, "/api/file", &[("path", "inside.js")]).1, json!({ "content": "function inner() {}\n" }));
}

#[test]
fn api_tree_lists_the_links_without_reading_through_them() {
    let (_tmp, s) = repo();
    s.reads();
    let (status, body) = get(&s, "/api/tree", &[]);
    assert_eq!(status, 200);
    for f in ["leak.js", "inside.js", "committed-link.js"] {
        assert!(body["files"].as_array().unwrap().contains(&json!(f)), "{f}");
    }
    // Each untracked link counts as git shows it, one line naming its target (secret.js has four).
    for f in ["leak.js", "inside.js", "dirlink"] {
        assert_eq!(body["changes"][f]["add"], 1, "{f}");
    }
    let reads = s.reads();
    assert!(!reads.iter().any(|p| p.ends_with("secret.js") || p.ends_with("real.js")), "{reads:?}");
}

#[test]
fn api_diff_shows_an_untracked_escaping_link_as_a_link_not_its_target() {
    let (_tmp, s) = repo();
    // get() already fails on the secret; the diff must also say it's a link (mode 120000) whose content is the
    // link target, not the file it points to.
    let (status, body) = get(&s, "/api/diff", &[("path", "leak.js"), ("status", "?")]);
    assert_eq!(status, 200);
    let diff = body["diff"].as_str().unwrap();
    assert!(diff.contains("new file mode 120000"), "not shown as a link: {diff}");
    assert!(diff.contains("outside/secret.js"), "the link target is missing: {diff}");
}

#[test]
fn api_def_does_not_read_through_an_escaping_link() {
    let (_tmp, s) = repo();
    let (_, body) = get(&s, "/api/def", &[("name", "secretFn"), ("from", "leak.js"), ("line", "4"), ("col", "10")]);
    let paths: Vec<&str> = body["candidates"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
    assert!(!paths.contains(&"leak.js") && !paths.contains(&"committed-link.js"), "{body}");
    // The clicked file's text is unreadable, so its import can't pick src/a.js over src/b.js.
    let (_, tie) = get(&s, "/api/def", &[("name", "tool"), ("from", "leak.js"), ("line", "2"), ("col", "1")]);
    assert_eq!(tie["jump"], Value::Null, "{tie}");
    // A tie, not an empty answer: both src/a.js and src/b.js are offered.
    assert!(tie["candidates"].as_array().unwrap().len() >= 2, "{tie}");
    let (_, inner) = get(&s, "/api/def", &[("name", "inner"), ("from", "inside.js"), ("line", "1"), ("col", "10")]);
    assert!(inner["candidates"].as_array().unwrap().iter().any(|c| c["path"] == "src/real.js"), "{inner}");
}

#[test]
fn untracked_links_are_counted_as_the_link_not_what_it_points_to() {
    // git shows an untracked link as itself, its target path as one line of text (`git diff --no-index` does too), so
    // the counts must not come from the file it leads to, nor fail when it leads to a directory or nowhere.
    let dir = TempDir::new("link-count");
    let root = dir.path();
    init(root);
    write(root, "three.txt", "a\nb\nc\n");
    write(root, "d/f.txt", "1\n2\n");
    commit_all(root, "files");
    symlink("three.txt", root.join("to-file")).unwrap();
    symlink("d", root.join("to-dir")).unwrap();
    symlink("nowhere", root.join("dangling")).unwrap();
    symlink("two\nlines", root.join("newline-in-target")).unwrap();
    let s = Server::start(root);
    let (status, body) = s.json("/api/tree", &[("mode", "working")]);
    assert_eq!(status, 200, "{body}");
    let ch = untagged(body["changes"].clone());
    let want = [("to-file", 1), ("to-dir", 1), ("dangling", 1), ("newline-in-target", 2)];
    for (f, add) in want {
        assert_eq!(ch[f], json!({ "status": "?", "add": add, "del": 0 }), "{f}");
    }
    // The same counts as git's once the links are added.
    git(root, &["add", "-A"]);
    let numstat = git(root, &["diff", "--cached", "--numstat"]);
    for (f, add) in want {
        let line = numstat.lines().find(|l| l.ends_with(&format!("\t{f}"))).unwrap_or_else(|| panic!("{f}: {numstat}"));
        assert!(line.starts_with(&format!("{add}\t0\t")), "git: {line}");
    }
}

#[test]
fn an_untracked_link_shows_its_target_path_as_git_does_wherever_it_points() {
    // `git diff --no-index` shows a link to a file as the link itself, but follows a link to a directory and fails
    // (with exit 1, as for any difference), which left that diff empty. Every new link's diff must be the one git
    // shows for it: --no-index's where git manages one, and the staged diff once the links are added.
    let dir = TempDir::new("link-diff");
    let root = dir.path();
    init(root);
    write(root, "three.txt", "a\nb\nc\n");
    write(root, "d/f.txt", "1\n");
    commit_all(root, "files");
    let links = [
        ("to-file", "three.txt"),
        ("to-dir", "d"),
        ("d/up", ".."),
        ("tab\there", "d"),
        ("dangling", "nowhere"),
        ("newline-in-target", "two\nlines"),
        ("newline-at-end", "end\n"),
    ];
    for (link, target) in links {
        symlink(target, root.join(link)).unwrap();
    }
    let s = Server::start(root);
    let diff_flags = ["--no-ext-diff", "--no-color", "--src-prefix=a/", "--dst-prefix=b/"];
    let diff_of = |p: &str| -> String {
        let (status, body) = s.json("/api/diff", &[("mode", "working"), ("path", p), ("old", ""), ("status", "?")]);
        assert_eq!(status, 200, "{p}: {body}");
        body["diff"].as_str().unwrap().to_string()
    };
    let mut batch: Vec<(&str, &str)> = vec![("mode", "working")];
    for (link, _) in links {
        batch.extend([("path", link), ("old", ""), ("status", "?")]);
    }
    let (status, body) = s.json("/api/diffs", &batch);
    assert_eq!(status, 200, "{body}");
    let mine: Vec<(&str, String)> = links.iter().map(|&(l, _)| (l, diff_of(l))).collect();
    for (link, d) in &mine {
        assert_eq!(body["diffs"][link], d.as_str(), "{link}: /api/diffs and /api/diff differ");
        assert!(d.contains("new file mode 120000\n"), "{link}: {d}");
    }
    // Where git's own --no-index diff works (it follows no directory), it is the same text.
    for (link, d) in mine.iter().filter(|(l, _)| ["to-file", "dangling", "newline-in-target", "newline-at-end"].contains(l)) {
        let out = isolate(&mut std::process::Command::new("git"))
            .args(["-c", "core.quotepath=off", "diff", "--no-index"])
            .args(diff_flags)
            .args(["--", "/dev/null", link])
            .current_dir(root)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{link}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(*d, String::from_utf8_lossy(&out.stdout), "{link}");
    }
    // And every one is the diff git shows once it is staged.
    git(root, &["add", "-A"]);
    for (link, d) in &mine {
        let mut args = vec!["-c", "core.quotepath=off", "diff", "--cached"];
        args.extend(diff_flags);
        args.extend(["--", link]);
        assert_eq!(*d, git(root, &args), "{link}");
    }
}
