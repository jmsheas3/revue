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
    self_check();
    let public = Path::new(env!("CARGO_MANIFEST_DIR")).join("public");
    println!("cargo:rerun-if-changed={}", public.display());
    let mut files = Vec::new();
    walk(&public, "", &mut files);
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // Pages with inlined scripts first: the scripts they inline aren't served on their own.
    let mut pages = Vec::new();
    let mut inlined = std::collections::BTreeSet::new();
    for (rel, path) in &files {
        if !rel.ends_with(".html") {
            continue;
        }
        let html = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("public/{rel}: {e}"));
        if let Some((page, hashes, used)) = inline_scripts(&public, rel, &html) {
            let built = out_dir.join("inline").join(rel);
            std::fs::create_dir_all(built.parent().unwrap()).unwrap();
            std::fs::write(&built, page).unwrap();
            pages.push((rel.clone(), built, hashes));
            inlined.extend(used);
        }
    }
    let mut src = String::from("pub static ASSETS: &[(&str, &[u8], &str)] = &[\n");
    for (rel, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        if inlined.contains(rel) {
            continue;
        }
        let path = pages.iter().find(|(r, _, _)| r == rel).map_or(path, |(_, built, _)| built);
        let data = std::fs::read(path).expect("read asset");
        let etag = format!("W/\\\"{}-{:016x}\\\"", data.len(), fnv(&data));
        let abs = path.to_str().unwrap_or_else(|| panic!("{} is not a UTF-8 path", path.display()));
        writeln!(src, "    ({rel:?}, include_bytes!({abs:?}), \"{etag}\"),").unwrap();
    }
    src.push_str("];\n");
    // CSP sources for each page's inlined scripts, as 'sha256-…' 'sha256-…'.
    src.push_str("pub static INLINE_HASHES: &[(&str, &str)] = &[\n");
    for (rel, _, hashes) in &pages {
        writeln!(src, "    ({rel:?}, {hashes:?}),").unwrap();
    }
    src.push_str("];\n");
    std::fs::write(out_dir.join("assets.rs"), src).unwrap();
    commit();
}

// An HTML page may inline a script from public/: each `<script data-inline="vendor/x.js"></script>` becomes a <script>
// holding that file's text. That is for a page served to a frame with an opaque origin (a sandbox), which can't load
// this server's scripts: every response says Cross-Origin-Resource-Policy: same-origin. Returns the page, its scripts'
// CSP hashes and the files inlined, or None for a page that inlines nothing.
const INLINE_TAG: &str = "<script data-inline=\"";

fn inline_scripts(public: &Path, rel: &str, html: &str) -> Option<(String, String, Vec<String>)> {
    if !html.contains(INLINE_TAG) {
        return None;
    }
    let (mut page, mut hashes, mut used) = (String::new(), Vec::new(), Vec::new());
    let mut rest = html;
    while let Some(at) = rest.find(INLINE_TAG) {
        page.push_str(&rest[..at]);
        let after = &rest[at + INLINE_TAG.len()..];
        let end = after.find("\"></script>").unwrap_or_else(|| panic!("public/{rel}: a data-inline tag must be <script data-inline=\"path\"></script>"));
        let name = &after[..end];
        let text = std::fs::read_to_string(public.join(name)).unwrap_or_else(|e| panic!("public/{rel} inlines {name}: {e}"));
        // The browser hashes the script's text as the HTML parser leaves it: it ends at the first </script, and the
        // parser turns CR and CRLF into LF and NUL into U+FFFD, which would no longer match a hash of the file.
        if text.to_ascii_lowercase().contains("</script") || text.contains(['\r', '\0']) {
            panic!("public/{name} can't be inlined in public/{rel}: it holds </script, a CR or a NUL");
        }
        page.push_str("<script>");
        page.push_str(&text);
        page.push_str("</script>");
        hashes.push(format!("'sha256-{}'", base64(&sha256(text.as_bytes()))));
        used.push(name.to_string());
        rest = &after[end + "\"></script>".len()..];
    }
    page.push_str(rest);
    Some((page, hashes.join(" "), used))
}

// SHA-256 (FIPS 180-4), for the CSP hashes above: written out here, since build.rs takes no dependencies either.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

// The hash and the encoding against known answers (FIPS 180-4's examples; RFC 4648's), so a slip in either fails the
// build rather than every inlined script in the browser.
fn self_check() {
    let hex = |d: [u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(hex(sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(hex(sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        hex(sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    for (input, want) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
        assert_eq!(base64(input.as_bytes()), want);
    }
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
