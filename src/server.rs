// revue: a tiny git change viewer. Read-only except for the opt-in branch switch and agent notes (opt-in file
// storage under .git/revue/). git's own output (diffs, grep) comes from the git CLI; everything else is read in process.
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::discovery::{self, Entry};
use crate::git::{git, git_allow, git_head, http_error, internal, spawn, spawn_scoped, split_z, Error, Result, EMPTY_TREE, MAX_FILE_BYTES};
use crate::http::{self, Query, Request, Response};
use crate::paths::{check_path, real_in_repo};
use crate::{definitions, folder, image, repo, watcher};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

// The page renders untrusted markdown and can call the write endpoints, so only our own scripts may run (no inline
// ones) and nothing loads from anywhere else. Styles too come only from our own files: the UI sets styles through the
// CSSOM, which this allows, never through style attributes. img-src stays 'self' rather than 'none' because Firefox
// applies it to its automatic /favicon.ico request and reports a violation on every page load.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

// The binary this process runs. public/ is built into it, so the page always matches the server, but after a
// rebuild or upgrade the process is older than what is on disk; /api/info and /api/server say so, and unknown /api
// paths say to restart. The binary counts as changed when its inode, size or mtime do.
type Stamp = Option<(u64, u64, std::time::SystemTime)>;

fn code_file() -> &'static Option<PathBuf> {
    static F: OnceLock<Option<PathBuf>> = OnceLock::new();
    F.get_or_init(|| {
        let test_file = std::env::var_os("REVUE_CODE_FILE").filter(|_| test_mode()).map(PathBuf::from);
        test_file.or_else(|| std::env::current_exe().ok())
    })
}

fn stamp() -> Stamp {
    let m = std::fs::metadata(code_file().as_ref()?).ok()?;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&m);
    #[cfg(not(unix))]
    let ino = 0;
    Some((ino, m.len(), m.modified().ok()?))
}

fn loaded_stamp() -> &'static Stamp {
    static S: OnceLock<Stamp> = OnceLock::new();
    S.get_or_init(stamp)
}

fn stale() -> bool {
    stamp() != *loaded_stamp()
}

// ---------------------------------------------------------------- JS-like coercions for browser input
// parseInt(s, 10): optional sign and leading digits, after leading whitespace.
pub fn js_parse_int(s: Option<&str>) -> Option<i64> {
    let s = s?.trim_start();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let end = digits.bytes().take_while(|b| b.is_ascii_digit()).count();
    let n: i64 = digits[..end].parse().ok()?;
    Some(if neg { -n } else { n })
}

// +s (Number(s)): NaN as None; the empty string is 0.
pub fn js_number(s: Option<&str>) -> Option<f64> {
    let t = s.unwrap_or("").trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

// String(v || ''): falsy values are the empty string.
pub fn js_string_or_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(0.0) => String::new(),
            Some(f) if f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Some(Value::Bool(true)) => "true".into(),
        Some(Value::Array(a)) => a.iter().map(|x| js_string_or_empty(Some(x))).collect::<Vec<_>>().join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => true,
    }
}

fn str_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

// ---------------------------------------------------------------- refs and ranges
// Refs come from the browser; only allow safe characters and never a leading dash.
pub fn check_ref(r: Option<&str>) -> Result<String> {
    match r {
        Some(s)
            if !s.is_empty()
                && !s.starts_with('-')
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./@^~{}-".contains(&b)) =>
        {
            Ok(s.to_string())
        }
        Some(s) => Err(http_error(400, format!("bad ref: {s}"))),
        None => Err(http_error(400, "bad ref: null")),
    }
}

// hasHead and defaultBase run on nearly every request, so their answers are cached per root. start() forgets them
// when the watcher fires and after a branch switch; the TTL covers live updates being off.
const REF_CACHE: Duration = Duration::from_secs(3);

// `epoch` moves every time the entry is forgotten or expires, so a value computed from before that (a request that
// was still working when the repo changed) is returned to its caller but never stored over a newer one.
#[derive(Clone)]
struct RefCache {
    at: Instant,
    epoch: u64,
    head: Option<bool>,
    base: Option<Option<String>>,
}

impl RefCache {
    fn reset(&mut self) {
        *self = RefCache { at: Instant::now(), epoch: self.epoch + 1, head: None, base: None };
    }
}

