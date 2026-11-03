// Agent notes (/api/annotations): add, list and clear per view, stale detection when the annotated lines change,
// file storage that survives a restart, the body size limit, and the SSE event that tells tabs to reload them.
mod common;
use common::*;

fn repo() -> TempDir {
    let dir = TempDir::new("notes");
    init(dir.path());
    write(dir.path(), "a.txt", lines(5, None));
    commit_all(dir.path(), "init");
    dir
}

fn add(s: &Server, body: Value) -> (u16, Value) {
    let mut b = body;
    b["action"] = json!("add");
    s.post("/api/annotations", &b.to_string(), &[])
}

fn list(s: &Server, params: &[(&str, &str)]) -> Vec<Value> {
    s.body("/api/annotations", params)["notes"].as_array().unwrap().clone()
}

#[test]
fn notes_are_added_listed_per_view_and_cleared() {
    let dir = repo();
    let s = Server::start(dir.path());
    let (status, res) = add(&s, json!({ "file": "a.txt", "line": 2, "endLine": 3, "text": "  look here  ", "author": "tester" }));
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["storage"], "memory");
    let note = &res["note"];
    assert_eq!(note["file"], "a.txt");
    assert_eq!((note["line"].clone(), note["endLine"].clone()), (json!(2), json!(3)));
    assert_eq!(note["text"], "look here"); // trimmed
    assert_eq!(note["author"], "tester");
    assert_eq!(note["key"], "working");
    assert_eq!(note["snippet"], "line 2\nline 3");
    let id = note["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 8);

    // A note on a commit belongs to that commit's view only.
    let (status, res) = add(&s, json!({ "file": "a.txt", "line": 1, "text": "on the commit", "mode": "commit", "ref": "HEAD" }));
    assert_eq!(status, 200, "{res}");
    assert!(res["note"]["key"].as_str().unwrap().starts_with("commit:"));
    assert_eq!(res["note"]["endLine"], Value::Null);

    let working = list(&s, &[("mode", "working")]);
    assert_eq!(working.len(), 1);
    assert_eq!(working[0]["id"], id);
    assert_eq!(working[0]["stale"], false);
    assert_eq!(list(&s, &[("mode", "commit"), ("ref", "HEAD")]).len(), 1);
    assert_eq!(list(&s, &[("all", "1")]).len(), 2);

    let (status, res) = s.post("/api/annotations", &json!({ "action": "clear", "id": id }).to_string(), &[]);
    assert_eq!((status, res["removed"].clone()), (200, json!(1)));
    assert!(list(&s, &[("mode", "working")]).is_empty());
    let (_, res) = s.post("/api/annotations", &json!({ "action": "clear", "all": true }).to_string(), &[]);
    assert_eq!(res["removed"], 1);
    assert!(list(&s, &[("all", "1")]).is_empty());
}

#[test]
fn a_note_goes_stale_when_its_lines_change() {
    let dir = repo();
    let s = Server::start(dir.path());
    assert_eq!(add(&s, json!({ "file": "a.txt", "line": 2, "text": "about line 2" })).0, 200);
    write(dir.path(), "a.txt", lines(5, Some(3))); // line 4 changes: the note's line doesn't
    assert_eq!(list(&s, &[("mode", "working")])[0]["stale"], false);
    write(dir.path(), "a.txt", lines(5, Some(1))); // now line 2 changes
    assert_eq!(list(&s, &[("mode", "working")])[0]["stale"], true);
}

#[test]
fn bad_notes_are_refused() {
    let dir = repo();
    let s = Server::start(dir.path());
    let cases = [
        (json!({ "file": "a.txt", "text": "x" }), "line is required"),
        (json!({ "file": "a.txt", "line": 0, "text": "x" }), "line must be a positive integer"),
        (json!({ "file": "a.txt", "line": 3, "endLine": 2, "text": "x" }), "endLine is before line"),
        (json!({ "file": "a.txt", "line": 1, "text": "   " }), "text is required"),
        (json!({ "file": "a.txt", "line": 9, "text": "x" }), "a.txt has only 5 lines"),
        (json!({ "file": "../x", "line": 1, "text": "x" }), "path escapes repo"),
        (json!({ "file": "nope.txt", "line": 1, "text": "x" }), "nope.txt does not exist on the new side of this range"),
    ];
    for (body, msg) in cases {
        let (status, res) = add(&s, body.clone());
        assert_eq!(status, 400, "{body}");
        assert_eq!(res["error"], msg, "{body}");
    }
}

#[test]
fn a_body_over_64_kb_is_refused_with_413() {
    let dir = repo();
    let s = Server::start(dir.path());
    let big = json!({ "action": "add", "file": "a.txt", "line": 1, "text": "x".repeat(70 * 1024) }).to_string();
    let (status, res) = s.post("/api/annotations", &big, &[]);
    assert_eq!(status, 413);
    assert_eq!(res["error"], "request body too large");
}

