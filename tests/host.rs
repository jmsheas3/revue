// Who may reach the server. DNS rebinding guard: every request, reads included, must carry a local Host header naming
// this server's port. Requests a page on another site sent are refused by the API, and no answer may be handed to one.
mod common;
use common::*;
use std::io::{Read, Write};
use std::net::TcpStream;

const PATHS: &[&str] = &[
    "/api/info",
    "/api/tree?mode=working",
    "/api/file?mode=working&path=secret.txt",
    "/api/image?mode=working&path=secret.png",
    "/",
    "/index.html",
    "/app.js",
    "/api/events",
];

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("host");
    write(dir.path(), "secret.txt", "secret\n");
    std::fs::write(dir.path().join("secret.png"), b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\0\0\0\0").unwrap();
    init(dir.path());
    commit_all(dir.path(), "init");
    let s = Server::start(dir.path());
    (dir, s)
}

#[test]
fn non_local_host_is_refused_on_every_path() {
    let (_dir, s) = repo();
    for p in PATHS {
        assert_eq!(status_only(s.port, p, &format!("evil.example:{}", s.port)), 403, "{p}");
        assert_eq!(status_only(s.port, p, "evil.example"), 403, "{p}");
    }
}

// A local name with another port is another origin (a page on another local server, or a rebound name on a port
// this server doesn't own), so the port has to match too.
#[test]
fn loopback_and_localhost_with_another_port_are_refused() {
    let (_dir, s) = repo();
    let other = s.port.checked_add(1).unwrap_or(s.port - 1);
    for p in PATHS {
        assert_eq!(status_only(s.port, p, &format!("127.0.0.1:{other}")), 403, "{p}");
        assert_eq!(status_only(s.port, p, &format!("localhost:{other}")), 403, "{p}");
    }
}

#[test]
fn loopback_and_localhost_with_the_server_port_are_allowed() {
    let (_dir, s) = repo();
    for p in PATHS {
        assert_eq!(status_only(s.port, p, &format!("127.0.0.1:{}", s.port)), 200, "{p}");
        assert_eq!(status_only(s.port, p, &format!("localhost:{}", s.port)), 200, "{p}");
    }
}

// A page on another site, or on another server on 127.0.0.1 (the same site, another origin), can make the browser send
// requests here, and the browser says so in Sec-Fetch-Site. The API refuses them, and any value it doesn't know, while
// the page's own files are served to every request, so a link to revue opens it from anywhere. Typed addresses (none),
// revue's own page (same-origin) and clients that send no such header (curl, revue mcp: every other test) get through.
#[test]
fn api_requests_from_another_site_are_refused() {
    let (_dir, s) = repo();
    for p in PATHS {
        let api = p.starts_with("/api/");
        for site in ["cross-site", "same-site", "Cross-Site", "somewhere-else"] {
            assert_eq!(head_only(s.port, p, &[("sec-fetch-site", site)]).0, if api { 403 } else { 200 }, "{p}, {site}");
        }
        for site in ["same-origin", "none", "Same-Origin"] {
            assert_eq!(head_only(s.port, p, &[("sec-fetch-site", site)]).0, 200, "{p}, {site}");
        }
    }
}

// Refused before anything is done: the browser would keep the answer from the other page anyway, but the file would
// have been read.
#[test]
fn a_refused_request_reads_nothing() {
    let (_dir, s) = repo();
    let reads = || {
        let (_, d) = s.json("/api/debug", &[]);
        d["reads"].as_array().unwrap().iter().filter(|r| r["path"].as_str().unwrap().ends_with("secret.txt")).count()
    };
    reads();
    let path = "/api/file?mode=working&path=secret.txt";
    let res = request(s.port, "GET", path, &[("sec-fetch-site", "cross-site")], b"");
    assert_eq!((res.status, res.text()), (403, r#"{"error":"cross-site request refused"}"#.to_string()));
    assert_eq!(reads(), 0);
    // The control: from revue's own page it is read.
    assert_eq!(request(s.port, "GET", path, &[("sec-fetch-site", "same-origin")], b"").status, 200);
    assert_eq!(reads(), 1);
}

// Every answer tells the browser not to hand it to a page on another origin, whatever kind of load that page tries
// (for an image, browsers would otherwise let it see the size): the API and the page's files, the event stream, a
// revalidated file's 304, and errors, those of the Host and Sec-Fetch-Site checks and a malformed request's included.
#[test]
fn every_answer_is_for_this_origin_only() {
    let (_dir, s) = repo();
    let corp = |h: &[(String, String)]| h.iter().find(|(k, _)| k == "cross-origin-resource-policy").map(|(_, v)| v.clone());
    let same_origin = Some("same-origin".to_string());
    let etag = s.get("/app.js", &[]).header("etag").unwrap().to_string();
    let host = format!("evil.example:{}", s.port);
    let cases = vec![
        ("/api/info", vec![], 200),
        ("/api/tree?mode=working", vec![], 200),
        ("/api/events", vec![], 200),
        ("/", vec![], 200),
        ("/app.js", vec![], 200),
        ("/app.js", vec![("if-none-match", etag.as_str())], 304),
        ("/api/file?mode=working&path=..%2Fx", vec![], 400),
        ("/api/file?mode=working&path=nope.txt", vec![], 404),
        ("/api/nope", vec![], 404),
        ("/nope.js", vec![], 404),
        ("/api/info", vec![("host", host.as_str())], 403),
        ("/api/info", vec![("sec-fetch-site", "cross-site")], 403),
    ];
    for (p, headers, status) in cases {
        let (got, h) = head_only(s.port, p, &headers);
        assert_eq!(got, status, "{p} {headers:?}");
        assert_eq!(corp(&h), same_origin, "{p} {headers:?}");
    }
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    c.write_all(b"NONSENSE\r\n\r\n").unwrap();
    let mut answer = String::new();
    c.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
    assert!(answer.to_ascii_lowercase().contains("\r\ncross-origin-resource-policy: same-origin\r\n"), "{answer}");
}
