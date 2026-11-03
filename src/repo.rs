// In-process reads of the repository through gitoxide: refs, revisions, trees, the index, blobs and the log.
// Each call takes a thread-local handle from one shared repository per root, so object and ref lookups are fresh
// (gix rereads loose refs, packed-refs and new packs as they change) without spawning git.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use gix::bstr::{BStr, ByteSlice};
use gix::ObjectId;

use crate::git::{http_error, internal, Result, EMPTY_TREE};

fn repos() -> &'static Mutex<HashMap<PathBuf, gix::ThreadSafeRepository>> {
    static REPOS: OnceLock<Mutex<HashMap<PathBuf, gix::ThreadSafeRepository>>> = OnceLock::new();
    REPOS.get_or_init(Default::default)
}

pub fn open(root: &Path) -> Result<gix::Repository> {
    let mut map = repos().lock().unwrap();
    if let Some(r) = map.get(root) {
        return Ok(r.to_thread_local());
    }
    // index.threads=1: gix would otherwise decode an index with extensions on threads it can't start without panicking
    // (`.expect`, see git::spawn), and an index is small enough that one thread costs nothing.
    let opts = gix::open::Options::default().config_overrides(["index.threads=1"]);
    let mut repo = gix::open_opts(root, opts).map_err(|e| internal(format!("cannot open repository {}: {e}", root.display())))?;
    repo.object_cache_size_if_unset(8 * 1024 * 1024);
    let sync = repo.into_sync();
    let local = sync.to_thread_local();
    map.insert(root.to_path_buf(), sync);
    Ok(local)
}

// The top level of the work tree that holds `dir` (like `git rev-parse --show-toplevel`), with symlinks resolved.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let repo = gix::discover(dir).ok()?;
    let wd = repo.workdir()?;
    std::fs::canonicalize(wd).ok()
}

// Where git keeps per-worktree files (like `git rev-parse --git-path`, for paths git doesn't share between worktrees).
pub fn git_dir(root: &Path) -> Result<PathBuf> {
    Ok(open(root)?.git_dir().to_path_buf())
}

fn resolve<'r>(repo: &'r gix::Repository, spec: &str) -> Option<gix::Id<'r>> {
    repo.rev_parse_single(BStr::new(spec)).ok()
}

// Like `git rev-parse --verify -q spec`.
pub fn exists(root: &Path, spec: &str) -> bool {
    open(root).map(|r| resolve(&r, spec).is_some()).unwrap_or(false)
}

pub fn has_head(root: &Path) -> bool {
    open(root).map(|r| r.head_id().is_ok()).unwrap_or(false)
}

// The full sha `spec` names, peeled to a commit (`spec^{commit}`).
pub fn commit_sha(root: &Path, spec: &str) -> Option<String> {
    let repo = open(root).ok()?;
    let obj = resolve(&repo, spec)?.object().ok()?;
    let sha = obj.peel_to_commit().ok()?.id.to_string();
    Some(sha)
}

// The full sha of `spec^1`, or None for a root commit or a bad ref.
// In a shallow clone the boundary commits name parents that were never fetched; those commits count as roots.
pub fn first_parent(root: &Path, spec: &str) -> Option<String> {
    let repo = open(root).ok()?;
    resolve(&repo, &format!("{spec}^1")).filter(|id| repo.has_object(id.detach())).map(|id| id.to_string())
}

pub fn merge_base(root: &Path, a: &str, b: &str) -> Option<String> {
    let repo = open(root).ok()?;
    let one = resolve(&repo, a)?.object().ok()?.peel_to_commit().ok()?.id;
    let two = resolve(&repo, b)?.object().ok()?.peel_to_commit().ok()?.id;
    repo.merge_base(one, two).ok().flatten().map(|id| id.to_string())
}

// Like `git show-ref --verify -q <full name>`.
pub fn ref_exists(root: &Path, full: &str) -> bool {
    let Ok(repo) = open(root) else { return false };
    matches!(repo.try_find_reference(full), Ok(Some(_)))
}

fn tree_of<'r>(repo: &'r gix::Repository, spec: &str) -> Option<gix::Tree<'r>> {
    resolve(repo, spec)?.object().ok()?.peel_to_tree().ok()
}

