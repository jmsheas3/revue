// Live updates: watch the repo and call on_change (debounced) when anything that could change a view changes.
// A recursive inotify watch would put a watch on every directory, node_modules and .git/objects included, so on
// Linux we watch directory by directory with inotify and skip what we don't need; macOS and Windows watch
// recursively natively (through notify) and we filter events instead.
//
// Paths are relative to the root. In a linked worktree or a submodule .git is a file and the git directory lives
// elsewhere; it is watched too, under the same names a normal repo would use: `.git/HEAD`, `.git/index` and
// `.git/refs/…` for the worktree's own git dir, `.git/@common/…` for the shared one (`git rev-parse
// --git-common-dir`), so changes there classify the same way.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::git::git_allow;

// Folders with these names are skipped only when git ignores them (and they hold no tracked files).
const BUILD_DIRS: &[&str] =
    &["node_modules", "target", "dist", "build", "out", ".venv", "venv", "__pycache__", ".next", ".cache", ".gradle", ".tox", "coverage"];
const GIT_SKIP: &[&str] = &[".git/objects", ".git/logs", ".git/revue", ".git/modules", ".git/worktrees"];
const DEBOUNCE: Duration = Duration::from_millis(250);
// Steady activity (a log file written every 100 ms) keeps pushing the debounce back; fire at least this often anyway.
const MAX_WAIT: Duration = Duration::from_secs(1);
// The shared git dir of a linked worktree, as seen from the root (see the top of the file).
const COMMON: &str = ".git/@common";

fn skip_file(rel: &str) -> bool {
    // The shared git dir's index belongs to the main worktree; it changes whenever that one is diffed.
    (rel.starts_with(".git/") && rel.ends_with(".lock")) || rel.ends_with('~') || rel.ends_with(".swp") || rel == ".git/@common/index"
}

// Changes that can make a folder start or stop being skipped: what git ignores, and what it tracks.
fn changes_ignores(rel: &str) -> bool {
    rel == ".gitignore" || rel.ends_with("/.gitignore") || rel == ".git/info/exclude" || rel == ".git/index"
}

// `rel` is `key` or below it: the rest of the path ("" for the key itself).
#[cfg(target_os = "linux")]
fn under<'a>(rel: &'a str, key: &str) -> Option<&'a str> {
    if rel == key {
        Some("")
    } else {
        rel.strip_prefix(key).and_then(|r| r.strip_prefix('/'))
    }
}

// What a backend reports: a path that changed (relative to the root; `rename` when something appeared or went away),
// a change it can't place, lost events (the kernel's queue overflowed), or a failure. Forget is a test hook.
enum Ev {
    Path { rel: String, rename: bool },
    Unknown { in_git: bool },
    Overflow,
    Forget(String),
    Fail(String),
}

pub struct Watcher {
    inner: Arc<Inner>,
}

// What is watched. A repo skips what git doesn't need (objects, logs, ignored build folders) and tells git changes
// apart. A plain folder (see folder.rs) is all files, .git included, every change is "worktree", and when its listing
// was cut short only the folders it reached are watched (`only`), so a huge folder doesn't use up inotify watches on
// files it never lists.
pub enum Scope {
    Repo,
    Folder { only: Option<HashSet<String>> },
}

struct Inner {
    root: PathBuf,
    // A plain folder: nothing is skipped and nothing counts as a git change (see Scope).
    folder: bool,
    // A plain folder's listing was cut short: only these folders are watched, and changes elsewhere go unseen.
    only: Option<HashSet<String>>,
    // The git dirs outside the root, in a linked worktree or a submodule (None in a normal repo).
    git_dir: Option<PathBuf>,
    common_dir: Option<PathBuf>,
    closed: AtomicBool,
    // Out of inotify watches: only the root and the git dirs are watched, so some changes go unseen.
    degraded: AtomicBool,
    tx: Mutex<Option<Sender<Ev>>>,
    skip_cache: Mutex<HashMap<String, bool>>,
    #[cfg(target_os = "linux")]
    ino: linux::Inotify,
    #[cfg(not(target_os = "linux"))]
    native: Mutex<Option<notify::RecommendedWatcher>>,
    // Changes seen so far, counted as they arrive rather than after the debounce (see changes_seen).
    seen: AtomicU64,
    // Events sent to run() that it hasn't handled yet (an Arc, so notify's callback can count without holding Inner).
    // Counted only in debug builds (COUNT_UNSENT).
    queued: Arc<AtomicUsize>,
    // run() holds changes it hasn't reported: from the first one after a report until on_change has returned. Kept only
    // in debug builds (COUNT_UNSENT).
    held: AtomicBool,
}

