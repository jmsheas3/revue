// Just enough HTTP/1.1 for a local server and its MCP client: requests with keep-alive, Content-Length or chunked
// bodies, and responses with a length (or chunked, for server-sent events). No TLS, no pipelining tricks.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const MAX_HEAD: usize = 64 * 1024;
// Larger bodies are refused outright; the JSON endpoints allow less (see Request::json).
const MAX_BODY: usize = 1024 * 1024;
// How long a connection may sit idle between requests, and how long one request (head and body) may take to arrive
// once it has started, so a client trickling bytes can't hold a thread forever.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- query strings
// URLSearchParams semantics: `+` is a space, bad percent escapes stay as written, repeated keys are kept in order.
#[derive(Debug, Default, Clone)]
pub struct Query(pub Vec<(String, String)>);

impl Query {
    pub fn parse(s: &str) -> Query {
        Query(
            s.split('&')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let (k, v) = p.split_once('=').unwrap_or((p, ""));
                    (form_decode(k), form_decode(v))
                })
                .collect(),
        )
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }

    // Like `q.get(k) || fallback`: an empty value counts as missing.
    pub fn get_or<'a>(&'a self, k: &str, fallback: &'a str) -> &'a str {
        self.get(k).filter(|v| !v.is_empty()).unwrap_or(fallback)
    }

    pub fn get_all(&self, k: &str) -> Vec<&str> {
        self.0.iter().filter(|(key, _)| key == k).map(|(_, v)| v.as_str()).collect()
    }

    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        self.0.retain(|(key, _)| key != k);
        self.0.push((k.to_string(), v.into()));
    }

    pub fn encode(&self) -> String {
        self.0.iter().map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v))).collect::<Vec<_>>().join("&")
    }
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

pub fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' if plus_is_space => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h * 16 + l);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_decode(s: &str) -> String {
    percent_decode(s, true)
}

// application/x-www-form-urlencoded, as URLSearchParams.toString() writes it.
pub fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => out.push(c as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{c:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- requests
pub struct Request {
    pub method: String,
    pub target: String,
    pub path: String,
    pub query: Query,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub keep_alive: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

// The pathname with dot segments resolved (like `new URL(target, base).pathname`), still percent-encoded.
fn normalize_path(p: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    let parts: Vec<&str> = p.split('/').skip(1).collect();
    for (i, s) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "." | "%2e" => {
                if last {
                    segs.push("");
                }
            }
            ".." | ".%2e" | "%2e." | "%2e%2e" => {
                segs.pop();
                if last {
                    segs.push("");
                }
            }
            _ => segs.push(s),
        }
    }
    format!("/{}", segs.join("/"))
}

fn read_line_limited(r: &mut impl BufRead, total: &mut usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take((MAX_HEAD - *total) as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    *total += n;
    if *total > MAX_HEAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

// The read side of a server connection. Each read waits at most until the deadline of the request being read, or
// IDLE_TIMEOUT between requests.
pub struct Conn {
    pub stream: TcpStream,
    deadline: Option<Instant>,
}

impl Conn {
    pub fn new(stream: TcpStream) -> Conn {
        Conn { stream, deadline: None }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(d) = self.deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "request took too long to arrive"));
            }
            self.stream.set_read_timeout(Some(left.min(IDLE_TIMEOUT)))?;
        }
        self.stream.read(buf)
    }
}

// Reads one request; Ok(None) when the client closed the connection between requests.
pub fn read_request(r: &mut BufReader<Conn>) -> io::Result<Option<Request>> {
    r.get_mut().deadline = None;
    r.get_ref().stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    if r.fill_buf()?.is_empty() {
        return Ok(None);
    }
    r.get_mut().deadline = Some(Instant::now() + REQUEST_DEADLINE);
    let req = read_request_head_and_body(r);
    r.get_mut().deadline = None;
    req
}

