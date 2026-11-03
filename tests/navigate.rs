// POST /api/navigate: an agent sends a view, the server checks it and broadcasts it as a named `navigate` event, and
// the answer counts the tabs that can follow it (those that connected with ?nav=1).
mod common;
use common::*;

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("navigate");
    init(dir.path());
    write(dir.path(), "a.txt", lines(5, None));
    commit_all(dir.path(), "init");
    let s = Server::start(dir.path());
    (dir, s)
}

#[test]
fn a_view_is_checked_broadcast_and_counted() {
    let (dir, s) = repo();
    let head = rev(dir.path(), "HEAD");
    let (status, res) = s.post("/api/navigate", r#"{"file":"a.txt","line":2}"#, &[]);
    assert_eq!(status, 200);
    let want = json!({ "mode": "working", "head": "HEAD", "view": "diff", "side": "new", "file": "a.txt", "line": 2, "endLine": null });
    assert_eq!(res, json!({ "view": want, "tabs": 0 }));

    let follower = Events::connect(s.port, "/api/events?nav=1");
    let _plain = Events::connect(s.port, "/api/events");
    assert!(wait_until(2000, || s.body("/api/server", &[])["tabs"] == 2));
    let server = s.body("/api/server", &[]);
    assert_eq!((server["tabs"].clone(), server["navTabs"].clone()), (json!(2), json!(1)));

    // A commit view is pinned to the full sha, which the commit list matches on.
    let (status, res) = s.post("/api/navigate", r#"{"mode":"commit","ref":"HEAD","file":"a.txt","view":"file","side":"old"}"#, &[]);
    assert_eq!(status, 200, "{res}");
    assert_eq!(res["tabs"], 1);
    assert_eq!(res["view"]["ref"], head);
    assert_eq!((res["view"]["view"].clone(), res["view"]["side"].clone()), (json!("file"), json!("old")));
    let sent = follower.named("navigate", 2000);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(serde_json::from_str::<Value>(&sent[0]).unwrap(), res["view"]);
}

#[test]
fn a_preview_is_sent_on_as_the_page_reads_a_link() {
    // view is read as the page's parseView reads it: file, preview, or else the diff. Whether a file has a preview
    // (markdown, an SVG) is the page's to judge, as for a link, so a.txt's preview goes out too.
    let (dir, s) = repo();
    write(dir.path(), "README.md", "# Title\n");
    let follower = Events::connect(s.port, "/api/events?nav=1");
    assert!(wait_until(2000, || s.body("/api/server", &[])["navTabs"] == 1));
    let (status, res) = s.post("/api/navigate", r#"{"file":"README.md","view":"preview"}"#, &[]);
    assert_eq!(status, 200, "{res}");
    let want = json!({ "mode": "working", "head": "HEAD", "view": "preview", "side": "new", "file": "README.md", "line": null, "endLine": null });
    assert_eq!(res, json!({ "view": want, "tabs": 1 }));
    let sent = follower.named("navigate", 2000);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(serde_json::from_str::<Value>(&sent[0]).unwrap(), want);
    for (body, view) in [
        (r#"{"mode":"commit","file":"a.txt","view":"preview"}"#, "preview"),
        (r#"{"file":"a.txt","view":"file"}"#, "file"),
        (r#"{"file":"a.txt","view":"bogus"}"#, "diff"),
    ] {
        let (status, res) = s.post("/api/navigate", body, &[]);
        assert_eq!((status, res["view"]["view"].clone()), (200, json!(view)), "{body}");
    }
}

#[test]
fn bad_views_are_refused() {
    let (_dir, s) = repo();
    for (body, msg) in [
        (r#"{"mode":"bogus"}"#, "unknown mode bogus"),
        (r#"{"file":"../x"}"#, "path escapes repo"),
        (r#"{"file":"a.txt","endLine":2}"#, "endLine needs line"),
        (r#"{"file":"a.txt","line":3,"endLine":2}"#, "endLine is before line"),
        (r#"{"file":"a.txt","line":1.5}"#, "line must be a positive integer"),
        (r#"{"mode":"branch","base":"nosuch"}"#, "unknown revision: nosuch"),
    ] {
        let (status, res) = s.post("/api/navigate", body, &[]);
        assert_eq!((status, res["error"].clone()), (400, json!(msg)), "{body}");
    }
}
