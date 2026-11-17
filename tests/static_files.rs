// Static file serving: the embedded public/ files, 404s for anything else, and the headers that keep the page safe.
mod common;
use common::*;

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("static");
    git(dir.path(), &["init", "-q"]);
    write(dir.path(), "a.txt", "hi\n");
    let s = Server::start(dir.path());
    (dir, s)
}

#[test]
fn directories_under_public_are_404_not_a_crash() {
    let (_dir, s) = repo();
    assert_eq!(s.get("/vendor", &[]).status, 404);
    assert_eq!(s.get("/vendor/", &[]).status, 404);
}

#[test]
fn missing_files_are_404() {
    let (_dir, s) = repo();
    assert_eq!(s.get("/no-such-file.js", &[]).status, 404);
}

#[test]
fn index_is_served_and_the_server_still_answers() {
    let (_dir, s) = repo();
    let res = s.get("/", &[]);
    assert_eq!(res.status, 200);
    assert!(res.header("content-type").unwrap().contains("text/html"));
    assert_eq!(s.get("/vendor", &[]).status, 404);
    assert_eq!(s.get("/api/info", &[]).status, 200);
}

#[test]
fn static_files_carry_a_csp_that_allows_no_inline_or_outside_scripts() {
    let (_dir, s) = repo();
    let res = s.get("/", &[]);
    let csp = res.header("content-security-policy").unwrap();
    let script = csp.split(';').map(str::trim).find(|d| d.starts_with("script-src")).unwrap();
    assert_eq!(script, "script-src 'self'");
    assert!(csp.contains("default-src 'none'"));
    assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
}

fn directive(csp: &str, name: &str) -> String {
    csp.split(';').map(str::trim).find(|d| d.split(' ').next() == Some(name)).unwrap_or_default().to_string()
}

// The UI sets styles through the CSSOM only, so a style attribute or <style> that got into the page (from a markdown
// preview) is blocked, images load only from this server or the data: URLs of the diagrams the page draws, frames
// only from this server (the diagram renderer), and a <base> can't move where relative URLs go.
#[test]
fn static_files_carry_a_csp_that_allows_no_inline_styles_or_outside_images() {
    let (_dir, s) = repo();
    let res = s.get("/app.js", &[]);
    let csp = res.header("content-security-policy").unwrap();
    assert_eq!(directive(csp, "style-src"), "style-src 'self'");
    assert_eq!(directive(csp, "img-src"), "img-src 'self' data:");
    assert_eq!(directive(csp, "frame-src"), "frame-src 'self'");
    assert_eq!(directive(csp, "frame-ancestors"), "frame-ancestors 'none'");
    assert_eq!(directive(csp, "base-uri"), "base-uri 'none'");
}

// The diagram renderer is a sandbox of its own (an opaque origin), which can load none of this server's files
// (every answer is CORP same-origin), so its scripts are part of the page: they run by their hashes, and nothing else
// does or loads. Only revue's page may frame it. The scripts it inlines aren't served on their own.
#[test]
fn the_diagram_renderer_is_a_sandbox_whose_inlined_scripts_run_by_hash() {
    let (_dir, s) = repo();
    let res = s.get("/mermaid/frame.html", &[]);
    assert_eq!(res.status, 200);
    assert!(res.header("content-type").unwrap().contains("text/html"));
    assert_eq!(res.header("cross-origin-resource-policy"), Some("same-origin"));
    let csp = res.header("content-security-policy").unwrap().to_string();
    assert_eq!(directive(&csp, "default-src"), "default-src 'none'");
    assert_eq!(directive(&csp, "sandbox"), "sandbox allow-scripts");
    assert_eq!(directive(&csp, "frame-ancestors"), "frame-ancestors 'self'");
    assert_eq!(directive(&csp, "style-src"), "style-src 'unsafe-inline'");
    let script = directive(&csp, "script-src");
    let hashes: Vec<&str> = script.split(' ').skip(1).collect();
    assert_eq!(hashes.len(), 2, "{script}");
    assert!(hashes.iter().all(|h| h.starts_with("'sha256-") && h.ends_with("='") && h.len() == 53), "{script}");
    let html = res.text();
    assert!(!html.contains("<script data-inline"));
    assert_eq!(html.matches("<script>").count(), 2);
    assert!(html.contains("mermaid.initialize("));
    // A revalidation keeps it.
    let etag = res.header("etag").unwrap().to_string();
    let again = request(s.port, "GET", "/mermaid/frame.html", &[("if-none-match", &etag)], b"");
    assert_eq!((again.status, again.header("content-security-policy")), (304, Some(csp.as_str())));
    for inlined in ["/mermaid/frame.js", "/vendor/mermaid.tiny.js"] {
        assert_eq!(s.get(inlined, &[]).status, 404, "{inlined}");
    }
}

#[test]
fn index_html_has_no_inline_scripts_the_csp_would_block_them() {
    let (_dir, s) = repo();
    let html = s.get("/", &[]).text();
    let tags: Vec<&str> = html.match_indices("<script").map(|(i, _)| &html[i..i + html[i..].find('>').unwrap() + 1]).collect();
    assert!(!tags.is_empty());
    for tag in tags {
        assert!(tag.contains(" src="), "{tag}");
    }
}