fn read_request_head_and_body(r: &mut BufReader<Conn>) -> io::Result<Option<Request>> {
    let mut total = 0;
    let mut line = match read_line_limited(r, &mut total)? {
        Some(l) => l,
        None => return Ok(None),
    };
    while line.is_empty() {
        // Tolerate stray CRLFs between requests.
        line = match read_line_limited(r, &mut total)? {
            Some(l) => l,
            None => return Ok(None),
        };
    }
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().ok_or_else(|| bad("bad request line"))?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();
    let mut headers = Vec::new();
    loop {
        let l = read_line_limited(r, &mut total)?.ok_or_else(|| bad("unexpected end of headers"))?;
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v): &(String, String)| v.to_ascii_lowercase());
    let connection = get("connection").unwrap_or_default();
    let keep_alive = if version == "HTTP/1.0" { connection.contains("keep-alive") } else { !connection.contains("close") };
    let mut body = Vec::new();
    if get("transfer-encoding").is_some_and(|t| t.contains("chunked")) {
        loop {
            let size_line = read_line_limited(r, &mut total)?.ok_or_else(|| bad("bad chunk"))?;
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad("bad chunk size"))?;
            if size == 0 {
                while !read_line_limited(r, &mut total)?.unwrap_or_default().is_empty() {}
                break;
            }
            if size > MAX_BODY - body.len() {
                return Err(bad("request body too large"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(&mut body[start..])?;
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf)?;
        }
    } else if let Some(len) = get("content-length") {
        let len: usize = len.trim().parse().map_err(|_| bad("bad content-length"))?;
        if len > MAX_BODY {
            return Err(bad("request body too large"));
        }
        body.resize(len, 0);
        r.read_exact(&mut body)?;
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target.as_str(), ""),
    };
    let path = if path.starts_with('/') { normalize_path(path) } else { "/".to_string() };
    Ok(Some(Request { method, path, query: Query::parse(query.split('#').next().unwrap_or("")), target, headers, body, keep_alive }))
}

// ---------------------------------------------------------------- responses
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, headers: Vec<(&'static str, String)>, body: impl Into<Vec<u8>>) -> Response {
        Response { status, headers, body: body.into() }
    }

    pub fn text(status: u16, body: &str) -> Response {
        Response::new(status, Vec::new(), body)
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Response {
        Response::new(status, vec![("content-type", "application/json".into())], value.to_string())
    }
}

fn date() -> String {
    // RFC 7231 date, computed by hand to avoid a time crate.
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(mo - 1) as usize];
    format!("{wd}, {d:02} {mon} {y} {h:02}:{m:02}:{s:02} GMT")
}

// Everything the server sends is for revue's own page. Cross-Origin-Resource-Policy has the browser refuse to hand any
// of it to a page on another origin, whatever kind of load that page tries (an image, which browsers otherwise let any
// page load and measure, a script, a stylesheet), and it goes on every answer, errors and the event stream included.
const CORP: &str = "cross-origin-resource-policy: same-origin\r\n";

pub fn write_response(w: &mut impl Write, res: &Response, head_only: bool, keep_alive: bool) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", res.status, reason(res.status));
    for (k, v) in &res.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(CORP);
    head.push_str(&format!("date: {}\r\n", date()));
    if res.status != 304 {
        head.push_str(&format!("content-length: {}\r\n", res.body.len()));
    }
    head.push_str(if keep_alive { "connection: keep-alive\r\n\r\n" } else { "connection: close\r\n\r\n" });
    let mut out = head.into_bytes();
    if !head_only && res.status != 304 {
        out.extend_from_slice(&res.body);
    }
    w.write_all(&out)?;
    w.flush()
}

// The response head of an event stream; messages follow as chunks (see sse_chunk).
pub fn write_sse_head(w: &mut impl Write) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n{CORP}connection: keep-alive\r\ndate: {}\r\ntransfer-encoding: chunked\r\n\r\n",
        date()
    );
    w.write_all(head.as_bytes())?;
    w.flush()
}

pub fn sse_chunk(msg: &str) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", msg.len()).into_bytes();
    out.extend_from_slice(msg.as_bytes());
    out.extend_from_slice(b"\r\n");
    out
}

// ---------------------------------------------------------------- client (for the MCP server)
pub struct ClientResponse {
    pub status: u16,
    pub body: String,
}

// The most of an answer the client takes. Whatever answers on the port (a revue server, or once one has exited, any
// local program) can claim any length, so a body is never allocated up front, only read as it arrives, up to this.
const MAX_RESPONSE: usize = 256 * 1024 * 1024;

fn too_large() -> io::Error {
    bad("response too large")
}

// Append the next n bytes of the body, which must all arrive.
fn read_body(r: &mut impl Read, n: usize, body: &mut Vec<u8>) -> io::Result<()> {
    if n > MAX_RESPONSE - body.len() {
        return Err(too_large());
    }
    if (r.take(n as u64).read_to_end(body)?) < n {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "response ended early"));
    }
    Ok(())
}