fn ref_cache() -> &'static Mutex<HashMap<PathBuf, RefCache>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, RefCache>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn ref_entry<'a>(map: &'a mut HashMap<PathBuf, RefCache>, root: &Path) -> &'a mut RefCache {
    map.entry(root.to_path_buf()).or_insert_with(|| RefCache { at: Instant::now(), epoch: 0, head: None, base: None })
}

fn cached<T: Clone>(root: &Path, get: impl Fn(&RefCache) -> Option<T>, set: impl Fn(&mut RefCache, T), compute: impl Fn() -> T) -> T {
    let epoch = {
        let mut map = ref_cache().lock().unwrap();
        let c = ref_entry(&mut map, root);
        if c.at.elapsed() > REF_CACHE {
            c.reset();
        }
        if let Some(v) = get(c) {
            return v;
        }
        c.epoch
    };
    let v = compute();
    let mut map = ref_cache().lock().unwrap();
    let c = ref_entry(&mut map, root);
    if c.epoch == epoch {
        set(c, v.clone());
    }
    v
}

pub fn forget_refs(root: &Path) {
    ref_entry(&mut ref_cache().lock().unwrap(), root).reset();
}

fn has_head(root: &Path) -> bool {
    cached(root, |c| c.head, |c, v| c.head = Some(v), || repo::has_head(root))
}

fn default_base(root: &Path) -> Option<String> {
    cached(root, |c| c.base.clone(), |c, v| c.base = Some(v), || {
        ["main", "master", "origin/main", "origin/master", "develop"].iter().find(|b| repo::exists(root, b)).map(|b| b.to_string())
    })
}

// A view resolved into the pair of trees to compare. `to == None` means the working tree. `base` is the base branch a
// branch comparison went by (after the default), which its notes are filed under (range_key).
pub struct Range {
    pub from: String,
    pub to: Option<String>,
    pub base: Option<String>,
}

// The view's refs are resolved here, once, to the commits' full shas (`to`, and `from` in working mode), and everything
// a request reads goes by those: a ref resolved again later (the file listing, a diff, a blob) could name another commit
// when one lands while the request runs, and the answer would mix the two (the parent of one commit, the files of the
// next). A ref that names no commit gets the message the file listing gave for it before.
pub fn resolve_range(root: &Path, q: &Query) -> Result<Range> {
    let mode = q.get_or("mode", "working");
    let commit = |r: &str| repo::commit_sha(root, r).ok_or_else(|| http_error(400, format!("fatal: Not a valid object name {r}")));
    match mode {
        "working" => {
            let head = if has_head(root) { repo::commit_sha(root, "HEAD") } else { None };
            Ok(Range { from: head.unwrap_or_else(|| EMPTY_TREE.into()), to: None, base: None })
        }
        "commit" => {
            let sha = commit(&check_ref(Some(q.get_or("ref", "HEAD")))?)?;
            let parent = repo::first_parent(root, &sha).unwrap_or_else(|| EMPTY_TREE.into());
            Ok(Range { from: parent, to: Some(sha), base: None })
        }
        "branch" => {
            let base = base_name(root, q)?;
            let head = commit(&check_ref(Some(q.get_or("head", "HEAD")))?)?;
            let mb = repo::merge_base(root, &base, &head).unwrap_or_else(|| base.clone());
            Ok(Range { from: mb, to: Some(head), base: Some(base) })
        }
        _ => Err(http_error(400, format!("unknown mode {mode}"))),
    }
}

// A plain folder has one view, its files on disk: the working tree with nothing to compare it with. The view's
// parameters are ignored rather than refused, so a page that asks with a mode before /api/info has told it there are
// none, or an old link to a commit, still gets the folder.
fn folder_range() -> Range {
    Range { from: EMPTY_TREE.into(), to: None, base: None }
}

// The key a plain folder's notes are filed under.
const FOLDER_KEY: &str = "files";

fn view_range(root: &Path, git: bool, q: &Query) -> Result<Range> {
    if git {
        resolve_range(root, q)
    } else {
        Ok(folder_range())
    }
}

// The base a branch comparison goes by: the one asked for, or the default base (HEAD when there is none).
fn base_name(root: &Path, q: &Query) -> Result<String> {
    let fallback = default_base(root).unwrap_or_else(|| "HEAD".into());
    check_ref(Some(q.get("base").filter(|s| !s.is_empty()).unwrap_or(&fallback)))
}

fn range_args(r: &Range) -> Vec<String> {
    match &r.to {
        Some(to) => vec![r.from.clone(), to.clone()],
        None => vec![r.from.clone()],
    }
}

// Every diff runs with these, so its text is the same whatever the user's config says: no external diff tool or
// color, and the standard a/ b/ prefixes (diff.noprefix or diff.mnemonicPrefix would change the headers that the
// client parses and that tracked_diffs matches sections by).
const DIFF_FLAGS: [&str; 4] = ["--no-ext-diff", "--no-color", "--src-prefix=a/", "--dst-prefix=b/"];

fn list_untracked(root: &Path) -> Result<Vec<String>> {
    Ok(split_z(&git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?))
}

// Status and line counts from one `diff --raw --numstat -z`. Raw records come first, as ":modes shas STATUS\0path\0"
// or "...R100\0old\0new\0" for renames and copies; numstat records follow, as "add\tdel\tpath\0" or
// "add\tdel\t\0old\0new\0" for renames. In working mode `untracked` saves listing untracked files again when the
// caller already does.
fn changes(root: &Path, r: &Range, untracked: Option<&[String]>) -> Result<Map<String, Value>> {
    let mut args: Vec<String> = ["diff", "-M", "--raw", "--numstat", "-z", "--no-abbrev"].map(String::from).to_vec();
    args.extend(DIFF_FLAGS.map(String::from));
    args.extend(range_args(r));
    args.push("--".into());
    let (z, counted) = std::thread::scope(|s| -> Result<_> {
        let counted = (r.to.is_none())
            .then(|| {
                spawn_scoped(s, || -> Result<(Vec<String>, Vec<Lines>)> {
                    let files = match untracked {
                        Some(f) => f.to_vec(),
                        None => list_untracked(root)?,
                    };
                    let lines = untracked_lines(root, &files);
                    Ok((files, lines))
                })
            })
            .transpose()?;
        let z = git(root, &args);
        Ok((z, counted.map(|h| h.join().unwrap())))
    })?;
    let z = z?;
    let mut out = Map::new();
    let f: Vec<&str> = z.split('\0').collect();
    let mut i = 0;
    while i + 1 < f.len() && f[i].starts_with(':') {
        let raw = f[i];
        let st = &raw[raw.rfind(' ').map(|p| p + 1).unwrap_or(0)..];
        let letter = st.chars().next().unwrap_or('?').to_string();
        i += 1;
        if letter == "R" || letter == "C" {
            let old = f.get(i).copied().unwrap_or("");
            let new = f.get(i + 1).copied().unwrap_or("");
            i += 2;
            let v = version(root, r, raw, old, new);
            out.insert(new.to_string(), json!({ "status": letter, "oldPath": old, "v": v }));
        } else {
            let p = f.get(i).copied().unwrap_or("");
            i += 1;
            out.insert(p.to_string(), json!({ "status": letter, "v": version(root, r, raw, "", p) }));
        }
    }
    while i + 1 < f.len() {
        let rec = f[i];
        i += 1;
        let mut parts = rec.splitn(3, '\t');
        let (a, d) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let mut file = parts.next().unwrap_or("").to_string(); // the path itself may contain tabs
        if file.is_empty() {
            // rename: old\0new\0 follow
            file = f.get(i + 1).copied().unwrap_or("").to_string();
            i += 2;
        }
        if let Some(Value::Object(e)) = out.get_mut(&file) {
            let count = |s: &str| if s == "-" { Value::Null } else { json!(s.parse::<u64>().unwrap_or(0)) };
            e.insert("add".into(), count(a));
            e.insert("del".into(), count(d));
        }
    }
    if let Some(c) = counted {
        let (files, lines) = c?;
        for (p, n) in files.into_iter().zip(lines) {
            let v = version(root, r, "?", "", &p);
            // A binary file has no counts, as git's numstat ("-\t-") gives a tracked one. A text file that wasn't
            // counted (too large, or unreadable) is still only added, so it keeps del 0.
            let (add, del) = match n {
                Lines::Counted(n) => (json!(n), json!(0)),
                Lines::Binary => (Value::Null, Value::Null),
                Lines::Uncounted => (Value::Null, json!(0)),
            };
            out.insert(p, json!({ "status": "?", "add": add, "del": del, "v": v }));
        }
    }
    Ok(out)
}

// A short tag that changes whenever a file's diff in this range could: the raw record (both modes, both full blob
// shas, the status with its score) and the old path, plus, when one side is the working tree, the file's lstat (size,
// mtime and ctime in ns, inode, mode), since git reports a modified working file's sha as zeros. Untracked files have
// only the stat, and a nested repo its checked-out commit too. The client compares tags to skip re-fetching diffs that cannot have changed.
fn version(root: &Path, r: &Range, raw: &str, old: &str, path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (raw, old, path).hash(&mut h);
    if r.to.is_none() {
        stat_key(&root.join(path)).hash(&mut h);
        if raw == "?" && path.ends_with('/') {
            // A nested repo's diff is its checked-out commit, which a commit there moves without touching the folder.
            repo::nested_head(&root.join(path)).hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

fn stat_key(p: &Path) -> Option<Stat> {
    std::fs::symlink_metadata(p).ok().map(|m| stat_of(&m))
}

// A file's size, mtime and ctime in ns, inode and mode.
type Stat = (u64, i128, i128, u64, u32);

fn stat_of(m: &std::fs::Metadata) -> Stat {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ns = |s: i64, n: i64| s as i128 * 1_000_000_000 + n as i128;
        (m.size(), ns(m.mtime(), m.mtime_nsec()), ns(m.ctime(), m.ctime_nsec()), m.ino(), m.mode())
    }
    #[cfg(not(unix))]
    {
        let ns = |t: std::io::Result<std::time::SystemTime>| {
            t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i128)
        };
        (m.len(), ns(m.modified()), ns(m.created()), 0, m.permissions().readonly() as u32)
    }
}

// Line counts of untracked files. Files are read a few at a time, and a count is reused while the file's size and
// mtime stay the same, so a refresh only reads what changed.
const UNTRACKED_READS: usize = 16;

// What reading an untracked file found: its lines, that it is binary (git counts none), or no count, for a text file
// too large to count (over MAX_FILE_BYTES: only its first BINARY_PEEK bytes are read, to tell which it is), or one that
// is unreadable or not a regular file. A link has lines too: git shows it as the text of its target path.
#[derive(Clone, Copy)]
enum Lines {
    Counted(u64),
    Binary,
    Uncounted,
}

#[derive(Clone)]
struct LineCount {
    real: PathBuf,
    size: u64,
    mtime: Option<std::time::SystemTime>,
    lines: Lines,
}

fn line_cache() -> &'static Mutex<HashMap<PathBuf, HashMap<String, LineCount>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, HashMap<String, LineCount>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

// git decides a file is binary by its first 8000 bytes (FIRST_FEW_BYTES in its xdiff-interface.c): binary when they
// hold a NUL. /api/file and the untracked counts decide it the same way.
const BINARY_PEEK: usize = 8000;

fn is_binary(buf: &[u8]) -> bool {
    buf[..buf.len().min(BINARY_PEEK)].contains(&0)
}

// Past the first BINARY_PEEK bytes, git counts lines as in text.
fn count_lines(buf: &[u8]) -> Lines {
    if is_binary(buf) {
        return Lines::Binary;
    }
    if buf.is_empty() {
        return Lines::Counted(0);
    }
    let n = buf.iter().filter(|&&b| b == b'\n').count() as u64;
    Lines::Counted(if buf.last() == Some(&b'\n') { n } else { n + 1 })
}

fn untracked_lines(root: &Path, files: &[String]) -> Vec<Lines> {
    let old = line_cache().lock().unwrap().get(root).cloned().unwrap_or_default();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Lines, Option<LineCount>)>> = Mutex::new(Vec::new());
    let work = || loop {
        let i = next.fetch_add(1, Ordering::SeqCst);
        let Some(f) = files.get(i) else { return };
        if let Some((lines, c)) = untracked_count(root, f, old.get(f)) {
            results.lock().unwrap().push((i, lines, c));
        }
    };
    // The calling thread is one of the readers, so the work still gets done when no other thread can be had.
    std::thread::scope(|s| {
        for _ in 1..UNTRACKED_READS.min(files.len()) {
            if spawn_scoped(s, work).is_err() {
                break;
            }
        }
        work();
    });
    let mut out = vec![Lines::Uncounted; files.len()];
    let mut fresh = HashMap::new();
    for (i, lines, c) in results.into_inner().unwrap() {
        out[i] = lines;
        if let Some(c) = c {
            fresh.insert(files[i].clone(), c);
        }
    }
    line_cache().lock().unwrap().insert(root.to_path_buf(), fresh);
    out
}

// One untracked file's lines, and the cache entry to keep for it (`cached` is the last one). None when it can't be
// counted. A link isn't cached: counting it takes one readlink.
fn untracked_count(root: &Path, f: &str, cached: Option<&LineCount>) -> Option<(Lines, Option<LineCount>)> {
    let abs = root.join(f);
    if std::fs::symlink_metadata(&abs).ok()?.is_symlink() {
        // git takes an untracked link as itself: its text is its target path, wherever that leads (or whether it
        // leads anywhere), and the untracked diff shows just that. So it is counted as that text, never followed.
        // Only the folder it is in has to be inside the repo, as for the diff.
        if let Some(dir) = Path::new(f).parent().filter(|d| !d.as_os_str().is_empty()) {
            real_in_repo(root, &dir.to_string_lossy()).ok()??;
        }
        let target = std::fs::read_link(&abs).ok()?;
        return Some((count_lines(target.as_os_str().as_encoded_bytes()), None));
    }
    let real = real_in_repo(root, f).ok()??;
    let st = std::fs::metadata(&real).ok()?;
    if st.is_dir() {
        // A nested repo: one line once added, its commit (nested_repo_diff), or none counted while it has none.
        return repo::nested_head(&real).map(|_| (Lines::Counted(1), None));
    }
    if !st.is_file() {
        return None;
    }
    let mtime = st.modified().ok();
    if let Some(c) = cached.filter(|c| c.real == real && c.size == st.len() && c.mtime == mtime) {
        return Some((c.lines, Some(c.clone())));
    }
    let lines = if st.len() > MAX_FILE_BYTES {
        // Too large to count, but its start still tells a binary file (a big image, say) from text, as git tells.
        if is_binary(&read_start(&real, BINARY_PEEK).ok()?) {
            Lines::Binary
        } else {
            Lines::Uncounted
        }
    } else {
        let buf = std::fs::read(&real).ok()?;
        debug_read(&real, buf.len());
        count_lines(&buf)
    };
    Some((lines, Some(LineCount { real, size: st.len(), mtime, lines })))
}

// The first `n` bytes of a file, or all of it when it is shorter.
fn read_start(p: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(n);
    std::fs::File::open(p)?.take(n as u64).read_to_end(&mut buf)?;
    debug_read(p, buf.len());
    Ok(buf)
}

// Files modified in the same second the index was written are "racily clean": git re-reads them on every diff, and
// git diff never rewrites the index to settle them (after a checkout that can be every file). Refreshing rewrites the
// stat info only, as `git status` does. It runs at most once per change the watcher reports (`generation`) and at
// most once a second: its own index write is a change too, and while a file is still racily clean every refresh
// rewrites the index again, so without the second's pause each refresh would set off the next until the clock moved
// on. Skipping one costs nothing but speed (the diff re-reads those files). It never runs while index.lock is held:
// taking the lock then could make the user's own commit or add fail. It exits 1 when files really changed, and may fail on a read-only repo; neither
// matters here.
fn refresh_index(s: &Shared) {
    let generation = s.generation.load(Ordering::SeqCst);
    let mut last = s.refreshed.lock().unwrap();
    if last.is_some_and(|(g, at)| g == generation || at.elapsed() < Duration::from_secs(1)) {
        return;
    }
    if repo::git_dir(&s.root).is_ok_and(|d| d.join("index.lock").exists()) {
        return; // try again on the next request
    }
    *last = Some((generation, Instant::now()));
    drop(last);
    let _ = git_allow(&s.root, &["update-index", "-q", "--refresh"], &[0, 1]);
}

// The file list and the diff run in parallel; in working mode the untracked list is shared with changes().
fn tree(root: &Path, r: &Range) -> Result<Value> {
    let (listed, ch) = match &r.to {
        Some(to) => std::thread::scope(|s| -> Result<_> {
            let ch = spawn_scoped(s, || changes(root, r, None))?;
            let files = repo::tree_files(root, to).map(|v| v.into_iter().map(|(p, _)| p).collect::<Vec<_>>());
            Ok((files, ch.join().unwrap()))
        })?,
        None => std::thread::scope(|s| -> Result<_> {
            let index = spawn_scoped(s, || repo::index_files(root))?;
            Ok(match list_untracked(root) {
                Ok(untracked) => {
                    let ch = changes(root, r, Some(&untracked));
                    let files = index.join().unwrap().map(|mut v| {
                        v.extend(untracked);
                        v
                    });
                    (files, ch)
                }
                Err(e) => (Err(e), Ok(Map::new())),
            })
        })?,
    };
    // The listing's error describes a bad ref best ("Not a valid object name"), so it goes first.
    let mut set: HashSet<String> = listed?.into_iter().collect();
    let ch = ch?;
    for f in ch.keys() {
        set.insert(f.clone()); // deleted files still appear in the tree
    }
    let mut files: Vec<String> = set.into_iter().collect();
    files.sort();
    // The commit listed (by its full sha), or null for the working tree: History opens at the first commit of its list
    // with the tree of the branch tip, fetched by the branch's name before the list, and checks that they are the same.
    Ok(json!({ "files": files, "changes": ch, "commit": r.to }))
}

// A plain folder's listing: every file the walk reached, no changes, and whether the cap cut it short.
fn folder_tree(root: &Path) -> Value {
    let w = folder::walk(root, folder::max_files());
    json!({ "files": w.files, "changes": {}, "commit": null, "truncated": w.truncated })
}

fn not_a_repo(root: &Path) -> Error {
    http_error(400, format!("{} is not a git repository", root.display()))
}

fn diff_context(q: &Query) -> String {
    if q.get("context") == Some("all") {
        return "999999".into();
    }
    match js_parse_int(q.get("context")) {
        Some(n) => n.clamp(0, 1000).to_string(), // 0 is valid: changed lines only
        None => "3".into(),
    }
}

// A file's diff, or that there is none to show: an untracked text file over MAX_FILE_BYTES isn't diffed, since its
// diff is all of it (git would read the whole file and write every line), and neither view shows a file that large
// (/api/file says tooLarge too). /api/diff answers it with `{diff: null, tooLarge: true, size}`, and /api/diffs gives
// it `{tooLarge: true, size}` in place of its text. Tracked files are diffed whatever their size, as git diffs them.
enum Diff {
    Text(String),
    TooLarge(u64),
}

impl Diff {
    // /api/diffs' entry for the file: its text, or why there is none.
    fn into_batch_value(self) -> Value {
        match self {
            Diff::Text(d) => Value::String(d),
            Diff::TooLarge(size) => json!({ "tooLarge": true, "size": size }),
        }
    }
}

// `git diff --no-index` reads the path it's given and follows symlinked directories on the way, so an untracked path
// under a link (linkdir/secret.txt) would show a file outside the repo. A link as the last part is fine (git shows
// the link itself), and anything that isn't a file, link or directory (a FIFO would block the read) is refused.
// A file over MAX_FILE_BYTES is told binary or text by its first BINARY_PEEK bytes, as the line counts tell it: a
// binary one is still diffed (git writes one line saying so), a text one isn't (Diff::TooLarge).
fn untracked_diff(root: &Path, ctx: &str, p: &str) -> Result<Diff> {
    if let Some(dir) = Path::new(p).parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(None) = real_in_repo(root, &dir.to_string_lossy()) {
            return Err(http_error(400, "path escapes repo"));
        }
    }
    if let Ok(md) = std::fs::symlink_metadata(root.join(p)) {
        if md.is_symlink() {
            return new_link_diff(root, p, &std::fs::read_link(root.join(p))?).map(Diff::Text);
        }
        if md.is_dir() {
            return nested_repo_diff(root, p).map(Diff::Text);
        }
        if !(md.is_file() || md.is_dir()) {
            return Err(http_error(400, format!("{p} is not a file")));
        }
        // A start that can't be read goes on to git, which reports the file as it did before.
        if md.len() > MAX_FILE_BYTES && read_start(&root.join(p), BINARY_PEEK).is_ok_and(|s| !is_binary(&s)) {
            return Ok(Diff::TooLarge(md.len()));
        }
    }
    let mut args: Vec<String> = vec!["diff".into(), "--no-index".into(), format!("-U{ctx}")];
    args.extend(DIFF_FLAGS.map(String::from));
    args.extend(["--".into(), "/dev/null".into(), p.into()]);
    git_allow(root, &args, &[0, 1]).map(Diff::Text)
}