// Whether `queued` and `held` are kept, which only `unsent` reads, for the tests (GET /api/debug exists only in debug
// builds): a release build doesn't count every event twice for nothing.
const COUNT_UNSENT: bool = cfg!(debug_assertions);

impl Watcher {
    pub fn close(&self) {
        self.inner.close();
    }

    // Whether every change to the repo reaches us: false once live updates are off (failed or closed), when out of
    // inotify watches, or when the git dir couldn't be found to watch. Callers that rely on change counts (the
    // response cache) fall back to expiring what they keep.
    pub fn sees_all(&self) -> bool {
        let i = &self.inner;
        !i.closed.load(Ordering::SeqCst) && !i.degraded.load(Ordering::SeqCst) && i.sees_git() && i.only.is_none()
    }

    // Whether live updates are on at all: false once the watcher failed or was closed.
    pub fn running(&self) -> bool {
        !self.inner.closed.load(Ordering::SeqCst)
    }

    // A version of "how much has changed", so something remembered can be dropped the moment a file changes
    // instead of 250 ms later, when on_change runs. It moves as soon as the kernel has queued an event (Linux: the
    // reader counts events before it hands them on, and changes still waiting to be read count too), and it never
    // returns to a value it had before a change. Pending first, then the count: the other way round, the reader could
    // take and count an event in between, and a request right after an edit would get the count from before it with
    // nothing pending, which is the version of the answer from before the edit.
    pub fn changes_seen(&self) -> u64 {
        let pending = self.inner.pending();
        #[cfg(target_os = "linux")]
        self.inner.ino.version_gap();
        let seen = self.inner.seen.load(Ordering::SeqCst);
        seen * 2 + u64::from(pending)
    }

    // Whether changes are waiting to be reported, for tests (GET /api/debug): queued in the kernel or being read
    // (Linux), sent on to run() and not handled yet, or held for the debounce until on_change has returned. A change
    // is marked at each stage before the stage before it lets go, and the stages are asked in the order changes move
    // through them, so one can't slip past unseen. False means on_change has been called for every change so far,
    // except on macOS and Windows for one the OS hasn't handed to notify yet, which can't be seen. Debug builds only
    // (COUNT_UNSENT): a release build sees only what the kernel has queued.
    pub fn unsent(&self) -> bool {
        let i = &self.inner;
        !i.closed.load(Ordering::SeqCst) && (i.pending() || i.queued.load(Ordering::SeqCst) > 0 || i.held.load(Ordering::SeqCst))
    }

    // Watched directories relative to the root ('' = root), for tests.
    pub fn dirs(&self) -> Vec<String> {
        self.inner.dirs()
    }

    // Report a failure as if the backend had, for tests.
    pub fn inject_error(&self, msg: &str) {
        self.inject(Ev::Fail(msg.to_string()));
    }

    // Report lost events as if the kernel's queue had overflowed, for tests.
    pub fn inject_overflow(&self) {
        self.inject(Ev::Overflow);
    }

    // Stop watching `rel` and everything under it without the watcher noticing, as lost events would, for tests.
    pub fn forget(&self, rel: &str) {
        self.inject(Ev::Forget(rel.to_string()));
    }

    fn inject(&self, ev: Ev) {
        if let Some(tx) = self.inner.tx.lock().unwrap().as_ref() {
            send_counted(&self.inner.queued, tx, ev);
        }
    }
}

