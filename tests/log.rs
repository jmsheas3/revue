// /api/log walks history itself (gix) but must list exactly what `git log` does: the same commits in the same
// order (equal commit times included), the same %D decorations and the same %s subjects.
mod common;
use common::*;
use std::path::Path;
use std::process::Command;

const FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%D%x1f%s%x1f%b%x1f%P%x1e";

// git with fixed author and committer dates, so commits can share a timestamp.
fn git_at(root: &Path, t: i64, args: &[&str]) {
    let date = format!("@{t} +0000");
    let out = isolate(&mut Command::new("git"))
        .args(["-c", "user.name=Tester", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

// History with a merge, a side branch, commits sharing a timestamp, both kinds of tag, a remote-tracking ref, a
// subject that starts with spaces and spans two lines, and a message with CRLF line endings (kept verbatim: the
// default cleanup would strip the CRs) whose subject and body are split by a whitespace-only line.
fn repo() -> TempDir {
    let dir = TempDir::new("log");
    let root = dir.path();
    init(root);
    let t = 1_700_000_000;
    write(root, "a.txt", "1\n");
    git_at(root, t, &["add", "-A"]);
    git_at(root, t, &["commit", "-q", "-m", "first"]);
    git_at(root, t, &["tag", "v1"]);
    git_at(root, t, &["switch", "-q", "-c", "side"]);
    for i in 0..3 {
        write(root, "side.txt", format!("{i}\n"));
        git_at(root, t + 10, &["add", "-A"]);
        git_at(root, t + 10, &["commit", "-q", "-m", &format!("side {i}")]);
    }
    git_at(root, t + 10, &["commit", "-q", "--allow-empty", "-m", "side same time"]);
    git_at(root, t, &["switch", "-q", "main"]);
    write(root, "a.txt", "2\n");
    git_at(root, t + 10, &["commit", "-q", "-am", "  indented subject\nthat wraps\n\nand a body"]);
    write(root, ".git/crlf-msg", "CRLF subject\r\nwrapped\r\n \r\nbody one\r\n\r\nbody two\r\n\r\n");
    git_at(root, t + 10, &["commit", "-q", "--allow-empty", "--cleanup=verbatim", "-F", ".git/crlf-msg"]);
    git_at(root, t + 10, &["tag", "-a", "v2", "-m", "release 2"]);
    git_at(root, t + 20, &["merge", "-q", "--no-ff", "side", "-m", "merge side"]);
    git_at(root, t + 20, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    dir
}

fn git_log(root: &Path, args: &[&str]) -> Vec<Value> {
    let mut a = vec!["log", FORMAT];
    a.extend_from_slice(args);
    a.push("--");
    git(root, &a)
        .split('\x1e')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|rec| {
            let f: Vec<&str> = rec.split('\x1f').collect();
            let parents: Vec<&str> = f[7].split_whitespace().collect();
            json!({ "sha": f[0], "short": f[1], "author": f[2], "time": f[3].parse::<i64>().unwrap(), "refs": f[4], "subject": f[5],
                "body": f[6].trim_end(), "parents": parents })
        })
        .collect()
}

// /api/log with each case's parameters against `git log` with its arguments.
type Case = (&'static [(&'static str, &'static str)], &'static [&'static str]);

fn assert_like_git(s: &Server, root: &Path, cases: &[Case]) {
    for (params, args) in cases {
        let want = git_log(root, args);
        let got = s.body("/api/log", params);
        let got = got.as_array().unwrap();
        assert_eq!(got.len(), want.len(), "{params:?}");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{params:?} line {i}");
        }
    }
}

#[test]
fn the_log_matches_git_log_line_for_line() {
    let dir = repo();
    let root = dir.path();
    let s = Server::start(root);
    let cases: &[Case] = &[
        (&[], &["--max-count=200"]),
        (&[("all", "1")], &["--all", "--max-count=200"]),
        (&[("branch", "side")], &["side", "--max-count=200"]),
        (&[("limit", "2"), ("skip", "1")], &["--max-count=2", "--skip=1"]),
        (&[("all", "1"), ("limit", "3"), ("skip", "2")], &["--all", "--max-count=3", "--skip=2"]),
    ];
    assert_like_git(&s, root, cases);
    // The cases above do exercise decorations, tags and the odd subject.
    let all = git_log(root, &["--all"]);
    assert!(all.iter().any(|c| c["refs"].as_str().unwrap().starts_with("HEAD -> main")));
    assert!(all.iter().any(|c| c["refs"].as_str().unwrap().contains("tag: v2")));
    assert!(all.iter().any(|c| c["subject"] == "  indented subject that wraps" && c["body"] == "and a body"));
    assert!(all.iter().any(|c| c["subject"] == "CRLF subject wrapped" && c["body"] == "body one\r\n\r\nbody two"));
    assert!(all.iter().any(|c| c["parents"].as_array().unwrap().len() == 2));
}

#[test]
fn on_a_branch_with_no_commits_the_other_branches_are_still_listed() {
    // `git switch --orphan` checks out a branch with no commits yet: HEAD names no commit, but the other branches
    // have history. All branches lists it as `git log --all` does (the page sends branch=HEAD along), and so does a
    // branch asked for by name.
    let dir = repo();
    let root = dir.path();
    git(root, &["switch", "-q", "--orphan", "new"]);
    let s = Server::start(root);
    let cases: &[Case] = &[
        (&[("all", "1"), ("branch", "HEAD")], &["--all", "--max-count=200"]),
        (&[("all", "1")], &["--all", "--max-count=200"]),
        (&[("all", "1"), ("limit", "3"), ("skip", "2")], &["--all", "--max-count=3", "--skip=2"]),
        (&[("branch", "side")], &["side", "--max-count=200"]),
        (&[("branch", "main")], &["main", "--max-count=200"]),
    ];
    assert_like_git(&s, root, cases);
    assert_eq!(s.body("/api/log", &[("all", "1")]).as_array().unwrap().len(), 8);
    // HEAD's own history is empty, under either name: no commits, and no error.
    for params in [&[][..], &[("branch", "HEAD")], &[("branch", "")], &[("branch", "new")], &[("branch", "refs/heads/new")]] {
        assert_eq!(s.json("/api/log", params), (200, json!([])), "{params:?}");
    }
    // A branch that doesn't exist is still an error, as it is with commits.
    let (status, body) = s.json("/api/log", &[("branch", "nope")]);
    assert_eq!((status, body["error"].as_str()), (400, Some("fatal: bad revision 'nope'")));
    // A ref that does exist by the branch's name (a tag here) is what that name means, to git too.
    git(root, &["tag", "new", "side~1"]);
    assert_like_git(&s, root, &[(&[("branch", "new")], &["new", "--max-count=200"])]);
    assert_eq!(s.body("/api/log", &[("branch", "new")]).as_array().unwrap().len(), 4); // side 2, side 1, side 0, first
}
