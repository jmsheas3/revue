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

// The UI sets styles through the CSSOM only, so a style attribute or <style> that got into the page (from a markdown
// preview) is blocked, images load only from this server, and a <base> can't move where relative URLs go.
#[test]
fn static_files_carry_a_csp_that_allows_no_inline_styles_or_outside_images() {
    let (_dir, s) = repo();
    let res = s.get("/app.js", &[]);
    let csp = res.header("content-security-policy").unwrap();
    let directive = |name: &str| csp.split(';').map(str::trim).find(|d| d.split(' ').next() == Some(name)).unwrap().to_string();
    assert_eq!(directive("style-src"), "style-src 'self'");
    assert_eq!(directive("img-src"), "img-src 'self'");
    assert_eq!(directive("base-uri"), "base-uri 'none'");
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
