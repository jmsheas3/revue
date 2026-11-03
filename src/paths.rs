// Paths from the browser, kept inside the repo.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::git::{http_error, Result};

// Lexically resolve `p` against `root`: no filesystem access, symlinks are not followed, and `..` pops a component.
pub fn resolve(root: &Path, p: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in root.join(p).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn check_path(root: &Path, p: Option<&str>) -> Result<String> {
    let p = match p {
        Some(p) if !p.is_empty() && !p.contains('\0') => p,
        _ => return Err(http_error(400, "bad path")),
    };
    let abs = resolve(root, p);
    if !abs.starts_with(root) {
        return Err(http_error(400, "path escapes repo"));
    }
    Ok(p.to_string())
}

// check_path only looks at the path as written, but working-tree reads follow symlinks, so a link in the repo could
// point anywhere. Returns the real path of root/p, or None when it resolves outside the real repo root. Errors (like a
// read would) when the path doesn't exist. Committed revisions store links as blobs, so only working-tree reads need this.
pub fn real_in_repo(root: &Path, p: &str) -> std::io::Result<Option<PathBuf>> {
    static REAL_ROOTS: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let top = {
        let mut map = REAL_ROOTS.get_or_init(Default::default).lock().unwrap();
        match map.get(root) {
            Some(t) => t.clone(),
            None => {
                let t = std::fs::canonicalize(root)?;
                map.insert(root.to_path_buf(), t.clone());
                t
            }
        }
    };
    // Like path.join, `..` is resolved before the filesystem sees the path, so x/../a works without an x/.
    let real = std::fs::canonicalize(resolve(root, p))?;
    Ok(real.starts_with(&top).then_some(real))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_is_lexical() {
        assert_eq!(resolve(Path::new("/r"), "a/./b/../c"), PathBuf::from("/r/a/c"));
        assert_eq!(resolve(Path::new("/r"), "../x"), PathBuf::from("/x"));
        assert_eq!(resolve(Path::new("/r"), "/etc/passwd"), PathBuf::from("/etc/passwd"));
    }

    #[test]
    fn check_path_keeps_paths_inside_the_root() {
        let root = Path::new("/r/repo");
        for ok in ["a.txt", "dir/a.txt", "x/../a.txt", "./a.txt"] {
            assert_eq!(check_path(root, Some(ok)).ok().as_deref(), Some(ok), "{ok}");
        }
        for bad in ["../a.txt", "a/../../b", "/etc/passwd", "/r/repo-other/a.txt"] {
            assert_eq!(check_path(root, Some(bad)).unwrap_err().message, "path escapes repo", "{bad}");
        }
        for bad in [Some(""), Some("a\0b"), None] {
            assert_eq!(check_path(root, bad).unwrap_err().message, "bad path", "{bad:?}");
        }
    }
}
