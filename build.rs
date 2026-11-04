// Embeds public/ in the binary: writes $OUT_DIR/assets.rs with each file's bytes and an ETag from its size and a
// content hash, so a copied binary needs nothing else. Cargo reruns this when anything under public/ changes.
// Dotfiles (editor swap files, .DS_Store) are left out, and a name that isn't UTF-8 stops the build: it couldn't be
// requested by URL or named in include_bytes! faithfully.
use std::fmt::Write as _;
use std::path::Path;

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, std::path::PathBuf)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read public/").flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(name) = e.file_name().to_str().map(String::from) else {
            panic!("public/: {} is not a UTF-8 name; rename it", e.path().display());
        };
        if name.starts_with('.') {
            continue;
        }
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let p = e.path();
        if p.is_dir() {
            walk(&p, &r, out);
        } else {
            out.push((r, p));
        }
    }
}

// FNV-1a, 64 bit: enough to tell versions of a file apart.
fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn main() {
    let public = Path::new(env!("CARGO_MANIFEST_DIR")).join("public");
    println!("cargo:rerun-if-changed={}", public.display());
    let mut files = Vec::new();
    walk(&public, "", &mut files);
    let mut src = String::from("pub static ASSETS: &[(&str, &[u8], &str)] = &[\n");
    for (rel, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let data = std::fs::read(path).expect("read asset");
        let etag = format!("W/\\\"{}-{:016x}\\\"", data.len(), fnv(&data));
        let abs = path.to_str().unwrap_or_else(|| panic!("{} is not a UTF-8 path", path.display()));
        writeln!(src, "    ({rel:?}, include_bytes!({abs:?}), \"{etag}\"),").unwrap();
    }
    src.push_str("];\n");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("assets.rs"), src).unwrap();
    commit();
}

// The commit the binary was built from, for /api/server: REVUE_COMMIT (short hash, empty outside a git checkout of
// revue or without git) and REVUE_MODIFIED ("1" when the files that go into the binary differ from that commit).
// Reruns when HEAD moves (HEAD, refs/heads, packed-refs) or one of those files changes.
const BUILD_INPUTS: &[&str] = &["src", "public", "build.rs", "Cargo.toml", "Cargo.lock"];

fn commit() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    for f in BUILD_INPUTS {
        println!("cargo:rerun-if-changed={}", dir.join(f).display());
    }
    // Only a checkout of revue itself: sources unpacked inside some other repo (a home folder kept in git, a project
    // that vendors them) would otherwise report that repo's commit, and as modified.
    let own = git(&["rev-parse", "--show-toplevel"])
        .and_then(|top| std::fs::canonicalize(top).ok())
        .is_some_and(|top| std::fs::canonicalize(dir).is_ok_and(|d| d == top));
    let hash = if own { git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default() } else { String::new() };
    if own {
        // Watched before the first commit too, when there is no hash yet: otherwise a build made then would report no
        // commit until a build input changed. A path that doesn't exist would make Cargo rerun this on every build, so
        // only existing ones are named; refs/heads exists from `git init` on, and a first commit adds its branch there.
        for p in ["HEAD", "refs/heads", "packed-refs"] {
            if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", p]).filter(|p| Path::new(p).exists()) {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    let mut modified = false;
    if !hash.is_empty() {
        let mut args = vec!["status", "--porcelain", "--"];
        args.extend(BUILD_INPUTS);
        modified = git(&args).is_some_and(|s| !s.is_empty());
    }
    println!("cargo:rustc-env=REVUE_COMMIT={hash}");
    println!("cargo:rustc-env=REVUE_MODIFIED={}", if modified { "1" } else { "" });
}