// A new link's diff, as git writes one: the link itself, its target path as text. `git diff --no-index` writes the
// same for a link to a file, but follows a link to a directory, fails, and exits with 1 as for any difference, which
// left that diff empty. The index line abbreviates to 7, git's default.
fn new_link_diff(root: &Path, p: &str, target: &Path) -> Result<String> {
    let data = target.as_os_str().as_encoded_bytes();
    let id = repo::blob_id(root, data)?;
    let text = String::from_utf8_lossy(data);
    let b = quote_header_path(&format!("b/{p}"));
    let mut out = format!(
        "diff --git {} {b}\nnew file mode 120000\nindex 0000000..{}\n--- /dev/null\n+++ {b}\n",
        quote_header_path(&format!("a/{p}")),
        id.to_hex_with_len(7)
    );
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    out += &match lines.len() {
        1 => "@@ -0,0 +1 @@\n".to_string(),
        n => format!("@@ -0,0 +1,{n} @@\n"),
    };
    for l in lines {
        out.push('+');
        out.push_str(l);
    }
    if !text.ends_with('\n') {
        out.push_str("\n\\ No newline at end of file\n");
    }
    Ok(out)
}

// git lists a repo nested in the work tree as one untracked entry, `dir/`, and `git diff --no-index` fails on it as on
// a link to a directory. Once added it is a gitlink, so its diff is the one git shows then: the commit checked out
// there. One with no commit yet can't be added, and shows only its header.
fn nested_repo_diff(root: &Path, p: &str) -> Result<String> {
    if let Ok(None) = real_in_repo(root, p) {
        return Err(http_error(400, "path escapes repo"));
    }
    let name = p.trim_end_matches('/');
    let (a, b) = (quote_header_path(&format!("a/{name}")), quote_header_path(&format!("b/{name}")));
    let Some(id) = repo::nested_head(&root.join(name)) else { return Ok(format!("diff --git {a} {b}\n")) };
    Ok(format!(
        "diff --git {a} {b}\nnew file mode 160000\nindex 0000000..{}\n--- /dev/null\n+++ {b}\n@@ -0,0 +1 @@\n+Subproject commit {id}\n",
        id.to_hex_with_len(7)
    ))
}

fn tracked_diff(root: &Path, r: &Range, ctx: &str, paths: &[&str]) -> Result<String> {
    let mut args: Vec<String> = vec!["diff".into(), "-M".into(), format!("-U{ctx}")];
    args.extend(DIFF_FLAGS.map(String::from));
    args.extend(range_args(r));
    args.push("--".into());
    args.extend(paths.iter().map(|p| p.to_string()));
    git(root, &args)
}

// /api/diff's answer: `{diff}`, or `{diff: null, tooLarge: true, size}` (Diff::TooLarge).
fn diff(root: &Path, r: &Range, q: &Query) -> Result<Value> {
    let p = check_path(root, q.get("path"))?;
    let old = match q.get("old").filter(|s| !s.is_empty()) {
        Some(o) => Some(check_path(root, Some(o))?),
        None => None,
    };
    let ctx = diff_context(q);
    let d = if q.get("status") == Some("?") {
        untracked_diff(root, &ctx, &p)?
    } else {
        let mut paths: Vec<&str> = old.as_deref().into_iter().collect();
        paths.push(&p);
        Diff::Text(tracked_diff(root, r, &ctx, &paths)?)
    };
    Ok(match d {
        Diff::Text(d) => json!({ "diff": d }),
        Diff::TooLarge(size) => json!({ "diff": null, "tooLarge": true, "size": size }),
    })
}

// git's C-style quoting of a path in a "diff --git" header (core.quotepath=off leaves non-ASCII alone).
fn quote_header_path(p: &str) -> String {
    let needs = |c: char| c == '"' || c == '\\' || (c as u32) < 0x20 || c as u32 == 0x7f;
    if !p.chars().any(needs) {
        return p.to_string();
    }
    let mut out = String::from("\"");
    for c in p.chars() {
        if !needs(c) {
            out.push(c);
            continue;
        }
        out.push('\\');
        match c as u32 {
            7 => out.push('a'),
            8 => out.push('b'),
            9 => out.push('t'),
            10 => out.push('n'),
            11 => out.push('v'),
            12 => out.push('f'),
            13 => out.push('r'),
            34 => out.push('"'),
            92 => out.push('\\'),
            k => out.push_str(&format!("{k:03o}")),
        }
    }
    out.push('"');
    out
}

// Run f over items, `n` at a time. The calling thread is one of the n, so the work still gets done (one at a time)
// when no other thread can be had.
pub(crate) fn pool<T: Sync, R: Send>(items: &[T], n: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::new());
    let work = || loop {
        let i = next.fetch_add(1, Ordering::SeqCst);
        let Some(item) = items.get(i) else { return };
        let r = f(item);
        out.lock().unwrap().push((i, r));
    };
    std::thread::scope(|s| {
        for _ in 1..n.min(items.len()) {
            if spawn_scoped(s, work).is_err() {
                break;
            }
        }
        work();
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

// Diffs for many files at once (the overview), each exactly what diff() returns for it: its text, or, for an untracked
// text file too large to diff, `{tooLarge: true, size}` (Diff). Tracked files share one `git diff` whose output is
// split at each "diff --git" header and matched to the expected header for the path (a/old b/new). git may pair
// renames differently when given several paths at once, so a path whose header isn't found while unmatched sections
// remain falls back to its own diff call.
const MAX_BATCH_DIFFS: usize = 50;

struct BatchFile {
    p: String,
    old: Option<String>,
    untracked: bool,
}

fn diffs(root: &Path, r: &Range, q: &Query) -> Result<Map<String, Value>> {
    let (paths, olds, statuses) = (q.get_all("path"), q.get_all("old"), q.get_all("status"));
    if paths.len() > MAX_BATCH_DIFFS {
        return Err(http_error(400, format!("at most {MAX_BATCH_DIFFS} paths per request")));
    }
    let ctx = diff_context(q);
    let mut files: Vec<BatchFile> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let f = BatchFile {
            p: check_path(root, Some(p))?,
            old: match olds.get(i).filter(|o| !o.is_empty()) {
                Some(o) => Some(check_path(root, Some(o))?),
                None => None,
            },
            untracked: statuses.get(i) == Some(&"?"),
        };
        match files.iter_mut().find(|x| x.p == f.p) {
            Some(x) => *x = f,
            None => files.push(f),
        }
    }
    let untracked: Vec<&BatchFile> = files.iter().filter(|f| f.untracked).collect();
    let tracked: Vec<&BatchFile> = files.iter().filter(|f| !f.untracked).collect();
    let (u, t) = std::thread::scope(|s| -> Result<_> {
        let u = spawn_scoped(s, || pool(&untracked, 4, |f| untracked_diff(root, &ctx, &f.p).map(|d| (f.p.clone(), d))))?;
        let t = tracked_diffs(root, r, &ctx, &tracked);
        Ok((u.join().unwrap(), t))
    })?;
    let mut out = Map::new();
    for res in u {
        let (p, d) = res?;
        out.insert(p, d.into_batch_value());
    }
    for (p, d) in t? {
        out.insert(p, Value::String(d));
    }
    Ok(out)
}

fn tracked_diffs(root: &Path, r: &Range, ctx: &str, files: &[&BatchFile]) -> Result<Vec<(String, String)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut specs: Vec<&str> = Vec::new();
    for f in files {
        for p in f.old.iter().map(|s| s.as_str()).chain([f.p.as_str()]) {
            if !specs.contains(&p) {
                specs.push(p);
            }
        }
    }
    let text = tracked_diff(root, r, ctx, &specs)?;
    let mut starts: Vec<usize> = Vec::new();
    let mut pos = 0;
    while let Some(i) = text[pos..].find("diff --git ") {
        let at = pos + i;
        if at == 0 || text.as_bytes()[at - 1] == b'\n' {
            starts.push(at);
        }
        pos = at + 1;
    }
    let mut sections: HashMap<String, String> = HashMap::new();
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(text.len());
        let sec = &text[s..end];
        let head = &sec[..sec.find('\n').unwrap_or(sec.len())];
        sections.insert(head.to_string(), sec.to_string());
    }
    let mut out = Vec::new();
    let mut missing: Vec<&BatchFile> = Vec::new();
    for f in files {
        let head = format!(
            "diff --git {} {}",
            quote_header_path(&format!("a/{}", f.old.as_deref().unwrap_or(&f.p))),
            quote_header_path(&format!("b/{}", f.p))
        );
        match sections.remove(&head) {
            Some(sec) => out.push((f.p.clone(), sec)),
            None => missing.push(f),
        }
    }
    // With every section claimed, a missing path simply has no diff.
    let left = !sections.is_empty();
    for res in pool(&missing, 4, |f| {
        if !left {
            return Ok((f.p.clone(), String::new()));
        }
        let mut paths: Vec<&str> = f.old.as_deref().into_iter().collect();
        paths.push(&f.p);
        tracked_diff(root, r, ctx, &paths).map(|d| (f.p.clone(), d))
    }) {
        out.push(res?);
    }
    Ok(out)
}

// A file over MAX_FILE_BYTES is shown by neither view, but its start (`start`) still tells binary from text, as
// for a small one, so a big image says it is binary rather than too large.
fn over_limit(start: &[u8], size: u64) -> Value {
    if is_binary(start) {
        json!({ "binary": true, "size": size })
    } else {
        json!({ "tooLarge": true, "size": size })
    }
}

// One side of a file, as /api/file and /api/image find it, before anything of it is read: a working-tree file (its
// real path and metadata), a blob, no bytes at all (the empty tree's side), or a symlink in the working tree that
// leads out of the repo, which isn't read.
enum Found {
    Disk(PathBuf, std::fs::Metadata),
    Blob(gix::ObjectId),
    Empty,
    Outside,
}

fn find_side(root: &Path, r: &Range, p: &str, old_side: bool) -> Result<Found> {
    let side = if old_side { Some(r.from.as_str()) } else { r.to.as_deref() };
    match side {
        None => {
            // realpath fails first when the file is missing
            let real = match real_in_repo(root, p) {
                Ok(Some(real)) => real,
                Ok(None) => return Ok(Found::Outside),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => {
                    return Err(http_error(404, format!("file not found: {p}")))
                }
                Err(e) => return Err(e.into()),
            };
            // Only regular files: opening a FIFO or a device could block forever.
            let md = std::fs::metadata(&real)?;
            if !md.is_file() {
                return Err(http_error(400, format!("{p} is not a file")));
            }
            Ok(Found::Disk(real, md))
        }
        Some(s) if s == EMPTY_TREE => Ok(Found::Empty),
        Some(s) => Ok(Found::Blob(repo::blob_at(root, s, p)?)),
    }
}

// One side of a file, as /api/file and /api/image read it: all of it when it is at most `limit` bytes, or else only
// its first BINARY_PEEK bytes and its size, so a huge file is never loaded just to be turned away. Outside: a symlink
// in the working tree that leads out of the repo, which isn't read.
enum Side {
    All(Vec<u8>),
    Start(Vec<u8>, u64),
    Outside,
}

fn read_found(root: &Path, found: &Found, limit: u64) -> Result<Side> {
    let buf = match found {
        Found::Outside => return Ok(Side::Outside),
        Found::Empty => Vec::new(),
        // The size is checked before reading.
        Found::Disk(real, md) => {
            if md.len() > limit {
                return Ok(Side::Start(read_start(real, BINARY_PEEK)?, md.len()));
            }
            let buf = std::fs::read(real)?;
            debug_read(real, buf.len());
            buf
        }
        Found::Blob(id) => {
            let size = repo::blob_size(root, *id)?;
            if size > limit {
                return Ok(Side::Start(git_head(root, &["cat-file", "blob", &id.to_string()], BINARY_PEEK)?, size));
            }
            repo::blob_data(root, *id)?
        }
    };
    if buf.len() as u64 > limit {
        let size = buf.len() as u64; // grew since the size check
        return Ok(Side::Start(buf[..BINARY_PEEK.min(buf.len())].to_vec(), size));
    }
    Ok(Side::All(buf))
}

fn read_file(root: &Path, r: &Range, path: Option<&str>, old_side: bool) -> Result<Value> {
    let p = check_path(root, path)?;
    let found = find_side(root, r, &p, old_side)?;
    let key = image_key(&found);
    if let Some(known) = key.as_ref().and_then(known_image) {
        return Ok(known);
    }
    let v = match read_found(root, &found, MAX_FILE_BYTES)? {
        Side::Outside => json!({ "outside": true }),
        // An SVG is text, and also an image: its size in bytes goes with it, for the image's caption.
        Side::All(buf) if !is_binary(&buf) => {
            let mut v = json!({ "content": String::from_utf8_lossy(&buf) });
            if let Some(image) = svg_image(key.as_ref(), &p, &buf, buf.len() as u64) {
                v["size"] = json!(buf.len());
                v["image"] = image;
            }
            v
        }
        Side::All(buf) => with_image(json!({ "binary": true, "size": buf.len() }), image::inspect(&buf, buf.len() as u64), &buf),
        Side::Start(start, size) if !is_binary(&start) => {
            let mut v = over_limit(&start, size);
            if let Some(image) = svg_image(key.as_ref(), &p, &start, size) {
                v["image"] = image;
            }
            v
        }
        Side::Start(start, size) => {
            let v = over_limit(&start, size);
            if image::mime(&start).is_none() {
                return Ok(v);
            }
            // An image too large for this view may still be one /api/image shows. Its size is read from the whole of
            // it, since a GIF's frames anywhere in it count, unless it is too large for that too.
            match read_found(root, &found, image::MAX_BYTES)? {
                Side::All(buf) => with_image(v, image::inspect(&buf, buf.len() as u64), &buf),
                Side::Start(start, size) => with_image(v, image::inspect(&start, size), &start),
                Side::Outside => v,
            }
        }
    };
    // Only a binary image's answer is kept, shown or refused (see image_cache).
    if let Some(key) = key.filter(|_| v["binary"] == true && v["image"].is_object()) {
        image_cache().lock().unwrap().put(key, v.clone());
    }
    Ok(v)
}