// Every file (blob, link or submodule) in the tree at `spec`, with its object id, like `git ls-tree -r`.
pub fn tree_files(root: &Path, spec: &str) -> Result<Vec<(String, ObjectId)>> {
    if spec == EMPTY_TREE {
        return Ok(Vec::new());
    }
    let repo = open(root)?;
    let tree = tree_of(&repo, spec).ok_or_else(|| http_error(400, format!("fatal: Not a valid object name {spec}")))?;
    let mut rec = gix::traverse::tree::Recorder::default();
    tree.traverse().breadthfirst(&mut rec).map_err(|e| internal(format!("fatal: cannot read tree {spec}: {e}")))?;
    Ok(rec
        .records
        .into_iter()
        .filter(|e| !e.mode.is_tree())
        .map(|e| (e.filepath.to_str_lossy().into_owned(), e.oid))
        .collect())
}

// Paths in the index, like `git ls-files --cached` (conflicted paths once).
pub fn index_files(root: &Path) -> Result<Vec<String>> {
    let repo = open(root)?;
    let index = repo.index_or_empty().map_err(|e| internal(format!("cannot read the index: {e}")))?;
    let mut out: Vec<String> = Vec::with_capacity(index.entries().len());
    for e in index.entries() {
        let p = e.path(&index).to_str_lossy();
        if out.last().map(|l| l.as_str()) != Some(&*p) {
            out.push(p.into_owned());
        }
    }
    Ok(out)
}

// The blob for `path` in the tree at `spec`. git's own messages for a bad revision or a missing path, so they
// map to 400 and 404 the same way.
pub fn blob_at(root: &Path, spec: &str, path: &str) -> Result<ObjectId> {
    let repo = open(root)?;
    let tree = tree_of(&repo, spec).ok_or_else(|| http_error(400, format!("fatal: Not a valid object name {spec}:{path}")))?;
    let entry = tree.lookup_entry_by_path(path).ok().flatten();
    match entry {
        // A submodule is a commit of another repo, so there is no blob here to show.
        Some(e) if e.mode().is_commit() => Err(http_error(400, format!("{path} is a submodule"))),
        Some(e) if !e.mode().is_tree() => Ok(e.object_id()),
        _ => {
            let on_disk = root.join(path).symlink_metadata().is_ok();
            Err(http_error(
                404,
                if on_disk {
                    format!("fatal: path '{path}' exists on disk, but not in '{spec}'")
                } else {
                    format!("fatal: path '{path}' does not exist in '{spec}'")
                },
            ))
        }
    }
}

pub fn blob_size(root: &Path, id: ObjectId) -> Result<u64> {
    let repo = open(root)?;
    repo.find_header(id).map(|h| h.size()).map_err(|_| internal(format!("fatal: unable to read {id}")))
}

// The commit checked out in the repo at `dir` (a repo nested in the work tree, which git lists untracked as `dir/`):
// what `git add` records for it, as a gitlink. None when it has none yet, or `dir` holds no repo of its own.
pub fn nested_head(dir: &Path) -> Option<ObjectId> {
    if std::fs::symlink_metadata(dir.join(".git")).is_err() {
        return None;
    }
    gix::open(dir).ok()?.head_id().ok().map(|id| id.detach())
}

// The id `git hash-object` gives `data` as a blob, in the repo's own hash.
pub fn blob_id(root: &Path, data: &[u8]) -> Result<ObjectId> {
    let repo = open(root)?;
    gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, data).map_err(|e| internal(e.to_string()))
}

pub fn blob_data(root: &Path, id: ObjectId) -> Result<Vec<u8>> {
    let repo = open(root)?;
    repo.find_object(id).map(|o| o.detach().data).map_err(|_| internal(format!("fatal: unable to read {id}")))
}

// Blobs by id, for go to definition: None for objects that aren't blobs or can't be read.
pub fn read_blobs(root: &Path, ids: &[ObjectId]) -> Vec<Option<Vec<u8>>> {
    let Ok(repo) = open(root) else { return ids.iter().map(|_| None).collect() };
    ids.iter()
        .map(|id| {
            let o = repo.find_object(*id).ok()?;
            (o.kind == gix::object::Kind::Blob).then(|| o.detach().data)
        })
        .collect()
}

// ---------------------------------------------------------------- refs and history
// The checked-out branch's short name (also on an unborn branch), like `git symbolic-ref --short -q HEAD`.
pub fn head_branch(root: &Path) -> Option<String> {
    let repo = open(root).ok()?;
    let name = repo.head_name().ok()??;
    Some(name.shorten().to_str_lossy().into_owned())
}

