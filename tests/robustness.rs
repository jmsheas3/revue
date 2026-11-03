// A tab that stops reading its event stream must not hold up the others or the API, and a thread the OS refuses costs
// only the request that wanted it.
#![cfg(unix)]
mod common;
use common::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// A connection to /api/events that never reads, with the smallest receive buffer the kernel allows, so the server's
// writes to it soon block.
fn stuck_tab(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    unsafe {
        use std::os::unix::io::AsRawFd;
        let size: libc::c_int = 1;
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    let mut w = s.try_clone().unwrap();
    write!(w, "GET /api/events HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\r\n").unwrap();
    s
}

#[test]
fn a_tab_that_stops_reading_does_not_hold_up_the_others() {
    let dir = TempDir::new("robust");
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    let s = Server::start(dir.path());
    let _stuck = stuck_tab(s.port);
    assert!(wait_until(2000, || s.body("/api/server", &[])["tabs"] == 1), "the stuck tab never registered");

    // Navigate events go to every tab. A long file name makes each one big, so the stuck tab's socket fills fast.
    let name = format!("{}x.txt", format!("{}/", "d".repeat(200)).repeat(15));
    write(dir.path(), &name, "x\n");
    let body = json!({ "file": name }).to_string();
    let port = s.port;
    // Keep broadcasting while the checks below run. Before the fix one broadcast soon stalls (the stuck tab's socket is
    // full and the server writes to it under the tab lock); wait for that, or 3 s, before checking.
    let (stalled, done) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (flag, stop) = (stalled.clone(), done.clone());
    let flood = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let t = Instant::now();
            request(port, "POST", "/api/navigate", &[("x-revue", "1")], body.as_bytes());
            if t.elapsed() > Duration::from_millis(300) {
                flag.store(true, Ordering::SeqCst);
            }
        }
    });
    let start = Instant::now();
    while !stalled.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(3) {
        sleep(50);
    }

    // A healthy tab gets a change event promptly, and the API still answers.
    let ev = s.events();
    write(dir.path(), "a.txt", "two\n");
    let start = Instant::now();
    assert!(ev.next(3000).is_some(), "the healthy tab got no change event");
    assert!(start.elapsed() < Duration::from_secs(2), "the change event took {:?}", start.elapsed());
    let start = Instant::now();
    assert_eq!(s.get("/api/server", &[]).status, 200);
    assert!(start.elapsed() < Duration::from_secs(1), "/api/server took {:?}", start.elapsed());
    done.store(true, Ordering::SeqCst);
    flood.join().unwrap();
}

// A connection the server closed: EOF, or a reset. A timeout (WouldBlock or TimedOut) is a connection left hanging.
fn closed(read: std::io::Result<usize>) -> bool {
    match read {
        Ok(n) => n == 0,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset || panic!("the connection was left hanging: {e}"),
    }
}

// One request on a kept-alive connection: its status and body, or None when the server closed the connection instead
// of answering. Anything else that goes wrong, a timeout included, fails the test.
fn ask(r: &mut BufReader<TcpStream>, port: u16, path: &str) -> Option<(u16, String)> {
    write!(r.get_mut(), "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\r\n").unwrap();
    let mut line = String::new();
    if closed(r.read_line(&mut line)) {
        return None;
    }
    let status = line.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("bad status line {line:?}"));
    let mut len = 0;
    loop {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    Some((status, String::from_utf8_lossy(&body).into_owned()))
}

fn spawn_repo(tag: &str) -> TempDir {
    let dir = TempDir::new(tag);
    init(dir.path());
    write(dir.path(), "a.txt", "one\n");
    commit_all(dir.path(), "init");
    dir
}

// The OS can refuse a thread (EAGAIN near the thread limit; /api/debug?failSpawns=n stands in for that). For a new
// connection that costs only that connection: the accept loop goes on, where its panic used to stop the server
// accepting anything at all.
#[test]
fn a_connection_that_gets_no_thread_is_dropped_and_the_next_is_served() {
    let dir = spawn_repo("robust-spawn-conn");
    let s = Server::start(dir.path());
    s.debug(&[("failSpawns", "1")]);
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut got = Vec::new();
    assert!(closed(c.read_to_end(&mut got).map(|_| 0))); // read to EOF, or reset
    assert!(got.is_empty(), "the dropped connection got {:?}", String::from_utf8_lossy(&got));
    assert_eq!(s.get("/api/info", &[]).status, 200);
    assert_eq!(s.get("/api/info", &[]).status, 200);
    assert!(s.stderr().contains("dropped a connection: could not start a thread"), "{}", s.stderr());
}

// A request whose own threads can't start fails with 503, and the next one is answered. An event stream that gets no
// writer thread is closed unanswered, which EventSource retries (it gives up for good on a 503).
#[test]
fn a_request_that_gets_no_thread_fails_with_503_and_the_next_is_served() {
    let dir = spawn_repo("robust-spawn-req");
    write(dir.path(), "a.txt", "two\n");
    write(dir.path(), "new.txt", "new\n");
    let s = Server::start(dir.path());
    let c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut r = BufReader::new(c);
    for tree in ["/api/tree?mode=working", "/api/tree?mode=commit&ref=HEAD"] {
        assert_eq!(ask(&mut r, s.port, "/api/debug?failSpawns=1").map(|a| a.0), Some(200));
        let (status, body) = ask(&mut r, s.port, tree).expect("the connection was closed");
        assert_eq!(status, 503, "{body}");
        assert!(body.contains("could not start a thread"), "{body}");
        let (status, body) = ask(&mut r, s.port, tree).expect("the connection was closed");
        assert_eq!(status, 200, "{body}");
    }
    assert_eq!(
        untagged(s.body("/api/tree", &[("mode", "working")])["changes"].clone()),
        json!({ "a.txt": { "status": "M", "add": 1, "del": 1 }, "new.txt": { "status": "?", "add": 1, "del": 0 } })
    );
    assert!(s.stderr().contains("revue: GET /api/tree?mode=working: could not start a thread"), "{}", s.stderr());

    assert_eq!(ask(&mut r, s.port, "/api/debug?failSpawns=1").map(|a| a.0), Some(200));
    assert_eq!(ask(&mut r, s.port, "/api/events"), None);
    let ev = s.events();
    write(dir.path(), "a.txt", "three\n");
    assert!(ev.wait_changes(1, 5000), "the event stream opened after the failure got no change");
}