// /api/file's answer for a binary file, plus, when it is an image (image.rs), what /api/image will make of it: its
// type, its size in pixels if the header gives it, and either why it won't be shown (`refused`: bytes, pixels, header,
// or for an SVG, complex when it draws too much, embedded when its data: URLs hold images that aren't shown, and
// malformed when it isn't well-formed XML, with the `reason` and its `line`, and no size) or the
// version of its bytes (`v`, /api/image's ETag), which the page puts in the image's address so that a changed image
// is fetched again even when its size in bytes and pixels stayed the same. `img` is what image.rs made of `data`
// (image::inspect for a binary file, image::svg for a text one), which is the whole file unless it is refused for its
// size. A text file that starts like a raster image is shown as the text it is.
fn with_image(mut v: Value, img: Option<image::Image>, data: &[u8]) -> Value {
    let Some(img) = img else { return v };
    let mut o = json!({ "type": img.mime });
    if let Some((w, h)) = img.reported_size() {
        o["width"] = json!(w);
        o["height"] = json!(h);
    }
    match img.refused {
        Some(why) => {
            o["refused"] = json!(why.name());
            if let image::Refusal::Malformed(m) = why {
                o["reason"] = json!(m.reason);
                o["line"] = json!(m.line);
            }
        }
        None => o["v"] = json!(image_version(data)),
    }
    v["image"] = o;
    v
}

// The version of an image's bytes: /api/file's `v` for it, and /api/image's ETag.
fn image_version(data: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    format!("{:016x}", h.finish())
}

// What /api/file said of each image, so that asking again reads nothing: the page asks about every image a markdown
// preview shows on every live update, and an image's answer takes all of it, up to image::MAX_BYTES, read and hashed
// for its version. Only a binary image's answer is kept (a binary file image::inspect takes for one, shown or
// refused); a text file's answer is its text, an SVG's included. An answer is found by a key that changes whenever the
// bytes could (ImageKey): a blob's id, or a working-tree file's real path and stat (size, mtime and ctime in ns, inode,
// mode), as the change tags (version) take theirs, since a working file has no id short of reading it. Like those
// tags, the stat can't see an edit that keeps the size and both times: rewriting the file at the same length within
// one tick of a coarse file system clock (a few ms). Until the file changes again, /api/file's answer and
// /api/image's revalidations (304) then describe the bytes from before that edit, though a 200 from /api/image always
// serves the file as it is. Both kinds of key name the same bytes whichever repo asks (a real path is absolute, a blob
// id is a hash of its content), so one cache serves every server in the process.
fn image_cache() -> &'static Mutex<ImageCache> {
    static C: OnceLock<Mutex<ImageCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn known_image(key: &ImageKey) -> Option<Value> {
    image_cache().lock().unwrap().get(key)
}

// What image::svg made of each SVG /api/file was asked about: the `image` part of its answer (its type and size, and
// why it is refused or the version of its bytes), never its text, found by the same keys as image_cache's answers.
// Reading an SVG takes two passes over up to MAX_SVG_BYTES (svg_elements, then well_formed), and its version a hash of
// it, which a preview that shows it would otherwise cost on every live update; the version also lets /api/image answer
// a revalidation with a 304 without reading the file.
fn svg_cache() -> &'static Mutex<ImageCache> {
    static C: OnceLock<Mutex<ImageCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

// The `image` part of /api/file's answer for an SVG whose bytes are `data` (see image::svg), from svg_cache, or read
// and kept there; None for a text file that isn't one. A key names bytes, which another path can hold too, so only a
// file named as an SVG is looked up: the rest of what image::svg decides comes from the bytes.
fn svg_image(key: Option<&ImageKey>, path: &str, data: &[u8], len: u64) -> Option<Value> {
    if !path.to_ascii_lowercase().ends_with(".svg") {
        return None;
    }
    if let Some(image) = key.and_then(|k| svg_cache().lock().unwrap().get(k)) {
        return Some(image);
    }
    let image = with_image(json!({}), Some(image::svg(path, data, len)?), data)["image"].take();
    if let Some(k) = key {
        svg_cache().lock().unwrap().put(k.clone(), image.clone());
    }
    Some(image)
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ImageKey {
    Disk(PathBuf, Stat),
    Blob(gix::ObjectId),
}

fn image_key(found: &Found) -> Option<ImageKey> {
    match found {
        Found::Disk(real, md) => Some(ImageKey::Disk(real.clone(), stat_of(md))),
        Found::Blob(id) => Some(ImageKey::Blob(*id)),
        Found::Empty | Found::Outside => None,
    }
}

// The answers are kept in two generations of at most IMAGE_CACHE each: a new one goes into the young one, and when
// that is full the old one is dropped and the young one takes its place. An answer found in the old one moves back
// into the young one, so the only answers dropped are those nobody asked for while IMAGE_CACHE others were kept.
const IMAGE_CACHE: usize = 2048;

#[derive(Default)]
struct ImageCache {
    young: HashMap<ImageKey, Value>,
    old: HashMap<ImageKey, Value>,
}

impl ImageCache {
    fn get(&mut self, key: &ImageKey) -> Option<Value> {
        if let Some(v) = self.young.get(key) {
            return Some(v.clone());
        }
        let v = self.old.remove(key)?;
        self.put(key.clone(), v.clone());
        Some(v)
    }

    fn put(&mut self, key: ImageKey, v: Value) {
        if self.young.len() >= IMAGE_CACHE && !self.young.contains_key(&key) {
            self.old = std::mem::take(&mut self.young);
        }
        self.young.insert(key, v);
    }
}

// An image opened on its own (Open image in new tab) is a document of revue's origin. sandbox gives it an opaque
// origin of its own and runs no script in it, and it may load nothing but this server's images, so not even a file
// that were a page could reach the API. The two exceptions are for the browsers' own image view: Firefox asks for
// /favicon.ico (as for the page; see CSP), and Chromium styles the view with inline styles, which can load nothing
// here. The type it is served as comes from its bytes, and nosniff holds the browser to it.
const IMAGE_CSP: &str = "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox";

// GET /api/image: the bytes of an image (image.rs) on one side of the view, read as /api/file reads that side, but up
// to image::MAX_BYTES. Anything that isn't an image shown is an error: 415 for a file that isn't one, whose size can't
// be read, or an SVG that isn't well formed (with why, and where), and 413 for one too large, or an SVG too complex to
// draw or whose embedded images aren't shown. An SVG is a document: opened on its own rather than in an <img>, it is
// handed over as a download (attachment) instead of being drawn, on top of the sandbox. A revalidation of an image
// whose answer /api/file has kept (image_cache, or svg_cache for an SVG) for the file as it is now gets its 304 without
// the file being read.
fn image_file(root: &Path, r: &Range, path: Option<&str>, old_side: bool, req: &Request) -> Result<Response> {
    let p = check_path(root, path)?;
    let found = find_side(root, r, &p, old_side)?;
    let key = image_key(&found);
    let raster = key.as_ref().and_then(known_image).map(|mut known| (known["image"].take(), false));
    let svg_named = p.to_ascii_lowercase().ends_with(".svg");
    let svg = || key.as_ref().filter(|_| svg_named).and_then(|k| svg_cache().lock().unwrap().get(k));
    let kept = raster.or_else(|| svg().map(|image| (image, true)));
    if let Some((image, svg)) = kept {
        if let Some(v) = image["v"].as_str() {
            let etag = format!("\"{v}\"");
            if etag_matches(req, &etag) {
                return Ok(Response::new(304, image_headers(&etag, svg), Vec::new()));
            }
        }
    }
    let (img, buf) = match read_found(root, &found, image::MAX_BYTES)? {
        Side::Outside => return Err(http_error(403, format!("{p} is a symlink to a location outside the repository"))),
        Side::Start(start, size) => (image::inspect(&start, size).or_else(|| image::svg(&p, &start, size)), Vec::new()),
        Side::All(buf) => {
            let len = buf.len() as u64;
            (image::inspect(&buf, len).or_else(|| image::svg(&p, &buf, len)), buf)
        }
    };
    let Some(img) = img else { return Err(http_error(415, format!("{p} is not an image revue shows"))) };
    let size = img.reported_size();
    match (img.refused, size) {
        (None, _) => {}
        (Some(image::Refusal::Header), _) => return Err(http_error(415, format!("{p}: the size of this image can't be read"))),
        (Some(image::Refusal::Pixels), Some((w, h))) => {
            let most = image::MAX_PIXELS / 1_000_000;
            return Err(http_error(413, format!("{p} is too large to show ({w} × {h} pixels, over {most} million)")));
        }
        (Some(image::Refusal::Complex), _) => {
            let most = image::MAX_SVG_ELEMENTS;
            return Err(http_error(413, format!("{p} is too complex to draw (over {},{:03} elements)", most / 1000, most % 1000)));
        }
        (Some(image::Refusal::Embedded), _) => {
            let most = image::MAX_PIXELS / 1_000_000;
            return Err(http_error(413, format!("{p} embeds an image revue can't check, or images of over {most} million pixels in all")));
        }
        (Some(image::Refusal::Malformed(m)), _) => {
            return Err(http_error(415, format!("{p} is not a well-formed SVG: {} (line {})", m.reason, m.line)));
        }
        (Some(_), _) => {
            let most = if img.mime == image::SVG { image::MAX_SVG_BYTES } else { image::MAX_BYTES };
            return Err(http_error(413, format!("{p} is too large to show (over {} MB)", most >> 20)));
        }
    }
    let etag = format!("\"{}\"", image_version(&buf));
    let mut headers = image_headers(&etag, img.mime == image::SVG);
    if etag_matches(req, &etag) {
        return Ok(Response::new(304, headers, Vec::new()));
    }
    headers.insert(0, ("content-type", img.mime.to_string()));
    Ok(Response::new(200, headers, buf))
}

// /api/image's headers, but its type, for a 200 and a 304 alike.
fn image_headers(etag: &str, svg: bool) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("cache-control", "no-cache".to_string()),
        ("etag", etag.to_string()),
        ("content-security-policy", IMAGE_CSP.to_string()),
        ("x-content-type-options", "nosniff".to_string()),
    ];
    if svg {
        headers.push(("content-disposition", "attachment".to_string()));
    }
    headers
}

fn log(root: &Path, q: &Query) -> Result<Value> {
    let limit = match js_number(q.get("limit")) {
        Some(n) if n != 0.0 => n.min(2000.0),
        _ => 200.0,
    };
    let limit = if limit < 0.0 { usize::MAX } else { limit as usize }; // git takes a negative count as no limit
    let skip = js_number(q.get("skip")).unwrap_or(0.0).max(0.0) as usize;
    let all = q.get("all") == Some("1");
    let branch = match q.get("branch").filter(|b| !b.is_empty()) {
        Some(b) if !all => Some(check_ref(Some(b))?),
        _ => None,
    };
    let list = repo::log(root, branch.as_deref(), all, skip, limit)?;
    Ok(Value::Array(
        list.into_iter()
            .map(|c| json!({ "sha": c.sha, "short": c.short, "author": c.author, "time": c.time, "refs": c.refs, "subject": c.subject, "body": c.body, "parents": c.parents }))
            .collect(),
    ))
}