pub fn head_short(root: &Path) -> Option<String> {
    let repo = open(root).ok()?;
    let id = repo.head_id().ok()?;
    Some(id.shorten_or_id().to_string())
}

// Local and remote branches (short names, sorted by full name), without refs/remotes/*/HEAD.
pub fn branches(root: &Path) -> Result<Vec<String>> {
    let repo = open(root)?;
    let mut names: Vec<String> = Vec::new();
    let refs = repo.references().map_err(|e| internal(e.to_string()))?;
    for r in refs.all().map_err(|e| internal(e.to_string()))?.flatten() {
        let full = r.name().as_bstr().to_str_lossy().into_owned();
        if full.starts_with("refs/heads/") || (full.starts_with("refs/remotes/") && !full.ends_with("/HEAD")) {
            names.push(full);
        }
    }
    names.sort();
    Ok(names
        .into_iter()
        .map(|f| f.strip_prefix("refs/heads/").or_else(|| f.strip_prefix("refs/remotes/")).unwrap_or(&f).to_string())
        .collect())
}

pub struct LogEntry {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub time: i64,
    pub refs: String,
    pub subject: String,
    pub body: String,
    pub parents: Vec<String>,
}

// Every ref with the object it points to after peeling tags. Packed refs carry that id (git writes packed-refs fully
// peeled), so only loose refs, usually a handful of branches, need objects read; repos with thousands of tags stay fast.
fn ref_targets(repo: &gix::Repository) -> Vec<(String, ObjectId)> {
    let loose: std::collections::HashSet<String> = repo
        .refs
        .loose_iter()
        .map(|it| it.flatten().map(|r| r.name.as_bstr().to_str_lossy().into_owned()).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    let Ok(refs) = repo.references() else { return out };
    let Ok(all) = refs.all() else { return out };
    for mut r in all.flatten() {
        let full = r.name().as_bstr().to_str_lossy().into_owned();
        let packed = match (&r.inner.target, r.inner.peeled) {
            _ if loose.contains(&full) => None,
            (gix::refs::Target::Object(id), peeled) => Some(peeled.unwrap_or(*id)),
            (gix::refs::Target::Symbolic(_), _) => None,
        };
        match packed {
            Some(id) => out.push((full, id)),
            None => {
                if let Ok(id) = r.peel_to_id() {
                    out.push((full, id.detach()));
                }
            }
        }
    }
    out
}

// Decorations as `git log --format=%D` shows them: "HEAD -> main, tag: v1, origin/main".
fn decorations(repo: &gix::Repository, refs: &[(String, ObjectId)]) -> HashMap<ObjectId, Vec<String>> {
    // git lists a commit's refs in reverse ref-name order, with the checked-out branch first as "HEAD -> name".
    let mut map: HashMap<ObjectId, Vec<String>> = HashMap::new();
    for (full, id) in refs.iter().rev() {
        let short = if let Some(s) = full.strip_prefix("refs/heads/") {
            s.to_string()
        } else if let Some(s) = full.strip_prefix("refs/remotes/") {
            s.to_string()
        } else if let Some(s) = full.strip_prefix("refs/tags/") {
            format!("tag: {s}")
        } else {
            continue;
        };
        map.entry(*id).or_default().push(short);
    }
    let head_id = repo.head_id().ok().map(|i| i.detach());
    let head_branch = repo.head_name().ok().flatten().map(|n| n.shorten().to_str_lossy().into_owned());
    if let Some(id) = head_id {
        let list = map.entry(id).or_default();
        match head_branch.and_then(|b| list.iter().position(|n| *n == b).map(|i| (b, i))) {
            Some((b, i)) => {
                list.remove(i);
                list.insert(0, format!("HEAD -> {b}"));
            }
            None => list.insert(0, "HEAD".into()),
        }
    }
    map
}

// The subject as `git log --format=%s` shows it: the first paragraph after any blank lines, its lines joined by
// spaces with trailing whitespace dropped (leading whitespace stays), which drops the CR of a CRLF line as git does.
fn subject_of(msg: &str) -> String {
    msg.lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join(" ")
}

// The body as `git log --format=%b` shows it, without the trailing whitespace: what follows the subject paragraph
// and the blank (whitespace-only) lines after it, exactly as the message has it, so CRLF lines keep their CRs.
fn body_of(msg: &str) -> String {
    let blank = |l: &&str| l.trim().is_empty();
    let mut lines = msg.split_inclusive('\n').peekable();
    while lines.next_if(blank).is_some() {}
    while lines.next_if(|l| !blank(l)).is_some() {}
    while lines.next_if(blank).is_some() {}
    lines.collect::<String>().trim_end().to_string()
}

fn commit_time(c: &gix::Commit<'_>) -> i64 {
    c.committer().ok().and_then(|s| s.time.split_whitespace().next().and_then(|t| t.parse().ok())).unwrap_or(0)
}

// Commits newest first by committer time, like `git log --skip --max-count`, from `tip` (a ref) or from every ref
// (`all`: the refs in name order, then HEAD, as git adds them). Commits with the same time come out in the order they
// were queued, as git's do, so a listing matches git's line for line. When the checked-out branch has no commits yet
// (a new repo, or `git switch --orphan`), its history is empty rather than an error (git log fails there), while
// `all` and other branches list as usual.
pub fn log(root: &Path, tip: Option<&str>, all: bool, skip: usize, limit: usize) -> Result<Vec<LogEntry>> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashSet};
    let repo = open(root)?;
    let refs = ref_targets(&repo);
    let mut tips: Vec<ObjectId> = Vec::new();
    if all {
        tips.extend(refs.iter().map(|(_, id)| *id));
        if let Ok(id) = repo.head_id() {
            tips.push(id.detach());
        }
    } else {
        let spec = tip.unwrap_or("HEAD");
        let Some(c) = resolve(&repo, spec).and_then(|id| id.object().ok()).and_then(|o| o.peel_to_commit().ok()) else {
            // The checked-out branch with no commits yet, asked for as HEAD or by its name, has none to list.
            let unborn = |h: gix::Head<'_>| {
                h.is_unborn() && (spec == "HEAD" || h.referent_name().is_some_and(|n| n.shorten() == spec || n.as_bstr() == spec))
            };
            if repo.head().is_ok_and(unborn) {
                return Ok(Vec::new());
            }
            return Err(http_error(400, format!("fatal: bad revision '{spec}'")));
        };
        tips.push(c.id);
    }
    let deco = decorations(&repo, &refs);
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut queue: BinaryHeap<(i64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |id: ObjectId, time: i64, queue: &mut BinaryHeap<(i64, Reverse<u64>, ObjectId)>| {
        queue.push((time, Reverse(seq), id));
        seq += 1;
    };
    for id in tips {
        // Refs can name trees and blobs too; the walk wants commits only.
        if !seen.insert(id) {
            continue;
        }
        if let Ok(c) = repo.find_commit(id) {
            push(id, commit_time(&c), &mut queue);
        }
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    while out.len() < limit {
        let Some((_, _, id)) = queue.pop() else { break };
        let commit = repo.find_commit(id).map_err(|_| internal(format!("fatal: unable to read {id}")))?;
        // A shallow clone's boundary commits name parents it never fetched: the history ends there, as git's does.
        // Only parents that exist go into `seen`, so a seen one needs no lookup and a missing one is looked up (and
        // dropped) each time a commit names it.
        let mut parents: Vec<ObjectId> = Vec::new();
        for p in commit.parent_ids().map(|p| p.detach()) {
            if !seen.contains(&p) {
                if !repo.has_object(p) {
                    continue;
                }
                seen.insert(p);
                let c = repo.find_commit(p).map_err(|_| internal(format!("fatal: unable to read {p}")))?;
                push(p, commit_time(&c), &mut queue);
            }
            parents.push(p);
        }
        if skipped < skip {
            skipped += 1;
            continue;
        }
        let author = commit.author().map_err(|e| internal(e.to_string()))?;
        let message = commit.message_raw().ok().map(|m| m.to_str_lossy().into_owned());
        let time = author.time.split_whitespace().next().and_then(|t| t.parse().ok()).unwrap_or(0);
        out.push(LogEntry {
            sha: id.to_string(),
            short: commit.id().shorten_or_id().to_string(),
            author: author.name.to_str_lossy().into_owned(),
            time,
            refs: deco.get(&id).map(|l| l.join(", ")).unwrap_or_default(),
            subject: message.as_deref().map(subject_of).unwrap_or_default(),
            body: message.as_deref().map(body_of).unwrap_or_default(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
        });
    }
    Ok(out)
}
