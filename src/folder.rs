// Plain folders: a folder outside any git repo, or one opened with --no-git. There is no index or history, so the file
// listing is a walk of the folder itself, and notes are kept in the user's data directory instead of .git/revue.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

// Files listed at most, so a server started in a home folder answers at once instead of walking the whole disk.
const MAX_FILES: usize = 50_000;

// The cap, which debug builds run with REVUE_TEST=1 take from REVUE_TEST_MAX_FILES so tests can reach it.
pub fn max_files() -> usize {
    let test = cfg!(debug_assertions) && std::env::var("REVUE_TEST").as_deref() == Ok("1");
    test.then(|| std::env::var("REVUE_TEST_MAX_FILES").ok()?.parse().ok()).flatten().unwrap_or(MAX_FILES)
}

pub struct Walk {
    // Every file (regular files and symlinks, never followed), relative to the root with `/`, sorted.
    pub files: Vec<String>,
    // Every directory that was read, relative to the root ('' = root): the ones the listing reached.
    pub dirs: Vec<String>,
    // Files were left out because the cap was reached.
    pub truncated: bool,
}

// Lists the folder breadth first, so a cap leaves out the deepest files rather than a whole branch of the tree. `.git`
// folders are listed too, but only once everything else has been: in a repo opened with --no-git, .git/objects would
// otherwise take up the cap before the user's own files deeper down. Directory symlinks are listed as files and never
// followed, so the walk stays inside the folder and can't loop. Names that aren't UTF-8 are left out: the page and the
// API name files by UTF-8 paths, so one could be listed but never opened.
pub fn walk(root: &Path, cap: usize) -> Walk {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut truncated = false;
    let mut queue = VecDeque::from([String::new()]);
    let mut git = VecDeque::new();
    'walk: while let Some(rel) = queue.pop_front().or_else(|| git.pop_front()) {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else { continue };
        dirs.push(rel.clone());
        let mut entries: Vec<(String, std::fs::FileType)> =
            rd.flatten().filter_map(|e| Some((e.file_name().into_string().ok()?, e.file_type().ok()?))).collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, t) in entries {
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if t.is_dir() {
                if name == ".git" {
                    git.push_back(path);
                } else {
                    queue.push_back(path);
                }
            } else if t.is_file() || t.is_symlink() {
                if files.len() >= cap {
                    truncated = true;
                    break 'walk;
                }
                files.push(path);
            }
        }
    }
    files.sort();
    Walk { files, dirs, truncated }
}

// Where `revue --notes file` keeps a folder's notes: <data dir>/revue/notes/<sha1 of the root>.json. Nothing is written
// into the folder itself. None when there is no data directory to use.
pub fn notes_file(root: &Path) -> Option<PathBuf> {
    let name = format!("{}.json", crate::discovery::sha1_hex(root.to_string_lossy().as_bytes()));
    Some(data_dir()?.join("revue").join("notes").join(name))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn data_dir() -> Option<PathBuf> {
    // The XDG spec ignores a relative XDG_DATA_HOME.
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|d| d.is_absolute()) {
        Some(d) => Some(d),
        None => Some(home()?.join(".local").join("share")),
    }
}
#[cfg(target_os = "macos")]
fn data_dir() -> Option<PathBuf> {
    Some(home()?.join("Library").join("Application Support"))
}
#[cfg(windows)]
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).filter(|d| d.is_absolute())
}
#[cfg(not(any(unix, windows)))]
fn data_dir() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|d| d.is_absolute())
}

// The notes directory, created readable only by us if needed, or why it can't be used: like the discovery directory,
// it must be a real directory that we own and nobody else can use.
pub fn notes_dir(file: &Path) -> Result<(), String> {
    let Some(d) = file.parent() else { return Err("no directory".into()) };
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(d).map_err(|e| format!("could not create {}: {e}", d.display()))?;
    match crate::discovery::untrusted(d) {
        None => Ok(()),
        Some(why) => Err(format!("not using {}: {why}", d.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::walk;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("revue-folder-{tag}-{}-{}", std::process::id(), crate::discovery::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(root: &std::path::Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn walks_breadth_first_and_leaves_git_for_last() {
        let d = temp_dir("order");
        for f in [".git/HEAD", ".git/objects/ab/cd", "a.txt", "deep/er/x.txt", "deep/y.txt", ".hidden/z.txt"] {
            touch(&d, f);
        }
        let all = walk(&d, 100);
        assert!(!all.truncated);
        assert_eq!(all.files, [".git/HEAD", ".git/objects/ab/cd", ".hidden/z.txt", "a.txt", "deep/er/x.txt", "deep/y.txt"]);
        // Three files: the top level, then the next level down; .git and the deepest file are left out.
        let three = walk(&d, 3);
        assert!(three.truncated);
        assert_eq!(three.files, [".hidden/z.txt", "a.txt", "deep/y.txt"]);
        // Exactly as many files as the cap: nothing was left out.
        assert!(!walk(&d, 6).truncated);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn directory_links_are_listed_and_not_followed() {
        let d = temp_dir("links");
        touch(&d, "real/a.txt");
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        std::os::unix::fs::symlink("/", d.join("root")).unwrap();
        let w = walk(&d, 100);
        assert_eq!(w.files, ["link", "real/a.txt", "root"]);
        assert_eq!(w.dirs, ["", "real"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