// A plain folder has no branches or history: `git: false` tells the page to show it as one.
fn folder_info(root: &Path) -> Value {
    json!({
        "git": false,
        "root": root.to_string_lossy(),
        "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    })
}

// `branch` is the checked-out branch, or HEAD's short sha when it is detached (`detached` says which), so there is
// always a name to show. A branch with no commits yet is checked out, not detached.
fn info(root: &Path) -> Result<Value> {
    let head = has_head(root);
    let mut branch = repo::head_branch(root).unwrap_or_default();
    let detached = branch.is_empty() && head;
    if detached {
        branch = repo::head_short(root).unwrap_or_default();
    }
    Ok(json!({
        "git": true,
        "root": root.to_string_lossy(),
        "name": root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "branch": branch,
        "detached": detached,
        "hasHead": head,
        "branches": repo::branches(root)?,
        "defaultBase": default_base(root),
    }))
}

// The one command that changes the repo: `git switch`, and only with a clean tracked tree.
fn switch_branch(root: &Path, name: Option<&str>) -> Result<()> {
    let name = check_ref(name)?;
    if !git(root, &["status", "--porcelain", "--untracked-files=no"])?.trim().is_empty() {
        return Err(http_error(409, "you have uncommitted changes; commit or stash them first"));
    }
    if repo::ref_exists(root, &format!("refs/heads/{name}")) {
        git(root, &["switch", &name])?;
    } else if repo::ref_exists(root, &format!("refs/remotes/{name}")) {
        // Remote-only branch: create a local tracking branch (origin/foo -> foo).
        let local = &name[name.find('/').map(|i| i + 1).unwrap_or(0)..];
        if repo::ref_exists(root, &format!("refs/heads/{local}")) {
            return Err(http_error(409, format!("a local branch \"{local}\" already exists; pick that one instead")));
        }
        git(root, &["switch", "--track", &name])?;
    } else {
        return Err(http_error(400, format!("no branch named {name}")));
    }
    Ok(())
}

// ---------------------------------------------------------------- agent views and notes
// A view is what the URL hash holds. Agents send one to /api/navigate; refs and paths are checked and the refs must
// exist, so a typo fails here instead of in the browser.
fn verify_rev(root: &Path, r: &str) -> Result<String> {
    check_ref(Some(r)).ok().and_then(|r| repo::commit_sha(root, &r)).ok_or_else(|| http_error(400, format!("unknown revision: {r}")))
}

fn view_query(v: &Value) -> Query {
    let mut q = Query::default();
    for k in ["mode", "ref", "base", "head"] {
        if let Some(x) = v.get(k).filter(|x| !x.is_null() && x.as_str() != Some("")) {
            q.set(k, js_string_or_empty(Some(x)));
        }
    }
    q
}

fn line_number(v: Option<&Value>, what: &str) -> Result<Option<i64>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f.fract() == 0.0 && f >= 1.0) => Ok(Some(n.as_f64().unwrap() as i64)),
        _ => Err(http_error(400, format!("{what} must be a positive integer"))),
    }
}

fn check_view(root: &Path, git: bool, v: &Value) -> Result<Value> {
    if !v.is_object() && !v.is_array() {
        return Err(http_error(400, "bad view"));
    }
    if !git {
        // A plain folder's view is a file and lines in it, read or previewed; anything else in it is ignored.
        let mut out = Map::new();
        out.insert("mode".into(), json!("folder"));
        out.insert("view".into(), json!(if str_of(v.get("view")) == Some("preview") { "preview" } else { "file" }));
        check_file_lines(root, v, &mut out)?;
        return Ok(Value::Object(out));
    }
    let mode = if truthy(v.get("mode")) { js_string_or_empty(v.get("mode")) } else { "working".into() };
    if !["working", "commit", "branch"].contains(&mode.as_str()) {
        return Err(http_error(400, format!("unknown mode {mode}")));
    }
    let mut out = Map::new();
    out.insert("mode".into(), json!(mode));
    out.insert("head".into(), json!("HEAD"));
    // Read as parseView reads a link's: whether the file has a preview (markdown, an SVG) is the page's to judge, and
    // without one it falls back as for a link (setFile).
    let view = str_of(v.get("view")).filter(|view| ["file", "preview"].contains(view)).unwrap_or("diff");
    out.insert("view".into(), json!(view));
    out.insert("side".into(), json!(if str_of(v.get("side")) == Some("old") { "old" } else { "new" }));
    if mode == "commit" {
        // full sha, which the commit list matches on
        let r = if truthy(v.get("ref")) { js_string_or_empty(v.get("ref")) } else { "HEAD".into() };
        out.insert("ref".into(), json!(verify_rev(root, &r)?));
    }
    // The checked strings go out, not the raw values: a tab parses what it gets, so `"head": 123` arrives as "123".
    if truthy(v.get("head")) && str_of(v.get("head")) != Some("HEAD") {
        let h = js_string_or_empty(v.get("head"));
        verify_rev(root, &h)?;
        out.insert("head".into(), json!(h));
    }
    if mode == "branch" && truthy(v.get("base")) {
        let b = js_string_or_empty(v.get("base"));
        verify_rev(root, &b)?;
        out.insert("base".into(), json!(b));
    }
    check_file_lines(root, v, &mut out)?;
    Ok(Value::Object(out))
}

// A view's file, line and endLine, checked into `out`.
fn check_file_lines(root: &Path, v: &Value, out: &mut Map<String, Value>) -> Result<()> {
    let file = match v.get("file") {
        None | Some(Value::Null) => None,
        Some(f) => Some(check_path(root, f.as_str())?),
    };
    if let Some(f) = &file {
        out.insert("file".into(), json!(f));
    }
    let line = line_number(v.get("line"), "line")?;
    let end = line_number(v.get("endLine"), "endLine")?;
    out.insert("line".into(), json!(line));
    out.insert("endLine".into(), json!(end));
    if (line.is_some() || end.is_some()) && file.is_none() {
        return Err(http_error(400, "line needs a file"));
    }
    if end.is_some() && line.is_none() {
        return Err(http_error(400, "endLine needs line"));
    }
    if let (Some(l), Some(e)) = (line, end) {
        if e < l {
            return Err(http_error(400, "endLine is before line"));
        }
    }
    Ok(())
}

// Notes belong to a range, so a note on a commit doesn't show up in the working tree view. Commits are pinned to
// their sha; branch comparisons follow the branch names as they move.
// The key a view's notes are filed under, from the view's range as resolve_range resolved it (`r`), so the key, the
// notes listed and their snippets all come from one resolution of its refs (see notes_view). A commit is filed by its
// sha. A branch comparison is filed by the names it goes by: `HEAD` is filed under the checked-out branch's name, so a
// comparison reached as `main...HEAD` (the page resets the head to HEAD after visiting Changes) and as `main...feature`
// keeps the same notes. A detached HEAD stays `HEAD`.
fn range_key(root: &Path, q: &Query, r: &Result<Range>) -> Result<String> {
    let named = |r: String| if r == "HEAD" { repo::head_branch(root).unwrap_or(r) } else { r };
    match q.get_or("mode", "working") {
        "commit" => match r {
            Ok(Range { to: Some(sha), .. }) => Ok(format!("commit:{sha}")),
            _ => Err(http_error(400, format!("unknown revision: {}", q.get_or("ref", "HEAD")))),
        },
        "branch" => {
            let base = match r {
                Ok(Range { base: Some(b), .. }) => b.clone(),
                _ => base_name(root, q)?, // a head that names no commit: its notes are still filed by name
            };
            Ok(format!("branch:{}...{}", named(base), named(check_ref(Some(q.get_or("head", "HEAD")))?)))
        }
        "working" => Ok("working".into()),
        m => Err(http_error(400, format!("unknown mode {m}"))),
    }
}

// A view as its notes see it: its range, for the snippets, and the key its notes are filed under, from one resolution
// of its refs. Each request about notes resolves its view once, here (it used to resolve a commit's ref for the key,
// again for the snippets, and once more for the summary of the other views' notes).
pub struct NotesView {
    range: Result<Range>,
    key: Result<String>,
}
fn notes_view(root: &Path, git: bool, q: &Query) -> NotesView {
    if !git {
        return NotesView { range: Ok(folder_range()), key: Ok(FOLDER_KEY.into()) };
    }
    let range = resolve_range(root, q);
    let key = range_key(root, q, &range);
    NotesView { range, key }
}

// The text of a file on one side of a range, for snippets.
fn side_text(root: &Path, r: &Result<Range>, file: &str, side: &str) -> Result<String> {
    let res = r
        .as_ref()
        .map_err(Error::clone)
        .and_then(|r| read_file(root, r, Some(file), side == "old"))
        .map_err(|_| http_error(400, format!("{file} does not exist on the {side} side of this range")))?;
    match res["content"].as_str() {
        Some(c) => Ok(c.to_string()),
        None => Err(http_error(400, format!("{file} is not a text file"))),
    }
}

// The text of lines [line, endLine] on one side of a range, used to spot notes whose code has moved.
fn snippet(root: &Path, r: &Result<Range>, file: &str, side: &str, line: i64, end: Option<i64>) -> Result<String> {
    snippet_of(&side_text(root, r, file, side)?, file, line, end)
}

fn snippet_of(content: &str, file: &str, line: i64, end: Option<i64>) -> Result<String> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    if line as usize > lines.len() {
        return Err(http_error(400, format!("{file} has only {} lines", lines.len())));
    }
    let stop = (end.unwrap_or(line) as usize).min(lines.len());
    Ok(lines[(line as usize - 1)..stop.max(line as usize - 1)].join("\n"))
}

const MAX_NOTES: usize = 200;
const MAX_NOTE_TEXT: usize = 4000;

// Notes live in memory, or with storage "file" in .git/revue/notes.json (per worktree), or for a plain folder in the
// user's data directory (folder::notes_file), so nothing is written into the folder.
pub struct Notes {
    root: PathBuf,
    git: bool,
    list: Mutex<Vec<Value>>,
    file: Option<PathBuf>,
}

fn random_id() -> String {
    let mut b = [0u8; 4];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).is_ok();
    if !ok {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(discovery::now_ms());
        b = (h.finish() as u32).to_be_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Notes {
    fn new(root: &Path, storage: &str, git: bool) -> Result<Notes> {
        let mut list = Vec::new();
        let mut file = None;
        if storage == "file" {
            let f = if git {
                repo::git_dir(root)?.join("revue").join("notes.json")
            } else {
                let f = folder::notes_file(root).ok_or_else(|| internal("--notes file: no data directory to keep notes in (set HOME or XDG_DATA_HOME)"))?;
                folder::notes_dir(&f).map_err(|e| internal(format!("--notes file: {e}")))?;
                f
            };
            match std::fs::read_to_string(&f) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Array(saved)) => {
                        list = saved.into_iter().filter(|n| truthy(n.get("id")) && truthy(n.get("file")) && truthy(n.get("key"))).collect();
                        if list.len() > MAX_NOTES {
                            list.drain(..list.len() - MAX_NOTES);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("revue: ignoring unreadable {}: {e}", f.display()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("revue: ignoring unreadable {}: {e}", f.display()),
            }
            file = Some(f);
        }
        Ok(Notes { root: root.to_path_buf(), git, list: Mutex::new(list), file })
    }

    pub fn storage(&self) -> &'static str {
        if self.file.is_some() {
            "file"
        } else {
            "memory"
        }
    }

    fn count(&self) -> usize {
        self.list.lock().unwrap().len()
    }

    fn save(&self, list: &[Value]) -> Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = PathBuf::from(format!("{}.{}.tmp", file.display(), std::process::id()));
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b" "));
        serde::Serialize::serialize(&list, &mut ser).map_err(|e| internal(e.to_string()))?;
        std::fs::write(&tmp, buf)?;
        std::fs::rename(&tmp, file)?;
        Ok(())
    }

    // The notes of `view`, or with none, every note.
    fn list(&self, view: Option<&NotesView>) -> Result<Vec<Value>> {
        let all = view.is_none();
        let key = view.map(|v| v.key.clone()).transpose()?;
        let picked: Vec<Value> =
            self.list.lock().unwrap().iter().filter(|n| key.as_deref().is_none_or(|k| n["key"].as_str() == Some(k))).cloned().collect();
        // Stale = the annotated lines changed since the note was added. Only checkable for the current range. The range
        // is resolved once and each file read once per side, however many notes it has.
        let working = view.is_none().then(|| view_range(&self.root, self.git, &Query::parse("mode=working")));
        let range = view.map_or_else(|| working.as_ref().unwrap(), |v| &v.range);
        let mut texts: HashMap<(String, String), Result<String>> = HashMap::new();
        let mut out = Vec::new();
        for mut n in picked {
            let mut stale = Value::Null;
            if !all || n["key"].as_str() == Some(if self.git { "working" } else { FOLDER_KEY }) {
                let (file, side) = (n["file"].as_str().unwrap_or("").to_string(), n["side"].as_str().unwrap_or("new").to_string());
                let text = texts.entry((file.clone(), side.clone())).or_insert_with(|| side_text(&self.root, range, &file, &side));
                let s = text.clone().and_then(|t| snippet_of(&t, &file, n["line"].as_i64().unwrap_or(1), n["endLine"].as_i64()));
                stale = json!(match s {
                    Ok(s) => n["snippet"].as_str() != Some(&s),
                    Err(_) => true,
                });
            }
            n["stale"] = stale;
            out.push(n);
        }
        Ok(out)
    }

    // Notes in views other than `key`, one entry per view: how many, the view itself (as /api/navigate takes it) and its
    // first note in file and line order, so the page can say "N notes in other views" and open them.
    fn others(&self, key: &str) -> Vec<Value> {
        let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
        for n in self.list.lock().unwrap().iter() {
            let k = n["key"].as_str().unwrap_or("").to_string();
            if k == key {
                continue;
            }
            match groups.iter_mut().find(|(g, _)| *g == k) {
                Some((_, v)) => v.push(n.clone()),
                None => groups.push((k, vec![n.clone()])),
            }
        }
        groups
            .into_iter()
            .map(|(k, mut v)| {
                v.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()).then(a["line"].as_i64().cmp(&b["line"].as_i64())));
                let view = if let Some(sha) = k.strip_prefix("commit:") {
                    json!({ "mode": "commit", "ref": sha })
                } else if let Some((base, head)) = k.strip_prefix("branch:").and_then(|r| r.split_once("...")) {
                    json!({ "mode": "branch", "base": base, "head": head })
                } else if k == FOLDER_KEY {
                    json!({ "mode": "folder" })
                } else {
                    json!({ "mode": "working" })
                };
                let first = &v[0];
                json!({ "key": k, "count": v.len(), "view": view,
                    "first": { "file": first["file"], "line": first["line"], "endLine": first["endLine"], "side": first["side"] } })
            })
            .collect()
    }

    fn add(&self, view: &NotesView, body: &Value) -> Result<Value> {
        let file = check_path(&self.root, str_of(body.get("file")))?;
        // A plain folder's files have one side.
        let side = if self.git && str_of(body.get("side")) == Some("old") { "old" } else { "new" };
        let line = line_number(body.get("line"), "line")?;
        let end = line_number(body.get("endLine"), "endLine")?;
        let Some(line) = line else { return Err(http_error(400, "line is required")) };
        if end.is_some_and(|e| e < line) {
            return Err(http_error(400, "endLine is before line"));
        }
        let text = js_string_or_empty(body.get("text")).trim().to_string();
        if text.is_empty() {
            return Err(http_error(400, "text is required"));
        }
        if text.encode_utf16().count() > MAX_NOTE_TEXT {
            return Err(http_error(400, format!("text is longer than {MAX_NOTE_TEXT} characters")));
        }
        if self.count() >= MAX_NOTES {
            return Err(http_error(409, format!("there are already {MAX_NOTES} notes; clear some first")));
        }
        let author = if truthy(body.get("author")) { js_string_or_empty(body.get("author")) } else { "agent".into() };
        let note = json!({
            "id": random_id(),
            "key": view.key.clone()?,
            "file": file,
            "side": side,
            "line": line,
            "endLine": end.filter(|&e| e != line),
            "text": text,
            "author": author.chars().take(60).collect::<String>(),
            "snippet": snippet(&self.root, &view.range, &file, side, line, end)?,
            "created": discovery::now_ms(),
        });
        let mut list = self.list.lock().unwrap();
        list.push(note.clone());
        self.save(&list)?;
        Ok(note)
    }

    // Clear one note by id, every note in the given range, or (all) every note.
    fn clear(&self, q: &Query, id: Option<&str>, all: bool) -> Result<usize> {
        let key = if id.is_none() && !all { Some(notes_view(&self.root, self.git, q).key?) } else { None };
        let mut list = self.list.lock().unwrap();
        let before = list.len();
        if let Some(id) = id {
            list.retain(|n| n["id"].as_str() != Some(id));
        } else if all {
            list.clear();
        } else {
            list.retain(|n| n["key"].as_str() != key.as_deref());
        }
        if list.len() != before {
            self.save(&list)?;
        }
        Ok(before - list.len())
    }
}