#[test]
fn file_storage_survives_a_restart() {
    let dir = repo();
    {
        let s = Server::try_start(dir.path(), &["--notes", "file"], &[]).ok().unwrap();
        let (status, res) = add(&s, json!({ "file": "a.txt", "line": 4, "text": "keep me" }));
        assert_eq!(status, 200, "{res}");
        assert_eq!(res["storage"], "file");
    }
    assert!(dir.path().join(".git/revue/notes.json").is_file());
    let s = Server::try_start(dir.path(), &["--notes", "file"], &[]).ok().unwrap();
    let notes = list(&s, &[("mode", "working")]);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], "keep me");
    // Memory storage starts empty, whatever the file holds.
    let mem = Server::start(dir.path());
    assert!(list(&mem, &[("mode", "working")]).is_empty());
}

#[test]
fn adding_a_note_tells_open_tabs() {
    let dir = repo();
    let s = Server::start(dir.path());
    let ev = Events::connect(s.port, "/api/events");
    // The server counts the tab, and sends it messages, only once the stream's head is out.
    assert!(wait_until(5000, || s.body("/api/server", &[])["tabs"] == 1));
    assert_eq!(add(&s, json!({ "file": "a.txt", "line": 1, "text": "hello" })).0, 200);
    assert_eq!(ev.named("annotations", 2000), ["{}"]);
}

#[test]
fn a_branch_view_reached_through_head_keeps_its_notes() {
    // The page resets the head to HEAD after visiting Changes; the same comparison must keep the same notes.
    let dir = repo();
    git(dir.path(), &["switch", "-q", "-c", "feature"]);
    write(dir.path(), "a.txt", lines(5, Some(1)));
    commit_all(dir.path(), "feature");
    let s = Server::start(dir.path());
    let (status, res) = add(&s, json!({ "file": "a.txt", "line": 2, "text": "by name", "mode": "branch", "base": "main", "head": "feature" }));
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["note"]["key"], "branch:main...feature");
    let by_head = list(&s, &[("mode", "branch"), ("base", "main"), ("head", "HEAD")]);
    assert_eq!(by_head.len(), 1, "the note vanished when the head was given as HEAD");
    let (_, res) = add(&s, json!({ "file": "a.txt", "line": 1, "text": "by HEAD", "mode": "branch", "base": "main", "head": "HEAD" }));
    assert_eq!(res["note"]["key"], "branch:main...feature");
    // A detached HEAD has no name to file it under.
    git(dir.path(), &["switch", "-q", "--detach"]);
    let (_, res) = add(&s, json!({ "file": "a.txt", "line": 1, "text": "detached", "mode": "branch", "base": "main", "head": "HEAD" }));
    assert_eq!(res["note"]["key"], "branch:main...HEAD");
}

#[test]
fn each_view_lists_how_many_notes_the_other_views_have() {
    let dir = repo();
    let s = Server::start(dir.path());
    add(&s, json!({ "file": "a.txt", "line": 4, "text": "w1" }));
    add(&s, json!({ "file": "a.txt", "line": 2, "text": "w2" }));
    add(&s, json!({ "file": "a.txt", "line": 1, "text": "c1", "mode": "commit", "ref": "HEAD" }));
    let sha = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();

    let working = s.body("/api/annotations", &[("mode", "working")]);
    assert_eq!(working["key"], "working");
    assert_eq!(working["notes"].as_array().unwrap().len(), 2);
    assert_eq!(working["others"], json!([{ "key": format!("commit:{sha}"), "count": 1,
        "view": { "mode": "commit", "ref": sha }, "first": { "file": "a.txt", "line": 1, "endLine": null, "side": "new" } }]));

    let commit = s.body("/api/annotations", &[("mode", "commit"), ("ref", "HEAD")]);
    let others = commit["others"].as_array().unwrap();
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["count"], 2);
    assert_eq!(others[0]["view"], json!({ "mode": "working" }));
    assert_eq!(others[0]["first"]["line"], 2, "the first note in line order");

    // all=1 lists every note, and seen from no view, every view's notes are another view's.
    let all = s.body("/api/annotations", &[("all", "1")]);
    assert_eq!(all["notes"].as_array().unwrap().len(), 3);
    assert!(all.get("key").is_none());
    let others = all["others"].as_array().unwrap();
    assert_eq!(others.iter().map(|o| (o["view"].clone(), o["count"].clone())).collect::<Vec<_>>(),
        [(json!({ "mode": "working" }), json!(2)), (json!({ "mode": "commit", "ref": sha }), json!(1))]);
    assert_eq!(others[0]["first"]["line"], 2, "the first note in line order");
}