// Hands an event to run(), counted in `queued` until run() has handled it (see unsent).
fn send_counted(queued: &AtomicUsize, tx: &Sender<Ev>, ev: Ev) {
    if COUNT_UNSENT {
        queued.fetch_add(1, Ordering::SeqCst);
    }
    if tx.send(ev).is_err() && COUNT_UNSENT {
        queued.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Inner {
    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.tx.lock().unwrap().take();
        #[cfg(target_os = "linux")]
        self.ino.close();
        #[cfg(not(target_os = "linux"))]
        self.native.lock().unwrap().take();
    }

    fn fail(&self, msg: &str) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        eprintln!("revue: live updates disabled: {msg}");
        self.close();
    }

    // The git dir is watched: it's inside the root, or it was found and watched outside it.
    fn sees_git(&self) -> bool {
        self.folder || self.git_dir.is_some() || self.root.join(".git").is_dir()
    }

    #[cfg(target_os = "linux")]
    fn pending(&self) -> bool {
        self.ino.pending()
    }
    #[cfg(not(target_os = "linux"))]
    fn pending(&self) -> bool {
        false
    }

    // The real path of a watched name (see the top of the file).
    #[cfg(target_os = "linux")]
    fn abs_of(&self, rel: &str) -> PathBuf {
        if let Some(c) = &self.common_dir {
            if let Some(rest) = under(rel, COMMON) {
                return c.join(rest);
            }
        }
        if let Some(g) = &self.git_dir {
            if let Some(rest) = under(rel, ".git") {
                return g.join(rest);
            }
        }
        self.root.join(rel)
    }

    // Outside the root only the git dirs themselves and their refs are watched, not objects, logs or worktrees. A
    // plain folder whose listing was cut short watches only the folders it reached.
    #[cfg(target_os = "linux")]
    fn watchable(&self, rel: &str) -> bool {
        if let Some(only) = &self.only {
            return only.contains(rel);
        }
        let rest = self.common_dir.as_ref().and(under(rel, COMMON)).or_else(|| self.git_dir.as_ref().and(under(rel, ".git")));
        match rest {
            None => true,
            Some(r) => r.is_empty() || r == "refs" || r.starts_with("refs/"),
        }
    }

    #[cfg(target_os = "linux")]
    fn dirs(&self) -> Vec<String> {
        self.ino.dirs()
    }
    #[cfg(not(target_os = "linux"))]
    fn dirs(&self) -> Vec<String> {
        if self.native.lock().unwrap().is_some() {
            vec![String::new()]
        } else {
            Vec::new()
        }
    }

    // Is this directory one we don't watch? Cheap for most paths; asks git (once) for build-like names.
    fn skip_dir(&self, rel: &str) -> bool {
        if self.folder {
            return false; // a plain folder lists everything
        }
        if GIT_SKIP.contains(&rel) {
            return true;
        }
        if rel == ".git" || rel.starts_with(".git/") {
            return false; // a branch can be called "build"; git doesn't ignore its own files
        }
        let base = rel.rsplit('/').next().unwrap_or(rel);
        if !BUILD_DIRS.contains(&base) {
            return false;
        }
        if let Some(&v) = self.skip_cache.lock().unwrap().get(rel) {
            return v;
        }
        let skip = self.ask_skip(rel);
        self.skip_cache.lock().unwrap().insert(rel.to_string(), skip);
        skip
    }

    // check-ignore exits 1 when the folder is not ignored; tracked files inside still count.
    fn ask_skip(&self, rel: &str) -> bool {
        let ignored = git_allow(&self.root, &["check-ignore", "-q", "--", &format!("{rel}/")], &[0]).is_ok();
        ignored && git_allow(&self.root, &["ls-files", "-z", "--", rel], &[0]).map(|s| s.is_empty()).unwrap_or(false)
    }

    // What git ignores or tracks changed: ask again about every build-like folder seen so far, and start or stop
    // watching the ones whose answer changed (`git add -f target/x`, a new .gitignore line).
    fn recheck_skips(self: &Arc<Self>) {
        let known: Vec<(String, bool)> = self.skip_cache.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (rel, was) in known {
            let now = self.ask_skip(&rel);
            self.skip_cache.lock().unwrap().insert(rel.clone(), now);
            if was != now {
                backend_skip_changed(self, &rel, now);
            }
        }
    }

    // Is a changed path a skipped file, or inside (or itself) a skipped directory?
    fn skipped(&self, rel: &str) -> bool {
        if skip_file(rel) {
            return true;
        }
        let mut end = 0;
        loop {
            let next = rel[end..].find('/').map(|i| end + i);
            let prefix = &rel[..next.unwrap_or(rel.len())];
            if self.skip_dir(prefix) {
                return true;
            }
            match next {
                Some(i) => end = i + 1,
                None => return false,
            }
        }
    }
}

// The git dirs of a linked worktree or a submodule, where .git is a file: the worktree's own, and the shared one when
// it differs. None when .git is a directory (a normal repo watches it as part of the tree) or git can't say.
fn outside_git_dirs(root: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    if root.join(".git").is_dir() {
        return (None, None);
    }
    let Ok(out) = git_allow(root, &["rev-parse", "--absolute-git-dir", "--git-common-dir"], &[0]) else { return (None, None) };
    let mut lines = out.lines();
    let real = |p: &str| std::fs::canonicalize(root.join(p)).ok();
    let git_dir = lines.next().and_then(real);
    let common = lines.next().and_then(real).filter(|c| Some(c) != git_dir.as_ref());
    (git_dir, common)
}