// One request to 127.0.0.1:port, over a fresh connection. A POST carries the x-revue header the write endpoints want.
pub fn request(port: u16, path: &str, post: Option<&str>, timeout: Duration) -> io::Result<ClientResponse> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let host = format!("127.0.0.1:{port}");
    let req = match post {
        None => format!("GET {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n"),
        Some(body) => format!(
            "POST {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\ncontent-type: application/json\r\nx-revue: 1\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ),
    };
    s.write_all(req.as_bytes())?;
    let mut r = BufReader::new(s);
    let mut total = 0;
    let status_line = read_line_limited(&mut r, &mut total)?.ok_or_else(|| bad("empty response"))?;
    let status: u16 = status_line.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad status line"))?;
    let mut length = None;
    let mut chunked = false;
    loop {
        let l = read_line_limited(&mut r, &mut total)?.ok_or_else(|| bad("unexpected end of headers"))?;
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = v.trim().parse::<usize>().ok(),
                "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut t = 0;
            let size_line = read_line_limited(&mut r, &mut t)?.unwrap_or_default();
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            read_body(&mut r, size, &mut body)?;
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf)?;
        }
    } else if let Some(n) = length {
        read_body(&mut r, n, &mut body)?;
    } else {
        r.by_ref().take(MAX_RESPONSE as u64 + 1).read_to_end(&mut body)?;
        if body.len() > MAX_RESPONSE {
            return Err(too_large());
        }
    }
    Ok(ClientResponse { status, body: String::from_utf8_lossy(&body).into_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing_follows_url_search_params() {
        let q = Query::parse("mode=working&path=a+b.txt&path=%C3%BC&empty=&flag&&bad=%zz");
        assert_eq!(q.get("mode"), Some("working"));
        assert_eq!(q.get_all("path"), ["a b.txt", "ü"]);
        assert_eq!(q.get("empty"), Some(""));
        assert_eq!(q.get_or("empty", "dflt"), "dflt");
        assert_eq!(q.get("flag"), Some(""));
        assert_eq!(q.get("bad"), Some("%zz")); // a bad escape stays as written
        assert_eq!(q.get("missing"), None);
    }

    #[test]
    fn query_encoding_round_trips() {
        let mut q = Query::default();
        q.set("file", "dír/a b&c=d.txt");
        q.set("line", "3");
        q.set("line", "4"); // set replaces
        assert_eq!(q.encode(), "file=d%C3%ADr%2Fa+b%26c%3Dd.txt&line=4");
        let back = Query::parse(&q.encode());
        assert_eq!(back.get("file"), Some("dír/a b&c=d.txt"));
        assert_eq!(back.get_all("line"), ["4"]);
    }

    // Answers one request on a free port with `answer`, then closes. Returns the port.
    fn answer_once(answer: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = c.write_all(answer);
        });
        (port, t)
    }

    fn get(answer: &'static [u8]) -> io::Result<String> {
        let (port, t) = answer_once(answer);
        let r = request(port, "/api/server", None, Duration::from_secs(5));
        t.join().unwrap();
        r.map(|r| r.body)
    }

    // Whatever answers on the port can claim any length: the client must neither allocate it up front (a huge
    // Content-Length aborted revue mcp) nor overflow adding a chunk's size (which panicked its worker thread).
    #[test]
    fn the_client_refuses_lengths_it_would_have_to_trust() {
        assert_eq!(get(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").unwrap(), "{}");
        assert_eq!(get(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n1\r\n{\r\n1\r\n}\r\n0\r\n\r\n").unwrap(), "{}");
        assert_eq!(get(b"HTTP/1.1 200 OK\r\n\r\n{}").unwrap(), "{}");

        let huge = get(b"HTTP/1.1 200 OK\r\ncontent-length: 18446744073709551615\r\n\r\n{}").unwrap_err();
        assert_eq!(huge.to_string(), "response too large");
        let short = get(b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\n{}").unwrap_err();
        assert_eq!(short.kind(), io::ErrorKind::UnexpectedEof, "{short}");
        let chunk = get(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n1\r\n{\r\nffffffffffffffff\r\n}").unwrap_err();
        assert_eq!(chunk.to_string(), "response too large");
    }

    #[test]
    fn dot_segments_are_resolved_in_paths() {
        assert_eq!(normalize_path("/a/./b/../c"), "/a/c");
        assert_eq!(normalize_path("/../../app.js"), "/app.js");
        assert_eq!(normalize_path("/vendor/%2e%2e/index.html"), "/index.html");
        assert_eq!(normalize_path("/vendor/"), "/vendor/");
    }
}
