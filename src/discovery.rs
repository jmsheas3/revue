// Each running server writes {root, port, pid, ...} to a per-user directory so the MCP server (and anything else)
// can find and reuse it instead of starting another one. Entries are removed on exit, so every exit goes through exit().
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub root: String,
    pub port: u16,
    pub pid: u32,
    pub lifetime: String,
    pub notes: String,
    pub started: u64,
    // false for a plain folder (no repo, or --no-git)
    pub git: bool,
}

static PUBLISHED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

// The list survives a thread that panicked while holding it: exit() must still be able to clean up.
fn published() -> MutexGuard<'static, Vec<Entry>> {
    PUBLISHED.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join("revue"),
        None => std::env::temp_dir().join(format!("revue-{}", uid())),
    }
}

#[cfg(unix)]
pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}
// Elsewhere the temp directory is already per user (on Windows it is under the user's profile).
#[cfg(not(unix))]
pub fn uid() -> u32 {
    0
}

// One file per root and kind: a repo's server and a --no-git server on the same folder are two servers.
fn file_name(e: &Entry) -> String {
    let key = if e.git { e.root.clone() } else { format!("folder\0{}", e.root) };
    format!("{}.json", sha1_hex(key.as_bytes()))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// A pid from JSON (a discovery entry, or whatever answers on a port), if it can name one process. On Unix, kill() takes
// 0 as the caller's process group and a negative pid as a group or every process, which is what a pid of 0 or past
// pid_t's range would become.
#[cfg(unix)]
const MAX_PID: u64 = libc::pid_t::MAX as u64;
#[cfg(not(unix))]
const MAX_PID: u64 = u32::MAX as u64;
pub fn valid_pid(pid: u64) -> bool {
    (1..=MAX_PID).contains(&pid)
}
pub fn pid_of(v: &Value) -> Option<u32> {
    v.as_u64().filter(|&p| valid_pid(p)).map(|p| p as u32)
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if !valid_pid(pid as u64) {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
// A process we may not query (another user's) still counts as alive, like EPERM above.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}
#[cfg(not(any(unix, windows)))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

fn to_json(e: &Entry) -> Value {
    json!({ "root": e.root, "port": e.port, "pid": e.pid, "lifetime": e.lifetime, "notes": e.notes, "started": e.started, "git": e.git })
}

// Why `d` can't be trusted with discovery files, or None if it can: it must be a real directory (not a link) that
// we own and nobody else can write to. Without XDG_RUNTIME_DIR the directory is /tmp/revue-<uid>, which another user
// could have created first and filled with symlinks.
#[cfg(unix)]
pub fn untrusted(d: &Path) -> Option<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let m = match std::fs::symlink_metadata(d) {
        Ok(m) => m,
        Err(e) => return Some(e.to_string()),
    };
    if !m.file_type().is_dir() {
        return Some("it is not a directory".into());
    }
    if m.uid() != uid() {
        return Some(format!("it belongs to uid {}", m.uid()));
    }
    let mode = m.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| format!("other users can use it (mode {mode:o})"))
}
#[cfg(not(unix))]
pub fn untrusted(_d: &Path) -> Option<String> {
    None
}

// The discovery directory, created if needed, or None (with one warning) if it isn't safe to use.
fn checked_dir(create: bool) -> Option<PathBuf> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let d = dir();
    if create {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        if let Err(e) = b.create(&d) {
            if !WARNED.swap(true, Ordering::SeqCst) {
                eprintln!("revue: could not create discovery directory {}: {e}", d.display());
            }
            return None;
        }
    } else if std::fs::symlink_metadata(&d).is_err() {
        return None;
    }
    match untrusted(&d) {
        None => Some(d),
        Some(why) => {
            if !WARNED.swap(true, Ordering::SeqCst) {
                eprintln!("revue: not using discovery directory {}: {why}", d.display());
            }
            None
        }
    }
}

// Reads `p` without following a symlink planted there.
fn read_no_follow(p: &Path) -> std::io::Result<String> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut o, libc::O_NOFOLLOW);
    std::io::read_to_string(o.open(p)?)
}

// Writes the entry into `d` through a temporary file that is never opened through a symlink: any stale file or link
// at the temporary name is unlinked first, and the new one is created exclusively, readable only by us.
fn write_in(d: &Path, entry: &Entry) -> std::io::Result<()> {
    use std::io::Write;
    let file = d.join(file_name(entry));
    let tmp = d.join(format!("{}.{}.tmp", file_name(entry), std::process::id()));
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    o.open(&tmp)?.write_all(to_json(entry).to_string().as_bytes())?;
    // rename replaces whatever is at `file`, a symlink included, without following it.
    std::fs::rename(&tmp, &file)
}

pub fn write(entry: Entry) {
    let Some(d) = checked_dir(true) else { return };
    match write_in(&d, &entry) {
        Ok(()) => published().push(entry),
        Err(e) => eprintln!("revue: could not write discovery file: {e}"),
    }
}