// on_change gets "git" when anything under .git/ (refs, HEAD, packed-refs, ...) changed in the debounce window, else
// "worktree", so views of commits only can skip edits to working files. The index counts as the working tree: only
// the working view reads it, and `git diff` rewrites it after files are touched. If the git dir can't be found (so it
// isn't watched), every change is reported as "git", so commit views still refresh.
pub fn start(root: &Path, scope: Scope, on_change: impl Fn(&'static str) + Send + 'static) -> Watcher {
    let (tx, rx) = mpsc::channel();
    let (folder, only) = match scope {
        Scope::Repo => (false, None),
        Scope::Folder { only } => (true, only),
    };
    let (git_dir, common_dir) = if folder { (None, None) } else { outside_git_dirs(root) };
    let inner = Arc::new(Inner {
        root: root.to_path_buf(),
        folder,
        only,
        git_dir,
        common_dir,
        closed: AtomicBool::new(false),
        degraded: AtomicBool::new(false),
        tx: Mutex::new(Some(tx.clone())),
        skip_cache: Mutex::new(HashMap::new()),
        #[cfg(target_os = "linux")]
        ino: linux::Inotify::new(),
        #[cfg(not(target_os = "linux"))]
        native: Mutex::new(None),
        seen: AtomicU64::new(0),
        queued: Arc::new(AtomicUsize::new(0)),
        held: AtomicBool::new(false),
    });
    if let Err(e) = backend_start(&inner, tx) {
        inner.fail(&e);
    }
    let worker = inner.clone();
    if let Err(e) = crate::git::spawn(move || run(worker, rx, on_change)) {
        inner.fail(&format!("could not start a thread: {e}"));
    }
    Watcher { inner }
}

fn run(inner: Arc<Inner>, rx: Receiver<Ev>, on_change: impl Fn(&'static str)) {
    let sees_git = inner.sees_git();
    // The debounce deadline, and when the changes waiting on it started (to cap the wait at MAX_WAIT).
    let mut deadline: Option<Instant> = None;
    let mut first: Option<Instant> = None;
    let mut git_changed = false;
    let mut ignores_changed = false;
    loop {
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        let due = deadline.map(|d| first.map_or(d, |f| d.min(f + MAX_WAIT)));
        let wait = due.map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(Duration::from_millis(500)).min(Duration::from_millis(500));
        let ev = match rx.recv_timeout(wait) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let got = ev.is_some();
        let mut trigger = |in_git: bool| {
            // Linux counts in the reader, before the event even gets here (see changes_seen).
            #[cfg(not(target_os = "linux"))]
            inner.seen.fetch_add(1, Ordering::SeqCst);
            git_changed |= !inner.folder && (in_git || !sees_git);
            let now = Instant::now();
            first.get_or_insert(now);
            deadline = Some(now + DEBOUNCE);
            if COUNT_UNSENT {
                inner.held.store(true, Ordering::SeqCst); // before the event stops counting as queued (see unsent)
            }
        };
        match ev {
            Some(Ev::Fail(msg)) => {
                inner.fail(&msg);
                return;
            }
            Some(Ev::Unknown { in_git }) => trigger(in_git),
            Some(Ev::Overflow) => {
                trigger(true);
                ignores_changed = true;
                backend_rescan(&inner);
            }
            Some(Ev::Forget(rel)) => backend_forget(&inner, &rel),
            Some(Ev::Path { rel, rename }) if !inner.skipped(&rel) => {
                ignores_changed |= changes_ignores(&rel);
                trigger(rel == ".git" || (rel.starts_with(".git/") && rel != ".git/index"));
                if rename {
                    backend_renamed(&inner, &rel);
                }
            }
            Some(Ev::Path { .. }) | None => {}
        }
        if got && COUNT_UNSENT {
            inner.queued.fetch_sub(1, Ordering::SeqCst);
        }
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d || first.is_some_and(|f| now >= f + MAX_WAIT)) {
            deadline = None;
            first = None;
            if std::mem::take(&mut ignores_changed) {
                inner.recheck_skips();
            }
            let kind = if git_changed { "git" } else { "worktree" };
            git_changed = false;
            if !inner.closed.load(Ordering::SeqCst) {
                on_change(kind);
            }
            if COUNT_UNSENT {
                inner.held.store(false, Ordering::SeqCst);
            }
        }
    }
}

// ---------------------------------------------------------------- Linux: inotify, one watch per directory
#[cfg(target_os = "linux")]
fn backend_start(inner: &Arc<Inner>, tx: Sender<Ev>) -> Result<(), String> {
    inner.ino.init()?;
    // The root folder itself, then the git dirs, then the rest: if the tree runs out of watches, the first two are
    // what still catches top-level edits and every git change.
    let mut walk = match inner.ino.watch(&inner.root, "") {
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSPC) | Some(libc::EMFILE)) => Err(e.to_string()),
        _ => Ok(()),
    };
    let mut git_keys = Vec::new();
    if inner.git_dir.is_some() {
        git_keys.push(".git");
    }
    if inner.common_dir.is_some() {
        git_keys.push(COMMON);
    }
    if inner.git_dir.is_none() && inner.root.join(".git").is_dir() {
        git_keys.push(".git");
    }
    for key in git_keys {
        if walk.is_ok() {
            walk = watch_tree(inner, key);
        }
    }
    if walk.is_ok() {
        walk = rescan_tree(inner, ""); // the root is watched already; this descends into it
    }
    if let Err(e) = walk {
        degrade(inner, &e);
    }
    let reader = inner.clone();
    inner.ino.reader_started();
    if let Err(e) = crate::git::spawn(move || linux::read_loop(&reader, &tx)) {
        inner.ino.reader_failed(); // so closing closes the fds itself
        return Err(format!("could not start a thread: {e}"));
    }
    Ok(())
}