// ---------------------------------------------------------------- test hooks
// With REVUE_TEST=1, GET /api/debug reports watched directories, file reads (each file's path and the bytes read of
// it) and whether the watcher has changes it hasn't sent yet (`unsent`), GET /api/debug?watchError=msg fails the
// watcher, ?failSpawns=n makes the next n threads fail to start (git::spawn), ?mark=n sends `: mark n` to every open
// event stream (Server::events in the tests waits on the two), and REVUE_CODE_FILE stands in for the binary in the
// staleness check, so the tests can check internals.
// Debug builds only (what `cargo test` runs): in a release build this is constant false, so the hooks never run and
// /api/debug is an unknown endpoint, whatever the environment says.
fn test_mode() -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var("REVUE_TEST").as_deref() == Ok("1"))
}

// Each file read: its path and how many bytes were read of it.
static READS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

fn debug_read(p: &Path, bytes: usize) {
    if test_mode() {
        READS.lock().unwrap().push((p.to_string_lossy().into_owned(), bytes));
    }
}

// ---------------------------------------------------------------- http
pub struct Options {
    pub port: u16,
    // "memory" (default) or "file" (.git/revue/notes.json, or for a plain folder folder::notes_file)
    pub notes: String,
    // minutes with no open tabs and no requests before on_idle runs (0 = never)
    pub idle_timeout: f64,
    // label recorded in the discovery file: "external" (started by hand), "session" or "detached"
    pub lifetime: String,
    pub on_idle: Option<Box<dyn Fn() + Send + Sync>>,
    // false for a plain folder (not a repo, or --no-git): only the file listing and file reads, no history or diffs
    pub git: bool,
}

// An open tab. Its own thread writes its messages, fed through a short queue, so a tab that stops reading only ever
// blocks that thread (up to the 5 s write timeout), never a broadcast or another tab.
struct Client {
    stream: TcpStream,
    queue: std::sync::mpsc::SyncSender<Arc<Vec<u8>>>,
    nav: bool,
}

// Messages a tab may have waiting before it counts as stuck and is dropped (EventSource reconnects by itself).
const CLIENT_QUEUE: usize = 64;

impl Client {
    fn new(stream: TcpStream, nav: bool) -> std::io::Result<Client> {
        let (queue, rx) = std::sync::mpsc::sync_channel::<Arc<Vec<u8>>>(CLIENT_QUEUE);
        let mut w = stream.try_clone().ok();
        let sock = stream.try_clone().ok();
        spawn(move || {
            for chunk in rx {
                let Some(s) = w.as_mut() else { return };
                if s.write_all(&chunk).and_then(|_| s.flush()).is_err() {
                    // The reader in serve_conn sees the shutdown and removes the tab.
                    if let Some(sock) = &sock {
                        let _ = sock.shutdown(Shutdown::Both);
                    }
                    return;
                }
            }
        })?;
        Ok(Client { stream, queue, nav })
    }
}

struct Shared {
    root: PathBuf,
    // false for a plain folder (see Options::git)
    git: bool,
    port: u16,
    lifetime: String,
    notes: Notes,
    clients: Mutex<HashMap<u64, Client>>,
    conns: Mutex<HashMap<u64, TcpStream>>,
    next_id: AtomicU64,
    last_active: Mutex<Instant>,
    closed: AtomicBool,
    watcher: Mutex<Option<watcher::Watcher>>,
    entry: Mutex<Option<Entry>>,
    // Bumped whenever the repo may have changed (watcher events, a branch switch); see remember().
    generation: AtomicU64,
    remembered: Mutex<HashMap<String, Remembered>>,
    // The generation the index was last refreshed at; see refresh_index().
    refreshed: Mutex<Option<(u64, Instant)>>, // generation and time of the last index refresh
}

pub struct Server {
    shared: Arc<Shared>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.shared.port
    }

    // Stop listening, drop every connection (open tabs included) and the discovery file.
    pub fn close(&self) {
        let s = &self.shared;
        if s.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(w) = s.watcher.lock().unwrap().take() {
            w.close();
        }
        let _ = TcpStream::connect(("127.0.0.1", s.port)); // wake the accept loop
        for (_, c) in s.clients.lock().unwrap().drain() {
            let _ = c.stream.shutdown(Shutdown::Both);
        }
        for (_, c) in s.conns.lock().unwrap().drain() {
            let _ = c.shutdown(Shutdown::Both);
        }
        if let Some(e) = s.entry.lock().unwrap().take() {
            discovery::remove(&e);
        }
    }
}

impl Shared {
    // Server-sent events, fanned out to all open tabs.
    fn send(&self, msg: &str) {
        // Only queues: no socket write happens under the lock (see Client).
        let chunk = Arc::new(http::sse_chunk(msg));
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|_, c| {
            let ok = c.queue.try_send(chunk.clone()).is_ok();
            if !ok {
                let _ = c.stream.shutdown(Shutdown::Both);
            }
            ok
        });
    }

    fn nav_tabs(&self) -> usize {
        self.clients.lock().unwrap().values().filter(|c| c.nav).count()
    }

    fn touch(&self) {
        *self.last_active.lock().unwrap() = Instant::now();
    }
}

// Run f every `period` until the server closes.
fn every(shared: &Arc<Shared>, period: Duration, f: impl Fn(&Shared) + Send + 'static) -> std::io::Result<()> {
    let s = Arc::downgrade(shared);
    spawn(move || {
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(500).min(period));
            let Some(s) = s.upgrade() else { return };
            if s.closed.load(Ordering::SeqCst) {
                return;
            }
            if last.elapsed() >= period {
                last = Instant::now();
                f(&s);
            }
        }
    })?;
    Ok(())
}

pub fn start(root: &Path, opts: Options) -> std::io::Result<Server> {
    loaded_stamp();
    let notes = Notes::new(root, &opts.notes, opts.git).map_err(|e| std::io::Error::other(e.message))?;
    let listener = TcpListener::bind(("127.0.0.1", opts.port))?;
    let port = listener.local_addr()?.port();
    let shared = Arc::new(Shared {
        root: root.to_path_buf(),
        git: opts.git,
        port,
        lifetime: opts.lifetime.clone(),
        notes,
        clients: Mutex::new(HashMap::new()),
        conns: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        last_active: Mutex::new(Instant::now()),
        closed: AtomicBool::new(false),
        watcher: Mutex::new(None),
        entry: Mutex::new(None),
        generation: AtomicU64::new(0),
        remembered: Mutex::new(HashMap::new()),
        refreshed: Mutex::new(None),
    });

    let accept = shared.clone();
    spawn(move || {
        for stream in listener.incoming() {
            if accept.closed.load(Ordering::SeqCst) {
                return;
            }
            let Ok(stream) = stream else {
                // Out of file handles (or similar): accepting again at once would spin, so give them time to free up.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            let s = accept.clone();
            // With no thread to be had, this one connection is closed unanswered (a write could block this loop) and
            // the next gets its own try. Closed rather than answered 503 in any case: an EventSource reconnects after
            // a dropped connection but gives up for good on a 503.
            if let Err(e) = spawn(move || serve_conn(&s, stream)) {
                eprintln!("revue: dropped a connection: could not start a thread for it: {e}");
            }
        }
    })?;

    // Registered as soon as it answers, before the watcher walks the tree (which takes a while in a big repo): the MCP
    // server waits only a few seconds for a detached server to show up here.
    let entry = Entry {
        root: root.to_string_lossy().into_owned(),
        port,
        pid: std::process::id(),
        lifetime: opts.lifetime.clone(),
        notes: shared.notes.storage().into(),
        started: discovery::now_ms(),
        git: opts.git,
    };
    discovery::write(entry.clone());
    *shared.entry.lock().unwrap() = Some(entry);

    // Watchers and timers are only created once listening succeeded, so a failed listen (port in use) leaves nothing behind.
    // A timer that can't get a thread fails the start, and closing leaves nothing behind either.
    let server = Server { shared: shared.clone() };
    let timers = (|| {
        every(&shared, Duration::from_secs(25), |s| s.send(": ping\n\n"))?;
        if opts.idle_timeout > 0.0 {
            let idle = Duration::from_secs_f64(opts.idle_timeout * 60.0);
            let on_idle = opts.on_idle;
            every(&shared, idle.min(Duration::from_secs(30)), move |s| {
                if s.clients.lock().unwrap().is_empty() && s.last_active.lock().unwrap().elapsed() > idle {
                    if let Some(f) = &on_idle {
                        f();
                    }
                }
            })?;
        }
        Ok(())
    })();
    if let Err(e) = timers {
        server.close();
        return Err(e);
    }
    // A plain folder's watcher watches the folders the listing reaches: all of them, or once the listing is cut short,
    // only those it reached then (see watcher::Scope).
    let scope = if opts.git {
        watcher::Scope::Repo
    } else {
        let w = folder::walk(root, folder::max_files());
        watcher::Scope::Folder { only: w.truncated.then(|| w.dirs.into_iter().collect()) }
    };
    let weak = Arc::downgrade(&shared);
    let w = watcher::start(root, scope, move |kind| {
        if let Some(s) = weak.upgrade() {
            // Before the message goes out, so a tab refetching on it never gets a remembered answer.
            s.generation.fetch_add(1, Ordering::SeqCst);
            forget_refs(&s.root);
            s.send(&format!("data: {kind}\n\n"));
        }
    });
    *shared.watcher.lock().unwrap() = Some(w);
    Ok(server)
}

// Open connections at most (tabs included); more get 503, so a flood of local clients can't exhaust threads.
const MAX_CONNS: usize = 256;

// Forgets a connection however its thread ends, a panic in a handler included, so its socket is never leaked.
struct ConnGuard<'a> {
    s: &'a Shared,
    id: u64,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.s.clients.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
        if let Some(c) = self.s.conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

fn serve_conn(s: &Arc<Shared>, stream: TcpStream) {
    let id = s.next_id.fetch_add(1, Ordering::SeqCst);
    {
        let mut conns = s.conns.lock().unwrap();
        if conns.len() >= MAX_CONNS {
            drop(conns);
            let mut w = stream;
            let _ = http::write_response(&mut w, &Response::text(503, "too many connections"), false, false);
            let _ = w.shutdown(Shutdown::Both);
            return;
        }
        if let Ok(c) = stream.try_clone() {
            conns.insert(id, c);
        }
    }
    let _guard = ConnGuard { s, id };
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(http::Conn::new(read_half));
    let mut w = stream;
    loop {
        let req = match http::read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    let status = if e.to_string().contains("too large") { 413 } else { 400 };
                    let _ = http::write_response(&mut w, &Response::text(status, &e.to_string()), false, false);
                }
                break;
            }
        };
        if s.closed.load(Ordering::SeqCst) {
            break;
        }
        let keep = req.keep_alive;
        match handle(s, &req) {
            Reply::Sse { nav } => {
                let Ok(clone) = w.try_clone() else { break };
                let _ = clone.set_write_timeout(Some(Duration::from_secs(5)));
                // Without a writer thread the connection is closed unanswered, which EventSource retries (a 503
                // would make it give up for good).
                let Ok(client) = Client::new(clone, nav) else { break };
                if http::write_sse_head(&mut w).is_err() || w.write_all(&http::sse_chunk(": hi\n\n")).is_err() {
                    break;
                }
                s.clients.lock().unwrap().insert(id, client);
                // The tab is open until its connection closes.
                let _ = reader.get_ref().stream.set_read_timeout(None);
                let mut sink = [0u8; 512];
                while std::io::Read::read(&mut reader, &mut sink).is_ok_and(|n| n > 0) {}
                s.clients.lock().unwrap().remove(&id);
                s.touch();
                break;
            }
            Reply::Res(res) => {
                if http::write_response(&mut w, &res, req.method == "HEAD", keep).is_err() || !keep {
                    break;
                }
            }
        }
    }
}