// Only remove the file if it still describes this server; another one may have replaced it.
pub fn remove(entry: &Entry) {
    published().retain(|e| e != entry);
    let Some(d) = checked_dir(false) else { return };
    let file = d.join(file_name(entry));
    let Ok(text) = read_no_follow(&file) else { return };
    let Ok(cur) = serde_json::from_str::<Value>(&text) else { return };
    if cur["pid"].as_u64() == Some(entry.pid as u64) && cur["port"].as_u64() == Some(entry.port as u64) {
        let _ = std::fs::remove_file(&file);
    }
}

// Exit the process, removing this process's discovery files first. Several threads may get here at once (a signal and
// the idle timer, say): the first one cleans up and exits, the others wait for that.
pub fn exit(code: i32) -> ! {
    static EXITING: AtomicBool = AtomicBool::new(false);
    if EXITING.swap(true, Ordering::SeqCst) {
        loop {
            std::thread::park();
        }
    }
    let entries: Vec<Entry> = published().clone();
    for e in &entries {
        remove(e);
    }
    std::process::exit(code)
}

// Live servers owned by this user, newest first. Entries for dead processes are cleaned up.
pub fn list_servers() -> Vec<Value> {
    checked_dir(false).map(|d| list_in(&d)).unwrap_or_default()
}

fn list_in(dir: &Path) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Value> = Vec::new();
    for d in rd.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        let is_entry = name.len() == 45 && name.ends_with(".json") && name[..40].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !is_entry {
            continue;
        }
        let path = d.path();
        // Only plain files we own: a symlink could point anywhere, and another user's file could name any pid.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {
                #[cfg(unix)]
                if std::os::unix::fs::MetadataExt::uid(&m) != uid() {
                    continue;
                }
            }
            _ => continue,
        }
        let Ok(text) = read_no_follow(&path) else { continue };
        let Ok(e) = serde_json::from_str::<Value>(&text) else { continue };
        let (Some(_pid), Some(_port), Some(_root)) = (e["pid"].as_u64(), e["port"].as_u64(), e["root"].as_str()) else { continue };
        // A pid that can't name one process (0, or past pid_t) is as good as a dead one.
        if pid_of(&e["pid"]).is_some_and(pid_alive) {
            out.push(e);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    out.sort_by(|a, b| b["started"].as_f64().unwrap_or(0.0).total_cmp(&a["started"].as_f64().unwrap_or(0.0)));
    out
}

// ---------------------------------------------------------------- SHA-1 (only for discovery file names)
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha1_matches_known_digests() {
        assert_eq!(super::sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(super::sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[cfg(unix)]
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("revue-disc-{tag}-{}-{}", std::process::id(), super::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn entry() -> super::Entry {
        super::Entry { root: "/some/repo".into(), port: 1, pid: std::process::id(), lifetime: "external".into(), notes: "memory".into(), started: 0, git: true }
    }

    // kill(0, …) is the caller's process group and kill(-1, …) every process the user owns, so a pid of 0, or one that
    // a cast to pid_t turns negative (or a u32 cast turns into 0), never counts as a live server's.
    #[cfg(unix)]
    #[test]
    fn a_pid_that_cant_name_one_process_is_no_servers() {
        use serde_json::json;
        for bad in [json!(0), json!(1u64 << 31), json!(u32::MAX), json!(1u64 << 32), json!(-1), json!("123"), json!(null)] {
            assert_eq!(super::pid_of(&bad), None, "{bad}");
        }
        assert_eq!(super::pid_of(&json!(std::process::id())), Some(std::process::id()));
        assert!(!super::pid_alive(0) && !super::pid_alive(u32::MAX));
        assert!(super::pid_alive(std::process::id()));

        let d = temp_dir("pids");
        let mut kept = entry();
        super::write_in(&d, &kept).unwrap();
        for (i, pid) in [0u64, 1 << 31, u32::MAX as u64, 1 << 32].into_iter().enumerate() {
            kept.root = format!("/bad/{i}");
            let path = d.join(super::file_name(&kept));
            std::fs::write(&path, json!({ "root": kept.root, "port": 1, "pid": pid }).to_string()).unwrap();
        }
        let listed = super::list_in(&d);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0]["root"], "/some/repo");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "entries with bad pids are removed like dead ones");
        std::fs::remove_dir_all(&d).unwrap();
    }

    // A symlink planted at the temporary name must not redirect the write to the file it points at.
    #[cfg(unix)]
    #[test]
    fn a_link_at_the_temporary_name_is_not_followed() {
        let d = temp_dir("link");
        let victim = d.join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        let e = entry();
        let tmp = d.join(format!("{}.{}.tmp", super::file_name(&e), std::process::id()));
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        super::write_in(&d, &e).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        let written = std::fs::read_to_string(d.join(super::file_name(&e))).unwrap();
        assert!(written.contains("/some/repo"), "{written}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    // A directory other users can write to (as /tmp/revue-<uid> would be if someone else made it first) is refused.
    #[cfg(unix)]
    #[test]
    fn a_directory_others_can_write_to_is_not_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp_dir("mode");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(super::untrusted(&d).unwrap().contains("mode 777"));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(super::untrusted(&d), None);
        let link = d.with_extension("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(super::untrusted(&link).is_some(), "a symlink to a good directory still isn't the directory");
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }
}