// Out of inotify watches (the user's limit is shared by every program): rather than turning live updates off, drop
// the per-folder watches, so other programs get theirs back, and keep the root and the git dirs, which still catch
// top-level edits, commits, branch changes and the index.
#[cfg(target_os = "linux")]
fn degrade(inner: &Arc<Inner>, err: &str) {
    if inner.ino.ino_of("").is_none() {
        inner.fail(err); // not even the root is watched
        return;
    }
    if !inner.degraded.swap(true, Ordering::SeqCst) {
        eprintln!("revue: out of inotify watches ({err}); live updates now only see the top folder and git changes");
    }
    for rel in inner.ino.dirs() {
        if !(rel.is_empty() || rel == ".git" || rel.starts_with(".git/")) {
            inner.ino.unwatch(&rel);
        }
    }
}

// A directory appeared (watch it) or went away, or was replaced (drop its watchers).
#[cfg(target_os = "linux")]
fn backend_renamed(inner: &Arc<Inner>, rel: &str) {
    use std::os::unix::fs::MetadataExt;
    let st = std::fs::symlink_metadata(inner.abs_of(rel)).ok();
    if let Some(ino) = inner.ino.ino_of(rel) {
        if st.as_ref().map(|m| m.ino()) != Some(ino) {
            inner.ino.unwatch(rel);
        }
    }
    if st.is_some_and(|m| m.is_dir()) {
        if let Err(e) = watch_tree(inner, rel) {
            degrade(inner, &e);
        }
    }
}

#[cfg(target_os = "linux")]
fn backend_forget(inner: &Arc<Inner>, rel: &str) {
    inner.ino.unwatch(rel);
}

// A build-like folder started (skip = false) or stopped (skip = true) being something we watch.
#[cfg(target_os = "linux")]
fn backend_skip_changed(inner: &Arc<Inner>, rel: &str, skip: bool) {
    if skip {
        inner.ino.unwatch(rel);
    } else if let Err(e) = watch_tree(inner, rel) {
        degrade(inner, &e);
    }
}

// The kernel dropped events, so directories may have come or gone unseen: drop the watches whose directory is gone or
// was replaced, then walk the whole tree and watch what's missing.
#[cfg(target_os = "linux")]
fn backend_rescan(inner: &Arc<Inner>) {
    use std::os::unix::fs::MetadataExt;
    for rel in inner.ino.dirs() {
        let st = std::fs::symlink_metadata(inner.abs_of(&rel)).ok();
        if inner.ino.ino_of(&rel).is_some_and(|ino| st.as_ref().map(|m| m.ino()) != Some(ino)) {
            inner.ino.unwatch(&rel);
        }
    }
    let mut keys = vec![""];
    if inner.git_dir.is_some() {
        keys.push(".git");
    }
    if inner.common_dir.is_some() {
        keys.push(COMMON);
    }
    for key in keys {
        if let Err(e) = rescan_tree(inner, key) {
            degrade(inner, &e);
            return;
        }
    }
}

// Like watch_tree, but also descends into directories that are already watched.
#[cfg(target_os = "linux")]
fn rescan_tree(inner: &Arc<Inner>, rel: &str) -> Result<(), String> {
    if inner.ino.ino_of(rel).is_none() {
        return watch_tree(inner, rel);
    }
    let Ok(rd) = std::fs::read_dir(inner.abs_of(rel)) else { return Ok(()) };
    for d in rd.flatten() {
        if d.file_type().is_ok_and(|t| t.is_dir()) {
            let name = d.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if inner.watchable(&child) && !inner.skip_dir(&child) {
                rescan_tree(inner, &child)?;
            }
        }
    }
    Ok(())
}