enum Reply {
    Res(Response),
    Sse { nav: bool },
}

// Answers for /api/tree and /api/info, reused until the repo may have changed. An answer stays valid while the
// version it was computed at is current: `generation` (bumped by every watcher event and by /api/switch) together
// with the watcher's count of changes, which moves as soon as a file changes rather than after the 250 ms debounce,
// so a request right after an edit (an agent's, say) never gets the answer from before it. The watcher can't see
// everything: when it has failed, or in a linked worktree or submodule (where .git is a file and the git directory
// isn't watched, so a commit there touches nothing it watches), answers expire after the ref cache's TTL instead.
// Errors are never remembered.
const MAX_REMEMBERED: usize = 64;

struct Remembered {
    version: (u64, u64),
    at: Instant,
    body: Arc<String>,
}

// The version answers are remembered at, and whether the watcher sees every change.
fn repo_version(s: &Shared) -> ((u64, u64), bool) {
    let w = s.watcher.lock().unwrap();
    let seen = w.as_ref().map_or(0, |w| w.changes_seen());
    let sees_all = w.as_ref().is_some_and(|w| w.sees_all());
    ((s.generation.load(Ordering::SeqCst), seen), sees_all)
}

// The cache key of a view: the endpoint and the range parameters as they arrived (empty counts as missing).
fn view_key(endpoint: &str, q: &Query) -> String {
    let mut key = endpoint.to_string();
    for k in ["mode", "ref", "base", "head"] {
        key.push('\0');
        key.push_str(q.get(k).unwrap_or(""));
    }
    key
}

fn remember(s: &Shared, key: String, compute: impl FnOnce() -> Result<Value>) -> Result<Arc<String>> {
    let (version, sees_all) = repo_version(s);
    if let Some(r) = s.remembered.lock().unwrap().get(&key) {
        if r.version == version && (sees_all || r.at.elapsed() < REF_CACHE) {
            return Ok(r.body.clone());
        }
    }
    let body = Arc::new(compute()?.to_string());
    // An answer computed while the repo changed under it may already be out of date; don't keep it.
    if repo_version(s).0 == version {
        let mut m = s.remembered.lock().unwrap();
        if m.len() >= MAX_REMEMBERED && !m.contains_key(&key) {
            m.clear();
        }
        m.insert(key, Remembered { version, at: Instant::now(), body: body.clone() });
    }
    Ok(body)
}

fn json_ok(v: Value) -> Result<Reply> {
    Ok(Reply::Res(Response::json(200, &v)))
}

fn forbidden() -> Reply {
    Reply::Res(Response::text(403, "forbidden"))
}

fn json_body(req: &Request) -> Result<Value> {
    if req.body.len() > 64 * 1024 {
        return Err(http_error(413, "request body too large"));
    }
    let text = String::from_utf8_lossy(&req.body);
    let text = if text.is_empty() { "{}" } else { &text };
    serde_json::from_str(text).map_err(|_| http_error(400, "bad JSON body"))
}

fn handle(s: &Arc<Shared>, req: &Request) -> Reply {
    // Only answer requests addressed to us by a local name. A rebound DNS name (evil.example -> 127.0.0.1) would
    // otherwise let any website read the repo through this server.
    let host = req.header("host").unwrap_or("");
    if host != format!("127.0.0.1:{}", s.port) && host != format!("localhost:{}", s.port) {
        return forbidden();
    }
    // Any page, on any site, can make the browser send requests here. The browser keeps the answers from it, but the
    // server would still do the work: read files, refresh the index, open an event stream, take the time it takes. So
    // the API answers only requests the browser says came from revue's own page (Sec-Fetch-Site same-origin) or from
    // no page (none: a typed address or a bookmark), and those without the header (curl, revue mcp). Every server on
    // 127.0.0.1 is the same site, so same-site is refused too. The page's own files are served whatever sent the
    // request, so a link to revue opens it from anywhere.
    let ours = |v: &str| v.eq_ignore_ascii_case("same-origin") || v.eq_ignore_ascii_case("none");
    if req.path.starts_with("/api/") && !req.header("sec-fetch-site").is_none_or(ours) {
        return Reply::Res(Response::json(403, &json!({ "error": "cross-site request refused" })));
    }
    s.touch();
    match route(s, req) {
        Ok(r) => r,
        Err(Error { status, message }) => {
            // Bad input carries a 4xx status; anything else is our fault. Log it to stderr (stdout is the protocol
            // channel when running under `revue mcp`). That includes a 503 for a thread the OS refused.
            if status >= 500 {
                eprintln!("revue: {} {}: {}", req.method, req.target, message);
            }
            Reply::Res(Response::json(status, &json!({ "error": message })))
        }
    }
}

