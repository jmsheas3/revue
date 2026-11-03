// The hand-written HTTP layer: HEAD requests get the headers without a body, and one connection carries several
// requests (keep-alive), which is how browsers load the page.
mod common;
use common::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("http");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    let s = Server::start(dir.path());
    (dir, s)
}

// Status and lowercased headers of one response.
fn head(r: &mut impl BufRead) -> (u16, Vec<(String, String)>) {
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        let l = l.trim_end();
        if l.is_empty() {
            return (status, headers);
        }
        let (k, v) = l.split_once(':').unwrap();
        headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
    }
}

fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[test]
fn head_gets_the_headers_and_no_body() {
    let (_dir, s) = repo();
    let get = s.get("/app.js", &[]);
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(c, "HEAD /app.js HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nconnection: close\r\n\r\n", s.port).unwrap();
    let mut r = BufReader::new(c);
    let (status, h) = head(&mut r);
    assert_eq!(status, 200);
    assert_eq!(header(&h, "content-length"), Some(get.body.len().to_string().as_str()));
    assert_eq!(header(&h, "etag"), get.header("etag"));
    assert_eq!(header(&h, "content-type"), Some("text/javascript"));
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "HEAD sent {} body bytes", rest.len());
}

#[test]
fn one_connection_serves_several_requests() {
    let (_dir, s) = repo();
    let c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut w = c.try_clone().unwrap();
    let mut r = BufReader::new(c);
    for path in ["/api/info", "/api/tree?mode=working", "/app.js"] {
        write!(w, "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{}\r\n\r\n", s.port).unwrap();
        let (status, h) = head(&mut r);
        assert_eq!(status, 200, "{path}");
        assert_eq!(header(&h, "connection"), Some("keep-alive"), "{path}");
        let len: usize = header(&h, "content-length").unwrap().parse().unwrap();
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        if path.starts_with("/api/") {
            serde_json::from_slice::<Value>(&body).unwrap_or_else(|e| panic!("{path}: {e}"));
        }
    }
    // Asking to close ends the connection after the answer.
    write!(w, "GET /api/info HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nconnection: close\r\n\r\n", s.port).unwrap();
    let (status, h) = head(&mut r);
    assert_eq!((status, header(&h, "connection")), (200, Some("close")));
    let len: usize = header(&h, "content-length").unwrap().parse().unwrap();
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).unwrap();
    assert_eq!(rest.len(), len);
}