// Watch `rel` and every directory under it that isn't skipped. Errs only when out of watches or file handles.
#[cfg(target_os = "linux")]
fn watch_tree(inner: &Arc<Inner>, rel: &str) -> Result<(), String> {
    if inner.closed.load(Ordering::SeqCst)
        || inner.degraded.load(Ordering::SeqCst)
        || inner.ino.ino_of(rel).is_some()
        || !inner.watchable(rel)
        || (!rel.is_empty() && inner.skip_dir(rel))
    {
        return Ok(());
    }
    let abs = inner.abs_of(rel);
    match inner.ino.watch(&abs, rel) {
        Ok(()) => {}
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSPC) | Some(libc::EMFILE)) => return Err(e.to_string()),
        Err(_) => return Ok(()), // gone already, not a directory any more, or unreadable: skip it
    }
    let Ok(rd) = std::fs::read_dir(&abs) else { return Ok(()) };
    for d in rd.flatten() {
        if d.file_type().is_ok_and(|t| t.is_dir()) {
            let name = d.file_name().to_string_lossy().into_owned();
            watch_tree(inner, &if rel.is_empty() { name } else { format!("{rel}/{name}") })?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{send_counted, skip_file, Ev, Inner, COUNT_UNSENT};
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, RwLock};
    use std::time::Duration;

    // Changes to entries (create, delete, move, modify, attributes), but never opens or reads, so git reading files
    // doesn't wake us. ATTRIB and MODIFY change a file in place; the rest add or remove a name. ONLYDIR and
    // DONT_FOLLOW: if a directory is swapped for a symlink between listing it and watching it, the watch fails
    // rather than following the link out of the repo.
    const MASK: u32 = libc::IN_ATTRIB
        | libc::IN_CREATE
        | libc::IN_MODIFY
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;

    // dir relative to the root -> (watch descriptor, inode), and back
    type Wds = HashMap<i32, String>;
    // Each watched directory's path to its watch descriptor and inode, and the reverse map.
    type Dirs = (HashMap<String, (i32, u64)>, Wds);

    pub struct Inotify {
        // The inotify fd. Only the reader thread closes it, under the write lock, so a watch call (read lock) never
        // uses a number that was closed and reused for a socket in the meantime.
        fd: RwLock<i32>,
        // close() wakes the reader through this pipe instead of closing the fd under it.
        wake: [AtomicI32; 2],
        reader: AtomicBool,
        // Set while the reader holds events it has read but not yet counted (see Watcher::changes_seen).
        busy: AtomicBool,
        dirs: Mutex<Dirs>,
        // Debug builds with REVUE_TEST=1: watches past this many fail with ENOSPC, to test running out of them.
        limit: Option<usize>,
        // Debug builds with REVUE_TEST=1: how long the reader waits before each read, and changes_seen between its two
        // looks, to test the reader taking an event while a request asks for the version.
        reader_delay: Option<Duration>,
        version_gap: Option<Duration>,
    }

    impl Inotify {
        pub fn new() -> Inotify {
            let test = cfg!(debug_assertions) && std::env::var("REVUE_TEST").as_deref() == Ok("1");
            let limit = test.then(|| std::env::var("REVUE_TEST_MAX_WATCHES").ok()?.parse().ok()).flatten();
            let ms = |name| test.then(|| std::env::var(name).ok()?.parse().ok()).flatten().map(Duration::from_millis);
            Inotify {
                fd: RwLock::new(-1),
                wake: [AtomicI32::new(-1), AtomicI32::new(-1)],
                reader: AtomicBool::new(false),
                busy: AtomicBool::new(false),
                dirs: Mutex::new((HashMap::new(), HashMap::new())),
                limit,
                reader_delay: ms("REVUE_TEST_READER_DELAY_MS"),
                version_gap: ms("REVUE_TEST_VERSION_GAP_MS"),
            }
        }

        pub fn init(&self) -> Result<(), String> {
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let mut p = [-1; 2];
            if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
                let e = std::io::Error::last_os_error().to_string();
                unsafe { libc::close(fd) };
                return Err(e);
            }
            *self.fd.write().unwrap() = fd;
            self.wake[0].store(p[0], Ordering::SeqCst);
            self.wake[1].store(p[1], Ordering::SeqCst);
            Ok(())
        }

        pub fn reader_started(&self) {
            self.reader.store(true, Ordering::SeqCst);
        }

        pub fn reader_failed(&self) {
            self.reader.store(false, Ordering::SeqCst);
        }

        pub fn close(&self) {
            if self.reader.load(Ordering::SeqCst) {
                // The reader closes everything when it wakes up.
                let w = self.wake[1].load(Ordering::SeqCst);
                if w >= 0 {
                    unsafe { libc::write(w, b"x".as_ptr() as *const libc::c_void, 1) };
                }
            } else {
                self.shutdown();
            }
            let mut d = self.dirs.lock().unwrap();
            d.0.clear();
            d.1.clear();
        }

        fn shutdown(&self) {
            let mut fd = self.fd.write().unwrap();
            if *fd >= 0 {
                unsafe { libc::close(*fd) };
                *fd = -1;
            }
            for w in &self.wake {
                let p = w.swap(-1, Ordering::SeqCst);
                if p >= 0 {
                    unsafe { libc::close(p) };
                }
            }
        }

        // Events the kernel has queued that nobody has counted yet: still unread, or read and not yet counted. busy is
        // asked again after the queue: an event the reader takes between the first look and FIONREAD is then still
        // being counted (busy) or counted already, which changes_seen reads after this.
        pub fn pending(&self) -> bool {
            if self.busy.load(Ordering::SeqCst) {
                return true;
            }
            let fd = self.fd.read().unwrap();
            let mut n: libc::c_int = 0;
            let queued = *fd >= 0 && unsafe { libc::ioctl(*fd, libc::FIONREAD, &mut n) } == 0 && n > 0;
            queued || self.busy.load(Ordering::SeqCst)
        }

        // The pause between changes_seen's two looks, for tests (see version_gap).
        pub fn version_gap(&self) {
            if let Some(d) = self.version_gap {
                std::thread::sleep(d);
            }
        }

        pub fn dirs(&self) -> Vec<String> {
            let mut v: Vec<String> = self.dirs.lock().unwrap().0.keys().cloned().collect();
            v.sort();
            v
        }

        pub fn ino_of(&self, rel: &str) -> Option<u64> {
            self.dirs.lock().unwrap().0.get(rel).map(|&(_, ino)| ino)
        }

        // Watch the directory at `abs` under the name `rel`.
        pub fn watch(&self, abs: &Path, rel: &str) -> std::io::Result<()> {
            let st = std::fs::symlink_metadata(abs)?;
            if !st.is_dir() {
                return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
            }
            if self.limit.is_some_and(|l| self.dirs.lock().unwrap().0.len() >= l) {
                return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
            }
            let c = CString::new(abs.as_os_str().as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            let fd = self.fd.read().unwrap();
            if *fd < 0 {
                return Err(std::io::Error::from_raw_os_error(libc::EBADF));
            }
            let wd = unsafe { libc::inotify_add_watch(*fd, c.as_ptr(), MASK) };
            if wd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut d = self.dirs.lock().unwrap();
            d.0.insert(rel.to_string(), (wd, st.ino()));
            d.1.insert(wd, rel.to_string());
            Ok(())
        }

        pub fn unwatch(&self, rel: &str) {
            let fd = self.fd.read().unwrap();
            let mut d = self.dirs.lock().unwrap();
            let gone: Vec<String> = d.0.keys().filter(|k| *k == rel || k.starts_with(&format!("{rel}/"))).cloned().collect();
            for k in gone {
                if let Some((wd, _)) = d.0.remove(&k) {
                    d.1.remove(&wd);
                    if *fd >= 0 {
                        unsafe { libc::inotify_rm_watch(*fd, wd) };
                    }
                }
            }
        }

        fn dir_of(&self, wd: i32) -> Option<String> {
            self.dirs.lock().unwrap().1.get(&wd).cloned()
        }

        fn forget_wd(&self, wd: i32) {
            let mut d = self.dirs.lock().unwrap();
            if let Some(rel) = d.1.remove(&wd) {
                if d.0.get(&rel).map(|&(w, _)| w) == Some(wd) {
                    d.0.remove(&rel);
                }
            }
        }
    }

    // The relative names of a batch of raw events, with their kind ("rename" or not); None for an overflow.
    fn parse(ino: &Inotify, buf: &[u8], out: &mut Vec<Option<(String, bool)>>) {
        let head = std::mem::size_of::<libc::inotify_event>();
        let mut off = 0usize;
        while off + head <= buf.len() {
            let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const libc::inotify_event) };
            let name_bytes = &buf[off + head..(off + head + ev.len as usize).min(buf.len())];
            off += head + ev.len as usize;
            let name = String::from_utf8_lossy(name_bytes.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
            if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                out.push(None);
                continue;
            }
            if ev.mask & libc::IN_IGNORED != 0 {
                ino.forget_wd(ev.wd);
                continue;
            }
            let Some(dir) = ino.dir_of(ev.wd) else { continue };
            let rel = match (dir.is_empty(), name.is_empty()) {
                (_, true) => dir.clone(),
                (true, false) => name,
                (false, false) => format!("{dir}/{name}"),
            };
            out.push(Some((rel, ev.mask & (libc::IN_ATTRIB | libc::IN_MODIFY) == 0)));
        }
    }

    pub fn read_loop(inner: &Inner, tx: &Sender<Ev>) {
        let ino = &inner.ino;
        let fd = *ino.fd.read().unwrap();
        let wake = ino.wake[0].load(Ordering::SeqCst);
        let mut buf = vec![0u8; 64 * 1024];
        let mut events = Vec::new();
        loop {
            if inner.closed.load(Ordering::SeqCst) {
                break;
            }
            let mut pfd = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 }];
            let r = unsafe { libc::poll(pfd.as_mut_ptr(), 2, 500) };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                send_counted(&inner.queued, tx, Ev::Fail(e.to_string()));
                break;
            }
            if pfd[1].revents != 0 {
                break; // close() asked us to stop
            }
            if r == 0 {
                continue;
            }
            // Busy before the read, cleared after counting: in between, the events are neither queued (so the
            // kernel no longer reports them) nor counted.
            if let Some(d) = ino.reader_delay {
                std::thread::sleep(d);
            }
            ino.busy.store(true, Ordering::SeqCst);
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                ino.busy.store(false, Ordering::SeqCst);
                let e = std::io::Error::last_os_error();
                if matches!(e.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EINTR)) {
                    continue;
                }
                send_counted(&inner.queued, tx, Ev::Fail(e.to_string()));
                break;
            }
            events.clear();
            parse(ino, &buf[..n as usize], &mut events);
            // Count before handing on, so a request right after a write never gets an answer from before it. Lock
            // files and editor temp files don't count: git takes index.lock even when it changes nothing.
            if events.iter().any(|e| e.as_ref().is_none_or(|(rel, _)| rel.is_empty() || !skip_file(rel))) {
                inner.seen.fetch_add(1, Ordering::SeqCst);
            }
            // Queued for run() before they stop being busy (see Watcher::unsent).
            if COUNT_UNSENT {
                inner.queued.fetch_add(events.len(), Ordering::SeqCst);
            }
            ino.busy.store(false, Ordering::SeqCst);
            for e in events.drain(..) {
                let ev = match e {
                    None => Ev::Overflow,
                    Some((rel, _)) if rel.is_empty() => Ev::Unknown { in_git: true },
                    Some((rel, rename)) => Ev::Path { rel, rename },
                };
                if tx.send(ev).is_err() {
                    ino.shutdown();
                    return;
                }
            }
        }
        ino.shutdown();
    }
}