fn route(s: &Arc<Shared>, req: &Request) -> Result<Reply> {
    let root = s.root.as_path();
    let q = &req.query;
    // Guard every write: POST plus a custom header, so a cross-site page can't send it without a CORS preflight we
    // never allow. The Host check already applies.
    let allow_write = || req.method == "POST" && req.header("x-revue") == Some("1");
    match req.path.as_str() {
        "/api/info" => {
            let body = remember(s, "info".into(), || if s.git { info(root) } else { Ok(folder_info(root)) })?;
            // `stale` is about this process, not the repo, so it stays live: appended as the last key, exactly where
            // setting it on the object would put it.
            let body = format!("{},\"stale\":{}}}", &body[..body.len() - 1], stale());
            Ok(Reply::Res(Response::new(200, vec![("content-type", "application/json".into())], body)))
        }
        "/api/tree" if !s.git => {
            let body = remember(s, "folder-tree".into(), || Ok(folder_tree(root)))?;
            Ok(Reply::Res(Response::new(200, vec![("content-type", "application/json".into())], body.as_bytes())))
        }
        "/api/diff" | "/api/diffs" | "/api/log" if !s.git => Err(not_a_repo(root)),
        "/api/tree" => {
            // The range is resolved first, and the answer is remembered by the commits it lists as well as by the view:
            // a branch that has moved is another tree at once, even where the watcher can't tell (then answers last 3 s,
            // and History, which looks the branch's latest commit up anew on r, showed its title over the files of the
            // commit before).
            let r = resolve_range(root, q)?;
            let key = format!("{}\0{}\0{}", view_key("tree", q), r.from, r.to.as_deref().unwrap_or(""));
            let body = remember(s, key, || {
                if r.to.is_none() {
                    refresh_index(s);
                }
                tree(root, &r)
            })?;
            Ok(Reply::Res(Response::new(200, vec![("content-type", "application/json".into())], body.as_bytes())))
        }
        "/api/diff" => json_ok(diff(root, &resolve_range(root, q)?, q)?),
        "/api/diffs" => json_ok(json!({ "diffs": diffs(root, &resolve_range(root, q)?, q)? })),
        "/api/file" => json_ok(read_file(root, &view_range(root, s.git, q)?, q.get("path"), s.git && q.get("side") == Some("old"))?),
        "/api/image" => {
            let old_side = s.git && q.get("side") == Some("old");
            Ok(Reply::Res(image_file(root, &view_range(root, s.git, q)?, q.get("path"), old_side, req)?))
        }
        "/api/log" => json_ok(log(root, q)?),
        "/api/def" => {
            let r = view_range(root, s.git, q)?;
            let rev = if s.git && q.get("side") == Some("old") { Some(r.from.as_str()) } else { r.to.as_deref() };
            if rev == Some(EMPTY_TREE) {
                return json_ok(json!({ "jump": null, "candidates": [] }));
            }
            let from = match q.get("from").filter(|f| !f.is_empty()) {
                Some(f) => check_path(root, Some(f))?,
                None => String::new(),
            };
            let num = |k| js_number(q.get(k)).filter(|n| *n != 0.0).map(|n| n as i64).unwrap_or(0);
            // A plain folder's candidates are the files its listing holds.
            let listing = (!s.git).then(|| folder::walk(root, folder::max_files()));
            json_ok(definitions::definitions(root, rev, listing, q.get("name"), &from, num("line"), num("col"))?)
        }
        "/api/server" => {
            // "ok": every change is seen; "partial": answers expire after REF_CACHE instead; "off": no live updates.
            let watcher = match s.watcher.lock().unwrap().as_ref() {
                Some(w) if w.sees_all() => "ok",
                Some(w) if w.running() => "partial",
                _ => "off",
            };
            let started = s.entry.lock().unwrap().as_ref().map(|e| e.started);
            json_ok(json!({
                "root": root.to_string_lossy(), "git": s.git, "pid": std::process::id(), "port": s.port, "lifetime": s.lifetime,
                "notes": s.notes.storage(), "tabs": s.clients.lock().unwrap().len(), "navTabs": s.nav_tabs(), "annotations": s.notes.count(), "stale": stale(),
                "version": env!("CARGO_PKG_VERSION"), "commit": env!("REVUE_COMMIT"), "modified": !env!("REVUE_MODIFIED").is_empty(),
                "started": started, "watcher": watcher,
            }))
        }
        "/api/switch" => {
            if !allow_write() {
                return Ok(forbidden());
            }
            if !s.git {
                return Err(not_a_repo(root));
            }
            let res = switch_branch(root, q.get("branch"));
            s.generation.fetch_add(1, Ordering::SeqCst);
            forget_refs(root);
            res?;
            json_ok(json!({ "ok": true }))
        }
        "/api/navigate" => {
            // Tell open tabs to show a view. The response says how many tabs can follow it, so the caller knows
            // whether to open a browser instead.
            if !allow_write() {
                return Ok(forbidden());
            }
            let view = check_view(root, s.git, &json_body(req)?)?;
            s.send(&format!("event: navigate\ndata: {view}\n\n"));
            json_ok(json!({ "view": view, "tabs": s.nav_tabs() }))
        }
        "/api/annotations" => {
            if req.method == "GET" {
                let view = (q.get("all") != Some("1")).then(|| notes_view(root, s.git, q));
                let mut out = json!({ "storage": s.notes.storage(), "notes": s.notes.list(view.as_ref())? });
                if let Some(view) = &view {
                    let key = view.key.clone()?;
                    out["others"] = json!(s.notes.others(&key));
                    out["key"] = json!(key);
                } else {
                    // Seen from no view, every view's notes are another view's: the page shows this summary where it
                    // has no range of its own (Branch mode with no base to compare with, History with no commits).
                    out["others"] = json!(s.notes.others(""));
                }
                return json_ok(out);
            }
            if !allow_write() {
                return Ok(forbidden());
            }
            let body = json_body(req)?;
            let rq = view_query(&body);
            let mut out = Map::new();
            out.insert("storage".into(), json!(s.notes.storage()));
            match str_of(body.get("action")) {
                Some("add") => {
                    out.insert("note".into(), s.notes.add(&notes_view(root, s.git, &rq), &body)?);
                }
                Some("clear") => {
                    let id = body.get("id").filter(|v| truthy(Some(v))).map(|v| js_string_or_empty(Some(v)));
                    out.insert("removed".into(), json!(s.notes.clear(&rq, id.as_deref(), body.get("all") == Some(&Value::Bool(true)))?));
                }
                _ => return Err(http_error(400, "action must be add or clear")),
            }
            s.send("event: annotations\ndata: {}\n\n");
            json_ok(Value::Object(out))
        }
        "/api/events" => Ok(Reply::Sse { nav: q.get("nav") == Some("1") }),
        "/api/debug" if test_mode() => {
            if let Some(n) = q.get("failSpawns") {
                crate::git::fail_spawns(n.parse().map_err(|_| http_error(400, "failSpawns must be a count"))?);
            }
            if let Some(msg) = q.get("watchError") {
                if let Some(w) = s.watcher.lock().unwrap().as_ref() {
                    w.inject_error(msg);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Lose a directory's watches (?forget=dir), then simulate the kernel dropping events (?overflow=1).
            if q.get("forget").is_some() || q.get("overflow").is_some() {
                if let Some(w) = s.watcher.lock().unwrap().as_ref() {
                    if let Some(dir) = q.get("forget") {
                        w.forget(dir);
                    }
                    if q.get("overflow") == Some("1") {
                        w.inject_overflow();
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let unsent = s.watcher.lock().unwrap().as_ref().is_some_and(|w| w.unsent());
            // After that look, so a stream that gets the mark has got every change message sent before it: with
            // unsent false, that is every message for the changes made so far. Answered without the read log, which
            // it leaves alone.
            if let Some(mark) = q.get("mark") {
                let mark: u64 = mark.parse().map_err(|_| http_error(400, "mark must be a number"))?;
                s.send(&format!(": mark {mark}\n\n"));
                return json_ok(json!({ "unsent": unsent }));
            }
            let reads: Vec<Value> =
                std::mem::take(&mut *READS.lock().unwrap()).into_iter().map(|(path, bytes)| json!({ "path": path, "bytes": bytes })).collect();
            let watched = s.watcher.lock().unwrap().as_ref().map(|w| w.dirs()).unwrap_or_default();
            json_ok(json!({ "watched": watched, "reads": reads, "unsent": unsent }))
        }
        p if p.starts_with("/api/") => Err(http_error(
            404,
            if stale() {
                format!("{p} is unknown to this revue server, which is running older code than is on disk; restart it")
            } else {
                format!("unknown endpoint {p}")
            },
        )),
        p => Ok(Reply::Res(static_file(p, req))),
    }
}

// A revalidation (If-None-Match) of what the browser has: it gets a bodiless 304.
fn etag_matches(req: &Request, etag: &str) -> bool {
    req.header("if-none-match").unwrap_or("").split(',').any(|t| t.trim() == etag || t.trim() == "*")
}

fn static_file(path: &str, req: &Request) -> Response {
    let rel = if path == "/" { "index.html" } else { &path[1..] };
    let Some(&(name, body, etag)) = assets::ASSETS.iter().find(|(n, _, _)| *n == rel) else {
        return Response::text(404, "not found");
    };
    // no-cache still lets the browser keep a copy; it revalidates with the ETag and gets a bodiless 304.
    let mut headers = vec![
        ("cache-control", "no-cache".to_string()),
        ("etag", etag.to_string()),
        ("content-security-policy", CSP.to_string()),
        ("x-content-type-options", "nosniff".to_string()),
    ];
    if etag_matches(req, etag) {
        return Response::new(304, headers, Vec::new());
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    let mime = match ext {
        "html" => "text/html",
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    headers.insert(0, ("content-type", mime.to_string()));
    Response::new(200, headers, body.to_vec())
}

pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let child = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        // Only reaps it; without a thread the opener stays a zombie until we exit.
        let _ = spawn(move || {
            let _ = c.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Request A is still computing when the repo changes: the watcher forgets the refs and request B stores the fresh
    // answer. A's answer, from before the change, must not replace B's.
    #[test]
    fn a_value_computed_across_forget_refs_is_not_stored() {
        let root = Path::new("/nonexistent/revue-ref-cache-test");
        let a = cached(root, |c| c.head, |c, v| c.head = Some(v), || {
            forget_refs(root);
            let b = cached(root, |c| c.head, |c, v| c.head = Some(v), || false);
            assert!(!b);
            true // stale
        });
        assert!(a);
        let now = cached(root, |c| c.head, |c, v| c.head = Some(v), || panic!("B's answer was not kept"));
        assert!(!now, "the stale answer replaced the fresh one");
    }

    // A throwaway repo for the tests below, with git isolated from the user's config and environment as the integration
    // tests' fixtures do it.
    fn scratch_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("revue-range-{tag}-{}-{}", std::process::id(), crate::discovery::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        run_git(&dir, &["init", "-q", "-b", "main"]);
        dir
    }
    fn run_git(root: &Path, args: &[&str]) -> String {
        let mut cmd = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.as_encoded_bytes().starts_with(b"GIT_") {
                cmd.env_remove(&k);
            }
        }
        let out = cmd.envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")])
            .args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(root).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn commit_file(root: &Path, file: &str) -> String {
        std::fs::write(root.join(file), file).unwrap();
        run_git(root, &["add", "-A"]);
        run_git(root, &["commit", "-q", "-m", file]);
        run_git(root, &["rev-parse", "HEAD"])
    }

    // gix decodes an index with extensions on as many threads as index.threads allows (all cores by default), and
    // panics when one can't start; revue reads it on one, whatever the repo says. An index with the end-of-entries
    // extension (which git writes for these settings) still lists the same files.
    #[test]
    fn the_index_is_read_on_one_thread_whatever_the_repo_config_says() {
        let root = scratch_repo("index-threads");
        run_git(&root, &["config", "index.threads", "4"]);
        run_git(&root, &["config", "index.recordEndOfIndexEntries", "true"]);
        let names: Vec<String> = (0..200).map(|i| format!("f{i:03}.txt")).collect();
        for n in &names {
            std::fs::write(root.join(n), n).unwrap();
        }
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "files"]);
        let index = std::fs::read(root.join(".git/index")).unwrap();
        assert!(index.windows(4).any(|w| w == b"EOIE"), "git wrote no EOIE extension");
        assert_eq!(repo::open(&root).unwrap().config_snapshot().integer("index.threads"), Some(1));
        assert_eq!(repo::index_files(&root).unwrap(), names);
        let _ = std::fs::remove_dir_all(&root);
    }

    // A range names its commits by sha, resolved once: whatever reads it later (the file listing, a diff) reads the
    // commits it was resolved to, even when the ref has moved since. Named by the ref instead, the listing resolved it
    // again and could read the next commit under the first one's parent. (The race itself, a commit landing between two
    // reads of one request, can't be timed from outside the request; this pins what makes it impossible.)
    #[test]
    fn a_range_is_resolved_once_to_shas_that_a_moved_ref_does_not_change() {
        let root = scratch_repo("once");
        let first = commit_file(&root, "a.txt");
        let second = commit_file(&root, "b.txt");
        let mut q = Query::default();
        q.set("mode", "commit");
        q.set("ref", "main");
        let r = resolve_range(&root, &q).unwrap();
        assert_eq!((r.from.as_str(), r.to.as_deref()), (first.as_str(), Some(second.as_str())));
        let mut w = Query::default();
        w.set("mode", "working");
        assert_eq!(resolve_range(&root, &w).unwrap().from, second, "working mode compares with HEAD's sha");

        let third = commit_file(&root, "c.txt"); // main moves on
        let files: Vec<String> = repo::tree_files(&root, r.to.as_deref().unwrap()).unwrap().into_iter().map(|(p, _)| p).collect();
        assert_eq!(files, ["a.txt", "b.txt"], "the range still lists the commit it was resolved to");
        assert_eq!(repo::first_parent(&root, r.to.as_deref().unwrap()).as_deref(), Some(r.from.as_str()));
        assert_eq!(resolve_range(&root, &q).unwrap().to.as_deref(), Some(third.as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    // A request about notes resolves its view once (notes_view): the key its notes are filed under, the notes listed and
    // their snippets all come from that one resolution. Resolved before main moves, the view still lists the note left on
    // the commit main named then, with its snippet checked against that commit (not stale); the key, resolved again,
    // would have named the new commit and listed none, while the snippets were read from yet another resolution.
    #[test]
    fn a_notes_view_gives_its_key_notes_and_snippets_from_one_resolution() {
        let root = scratch_repo("notes");
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-q", "-m", "first"]);
        let first = run_git(&root, &["rev-parse", "HEAD"]);
        let notes = Notes::new(&root, "memory", true).unwrap();
        let mut q = Query::default();
        q.set("mode", "commit");
        q.set("ref", "main");
        let note = notes.add(&notes_view(&root, true, &q), &json!({ "file": "a.txt", "line": 2, "text": "about two" })).unwrap();
        assert_eq!(note["key"], format!("commit:{first}"));
        assert_eq!(note["snippet"], "two");

        let view = notes_view(&root, true, &q);
        std::fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
        run_git(&root, &["commit", "-q", "-am", "second"]); // main moves on, and line 2 changes
        let listed = notes.list(Some(&view)).unwrap();
        assert_eq!(listed.len(), 1, "the view lists the notes of the commit it was resolved to");
        assert_eq!((listed[0]["text"].as_str(), listed[0]["stale"].as_bool()), (Some("about two"), Some(false)));
        assert_eq!(view.key.as_deref().unwrap(), format!("commit:{first}"));

        let now = notes_view(&root, true, &q); // main's new commit: no notes of its own
        assert!(notes.list(Some(&now)).unwrap().is_empty());
        assert_ne!(now.key.as_deref().unwrap(), view.key.as_deref().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_int_reads_a_leading_integer_like_js() {
        assert_eq!(js_parse_int(Some("10")), Some(10));
        assert_eq!(js_parse_int(Some("  7abc")), Some(7));
        assert_eq!(js_parse_int(Some("-3")), Some(-3));
        assert_eq!(js_parse_int(Some("+4")), Some(4));
        assert_eq!(js_parse_int(Some("abc")), None);
        assert_eq!(js_parse_int(Some("")), None);
        assert_eq!(js_parse_int(None), None);
    }

    #[test]
    fn number_is_like_unary_plus() {
        assert_eq!(js_number(Some("12")), Some(12.0));
        assert_eq!(js_number(Some(" 1.5 ")), Some(1.5));
        assert_eq!(js_number(Some("")), Some(0.0));
        assert_eq!(js_number(None), Some(0.0));
        assert_eq!(js_number(Some("12px")), None);
        assert_eq!(js_number(Some("inf")), None);
    }

    #[test]
    fn string_or_empty_treats_falsy_values_as_empty() {
        assert_eq!(js_string_or_empty(Some(&json!("x"))), "x");
        assert_eq!(js_string_or_empty(Some(&json!(5))), "5");
        assert_eq!(js_string_or_empty(Some(&json!(0))), "");
        assert_eq!(js_string_or_empty(Some(&json!(false))), "");
        assert_eq!(js_string_or_empty(Some(&Value::Null)), "");
        assert_eq!(js_string_or_empty(None), "");
        assert_eq!(js_string_or_empty(Some(&json!({}))), "[object Object]");
    }

    #[test]
    fn check_ref_allows_ref_syntax_and_refuses_options() {
        for ok in ["main", "HEAD~1", "main^", "HEAD@{0}", "origin/feature-x", "v1.2.3"] {
            assert_eq!(check_ref(Some(ok)).ok().as_deref(), Some(ok), "{ok}");
        }
        for bad in ["-p", "--all", "a b", "a;b", "$(id)", "main:file", "a\\b", ""] {
            assert!(check_ref(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(check_ref(None).unwrap_err().message, "bad ref: null");
    }

    #[test]
    fn header_paths_are_quoted_like_git() {
        assert_eq!(quote_header_path("a/plain name.txt"), "a/plain name.txt");
        assert_eq!(quote_header_path("a/ünï.txt"), "a/ünï.txt"); // core.quotepath=off
        assert_eq!(quote_header_path("a/tab\there"), "\"a/tab\\there\"");
        assert_eq!(quote_header_path("a/q\"uote"), "\"a/q\\\"uote\"");
        assert_eq!(quote_header_path("a/back\\slash"), "\"a/back\\\\slash\"");
        assert_eq!(quote_header_path("a/\x01x"), "\"a/\\001x\"");
    }

    // The image cache keeps at most two generations: an answer asked for again stays, and one nobody asked for while
    // IMAGE_CACHE others were kept is dropped.
    #[test]
    fn the_image_cache_is_bounded_and_keeps_what_is_asked_for() {
        let key = |i: usize| ImageKey::Disk(PathBuf::from(format!("/r/{i}.png")), (i as u64, 0, 0, 0, 0));
        let mut c = ImageCache::default();
        for i in 0..5 * IMAGE_CACHE {
            c.put(key(i), json!(i));
            assert!(c.young.len() + c.old.len() <= 2 * IMAGE_CACHE, "{i}");
            // Answer 0 is asked for every so often, as a preview shown again asks for its images.
            if i % (IMAGE_CACHE / 2) == 0 {
                assert_eq!(c.get(&key(0)), Some(json!(0)), "{i}");
            }
        }
        assert_eq!(c.get(&key(1)), None);
        assert_eq!(c.get(&key(5 * IMAGE_CACHE - 1)), Some(json!(5 * IMAGE_CACHE - 1)));
    }
}