// ---------------------------------------------------------------- macOS and Windows: one recursive watch
#[cfg(not(target_os = "linux"))]
fn backend_start(inner: &Arc<Inner>, tx: Sender<Ev>) -> Result<(), String> {
    use notify::{RecursiveMode, Watcher as _};
    // Each watched folder and the name its paths get (see the top of the file).
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut bases: Vec<(PathBuf, String)> = Vec::new();
    if let Some(c) = &inner.common_dir {
        bases.push((real(c), COMMON.to_string()));
    }
    if let Some(g) = &inner.git_dir {
        bases.push((real(g), ".git".to_string()));
    }
    bases.push((inner.root.clone(), String::new()));
    bases.push((real(&inner.root), String::new()));
    let map = bases.clone();
    // Counted until run() has handled each one (see Watcher::unsent).
    let queued = inner.queued.clone();
    let send = move |ev| send_counted(&queued, &tx, ev);
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
        Err(e) => send(Ev::Fail(e.to_string())),
        Ok(ev) => {
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            if ev.paths.is_empty() {
                send(Ev::Unknown { in_git: true });
            }
            for p in ev.paths {
                let rel = map.iter().find_map(|(base, key)| {
                    let r = p.strip_prefix(base).ok()?.to_string_lossy().replace('\\', "/");
                    Some(match (key.is_empty(), r.is_empty()) {
                        (true, _) => r,
                        (false, true) => key.clone(),
                        (false, false) => format!("{key}/{r}"),
                    })
                });
                match rel {
                    Some(rel) if !rel.is_empty() => send(Ev::Path { rel, rename: false }),
                    _ => send(Ev::Unknown { in_git: true }),
                }
            }
        }
    })
    .map_err(|e| e.to_string())?;
    w.watch(&inner.root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    // Outside the root: each git dir itself (HEAD, index, packed-refs) and its refs.
    for (base, _) in bases.iter().filter(|(_, key)| !key.is_empty()) {
        let _ = w.watch(base, RecursiveMode::NonRecursive);
        let refs = base.join("refs");
        if refs.is_dir() {
            let _ = w.watch(&refs, RecursiveMode::Recursive);
        }
    }
    *inner.native.lock().unwrap() = Some(w);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn backend_renamed(_inner: &Arc<Inner>, _rel: &str) {}

#[cfg(not(target_os = "linux"))]
fn backend_forget(_inner: &Arc<Inner>, _rel: &str) {}

// One recursive watch covers everything; skipping only filters events, which the refreshed cache already does.
#[cfg(not(target_os = "linux"))]
fn backend_skip_changed(_inner: &Arc<Inner>, _rel: &str, _skip: bool) {}

// One recursive watch covers everything; there is nothing to rescan.
#[cfg(not(target_os = "linux"))]
fn backend_rescan(_inner: &Arc<Inner>) {}
