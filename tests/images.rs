// Images: what /api/file says about one, and /api/image, which serves it. The server never decodes an image, it only
// reads its header, so each file here is a real header followed by filler. An image is told by its bytes, never its
// name, and refused when its size can't be read or it would take over 50 million pixels (its header, or a GIF's frames
// or an ICO's icons, say how many) or it is over 20 MB, so a browser is never handed one that would take gigabytes.
mod common;
use common::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------- image files

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut d = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    d.extend(w.to_be_bytes());
    d.extend(h.to_be_bytes());
    d.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]); // bit depth, color type, compression, filter, interlace, CRC
    d.extend(b"\0\0\0\0IEND\xaeB`\x82");
    d
}

// A JPEG whose frame header comes after `meta` bytes of metadata, in segments of at most 65,533 bytes, each starting
// with a frame header of its own (an EXIF thumbnail's), which must not be taken for the image's.
fn jpeg(w: u16, h: u16, meta: usize) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut left = meta;
    while left > 0 {
        let n = left.clamp(9, 65533);
        d.extend([0xFF, 0xE1]);
        d.extend(((n + 2) as u16).to_be_bytes());
        let mut body = vec![0u8; n];
        body[..9].copy_from_slice(&[0xFF, 0xC0, 0, 17, 8, 0, 1, 0, 1]);
        d.extend(body);
        left = left.saturating_sub(n);
    }
    d.extend([0xFF, 0xFF]); // fill bytes before the marker
    d.extend([0xFF, 0xC0, 0, 17, 8]);
    d.extend(h.to_be_bytes());
    d.extend(w.to_be_bytes());
    d.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    d.extend([0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
    d
}

// The TIFF structure an EXIF block holds, in either byte order, whose first directory has `entries`: (tag, type, count,
// value), the value a SHORT in the first 2 of its 4 bytes.
fn tiff(big: bool, entries: &[(u16, u16, u32, u16)]) -> Vec<u8> {
    let u16b = |v: u16| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let u32b = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut d = if big { b"MM".to_vec() } else { b"II".to_vec() };
    d.extend(u16b(42));
    d.extend(u32b(8)); // the directory's offset
    d.extend(u16b(entries.len() as u16));
    for &(tag, kind, count, value) in entries {
        d.extend(u16b(tag));
        d.extend(u16b(kind));
        d.extend(u32b(count));
        d.extend(u16b(value));
        d.extend([0, 0]);
    }
    d.extend([0; 4]); // no next directory
    d
}
// An orientation (tag 0x112, one SHORT), after the camera's make, as cameras write them.
fn orientation(big: bool, o: u16) -> Vec<u8> {
    tiff(big, &[(0x10F, 2, 4, 0), (0x112, 3, 1, o)])
}
// A little-endian TIFF structure whose directories follow one another, each written as tiff() writes its one (in
// little endian a LONG under 65,536, such as an offset, is written as a SHORT is), with its entries in the order given.
// The first is the first directory (IFD0); dir_at says where the others are, for an entry to point to.
fn tiff_dirs(dirs: &[&[(u16, u16, u32, u16)]]) -> Vec<u8> {
    let mut d = tiff(false, dirs[0]);
    for dir in &dirs[1..] {
        d.extend(&tiff(false, dir)[8..]);
    }
    d
}
// Where tiff_dirs writes the directory after ones of `entries` entries each.
fn dir_at(entries: &[usize]) -> u16 {
    (8 + entries.iter().map(|n| 2 + 12 * n + 4).sum::<usize>()) as u16
}

// A JPEG with an EXIF block (an APP1 segment: "Exif\0\0", then the TIFF structure) before its frame header.
fn jpeg_exif(w: u16, h: u16, tiff: &[u8]) -> Vec<u8> {
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend(((2 + 6 + tiff.len()) as u16).to_be_bytes());
    app1.extend(b"Exif\0\0");
    app1.extend(tiff);
    let mut d = jpeg(w, h, 0);
    d.splice(2..2, app1);
    d
}

// A PNG with an eXIf chunk (the TIFF structure itself) before its image data (IDAT), or after it, where browsers don't
// read it.
fn png_exif(w: u32, h: u32, tiff: &[u8], after_data: bool) -> Vec<u8> {
    let chunk = |kind: &[u8], data: &[u8]| [&(data.len() as u32).to_be_bytes()[..], kind, data, &[0; 4]].concat();
    let (exif, idat) = (chunk(b"eXIf", tiff), chunk(b"IDAT", &[]));
    let mut d = png(w, h);
    let iend = d.len() - 12;
    d.splice(iend..iend, if after_data { [idat, exif].concat() } else { [exif, idat].concat() });
    d
}

// A TIFF value: a SHORT, a LONG, or a RATIONAL (numerator, denominator), which doesn't fit in an entry, so it is
// written after the directories and its entry holds its offset.
#[derive(Clone, Copy)]
enum Val {
    Short(u16),
    Long(u32),
    Ratio(u32, u32),
}

// The TIFF structure an EXIF block holds, in either byte order, whose first directory has `ifd0`'s entries (tag,
// value) and, when `sub` has any, points (tag 0x8769) to an Exif directory with those. Each directory is sorted by tag,
// as TIFF has it, keeping the order of entries for the same tag.
fn exif_tiff(big: bool, ifd0: &[(u16, Val)], sub: &[(u16, Val)]) -> Vec<u8> {
    let u16b = |v: u16| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let u32b = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let size = |n: usize| 2 + 12 * n as u32 + 4;
    let mut dirs = vec![ifd0.to_vec()];
    if !sub.is_empty() {
        dirs[0].push((0x8769, Val::Long(8 + size(ifd0.len() + 1))));
        dirs.push(sub.to_vec());
    }
    let mut ratio_at = 8 + dirs.iter().map(|dir| size(dir.len())).sum::<u32>();
    let mut d = if big { b"MM".to_vec() } else { b"II".to_vec() };
    d.extend(u16b(42));
    d.extend(u32b(8)); // the first directory's offset
    let mut ratios = vec![];
    for dir in &mut dirs {
        dir.sort_by_key(|&(tag, _)| tag);
        d.extend(u16b(dir.len() as u16));
        for &(tag, val) in dir.iter() {
            let (kind, value) = match val {
                Val::Short(v) => (3, [u16b(v), [0, 0]].concat()),
                Val::Long(v) => (4, u32b(v).to_vec()),
                Val::Ratio(n, den) => {
                    ratios.extend([u32b(n), u32b(den)].concat());
                    ratio_at += 8;
                    (5, u32b(ratio_at - 8).to_vec())
                }
            };
            d.extend(u16b(tag));
            d.extend(u16b(kind));
            d.extend(u32b(1));
            d.extend(value);
        }
        d.extend([0; 4]); // no next directory
    }
    d.extend(ratios);
    d
}
// A resolution in dots per inch, across and down: ResolutionUnit 2 (inches), XResolution and YResolution.
fn dpi(x: u32, y: u32) -> Vec<(u16, Val)> {
    vec![(0x128, Val::Short(2)), (0x11A, Val::Ratio(x, 1)), (0x11B, Val::Ratio(y, 1))]
}
// 1/n dots per inch.
fn tiny_dpi(n: u32) -> Vec<(u16, Val)> {
    vec![(0x128, Val::Short(2)), (0x11A, Val::Ratio(1, n)), (0x11B, Val::Ratio(1, n))]
}
// PixelXDimension and PixelYDimension, as SHORTs or as LONGs.
fn dims(w: u16, h: u16) -> Vec<(u16, Val)> {
    vec![(0xA002, Val::Short(w)), (0xA003, Val::Short(h))]
}
fn long_dims(w: u32, h: u32) -> Vec<(u16, Val)> {
    vec![(0xA002, Val::Long(w)), (0xA003, Val::Long(h))]
}
// A resolution in the first directory and pixel dimensions in the Exif directory, as cameras write them.
fn resolution(x: u32, y: u32, (w, h): (u16, u16)) -> Vec<u8> {
    exif_tiff(false, &dpi(x, y), &dims(w, h))
}

// A GIF of a screen size, with frames at (left, top, width, height).
fn gif(screen: (u16, u16), frames: &[(u16, u16, u16, u16)]) -> Vec<u8> {
    gif_with_comment(screen, 0, frames)
}

// The same, with a comment of at least `comment` bytes before the frames.
fn gif_with_comment(screen: (u16, u16), comment: usize, frames: &[(u16, u16, u16, u16)]) -> Vec<u8> {
    let mut d = b"GIF89a".to_vec();
    d.extend(screen.0.to_le_bytes());
    d.extend(screen.1.to_le_bytes());
    d.extend([0x80, 0, 0, 0, 0, 0, 255, 255, 255]); // a global color table of 2 colors
    d.extend([0x21, 0xF9, 4, 0, 0, 0, 0, 0]); // a graphic control extension
    if comment > 0 {
        d.extend([0x21, 0xFE]);
        for _ in 0..comment.div_ceil(255) {
            d.push(255);
            d.extend([b'c'; 255]);
        }
        d.push(0);
    }
    for &(l, t, w, h) in frames {
        d.push(0x2C);
        for v in [l, t, w, h] {
            d.extend(v.to_le_bytes());
        }
        d.extend([0, 2, 2, 0x4C, 0x01, 0]); // no local color table, LZW code size 2, one sub-block of 2 bytes
    }
    d.push(0x3B);
    d
}

fn webp(chunk: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut d = b"RIFF".to_vec();
    d.extend(((12 + payload.len()) as u32).to_le_bytes());
    d.extend(b"WEBP");
    d.extend(chunk);
    d.extend((payload.len() as u32).to_le_bytes());
    d.extend(payload);
    d
}
fn webp_lossy(w: u16, h: u16) -> Vec<u8> {
    let mut p = vec![0x10, 0x02, 0x00, 0x9D, 0x01, 0x2A];
    p.extend(w.to_le_bytes());
    p.extend(h.to_le_bytes());
    p.extend([0; 10]);
    webp(b"VP8 ", &p)
}
fn webp_lossless(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0x2F];
    p.extend(((w - 1) | ((h - 1) << 14)).to_le_bytes());
    p.extend([0; 8]);
    webp(b"VP8L", &p)
}
fn webp_extended(w: u32, h: u32) -> Vec<u8> {
    let mut p = vec![0x10, 0, 0, 0];
    p.extend(&(w - 1).to_le_bytes()[..3]);
    p.extend(&(h - 1).to_le_bytes()[..3]);
    webp(b"VP8X", &p)
}

// An ICO whose directory lists each icon at a size (0 for 256), holding the icon itself, a PNG or a bitmap.
fn ico(icons: &[((u8, u8), Vec<u8>)]) -> Vec<u8> {
    let mut d = vec![0, 0, 1, 0];
    d.extend((icons.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * icons.len();
    for ((w, h), icon) in icons {
        d.extend([*w, *h, 0, 0, 1, 0, 32, 0]);
        d.extend((icon.len() as u32).to_le_bytes());
        d.extend((offset as u32).to_le_bytes());
        offset += icon.len();
    }
    for (_, icon) in icons {
        d.extend(icon);
    }
    d
}
// A bitmap icon: its header (there is no file header) gives the height of the image and its mask together.
fn bmp_icon(w: i32, h: i32) -> Vec<u8> {
    let mut d = 40u32.to_le_bytes().to_vec();
    d.extend(w.to_le_bytes());
    d.extend((2 * h).to_le_bytes());
    d.extend([1, 0, 32, 0]);
    d.extend([0; 24]);
    d
}

// ---------------------------------------------------------------- helpers

fn repo() -> (TempDir, Server) {
    let dir = TempDir::new("images");
    init(dir.path());
    write(dir.path(), "README.md", "images\n");
    commit_all(dir.path(), "init");
    let s = Server::start(dir.path());
    (dir, s)
}

fn put(dir: &TempDir, path: &str, data: &[u8]) {
    let p = dir.path().join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
}

// Writes over an image the server has already described, with an mtime it can't have had. What /api/file says of an
// image is kept until its stat moves, and a rewrite at the same length within one tick of a coarse file system clock
// would leave its size and times as they were.
fn rewrite(dir: &TempDir, path: &str, data: &[u8]) {
    put(dir, path, data);
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options().write(true).open(dir.path().join(path)).unwrap().set_modified(later).unwrap();
}

fn file(s: &Server, path: &str, params: &[(&str, &str)]) -> Value {
    let mut p = vec![("mode", "working"), ("path", path)];
    p.extend_from_slice(params);
    let (status, body) = s.json("/api/file", &p);
    assert_eq!(status, 200, "{path} {params:?}: {body}");
    body
}

fn image(s: &Server, path: &str, params: &[(&str, &str)]) -> Res {
    let mut p = vec![("mode", "working"), ("path", path)];
    p.extend_from_slice(params);
    s.get("/api/image", &p)
}

// The image /api/image serves for `path`, as /api/file describes it.
// Its version (`v`) is /api/image's ETag.
fn shown(s: &Server, path: &str, data: &[u8], mime: &str, (w, h): (u32, u32)) {
    let res = image(s, path, &[]);
    assert_eq!(res.status, 200, "{path}: {}", res.text());
    assert_eq!(res.header("content-type"), Some(mime), "{path}");
    assert!(res.body == data, "{path}: served {} bytes, not the file's {}", res.body.len(), data.len());
    let v = res.header("etag").unwrap().trim_matches('"');
    let info = json!({ "binary": true, "size": data.len(), "image": { "type": mime, "width": w, "height": h, "v": v } });
    assert_eq!(file(s, path, &[]), info, "{path}");
}

// What /api/file says of a file /api/image refuses, and the status and error /api/image refuses it with.
fn refused(s: &Server, path: &str, image_info: Value, status: u16, error: &str) {
    assert_eq!(file(s, path, &[])["image"], image_info, "{path}");
    let res = image(s, path, &[]);
    assert_eq!((res.status, res.json()["error"].as_str().unwrap_or("")), (status, error), "{path}");
}

const IMAGE_CSP: &str = "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox";

// A revalidation of /api/image with `etag`: its status, ETag and CSP.
fn revalidate(s: &Server, params: &[(&str, &str)], etag: &str) -> (u16, Option<String>, Option<String>) {
    let res = request(s.port, "GET", &format!("/api/image?{}", qs(params)), &[("if-none-match", etag)], b"");
    let header = |name| res.header(name).map(str::to_string);
    (res.status, header("etag"), header("content-security-policy"))
}

// ---------------------------------------------------------------- tests

#[test]
fn every_format_is_told_by_its_signature_and_sized_by_its_header() {
    let (dir, s) = repo();
    let cases = vec![
        ("a.png", png(3, 2), "image/png", (3, 2)),
        ("b.jpg", jpeg(640, 480, 0), "image/jpeg", (640, 480)),
        // EXIF, ICC profiles and thumbnails come first, and can run to hundreds of KB.
        ("c.jpg", jpeg(4000, 3000, 150_000), "image/jpeg", (4000, 3000)),
        ("d.gif", gif((10, 20), &[(0, 0, 10, 20)]), "image/gif", (10, 20)),
        // A frame that reaches past the screen makes room for itself.
        ("e.gif", gif((10, 10), &[(0, 0, 10, 10), (5, 5, 30, 40)]), "image/gif", (35, 45)),
        ("f.webp", webp_lossy(300, 200), "image/webp", (300, 200)),
        ("g.webp", webp_lossless(16383, 2), "image/webp", (16383, 2)),
        ("h.webp", webp_extended(5000, 4000), "image/webp", (5000, 4000)),
        // The largest icon, by its own header: a PNG of 48 × 48 listed as 16 × 16, and a bitmap of 256 × 256.
        ("i.ico", ico(&[((16, 16), png(48, 48)), ((0, 0), bmp_icon(256, 256))]), "image/x-icon", (256, 256)),
    ];
    for (path, data, _, _) in &cases {
        put(&dir, path, data);
    }
    for (path, data, mime, size) in &cases {
        shown(&s, path, data, mime, *size);
    }
}

// Browsers draw a JPEG or a PNG turned as its EXIF orientation says: 5 to 8 are a quarter turn, which swaps its width
// and height, and 1 to 4 aren't. The size is the one it is drawn at, so the page's caption and the room it makes for
// the image are right.
#[test]
fn a_jpeg_or_png_is_sized_as_its_exif_orientation_turns_it() {
    let (dir, s) = repo();
    let mut cases = vec![];
    for o in 1..=8 {
        let size = if o >= 5 { (10, 40) } else { (40, 10) };
        for (order, big) in [("ii", false), ("mm", true)] {
            cases.push((format!("{o}-{order}.jpg"), jpeg_exif(40, 10, &orientation(big, o)), "image/jpeg", size));
            cases.push((format!("{o}-{order}.png"), png_exif(40, 10, &orientation(big, o), false), "image/png", size));
        }
    }
    // Browsers don't read a PNG's after its image data, or turn a WebP at all.
    cases.push(("late.png".into(), png_exif(40, 10, &orientation(false, 6), true), "image/png", (40, 10)));
    let mut webp = webp_extended(40, 10);
    webp[20] |= 0x08; // the flag that says it has EXIF
    webp.extend(b"EXIF");
    webp.extend((orientation(false, 6).len() as u32).to_le_bytes());
    webp.extend(orientation(false, 6));
    cases.push(("turned.webp".into(), webp, "image/webp", (40, 10)));
    for (path, data, _, _) in &cases {
        put(&dir, path, data);
    }
    for (path, data, mime, size) in &cases {
        shown(&s, path, data, mime, *size);
    }

    // Turning it changes no pixel count: 50 million exactly is shown, and more is refused, turned.
    let most = png_exif(10_000, 5_000, &orientation(true, 8), false);
    put(&dir, "most.png", &most);
    shown(&s, "most.png", &most, "image/png", (5_000, 10_000));
    put(&dir, "huge.jpg", &jpeg_exif(10_000, 6_000, &orientation(false, 6)));
    let error = "huge.jpg is too large to show (6000 × 10000 pixels, over 50 million)";
    refused(&s, "huge.jpg", json!({ "type": "image/jpeg", "width": 6_000, "height": 10_000, "refused": "pixels" }), 413, error);
}

// EXIF is the repo's, as untrusted as the rest: one that can't be read leaves the image as it is stored, and is no
// reason to refuse it.
#[test]
fn exif_that_cant_be_read_leaves_the_image_as_stored() {
    let (dir, s) = repo();
    let six = orientation(false, 6);
    let mut far = six.clone();
    far[4..8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // the directory past the end
    let mut many = tiff(true, &[(0x10F, 2, 4, 0)]);
    many[8..10].copy_from_slice(&[0xFF, 0xFF]); // 65,535 entries claimed, one there, and no orientation
    let mut magic = six.clone();
    magic[2] = 43;
    let damaged = [
        ("cut", six[..six.len() - 10].to_vec()), // ends inside the orientation's entry
        ("cut-after", six[..six.len() - 6].to_vec()), // ends 2 bytes short of its end, after its value
        ("far", far),
        ("many", many),
        ("order", [&b"IM"[..], &six[2..]].concat()),
        ("magic", magic),
        ("long", tiff(false, &[(0x112, 4, 1, 6)])), // a LONG, not a SHORT
        ("count", tiff(false, &[(0x112, 3, 2, 6)])),
        ("nine", tiff(false, &[(0x112, 3, 1, 9)])),
        ("empty", vec![]),
    ];
    // Undamaged, it is turned.
    put(&dir, "six.jpg", &jpeg_exif(40, 10, &six));
    shown(&s, "six.jpg", &jpeg_exif(40, 10, &six), "image/jpeg", (10, 40));
    put(&dir, "six.png", &png_exif(40, 10, &six, false));
    shown(&s, "six.png", &png_exif(40, 10, &six, false), "image/png", (10, 40));
    for (name, tiff) in &damaged {
        for (path, data, mime) in [
            (format!("{name}.jpg"), jpeg_exif(40, 10, tiff), "image/jpeg"),
            (format!("{name}.png"), png_exif(40, 10, tiff, false), "image/png"),
        ] {
            put(&dir, &path, &data);
            shown(&s, &path, &data, mime, (40, 10));
        }
    }
    // An eXIf chunk that runs past the end of the file.
    let mut long = png_exif(40, 10, &six, false);
    long[33..37].copy_from_slice(&1_000_000u32.to_be_bytes());
    put(&dir, "long.png", &long);
    shown(&s, "long.png", &long, "image/png", (40, 10));
}

// Each case's EXIF on a JPEG and a PNG stored at 40 × 10, sized turned (10 × 40) or as stored.
fn each_turned_or_not(dir: &TempDir, s: &Server, cases: &[(&str, Vec<u8>, bool)]) {
    for (name, tiff, turned) in cases {
        let size = if *turned { (10, 40) } else { (40, 10) };
        for (path, data, mime) in [
            (format!("{name}.jpg"), jpeg_exif(40, 10, tiff), "image/jpeg"),
            (format!("{name}.png"), png_exif(40, 10, tiff, false), "image/png"),
        ] {
            put(dir, &path, &data);
            shown(s, &path, &data, mime, size);
        }
    }
}

// Browsers read the orientation in the Exif directory too, which the first directory points to (tag 0x8769). Where
// the entries disagree, the size is Chromium's: the first entry that is one SHORT from 1 to 8 wins, in the order they
// are read, the Exif directory's where the first directory points to it, so an entry that isn't one is passed over.
// (Firefox stops at that entry and turns nothing, and takes the last of two good ones.)
#[test]
fn the_orientation_is_the_first_one_chromium_takes_in_either_directory() {
    let (dir, s) = repo();
    let o = |v: u16| (0x112, Val::Short(v));
    let short = |v: u16| (0x112, 3, 1, v);
    let exif_after = |entries: usize| (0x8769, 4, 1, dir_at(&[entries]));
    each_turned_or_not(&dir, &s, &[
        // Only in the Exif directory, as some cameras and editors write it.
        ("sub-ii", exif_tiff(false, &[], &[o(6)]), true),
        ("sub-mm", exif_tiff(true, &[], &[o(6)]), true),
        ("sub-nine", exif_tiff(false, &[], &[o(9), o(6)]), true),
        // An entry that isn't one, then one that is, or the other way round.
        ("nine-six", tiff(false, &[short(9), short(6)]), true),
        ("zero-six", tiff(false, &[short(0), short(6)]), true),
        ("long-six", tiff(false, &[(0x112, 4, 1, 6), short(6)]), true),
        ("count-six", tiff(false, &[(0x112, 3, 2, 6), short(6)]), true),
        ("six-nine", tiff(false, &[short(6), short(9)]), true),
        ("six-long", tiff(false, &[short(6), (0x112, 4, 1, 1)]), true),
        // Two that are: the first.
        ("six-three", tiff(false, &[short(6), short(3)]), true),
        ("three-six", tiff(false, &[short(3), short(6)]), false),
        ("one-six", tiff(false, &[short(1), short(6)]), false),
        // The two directories disagree. Sorted by tag, as TIFF has it, the first directory's orientation (0x112) comes
        // before its pointer (0x8769), and one in the wrong order is read in that order.
        ("first-one", exif_tiff(false, &[o(1)], &[o(6)]), false),
        ("first-six", exif_tiff(true, &[o(6)], &[o(1)]), true),
        ("first-nine", exif_tiff(false, &[o(9)], &[o(6)]), true),
        ("pointer-first-six", tiff_dirs(&[&[exif_after(2), short(1)], &[short(6)]]), true),
        ("pointer-first-one", tiff_dirs(&[&[exif_after(2), short(6)], &[short(1)]]), false),
    ]);
}

// The Exif directory is read only as Chromium reads it: through the first pointer to it in the first directory that
// is one LONG, even one that can't be followed, and through no other, so at most two directories are read. A
// directory that can't be read reads nothing, and the first directory is read on past its pointer.
#[test]
fn an_exif_directory_is_read_only_through_the_first_pointer_to_it() {
    let (dir, s) = repo();
    let six = (0x112, 3, 1, 6);
    let make = (0x10F, 2, 4, 0);
    let to = |at: u16| (0x8769, 4, 1, at);
    let far = 0xFFF0; // past the end
    let mut claims_more = tiff_dirs(&[&[to(dir_at(&[2])), six], &[make]]);
    let at = dir_at(&[2]) as usize;
    claims_more[at..at + 2].copy_from_slice(&500u16.to_le_bytes()); // 500 entries claimed, one there
    let mut next = tiff_dirs(&[&[], &[six]]);
    next[10..14].copy_from_slice(&(dir_at(&[0]) as u32).to_le_bytes()); // the next directory, a thumbnail's (IFD1)
    each_turned_or_not(&dir, &s, &[
        // A pointer past the end, into a directory that runs past it, or back to the first directory.
        ("far", tiff(false, &[to(far), six]), true),
        ("claims-more", claims_more, true),
        ("back", tiff(false, &[to(8), six]), true),
        ("loop", tiff_dirs(&[&[to(dir_at(&[1]))], &[to(dir_at(&[1])), six]]), true),
        // One that isn't one LONG is passed over, for the pointer after it.
        ("short", tiff_dirs(&[&[(0x8769, 3, 1, dir_at(&[1]))], &[six]]), false),
        ("count", tiff_dirs(&[&[(0x8769, 4, 2, dir_at(&[1]))], &[six]]), false),
        ("after-short", tiff_dirs(&[&[(0x8769, 3, 1, dir_at(&[2])), to(dir_at(&[2]))], &[six]]), true),
        // Only the first that is one is followed, whatever it points to.
        ("second", tiff_dirs(&[&[to(dir_at(&[2])), to(dir_at(&[2, 1]))], &[make], &[six]]), false),
        ("after-far", tiff_dirs(&[&[to(far), to(dir_at(&[2]))], &[six]]), false),
        // Nor is a pointer in the Exif directory followed, or another directory read: GPS (0x8825) or the next one.
        ("deeper", tiff_dirs(&[&[to(dir_at(&[1]))], &[to(dir_at(&[1, 1]))], &[six]]), false),
        ("gps", tiff_dirs(&[&[(0x8825, 4, 1, dir_at(&[1]))], &[six]]), false),
        ("next", next, false),
    ]);
    // 65,534 pointers back to the first directory, of 65,535 entries, read it once more, not 65,534 times (4 billion
    // entries). A PNG's eXIf can be that long, a JPEG's EXIF segment can't.
    let many = png_exif(40, 10, &tiff(false, &[vec![to(8); 65_534], vec![six]].concat()), false);
    put(&dir, "many.png", &many);
    shown(&s, "many.png", &many, "image/png", (10, 40));
}

// Browsers draw a JPEG at the size its EXIF resolution gives (its density-corrected size): in inches, at other than 72
// dots per inch, at 72 / that of its size, each side rounded, when its pixel dimensions are just that size. 144 dpi is
// drawn at half its size, and under 72 larger. It is turned after, as its orientation says.
#[test]
fn a_jpeg_is_sized_as_its_exif_resolution_scales_it() {
    let (dir, s) = repo();
    let turned = |x, y| [dpi(x, y), vec![(0x112, Val::Short(6))]].concat();
    let tiny = exif_tiff(false, &tiny_dpi(800), &long_dims(11_520_000, 5_760_000));
    let unit_long_first = [vec![(0x128, Val::Long(2))], dpi(144, 144)].concat();
    let both_dims = [dpi(144, 144), dims(200, 100)].concat();
    let sub_turned = [dims(100, 50), vec![(0x112, Val::Short(6))]].concat();
    let upright = [dpi(144, 144), vec![(0x112, Val::Short(1))]].concat();
    let cases = vec![
        ("144.jpg", (200, 100), resolution(144, 144, (100, 50)), (100, 50)),
        ("96.jpg", (200, 100), resolution(96, 96, (150, 75)), (150, 75)),
        ("36.jpg", (200, 100), resolution(36, 36, (400, 200)), (400, 200)),
        ("half-up.jpg", (201, 101), resolution(144, 144, (101, 51)), (101, 51)), // 100.5 × 50.5
        ("170.jpg", (200, 100), resolution(170, 170, (85, 42)), (85, 42)), // 84.7 × 42.4
        ("across.jpg", (200, 100), resolution(144, 72, (100, 100)), (100, 100)),
        ("tiny.jpg", (200, 100), tiny, (11_520_000, 5_760_000)), // 1/800 dpi
        // Chromium gives no side over 2^25 - 1 pixels, and takes pixel dimensions up to 2^31 - 1.
        ("huge.jpg", (200, 100), exif_tiff(false, &tiny_dpi(10_000), &long_dims(144_000_000, 72_000_000)), (33_554_431, 33_554_431)),
        ("most.jpg", (200, 100), exif_tiff(false, &tiny_dpi(149_130), &long_dims(2_147_472_000, 1_073_736_000)), (33_554_431, 33_554_431)),
        ("mm.jpg", (200, 100), exif_tiff(true, &dpi(144, 144), &dims(100, 50)), (100, 50)),
        ("long.jpg", (200, 100), exif_tiff(false, &dpi(144, 144), &long_dims(100, 50)), (100, 50)),
        // Every tag may be in either directory.
        ("ifd0.jpg", (200, 100), exif_tiff(false, &[dpi(144, 144), dims(100, 50)].concat(), &[]), (100, 50)),
        ("sub.jpg", (200, 100), exif_tiff(false, &[], &[dpi(144, 144), dims(100, 50)].concat()), (100, 50)),
        // Scaled as stored, then turned: 200 × 120 at 144 × 72 dpi is 100 × 120, turned to 120 × 100.
        ("turned.jpg", (200, 100), exif_tiff(false, &turned(144, 144), &dims(100, 50)), (50, 100)),
        ("turned-across.jpg", (200, 120), exif_tiff(false, &turned(144, 72), &dims(100, 120)), (120, 100)),
        // An orientation in the Exif directory turns it too, unless the first directory gives one before it.
        ("turned-sub.jpg", (200, 100), exif_tiff(false, &dpi(144, 144), &sub_turned), (50, 100)),
        ("upright.jpg", (200, 100), exif_tiff(false, &upright, &sub_turned), (100, 50)),
        // Where the tags say two things, Chromium's reading wins: each from the first entry for it that it can read,
        // and the Exif directory's read where the first directory points to it, before that one's entries after.
        ("later.jpg", (200, 100), exif_tiff(false, &unit_long_first, &dims(100, 50)), (100, 50)),
        ("sub-first.jpg", (200, 100), exif_tiff(false, &both_dims, &dims(100, 50)), (100, 50)),
    ];
    for (path, (w, h), tiff, size) in &cases {
        let data = jpeg_exif(*w, *h, tiff);
        put(&dir, path, &data);
        shown(&s, path, &data, "image/jpeg", *size);
    }
}

// The tags must say all of it, as browsers read them: a resolution alone, which cameras and editors write for print, or
// one with pixel dimensions of another size (such as the stored one), leaves a JPEG as stored, and so does a tag that
// can't be read. A PNG's is left alone too: Chromium doesn't apply it, though Firefox does.
#[test]
fn a_resolution_scales_a_jpeg_only_when_its_tags_say_all_of_it() {
    let (dir, s) = repo();
    let good = resolution(144, 144, (100, 50));
    put(&dir, "good.jpg", &jpeg_exif(200, 100, &good));
    shown(&s, "good.jpg", &jpeg_exif(200, 100, &good), "image/jpeg", (100, 50));
    // 144 dpi without one of its tags, or with other entries for it in its place, and pixel dimensions for 144.
    let without = |tag: u16| dpi(144, 144).into_iter().filter(|&(t, _)| t != tag).collect::<Vec<_>>();
    let with = |tag: u16, vals: &[Val]| [without(tag), vals.iter().map(|&v| (tag, v)).collect()].concat();
    let at = |ifd0: Vec<(u16, Val)>| exif_tiff(false, &ifd0, &dims(100, 50));
    let mut far = good.clone();
    far[18..22].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // XResolution's value past the end
    let mut lost = good.clone();
    lost[54..58].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // the Exif directory past the end
    let stored = [
        ("alone", exif_tiff(false, &dpi(144, 144), &[])),
        ("as-stored", resolution(144, 144, (200, 100))),
        ("off", resolution(144, 144, (101, 50))),
        ("72", resolution(72, 72, (100, 50))),
        // No unit is inches to TIFF, but browsers want it said.
        ("no-unit", at(without(0x128))),
        ("cm", at(with(0x128, &[Val::Short(3)]))),
        ("unit-long", at(with(0x128, &[Val::Long(2)]))),
        ("x-only", at(without(0x11B))),
        ("short", at(with(0x11A, &[Val::Short(144)]))),
        ("zero", at(with(0x11A, &[Val::Ratio(0, 1)]))),
        ("over-zero", at(with(0x11A, &[Val::Ratio(144, 0)]))),
        ("first-wins", at(with(0x11A, &[Val::Ratio(72, 1), Val::Ratio(144, 1)]))),
        // A resolution of 0, or over 0, is one Chromium reads (as 0), so an entry after it doesn't count.
        ("zero-first", at(with(0x11A, &[Val::Ratio(0, 1), Val::Ratio(144, 1)]))),
        ("over-zero-first", at(with(0x11A, &[Val::Ratio(144, 0), Val::Ratio(144, 1)]))),
        // Pixel dimensions just past 2^31 - 1, which those of 1/149,131 dpi come to.
        ("past-int", exif_tiff(false, &tiny_dpi(149_131), &long_dims(2_147_486_464, 1_073_743_232))),
        ("width-only", exif_tiff(false, &dpi(144, 144), &dims(100, 50)[..1])),
        // So high that it rounds to nothing.
        ("nothing", resolution(u32::MAX, u32::MAX, (0, 0))),
        // 1/1000 dpi gives exactly 14,400,000 × 7,200,000, but browsers work it out in single precision, which comes
        // to 14,399,999.
        ("single", exif_tiff(false, &tiny_dpi(1000), &long_dims(14_400_000, 7_200_000))),
        ("far", far),
        ("lost", lost),
        // Only the first pointer to the Exif directory is followed, even one past the end.
        ("second-pointer", at([dpi(144, 144), vec![(0x8769, Val::Long(0xFFFF_FFF0))]].concat())),
        ("cut", good[..good.len() - 4].to_vec()), // ends inside YResolution
    ];
    for (name, tiff) in &stored {
        let (path, data) = (format!("{name}.jpg"), jpeg_exif(200, 100, tiff));
        put(&dir, &path, &data);
        shown(&s, &path, &data, "image/jpeg", (200, 100));
    }
    let png = png_exif(200, 100, &good, false);
    put(&dir, "good.png", &png);
    shown(&s, "good.png", &png, "image/png", (200, 100));
}

// Decoding an image takes the pixels it stores, whatever size it is drawn at: over 50 million it is refused, and says
// so of those pixels, even drawn smaller, and under, it is shown, even drawn larger.
#[test]
fn the_pixel_limit_is_on_the_pixels_stored_not_the_size_drawn() {
    let (dir, s) = repo();
    put(&dir, "big.jpg", &jpeg_exif(10_000, 6_000, &resolution(144, 144, (5_000, 3_000))));
    let info = json!({ "type": "image/jpeg", "width": 10_000, "height": 6_000, "refused": "pixels" });
    refused(&s, "big.jpg", info, 413, "big.jpg is too large to show (10000 × 6000 pixels, over 50 million)");
    let small = jpeg_exif(5_000, 5_000, &resolution(36, 36, (10_000, 10_000)));
    put(&dir, "small.jpg", &small);
    shown(&s, "small.jpg", &small, "image/jpeg", (10_000, 10_000));
}

#[test]
fn the_name_proves_nothing() {
    let (dir, s) = repo();
    let page = "<!doctype html><script>fetch('/api/info')</script>\n";
    put(&dir, "page.png", page.as_bytes());
    put(&dir, "photo.dat", &png(1, 1));
    put(&dir, "data.bin", &[0, 1, 2, 3]);
    // A page named .png is the text it is, and never served as an image.
    assert_eq!(file(&s, "page.png", &[]), json!({ "content": page }));
    // Whatever its name, an image is one.
    shown(&s, "photo.dat", &png(1, 1), "image/png", (1, 1));
    // A binary file that isn't an image is only binary.
    assert_eq!(file(&s, "data.bin", &[]), json!({ "binary": true, "size": 4 }));
    for path in ["page.png", "data.bin"] {
        let res = image(&s, path, &[]);
        assert_eq!((res.status, res.json()["error"].clone()), (415, json!(format!("{path} is not an image revue shows"))));
    }
}

// An SVG is text, and an image too: a file named .svg whose text starts with an <svg> element. It is served as one, but
// as a download when opened on its own, since it is a document that can hold script. Its size is its root's width
// and height, or its viewBox's.
#[test]
fn an_svg_is_an_image_by_its_name_and_its_root_element() {
    let (dir, s) = repo();
    let cases = [
        ("plain.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"16px\"><rect width=\"9\" height=\"9\"/></svg>\n", Some((24, 16))),
        // A byte order mark, an XML declaration, a comment and a doctype may come first; a '>' in a quoted value isn't
        // the end of the tag, and stroke-width isn't width.
        ("prolog.svg", "\u{feff}<?xml version=\"1.0\"?>\n<!-- made by hand -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\">\n<svg xmlns=\"http://www.w3.org/2000/svg\" data-x=\"a>b\" stroke-width=\"3\" viewBox=\"0 0 300 150\"></svg>\n", Some((300, 150))),
        ("percent.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"100%\" viewBox=\"0,0,40.4,20.6\"></svg>", Some((40, 21))),
        // The size is the attributes', not what another's quoted value says, and their references are read.
        ("decoy.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" title=' width=\"1\" height=\"1\"' width=\"5000\" height=\"5000\"></svg>\n", Some((5000, 5000))),
        ("references.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"&#51;2\" height='2&#x34;'></svg>\n", Some((32, 24))),
        ("sizeless.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>\n", None),
    ];
    for (path, text, size) in &cases {
        put(&dir, path, text.as_bytes());
        let res = image(&s, path, &[]);
        assert_eq!(res.status, 200, "{path}: {}", res.text());
        assert!(res.body == text.as_bytes(), "{path}");
        for (name, value) in [
            ("content-type", "image/svg+xml"),
            ("content-disposition", "attachment"),
            ("content-security-policy", "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox"),
            ("x-content-type-options", "nosniff"),
        ] {
            assert_eq!(res.header(name), Some(value), "{path}: {name}");
        }
        let v = res.header("etag").unwrap().trim_matches('"');
        let mut info = json!({ "type": "image/svg+xml", "v": v });
        if let Some((w, h)) = size {
            info["width"] = json!(w);
            info["height"] = json!(h);
        }
        assert_eq!(file(&s, path, &[]), json!({ "content": text, "size": text.len(), "image": info }), "{path}");
    }

    // Not one: another name, a root that isn't <svg>, a doctype that defines entities, or more than the text limit.
    let not = [
        ("logo.xml", "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n".to_string()),
        ("page.svg", "<!doctype html><html><body><svg></svg></body></html>\n".to_string()),
        ("svgish.svg", "<svgx></svgx>\n".to_string()),
        ("laughs.svg", "<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY a \"aaaa\">]><svg>&a;</svg>\n".to_string()),
    ];
    for (path, text) in &not {
        put(&dir, path, text.as_bytes());
        assert_eq!(file(&s, path, &[]), json!({ "content": text }), "{path}");
        let res = image(&s, path, &[]);
        assert_eq!((res.status, res.json()["error"].clone()), (415, json!(format!("{path} is not an image revue shows"))), "{path}");
    }
    let big = format!("<svg viewBox=\"0 0 10 10\">{}</svg>\n", "<!-- padding -->".repeat(140_000));
    put(&dir, "big.svg", big.as_bytes());
    assert_eq!(file(&s, "big.svg", &[]), json!({ "tooLarge": true, "size": big.len(), "image": { "type": "image/svg+xml", "refused": "bytes" } }));
    let res = image(&s, "big.svg", &[]);
    assert_eq!((res.status, res.json()["error"].clone()), (413, json!("big.svg is too large to show (over 2 MB)")));
}

// An SVG whose root has `attrs`, with `inside` before a rectangle that fills it.
fn svg(attrs: &str, inside: &str) -> String {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" {attrs}>{inside}<rect width=\"100%\" height=\"100%\"/></svg>\n")
}

// A file's name, its text, and the size /api/file gives it (None for none).
type SvgCase<'a> = (&'a str, String, Option<(u32, u32)>);

fn svg_sizes(dir: &TempDir, s: &Server, cases: &[SvgCase]) {
    for (name, text, size) in cases {
        let path = format!("{name}.svg");
        put(dir, &path, text.as_bytes());
        let info = &file(s, &path, &[])["image"];
        assert_eq!(info["type"], json!("image/svg+xml"), "{path}: {text}");
        let want = size.map_or((Value::Null, Value::Null), |(w, h)| (json!(w), json!(h)));
        assert_eq!((info["width"].clone(), info["height"].clone()), want, "{path}: {text}");
    }
}

// An SVG's size is the one an <img> draws it at with no size of its own, where Chromium 156 and Firefox 157 gave the
// same naturalWidth and naturalHeight: from the root's width and height in any unit whose size is known, or one of
// them and the viewBox's proportions. Where browsers make a size up (300 × 150, or 150 or 300 square), it is the
// viewBox's, or none; where the two differ, where the size depends on what the server can't know (the browser's fonts,
// its viewport), or is 0, it is none, and the browser draws it at its own.
#[test]
fn an_svgs_size_is_the_one_browsers_draw_it_at() {
    let (dir, s) = repo();
    let max = 33_554_428; // Chromium's largest
    let cases = [
        // Units, in any case, a viewBox making no difference; em and rem are of the default font size, 16px.
        ("px", svg("width=\"40px\" height=\"20\"", ""), Some((40, 20))),
        ("points", svg("width=\"30pt\" height=\"1.5pc\"", ""), Some((40, 24))),
        ("inches", svg("width=\"1in\" height=\"2.54cm\"", ""), Some((96, 96))),
        ("metric", svg("width=\"25.4mm\" height=\"101.6Q\"", ""), Some((96, 96))),
        ("case", svg("width=\"1IN\" height=\"0.5In\"", ""), Some((96, 48))),
        ("em", svg("width=\"10em\" height=\"5rem\" viewBox=\"0 0 100 50\"", ""), Some((160, 80))),
        ("icon", svg("width=\"1em\" height=\"1em\" viewBox=\"0 0 24 24\"", ""), Some((16, 16))),
        // Numbers as CSS writes them, between spaces, rounded as naturalWidth rounds them.
        ("numbers", svg("width=\" 4e1 \" height=\"+.25E2px\"", ""), Some((40, 25))),
        ("rounded", svg("width=\"40.5\" height=\"10mm\"", ""), Some((41, 38))),
        ("half", svg("width=\"0.5\" height=\"0.5\"", ""), Some((1, 1))),
        ("long", svg(&format!("width=\"40{}\" height=\"20\"", " ".repeat(64 * 1024)), ""), Some((40, 20))),
        // Comments and escapes: Chromium reads them as CSS does, Firefox ignores such a width.
        ("comment", svg("width=\"/*a*/40/*b*/\" height=\"20\"", ""), None),
        ("escape", svg("width=\"40p\\78\" height=\"20\"", ""), None),
        ("split", svg("width=\"40/*a*/px\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        // One of them and the viewBox's proportions; a percentage, auto or no length at all is as good as missing.
        ("width", svg("width=\"40\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        ("height", svg("height=\"1in\" viewBox=\"0,0,30,10\"", ""), Some((288, 96))),
        ("third", svg("width=\"40\" viewBox=\"0 0 30 10\"", ""), Some((40, 13))),
        ("percent", svg("width=\"50%\" height=\"20\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        ("auto", svg("width=\"auto\" height=\"20\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        ("junk", svg("width=\"40 px\" height=\"20\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        ("unit", svg("width=\"40deg\" height=\"20\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        ("important", svg("width=\"40 !important\" height=\"20\" viewBox=\"0 0 100 50\"", ""), Some((40, 20))),
        // Without a valid viewBox the other is a default (300 wide or 150 tall). Numbers apart by nothing are two in
        // Chromium, and in Firefox an error.
        ("alone", svg("width=\"40\"", ""), None),
        ("comma", svg("width=\"40\" viewBox=\"0 0 100 50,\"", ""), None),
        ("dot", svg("height=\"20\" viewBox=\"0 0 100. 50\"", ""), None),
        ("unspaced", svg("width=\"40\" viewBox=\"-10-10 100 50\"", ""), None),
        // Neither: the viewBox's.
        ("neither", svg("width=\"auto\" height=\"100%\" viewBox=\"0 0 100 50\"", ""), Some((100, 50))),
        // CSS widths and heights change nothing; zoom does in Firefox, from the style attribute or a style sheet.
        ("css", svg("width=\"40\" height=\"20\" style=\"width: 50px\"", "<style>svg { height: 30px }</style>"), Some((40, 20))),
        ("zoom-attribute", svg("width=\"40\" height=\"20\" zoom=\"2\"", ""), Some((40, 20))),
        ("zoom", svg("width=\"40\" height=\"20\" style=\"zoom: 2\"", ""), None),
        ("zoom-sheet", svg("width=\"40\" height=\"20\"", "<style>* { ZOOM: 2 }</style>"), None),
        // Units of the font (which differ by browser), the viewport (0) or a container, and functions (see the math
        // ones below), are unknown.
        ("ex", svg("width=\"10ex\" height=\"5ex\" viewBox=\"0 0 100 50\"", ""), None),
        ("ch", svg("width=\"10ch\" viewBox=\"0 0 100 50\"", ""), None),
        ("viewport", svg("width=\"10vw\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        ("round", svg("width=\"round(41px, 2px)\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        ("var", svg("width=\"var(--w) 1px\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        // So are em and rem when a style sheet may set the root's font size.
        ("sheet", svg("width=\"1em\" height=\"1em\"", "<defs><style>svg { font-size: 20px }</style></defs>"), None),
        ("prefixed", svg("xmlns:s=\"http://www.w3.org/2000/svg\" width=\"1rem\" height=\"1rem\"", "<s:style>svg { font-size: 20px }</s:style>"), None),
        ("linked", format!("<?xml-stylesheet href=\"data:text/css,svg%7Bfont-size:20px%7D\"?>\n{}", svg("width=\"1em\" height=\"1em\"", "")), None),
        ("commented", svg("width=\"1em\" height=\"1em\"", "<!-- <style>svg { font-size: 20px }</style> -->"), Some((16, 16))),
        ("absolute", svg("width=\"40\" height=\"1in\" font-size=\"20\"", "<style>svg { font-size: 20px }</style>"), Some((40, 96))),
        // 0, a side of 0 once rounded, or a negative one, which Chromium takes for 0 and Firefox ignores.
        ("zero", svg("width=\"0\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        ("small", svg("width=\"0.4\" height=\"20\"", ""), None),
        ("flat", svg("width=\"499\" viewBox=\"0 0 1000 1\"", ""), None),
        ("negative", svg("width=\"-40\" height=\"20\" viewBox=\"0 0 100 50\"", ""), None),
        // Huge: in single precision, up to Chromium's largest, past which Chromium keeps to it and Firefox doesn't.
        ("huge", svg("width=\"100000\" height=\"1e7\"", ""), Some((100_000, 10_000_000))),
        ("single", svg("width=\"17895697\" height=\"10\"", ""), Some((17_895_696, 10))),
        ("largest", svg(&format!("width=\"{max}\" height=\"10\""), ""), Some((max, 10))),
        ("proportions", svg("width=\"2e7\" viewBox=\"0 0 1 1\"", ""), Some((20_000_000, 20_000_000))),
        ("over", svg("width=\"33554430\" height=\"10\"", ""), None),
        ("over-tall", svg("width=\"1e7\" viewBox=\"0 0 1 4\"", ""), None),
        ("over-viewbox", svg("viewBox=\"0 0 1e30 1e30\"", ""), None),
    ];
    svg_sizes(&dir, &s, &cases);

    // A file refused for what it draws was read only up to the refusal, so a style sheet or an animation may come
    // after: neither em nor px gives it a size.
    for (name, attrs) in [("complex-em", "width=\"1em\" height=\"1em\""), ("complex-px", "width=\"10\" height=\"10\"")] {
        let path = format!("{name}.svg");
        put(&dir, &path, svg(attrs, &"<rect/>".repeat(50_000)).as_bytes());
        assert_eq!(file(&s, &path, &[])["image"], json!({ "type": "image/svg+xml", "refused": "complex" }), "{path}");
    }
}

// em and rem are of the root's font size, where Chromium and Firefox agree on it: its font-size attribute, a number that
// isn't negative in px or with a unit whose size is known (em, rem and % being of the default 16px), or the font-size
// declarations of a plain style attribute (name: value, apart by ';'), which beat it, the last one winning. Anything
// else the browsers read differently or the server doesn't read (a keyword, a function, a negative size, the font
// shorthand, all, !important, a string, a comment, an escape, a bare number in a declaration) leaves the size unknown,
// as do a font size of 0 and one over 8,192px, past which Firefox rounds it (9,000 is 8,992).
#[test]
fn an_svgs_em_is_of_the_font_size_its_root_sets() {
    let (dir, s) = repo();
    let em = |attrs: &str| svg(&format!("width=\"10em\" height=\"5em\" {attrs}"), "");
    let font_size = |v: &str| em(&format!("font-size=\"{v}\""));
    let style = |v: &str| em(&format!("style=\"{v}\""));
    let cases = [
        // The attribute.
        ("none", em(""), Some((160, 80))),
        ("number", font_size("20"), Some((200, 100))),
        ("px", font_size("20px"), Some((200, 100))),
        ("pt", font_size("12pt"), Some((160, 80))),
        ("pc", font_size("1.5pc"), Some((240, 120))),
        ("in", font_size("0.25in"), Some((240, 120))),
        ("em", font_size("2em"), Some((320, 160))),
        ("rem", font_size("2rem"), Some((320, 160))),
        ("percent", font_size("150%"), Some((240, 120))),
        ("fraction", font_size("20.5"), Some((205, 103))),
        ("spaced", font_size(" +2e1 "), Some((200, 100))),
        ("upper", font_size("20PX"), Some((200, 100))),
        ("largest", svg("width=\"1em\" height=\"2em\" font-size=\"8192\"", ""), Some((8192, 16384))),
        ("keyword", font_size("large"), None),
        ("relative-keyword", font_size("larger"), None),
        ("calc", font_size("calc(10px + 10px)"), None),
        ("comment", font_size("/*a*/20"), None),
        ("escape", font_size("l\\61 rge"), None),
        ("negative", font_size("-5"), None),
        ("junk", font_size("big"), None),
        ("apart", font_size("20 px"), None),
        ("viewport", font_size("10vw"), None),
        ("zero", font_size("0"), None),
        ("over", svg("width=\"1em\" height=\"2em\" font-size=\"9000\"", ""), None),
        // A style attribute that sets no font size changes nothing; one that does beats the attribute.
        ("fill", style("fill:red"), Some((160, 80))),
        ("style", style("font-size:20px"), Some((200, 100))),
        ("style-rem", svg("width=\"10rem\" height=\"5rem\" style=\"font-size:20px\"", ""), Some((200, 100))),
        ("both", em("font-size=\"30\" style=\"font-size:20px\""), Some((200, 100))),
        ("attribute-kept", em("font-size=\"30\" style=\"fill:red\""), Some((300, 150))),
        ("style-upper", style("FONT-SIZE:20PX"), Some((200, 100))),
        ("style-spaced", style(" font-size : 20px "), Some((200, 100))),
        ("among", style("fill:red;font-size:20px;stroke:blue"), Some((200, 100))),
        ("empty", style("font-size:20px;;fill:red"), Some((200, 100))),
        ("last", style("font-size:20px; font-size:30px"), Some((300, 150))),
        ("entity", style("font-size:20px&#59;font-size:30px"), Some((300, 150))),
        ("style-units", style("font-size:150%"), Some((240, 120))),
        // Unknown in the style attribute: a bare number (Chromium takes it for px, Firefox ignores it), and anything
        // but plain declarations of a font size the attribute could hold.
        ("bare", style("font-size:20"), None),
        ("font", style("font: 20px serif"), None),
        ("all", style("font-size:20px; all: initial"), None),
        ("important", style("font-size:20px !important"), None),
        ("string", style("content:'a'; font-size:20px"), None),
        ("comment-in-style", style("font-size:/*x*/20px"), None),
        ("escape-in-style", style("f\\6f nt-size:20px"), None),
        ("url", style("fill:url(#a); font-size:20px"), None),
        ("brace", style("font-size:30px; }font-size:20px"), None),
        ("style-keyword", style("font-size:large"), None),
        ("style-junk", style("font-size:nope"), None),
        ("no-colon", style("font-size 20px"), None),
        // A size given in px doesn't need the font's.
        ("px-anyway", svg("width=\"40\" height=\"20\" style=\"font: 20px var(--f)\"", ""), Some((40, 20))),
    ];
    svg_sizes(&dir, &s, &cases);
}

// A style sheet the file applies leaves em and rem unknown only when it could set the root's font size: when it says
// font anywhere (any case), sets the all property, or holds an escape. A <style>'s text is read with its references
// and CDATA, a data: URL's (an xml-stylesheet's, a link's, an @import's) decoded, and a data: URL that isn't text/css,
// an xml-stylesheet that names an element or another file, and an SVG the file embeds apply nothing to its root.
#[test]
fn a_style_sheet_leaves_em_unknown_only_if_it_could_set_a_font_size() {
    let (dir, s) = repo();
    let sheet = |css: &str| svg("width=\"10em\" height=\"5em\" font-size=\"20\"", &format!("<style>{css}</style>"));
    let linked = |href: &str| format!("<?xml-stylesheet href=\"{href}\" type=\"text/css\"?>\n{}", svg("width=\"10em\" height=\"5em\"", ""));
    let embedded = "%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='10'%3E%3Cstyle%3Esvg{font-size:20px}%3C/style%3E%3C/svg%3E";
    let cases = [
        ("fill", sheet("rect { fill: red }"), Some((200, 100))),
        ("all-words", sheet(".all { fill: red } @media all { rect { fill: blue } }"), Some((200, 100))),
        ("cdata", sheet("<![CDATA[ rect > g { fill: red } ]]>"), Some((200, 100))),
        ("font-size", sheet("svg { font-size: 30px }"), None),
        ("root", sheet(":root { font-size: 30px }"), None),
        ("anywhere", sheet("rect { FONT-family: serif }"), None),
        ("keyframes", sheet("@keyframes a { to { font-size: 30px } } svg { animation: a 1s }"), None),
        ("all", sheet("svg { ALL /* x */ : initial }"), None),
        ("escape", sheet("svg { f\\6f nt-size: 30px }"), None),
        ("reference", sheet("svg { &#102;ont-size: 30px }"), None),
        ("in-cdata", sheet("<![CDATA[ svg { font-size: 30px } ]]>"), None),
        ("import", sheet("@import url(\"data:text/css,svg%7Bf%6Fnt-size:30px%7D\");"), None),
        ("import-plain", sheet("@import url(\"data:,svg%7Bf%6Fnt-size:30px%7D\");"), Some((200, 100))),
        ("linked", linked("data:text/css,rect%7Bfill:red%7D"), Some((160, 80))),
        ("linked-font", linked("data:text/css,svg%7Bf%6Fnt-size:20px%7D"), None),
        ("linked-base64", linked("data:text/css;base64,c3Zne2ZvbnQtc2l6ZToyMHB4fQ=="), None),
        ("linked-plain", linked("data:,svg%7Bfont-size:20px%7D"), Some((160, 80))),
        ("linked-element", linked("#s").replace("<rect", "<desc id=\"s\">svg{font-size:20px}</desc><rect"), Some((160, 80))),
        ("linked-file", linked("style.css"), Some((160, 80))),
        ("html-link", svg("width=\"10em\" height=\"5em\"", "<foreignObject><link xmlns=\"http://www.w3.org/1999/xhtml\" rel=\"stylesheet\" href=\"data:text/css,svg%7Bfont-size:20px%7D\"/></foreignObject>"), None),
        ("html-style", svg("width=\"10em\" height=\"5em\"", "<foreignObject><style xmlns=\"http://www.w3.org/1999/xhtml\">svg{font-size:20px}</style></foreignObject>"), None),
        ("embedded", svg("width=\"10em\" height=\"5em\"", &format!("<image width=\"10\" height=\"10\" href=\"data:image/svg+xml,{embedded}\"/>")), Some((160, 80))),
    ];
    svg_sizes(&dir, &s, &cases);
}

// calc(), min(), max() and clamp() in a width or height: Chromium works them out, and Firefox ignores them and draws its
// default 300 wide, so they leave the size unknown, as any function does, in any spelling.
#[test]
fn an_svgs_math_functions_leave_its_size_unknown() {
    let (dir, s) = repo();
    let wide = |w: &str| svg(&format!("width=\"{w}\" height=\"20\" viewBox=\"0 0 100 50\""), "");
    let cases = [
        ("sum", svg("width=\"calc(10px + 20px)\" height=\"calc(2 * 10px)\"", ""), None),
        ("height", svg("height=\"calc(10px + 10px)\" viewBox=\"0 0 100 50\"", ""), None),
        ("min", wide("min(40px, 60px)"), None),
        ("max", wide("max(10px, 20px)"), None),
        ("clamp", wide("clamp(10px, 50px, 40px)"), None),
        ("webkit", wide("-webkit-calc(10px)"), None),
        ("upper", wide("CALC(30px)"), None),
        ("spaced", wide(" calc(30px) "), None),
        ("escape", wide("c\\61 lc(10px)"), None),
        ("unclosed", wide("calc((10px + 20px"), None),
    ];
    svg_sizes(&dir, &s, &cases);
}

// An animation's references are checked against the root's ids as a set, so the work stays linear in the file's length:
// a root of 15,000 ids with an animation of 15,000 references took 2 s in a debug build when each was compared with
// each, and twice those 8 s. This one, of 60,000 each (1 MB), would take about half a minute.
#[test]
fn an_animation_with_many_references_is_read_quickly() {
    let (dir, s) = repo();
    let n = 60_000;
    let text = format!(
        "<svg {SVG_NS} width=\"10\" height=\"5\"{}><set attributeName=\"width\" to=\"20\"{}/></svg>\n",
        " id=\"a\"".repeat(n),
        " href=\"#b\"".repeat(n)
    );
    put(&dir, "many.svg", text.as_bytes());
    let start = std::time::Instant::now();
    let info = &file(&s, "many.svg", &[])["image"];
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "many.svg took {:?}", start.elapsed());
    // Its attributes repeat, so it isn't well formed either.
    assert_eq!(info["refused"], json!("malformed"));
}

// An animation of the root's width, height or viewBox, or of its font-size when em or rem size it, may change the size
// it is drawn at (Chromium animates a width, both browsers a viewBox and a font-size), so it leaves the size unknown,
// whenever it begins: one the root holds that names no other element, or one that names the root by its id. An
// animation of another element's size changes nothing.
#[test]
fn an_animation_of_the_roots_size_leaves_it_unknown() {
    let (dir, s) = repo();
    let em = |inside: &str| svg("id=\"r\" width=\"10em\" height=\"5em\"", inside);
    let px = |inside: &str| svg("id=\"r\" width=\"10\" height=\"5\"", inside);
    let cases = [
        ("set-font", em("<set attributeName=\"font-size\" to=\"20\"/>"), None),
        ("animate-font", em("<animate attributeName=\"font-size\" values=\"20;20\" dur=\"10s\"/>"), None),
        ("later", em("<set attributeName=\"font-size\" to=\"20\" begin=\"5s\"/>"), None),
        ("font", em("<set attributeName=\"font\" to=\"20px serif\"/>"), None), // Firefox
        ("upper", em("<set attributeName=\"FONT-SIZE\" to=\"20\"/>"), None), // Firefox
        ("width", px("<set attributeName=\"width\" to=\"20\"/>"), None),
        ("height", px("<animate attributeName=\"height\" from=\"20\" to=\"20\" dur=\"1s\"/>"), None),
        ("viewbox", svg("width=\"40\" viewBox=\"0 0 100 50\"", "<set attributeName=\"viewBox\" to=\"0 0 100 100\"/>"), None),
        ("by-id", px("<g><g><set href=\"#r\" attributeName=\"width\" to=\"20\"/></g></g>"), None),
        ("by-xlink", em("<g><set xlink:href=\"#r\" attributeName=\"font-size\" to=\"20\"/></g>"), None),
        ("by-escaped-id", em("<g><set href=\"#%72\" attributeName=\"font-size\" to=\"20\"/></g>"), None),
        ("after", em("<defs><set href=\"#r\" attributeName=\"font-size\" to=\"20\"/></defs>"), None),
        ("empty-href", px("<set href=\"\" attributeName=\"width\" to=\"20\"/>"), None), // Chromium
        ("prefixed", px("<s:set xmlns:s=\"http://www.w3.org/2000/svg\" attributeName=\"width\" to=\"20\"/>"), None),
        // Left alone.
        ("font-px", px("<set attributeName=\"font-size\" to=\"20\"/>"), Some((10, 5))),
        ("rect", px("<rect width=\"8\" height=\"6\"><set attributeName=\"width\" to=\"20\"/></rect>"), Some((10, 5))),
        ("group", px("<g><set attributeName=\"width\" to=\"20\"/></g>"), Some((10, 5))),
        ("names-other", px("<set href=\"#x\" attributeName=\"width\" to=\"20\"/><rect id=\"x\"/>"), Some((10, 5))),
        ("names-nothing", px("<set href=\"#nope\" attributeName=\"width\" to=\"20\"/>"), Some((10, 5))),
        ("spaced", px("<set attributeName=\" width \" to=\"20\"/>"), Some((10, 5))),
        ("style", em("<set attributeName=\"style\" to=\"font-size:20px\"/>"), Some((160, 80))),
        ("transform", px("<animateTransform attributeName=\"width\" type=\"scale\" to=\"2\" dur=\"1s\"/>"), Some((10, 5))),
        ("css", px("<style>@keyframes a { to { width: 20px } } svg { animation: a 1s }</style>"), Some((10, 5))),
        ("inner-svg", px("<svg><set attributeName=\"width\" to=\"20\"/></svg>"), Some((10, 5))),
        ("embedded", px("<image href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='9' height='9'%3E%3Cset attributeName='width' to='20'/%3E%3C/svg%3E\"/>"), Some((10, 5))),
    ];
    svg_sizes(&dir, &s, &cases);
}

// ---------------------------------------------------------------- SVGs that draw too much

// An SVG of 10 × 10 whose `body` follows the root's start tag.
fn svg_doc(body: &str) -> String {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 10 10\">{body}</svg>\n")
}

// An SVG that draws 10^levels squares in about a kilobyte: l0 is a square, each group l<n> in <defs> holds 10 <use>s
// of l<n-1>, and the drawing is 10 <use>s of the last. `refer(id)` is the attribute a <use> names a group by.
// Counted with each <use> drawn out, 4 levels are 24,687 elements, and 5 are over 200,000.
fn nested(levels: u32, refer: impl Fn(&str) -> String) -> String {
    let uses = |n: u32| format!("<use {}/>", refer(&format!("l{n}"))).repeat(10);
    let mut defs = "<rect id=\"l0\" width=\"1\" height=\"1\"/>".to_string();
    for n in 1..levels {
        defs += &format!("\n<g id=\"l{n}\">{}</g>", uses(n - 1));
    }
    svg_doc(&format!("<defs>{defs}</defs>\n<g>{}</g>", uses(levels - 1)))
}
fn href(id: &str) -> String {
    format!("href=\"#{id}\"")
}

// An SVG /api/image serves, and /api/file describes as one with a version.
fn svg_drawn(s: &Server, path: &str) {
    let res = image(s, path, &[]);
    assert_eq!(res.status, 200, "{path}: {}", res.text());
    let v = res.header("etag").unwrap().trim_matches('"');
    assert_eq!(file(s, path, &[])["image"], json!({ "type": "image/svg+xml", "width": 10, "height": 10, "v": v }), "{path}");
}

// An SVG refused for what it would draw, with no size: it was read only up to the refusal, and what came after could
// change it.
fn too_complex(s: &Server, path: &str) {
    let info = json!({ "type": "image/svg+xml", "refused": "complex" });
    refused(s, path, info, 413, &format!("{path} is too complex to draw (over 50,000 elements)"));
}

// A <use> draws a copy of the group it names, and that copy's <use>s draw theirs, so a kilobyte can draw millions of
// elements and hold a browser's tab for longer than 20 seconds. An SVG that would draw over 50,000 is
// refused, as /api/image refuses an image too large.
#[test]
fn an_svg_whose_uses_would_draw_over_50_000_elements_is_refused() {
    let (dir, s) = repo();
    let deep = nested(5, href);
    assert!(deep.len() < 2000, "{} bytes", deep.len());
    put(&dir, "deep.svg", deep.as_bytes());
    let info = json!({ "type": "image/svg+xml", "refused": "complex" });
    assert_eq!(file(&s, "deep.svg", &[]), json!({ "content": deep, "size": deep.len(), "image": info }));
    let res = image(&s, "deep.svg", &[]);
    assert_eq!((res.status, res.json()), (413, json!({ "error": "deep.svg is too complex to draw (over 50,000 elements)" })));
    // 10,000 squares are drawn.
    put(&dir, "four.svg", nested(4, href).as_bytes());
    svg_drawn(&s, "four.svg");

    // So is a flat drawing of 50,000 elements, the root among them, and one more is refused.
    put(&dir, "flat.svg", svg_doc(&"<rect/>".repeat(49_999)).as_bytes());
    svg_drawn(&s, "flat.svg");
    put(&dir, "flatter.svg", svg_doc(&"<rect/>".repeat(50_000)).as_bytes());
    too_complex(&s, "flatter.svg");
    // From a commit, as from the working tree.
    commit_all(dir.path(), "svgs");
    let at = [("mode", "commit"), ("ref", "HEAD"), ("path", "deep.svg")];
    assert_eq!(s.body("/api/file", &at)["image"], info);
    assert_eq!(s.get("/api/image", &at).status, 413);
}

// However a <use> writes the name of what it draws, it counts: href or xlink:href (under any prefix, since the document
// binds it), with references in place of characters, percent-encoded, or with spaces around it. A reference to
// another file can't load in an image, so it draws nothing, but a data: URL is a document of its own, not counted, so
// it is refused.
#[test]
fn a_use_counts_however_it_names_its_group() {
    let (dir, s) = repo();
    let refused_as = [
        ("xlink.svg", nested(5, |id| format!("xlink:href=\"#{id}\""))),
        ("prefix.svg", nested(5, |id| format!("xmlns:x=\"http://www.w3.org/1999/xlink\" x:href=\"#{id}\""))),
        ("entity.svg", nested(5, |id| format!("href=\"&#35;{}&#x{:x};\"", &id[..1], id.as_bytes()[1]))),
        ("percent.svg", nested(5, |id| format!("href=\"#%{:02X}{}\"", id.as_bytes()[0], &id[1..]))),
        ("spaced.svg", nested(5, |id| format!("href=\" #{id}&#10;\""))),
        ("both.svg", nested(5, |id| format!("href=\"#{id}\" xlink:href=\"#{id}\""))),
        // The id is the attribute's, not one in another's quoted value, where '>' doesn't end the tag either.
        ("decoy.svg", nested(5, href).replace("<g id=\"l4\">", "<g data-x=\"1 > 0\" title=' id=\"decoy\"' id=\"l4\">")),
        ("data.svg", svg_doc("<use href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'%3E%3C/svg%3E#a\"/>")),
    ];
    for (path, text) in &refused_as {
        put(&dir, path, text.as_bytes());
        too_complex(&s, path);
    }
    put(&dir, "xlink4.svg", nested(4, |id| format!("xlink:href=\"#{id}\"")).as_bytes());
    svg_drawn(&s, "xlink4.svg");
    put(&dir, "external.svg", nested(5, |id| format!("href=\"parts.svg#{id}\" xlink:href=\"/x/parts.svg#{id}\"")).as_bytes());
    svg_drawn(&s, "external.svg");
}

// A group that holds a <use> of itself, however far round, would draw without end.
#[test]
fn an_svg_whose_uses_draw_each_other_is_refused() {
    let (dir, s) = repo();
    let cases = [
        ("pair.svg", svg_doc("<defs><g id=\"a\"><use href=\"#b\"/></g><g id=\"b\"><rect/><use href=\"#a\"/></g></defs><use href=\"#a\"/>")),
        ("self.svg", svg_doc("<g id=\"a\"><rect/><use href=\"#a\"/></g>")),
        ("use.svg", svg_doc("<use id=\"u\" href=\"#u\"/>")),
        ("within.svg", svg_doc("<g id=\"a\"><g><g id=\"b\"><use href=\"#a\"/></g></g></g>")),
        // Even in a definition nothing draws.
        ("unused.svg", svg_doc("<defs><g id=\"a\"><use href=\"#a\"/></g></defs><rect/>")),
    ];
    for (path, text) in &cases {
        put(&dir, path, text.as_bytes());
        too_complex(&s, path);
    }
    // The root's id is a group like any other.
    let root = "<svg id=\"top\" viewBox=\"0 0 10 10\"><use href=\"#top\"/></svg>\n";
    put(&dir, "root.svg", root.as_bytes());
    too_complex(&s, "root.svg");
    // A group drawn twice, or a <use> of a group inside it, isn't a cycle.
    let twice = svg_doc("<g id=\"a\"><rect/><g id=\"b\"/><use href=\"#b\"/></g><use href=\"#a\"/><use href=\"#a\"/>");
    put(&dir, "twice.svg", twice.as_bytes());
    svg_drawn(&s, "twice.svg");
}

// A browser stops at the first error in an SVG's markup, so where it isn't well formed the count goes on as far as it
// can and only ever counts more: an element left open holds the rest of the file, and a stray end tag is skipped. A
// file that draws too much is refused for that, well formed or not; one that doesn't is refused as malformed. Each of
// these is answered (the client gives up after 20 seconds), without a panic (a 500).
#[test]
fn markup_that_isnt_well_formed_is_still_counted_and_never_hangs() {
    let (dir, s) = repo();
    let deep = nested(5, href);
    let svg_tag = deep.split_inclusive('>').next().unwrap();
    let refused_as = [
        // End tags that close nothing, and one that closes the root before its content.
        ("stray.svg", deep.replacen("<defs>", "</g></use></svg><defs></rect>", 1)),
        // Every group left open, so each holds those after it, and their <use>s of it.
        ("unclosed.svg", deep.replace("</g>", "")),
        // End tags of the wrong name: the first closes <defs> and the group in it, the rest nothing.
        ("mismatched.svg", deep.replace("</g>", "</defs>")),
        // Cut short inside a quoted value or a comment, after the <use>s.
        ("cut.svg", format!("{}<g id=\"x", deep.strip_suffix("</svg>\n").unwrap())),
        ("comment.svg", format!("{}<!-- no end", deep.strip_suffix("</svg>\n").unwrap())),
    ];
    for (path, text) in &refused_as {
        put(&dir, path, text.as_bytes());
        too_complex(&s, path);
    }
    // A CDATA section, a comment and a processing instruction hold no tags: were these read, the group would draw itself.
    let hidden = svg_doc(&format!(
        "<g id=\"a\"><![CDATA[<use href=\"#a\"/>]]><!-- <use href=\"#a\"/> --><?pi <use href=\"#a\"/> ?><rect/></g>{}",
        "<use href=\"#a\"/>".repeat(10)
    ));
    put(&dir, "hidden.svg", hidden.as_bytes());
    svg_drawn(&s, "hidden.svg");
    // Malformed in small ways, each counted to its end and refused for its first error.
    let broken = [
        (format!("{svg_tag}<g><rect></svg>\n"), "</svg> doesn't close <rect>"),
        (format!("{svg_tag}</g></g><rect/>"), "</g> doesn't close <svg>"),
        (format!("{svg_tag}< g><g/ ><g a\"b c='d'>"), "a '<' that starts no tag (in text, write &lt;)"),
        (format!("{svg_tag}<"), "a '<' that starts no tag (in text, write &lt;)"),
        (format!("{svg_tag}</"), "a '</' that starts no end tag"),
        (format!("{svg_tag}<g id=\"a\" href=\"#a\"><use"), "the start tag of <use> isn't ended by '>'"),
        (format!("{svg_tag}<use href=\"#a&#99999999999;&#x;&bogus;&\"/><g id=\"a\"/>"), "&#99999999999; is a character XML doesn't allow"),
    ];
    for (n, (text, reason)) in broken.iter().enumerate() {
        let path = format!("broken{n}.svg");
        put(&dir, &path, text.as_bytes());
        malformed(&s, &path, reason, 1);
    }
    // What would take a naive reader quadratic time, longer than the client waits: 400,000 stray end tags under 40,000
    // open elements, and one <use> with 100,000 references.
    let open = svg_doc(&format!("{}{}", "<g>".repeat(40_000), "</x>".repeat(400_000)));
    put(&dir, "open.svg", open.as_bytes());
    malformed(&s, "open.svg", "elements nested over 5,000 deep", 1);
    let refs: String = (0..100_000).map(|n| format!(" href=\"#n{n}\"")).collect();
    put(&dir, "refs.svg", svg_doc(&format!("<use{refs}/>")).as_bytes());
    malformed(&s, "refs.svg", "<use> has two href attributes", 1);
    // A chain of 20,000 groups, each a <use> of the next, is walked 20,000 deep without recursion (which would overflow
    // the stack), and is refused: its <use>s, each counted where it stands, draw about 400 million.
    let chain: String = (1..20_000).rev().map(|n| format!("<g id=\"c{n}\"><use href=\"#c{}\"/></g>", n - 1)).collect();
    put(&dir, "chain.svg", svg_doc(&format!("<defs><rect id=\"c0\"/>{chain}</defs>")).as_bytes());
    too_complex(&s, "chain.svg");
}

// ---------------------------------------------------------------- SVGs browsers draw nothing of

// An SVG refused as not well formed, with the reason and the line /api/file gives, which /api/image's 415 says too.
fn malformed(s: &Server, path: &str, reason: &str, line: usize) {
    let info = json!({ "type": "image/svg+xml", "refused": "malformed", "reason": reason, "line": line });
    refused(s, path, info, 415, &format!("{path} is not a well-formed SVG: {reason} (line {line})"));
}

const SVG_NS: &str = "xmlns=\"http://www.w3.org/2000/svg\"";
const SVG11: &str = "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">";

// A doctype with the public ID `id`.
fn public(id: &str) -> String {
    format!("<!DOCTYPE svg PUBLIC \"{id}\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">")
}

// An SVG of 40 × 30 whose root's start tag is on line 1, after `before` (an XML declaration, a doctype), and `body`
// from line 2.
fn svg_lines(before: &str, body: &str) -> Vec<u8> {
    format!("{before}<svg {SVG_NS} width=\"40\" height=\"30\">\n{body}\n</svg>\n").into_bytes()
}

// The same with `bytes`, which needn't be UTF-8, in its body between `pre` and `post`.
fn svg_bytes(before: &str, pre: &str, bytes: &[u8], post: &str) -> Vec<u8> {
    let mut svg = format!("{before}<svg {SVG_NS} width=\"40\" height=\"30\">\n{pre}").into_bytes();
    svg.extend_from_slice(bytes);
    svg.extend_from_slice(format!("{post}\n</svg>\n").as_bytes());
    svg
}

// Browsers draw nothing of an SVG that isn't well-formed XML (with namespaces) or whose root isn't an <svg> in the SVG
// namespace: Chromium 156 and Firefox 157 both fail to load each of these as an <img>. So rather than an image that
// can't be loaded, /api/file says why and where, as the first error the file has, and /api/image refuses it with that.
// Each was drawn in both browsers, and those that either browser draws, below, are not refused: a refusal hides the
// image in every browser.
#[test]
fn an_svg_that_isnt_well_formed_is_refused_with_why_and_where() {
    let (dir, s) = repo();
    let svg = |body: &str| svg_lines("", body);
    let after = |end: &str| [svg(""), end.as_bytes().to_vec()].concat();
    let root = |attrs: &str| format!("<svg {attrs}><rect/></svg>\n").into_bytes();
    let deep = |n: usize| svg(&format!("{}{}", "<g>".repeat(n - 1), "</g>".repeat(n - 1)));
    let no_tag = "a '<' that starts no tag (in text, write &lt;)";
    let slash = "a '/' in <rect> that isn't right before its '>'";
    let dashes = "\"--\" in a comment";
    let late = "an XML declaration that isn't at the very start";
    let no_svg = "no xmlns=\"http://www.w3.org/2000/svg\" on the root <svg>";
    let not_utf8 = "bytes that aren't UTF-8";
    let undeclare = "xmlns:x=\"\" can't undeclare a prefix";
    let cases = [
        // Tags. Lines end at "\n", "\r\n" or a "\r" alone.
        ("mismatched", svg("<g>\n<rect/>\n</h>\n</g>"), "</h> doesn't close <g>", 4),
        ("crlf", format!("<svg {SVG_NS}>\r\n<g>\r\n</h></g></svg>").into_bytes(), "</h> doesn't close <g>", 3),
        ("cr", format!("<svg {SVG_NS}>\r<g>\r\r</h></g></svg>").into_bytes(), "</h> doesn't close <g>", 4),
        ("unclosed-root", format!("<svg {SVG_NS}>\n<rect/>\n").into_bytes(), "<svg> isn't closed", 1),
        ("unclosed", svg("<g>\n<rect/>"), "</svg> doesn't close <g>", 4),
        ("second-root", after("<svg/>"), "<svg> after the root element ends", 4),
        ("text-after", after("hello"), "text after the root element ends", 4),
        ("element-after", after("<rect/>"), "<rect> after the root element ends", 4),
        ("doctype-inside", svg("<!DOCTYPE svg>"), "a doctype after the root element starts", 2),
        ("doctype-after", after("<!DOCTYPE svg>"), "a doctype after the root element starts", 4),
        ("spaced-tag", svg("< rect/>"), no_tag, 2),
        ("spaced-end", svg("<g></ g>"), "a '</' that starts no end tag", 2),
        ("end-attribute", svg("<g></g a=\"1\">"), "the end tag </g> holds more than its name", 2),
        ("deep", deep(5_001), "elements nested over 5,000 deep", 2),
        // Attributes.
        ("repeated", svg("<rect width=\"8\" width=\"6\"/>"), "<rect> has two width attributes", 2),
        ("repeated-root", root(&format!("{SVG_NS} width=\"40\" width=\"30\"")), "<svg> has two width attributes", 1),
        ("no-value", svg("<rect hidden/>"), "hidden on <rect> has no value", 2),
        ("unquoted", svg("<rect width=8/>"), "the value of width on <rect> isn't quoted", 2),
        ("unclosed-quote", svg("<rect class=\"a/>"), "the value of class on <rect> isn't closed", 2),
        ("unspaced", svg("<rect a=\"1\"b=\"2\"/>"), "no space before b in <rect>", 2),
        ("unspaced-root", root(&format!("{SVG_NS} width=\"40\"height=\"30\"")), "no space before height in <svg>", 1),
        ("lt-in-value", svg("<rect class=\"a<b\"/>"), "a '<' in the value of class on <rect>", 2),
        ("lt-spaced", svg("<rect title=\"a < b\"/>"), "a '<' in the value of title on <rect>", 2),
        ("slash", svg("<rect / >"), slash, 2),
        ("slash-before", svg("<rect/ >"), slash, 2),
        // Text and references: XML defines five entities, which a doctype naming a DTD adds none to, unless its public
        // ID is one of XHTML's or MathML's (see the drawn cases), exactly.
        ("amp", svg("<text>a & b</text>"), "a '&' that starts no reference (in text, write &amp;)", 2),
        ("lt", svg("<text>a < b</text>"), no_tag, 2),
        ("cdata-end", svg("<text>a ]]> b</text>"), "\"]]>\" in text", 2),
        ("nbsp", svg_lines(SVG11, "<text>&nbsp;</text>"), "&nbsp; isn't defined", 2),
        ("nbsp-bare", svg("<text>&nbsp;</text>"), "&nbsp; isn't defined", 2),
        ("nbsp-basic", svg_lines(&public("-//W3C//DTD XHTML Basic 1.1//EN"), "<text>&nbsp;</text>"), "&nbsp; isn't defined", 2),
        ("nbsp-case", svg_lines(&public("-//W3C//DTD XHTML 1.1//en"), "<text>&nbsp;</text>"), "&nbsp; isn't defined", 2),
        ("copy", svg_lines("<!DOCTYPE svg SYSTEM \"svg.dtd\">", "<text>&copy;</text>"), "&copy; isn't defined", 2),
        ("upper-x", svg("<text>&#X41;</text>"), "&#X41; isn't a character reference", 2),
        ("no-digits", svg("<text>&#;</text>"), "&#; isn't a character reference", 2),
        ("no-semicolon", svg("<text>&amp</text>"), "&amp isn't ended by ';'", 2),
        ("ref-control", svg("<text>&#1;</text>"), "&#1; is a character XML doesn't allow", 2),
        ("ref-nul", svg("<text>&#0;</text>"), "&#0; is a character XML doesn't allow", 2),
        ("ref-surrogate", svg("<text>&#xD800;</text>"), "&#xD800; is a character XML doesn't allow", 2),
        ("ref-fffe", svg("<text>&#xFFFE;</text>"), "&#xFFFE; is a character XML doesn't allow", 2),
        ("ref-over", svg("<text>&#x110000;</text>"), "&#x110000; is a character XML doesn't allow", 2),
        // Comments, CDATA sections, processing instructions and the XML declaration.
        ("dashes", svg("<!-- a -- b -->"), dashes, 2),
        ("dash-end", svg("<!-- a --->"), dashes, 2),
        ("cdata-open", svg("<style><![CDATA[rect {}</style>"), "a CDATA section isn't closed", 2),
        ("cdata-case", svg("<![cdata[ x ]]>"), "a '<!' that starts no comment or CDATA section", 2),
        ("no-target", svg("<? ?>"), "a processing instruction without a target", 2),
        ("late-declaration", svg("<?xml version=\"1.0\"?>"), late, 2),
        ("spaced-declaration", svg_lines(" <?xml version=\"1.0\"?>", ""), late, 1),
        ("commented-declaration", svg_lines("<!-- x --><?xml version=\"1.0\"?>", ""), late, 1),
        ("upper-declaration", svg_lines("<?XML version=\"1.0\"?>", ""), "a processing instruction can't be named XML", 1),
        ("version", svg_lines("<?xml version=\"2.0\"?>", ""), "version=\"2.0\" isn't an XML version", 1),
        ("no-version", svg_lines("<?xml encoding=\"UTF-8\"?>", ""), "an XML declaration without a version", 1),
        ("pseudo-attribute", svg_lines("<?xml version=\"1.0\" foo=\"bar\"?>", ""), "foo isn't part of an XML declaration", 1),
        ("doctype-case", svg_lines("<!doctype svg>", ""), "a doctype is written <!DOCTYPE", 1),
        ("doctype-keyword", svg_lines("<!DOCTYPE svg SYSTEMx \"a\">", ""), "the doctype has no quoted system ID", 1),
        // Characters: those XML doesn't allow, and bytes that aren't UTF-8 where it is the encoding.
        ("fffe", svg("<text>\u{fffe}</text>"), "U+FFFE isn't allowed in XML", 2),
        ("ffff", svg("<text>\u{ffff}</text>"), "U+FFFF isn't allowed in XML", 2),
        ("control", svg("<text>a\u{1}b</text>"), "U+0001 isn't allowed in XML", 2),
        // ESC too, in UTF-8: only under a legacy encoding, where it can switch to ISO-2022-JP, is it refused for that.
        ("escape", svg("<text>a\u{1b}b</text>"), "U+001B isn't allowed in XML", 2),
        ("latin1", svg_bytes("", "<text>", &[0xe9], "</text>"), not_utf8, 2),
        ("latin1-comment", svg_bytes("", "<!-- ", &[0xe9], " -->"), not_utf8, 2),
        ("latin1-attribute", svg_bytes("", "<rect class=\"", &[0xe9], "\"/>"), not_utf8, 2),
        ("overlong", svg_bytes("", "<text>", &[0xc0, 0xaf], "</text>"), not_utf8, 2),
        ("surrogate", svg_bytes("", "<text>", &[0xed, 0xa0, 0x80], "</text>"), not_utf8, 2),
        ("utf8-label", svg_bytes("<?xml version=\"1.0\" encoding=\"utf8\"?>", "<text>", &[0xe9], "</text>"), not_utf8, 2),
        ("utf16-label", svg_bytes("<?xml version=\"1.0\" encoding=\"UTF-16\"?>", "<text>", &[0xe9], "</text>"), not_utf8, 2),
        ("replacement-label", svg_bytes("<?xml version=\"1.0\" encoding=\"iso-2022-kr\"?>", "<text>", &[0xe9], "</text>"), not_utf8, 2),
        ("bom-latin1", [b"\xef\xbb\xbf".to_vec(), svg_bytes("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>", "<text>", &[0xe9], "</text>")].concat(), not_utf8, 2),
        // Names and namespaces.
        ("digit", svg("<1a/>"), "a name can't start with '1'", 2),
        ("colon", svg("<:a/>"), "the name :a starts with ':'", 2),
        ("colons", svg("<a:b:c xmlns:a=\"urn:x\"/>"), "the name a:b:c has more than one ':'", 2),
        ("nbsp-name", svg("<rect\u{a0}width=\"1\"/>"), "U+00A0 in <rect>, where an attribute's name should be", 2),
        ("prefix", svg("<a:b/>"), "the prefix a of <a:b> isn't declared", 2),
        ("xlink", svg("<use xlink:href=\"#a\"/>"), "the prefix xlink of xlink:href on <use> isn't declared", 2),
        ("undeclared", root(&format!("{SVG_NS} xmlns:x=\"\"")), undeclare, 1),
        ("undeclared-within", svg("<g xmlns:x=\"\"/>"), undeclare, 2),
        ("rebound-xml", svg("<g xmlns:xml=\"urn:x\"/>"), "the prefix xml can't be bound to another namespace", 2),
        ("bound-xmlns", svg("<g xmlns:xmlns=\"urn:x\"/>"), "the prefix xmlns can't be declared", 2),
        ("same-attribute", svg("<rect xmlns:a=\"urn:u\" xmlns:b=\"urn:u\" a:x=\"1\" b:x=\"2\"/>"), "a:x and b:x on <rect> are the same attribute", 2),
        ("out-of-scope", svg("<g xmlns:a=\"urn:x\"><a:r/></g>\n<a:r/>"), "the prefix a of <a:r> isn't declared", 3),
        ("open-br", svg("<foreignObject><div xmlns=\"http://www.w3.org/1999/xhtml\">a<br>b</div></foreignObject>"), "</div> doesn't close <br>", 2),
        ("no-xmlns", root("width=\"40\" height=\"30\""), no_svg, 1),
        ("prefixed-xmlns", root("xmlns:svg=\"http://www.w3.org/2000/svg\""), no_svg, 1),
        ("xhtml", root("xmlns=\"http://www.w3.org/1999/xhtml\""), "the root <svg> is in the namespace \"http://www.w3.org/1999/xhtml\", not \"http://www.w3.org/2000/svg\"", 1),
    ];
    for (name, text, reason, line) in &cases {
        let path = format!("{name}.svg");
        put(&dir, &path, text);
        malformed(&s, &path, reason, *line);
    }

    let latin1 = |label: &str, pre: &str, bytes: &[u8], post: &str| svg_bytes(&format!("<?xml version=\"1.0\" encoding=\"{label}\"?>"), pre, bytes, post);
    let html_entities = |id: &str| svg_lines(&public(id), "<text id=\"a&eacute;\">a&nbsp;b&hellip;&alpha;</text>");
    let drawn = [
        // Both browsers draw these.
        ("references", svg("<text>&amp;&#65;&#x41;&lt;&#0065;&#x9;&apos;&quot;&gt;</text>")),
        // Under a doctype whose public ID is one of these, both define HTML's entities, whatever the root's name.
        ("xhtml-transitional", html_entities("-//W3C//DTD XHTML 1.0 Transitional//EN")),
        ("xhtml-strict", html_entities("-//W3C//DTD XHTML 1.0 Strict//EN")),
        ("xhtml-frameset", html_entities("-//W3C//DTD XHTML 1.0 Frameset//EN")),
        ("xhtml11", html_entities("-//W3C//DTD XHTML 1.1//EN")),
        ("xhtml-basic", html_entities("-//W3C//DTD XHTML Basic 1.0//EN")),
        ("xhtml-mathml", html_entities("-//W3C//DTD XHTML 1.1 plus MathML 2.0//EN")),
        ("xhtml-mathml-svg", html_entities("-//W3C//DTD XHTML 1.1 plus MathML 2.0 plus SVG 1.1//EN")),
        ("mathml", html_entities("-//W3C//DTD MathML 2.0//EN")),
        ("mobile10", html_entities("-//WAPFORUM//DTD XHTML Mobile 1.0//EN")),
        ("mobile11", html_entities("-//WAPFORUM//DTD XHTML Mobile 1.1//EN")),
        ("mobile12", html_entities("-//WAPFORUM//DTD XHTML Mobile 1.2//EN")),
        ("html-root", svg_lines("<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"about:blank\">", "<text>&nbsp;</text>")),
        ("gt", svg("<text class=\"a>b\">a > b ]] ></text><rect class='a\"b' id=\"x\ny\"/>")),
        ("characters", svg("<text>\u{fdd0}\u{85}\u{2028}\u{7f}\u{80}</text>")),
        ("spaced", format!("<svg {SVG_NS} width = \"40\"  height =\n\"30\"><g></g ></svg>").into_bytes()),
        ("leading-space", format!("  \n<svg {SVG_NS}/>").into_bytes()),
        ("bom", svg_lines("\u{feff}<?xml version=\"1.0\"?>", "")),
        ("declaration", svg_lines("<?xml version='1.0' encoding='bogus-enc' standalone=\"yes\"?>", "")),
        ("utf16-declared", svg_lines("<?xml version=\"1.0\" encoding=\"UTF-16\"?>", "")),
        ("xml-space", svg("<text xml:space=\"preserve\">a</text><g xmlns=\"\"><x/></g>")),
        ("xml-bound", root(&format!("{SVG_NS} xmlns:xml=\"http://www.w3.org/XML/1998/namespace\""))),
        ("xlink-declared", svg_lines("", "<use xmlns:xlink=\"http://www.w3.org/1999/xlink\" xlink:href=\"#a\"/>")),
        ("non-ascii", svg("<é/>")),
        ("pis", svg_lines("<?xml-stylesheet href=\"#x\"?>", "<?xmlfoo x?><?p x?>")),
        ("hidden-markup", svg("<!-- & < --><style><![CDATA[ a & b < c ]]></style><![CDATA[ x ]]><!--- a -->")),
        ("doctype", svg_lines(SVG11, "")),
        ("long-name", svg(&format!("<{}/>", "a".repeat(5_000)))),
        ("attributes", svg(&format!("<rect {}/>", (0..3_000).map(|n| format!("a{n}=\"1\"")).collect::<Vec<_>>().join(" ")))),
        ("deep-enough", deep(5_000)),
        ("after-root", after("\n<!-- x -->\n<?p x?>\n")),
        // Chromium draws these, and Firefox doesn't, or the other way round: they are let be.
        ("xml11", svg_lines("<?xml version=\"1.1\"?>", "")),
        ("xmlns-twice", root(&format!("{SVG_NS} {SVG_NS}"))),
        ("pi-colon", svg("<?a:b x?>")),
        ("local-digit", svg("<a:1b xmlns:a=\"urn:x\"/>")),
        ("xml-element", svg("<xml:a/>")),
        ("bound-to-xml", svg("<g xmlns:a=\"http://www.w3.org/XML/1998/namespace\"/>")),
        ("encoding-name", svg_lines("<?xml version=\"1.0\" encoding=\"8bit\"?>", "")),
        ("doctype-unspaced", svg_lines("<!DOCTYPEsvg SYSTEM\"a\">", "")),
        // A '<' followed by '/' or '!' in a value, after which Chromium reads on as XML doesn't.
        ("lt-slash", svg("<rect class=\"</b\"/>")),
        ("lt-comment", svg("<rect class=\"<!--x-->\"/>")),
        ("lt-cdata", svg("<rect a=\"<![CDATA[\" b=\"]]>\"/>")),
        ("fifth-edition", svg("<a\u{221}/><a\u{1f600}/>")),
        ("latin1-declared", latin1("ISO-8859-1", "<text>", &[0xe9], "</text>")),
        ("windows-1252", latin1("windows-1252", "<text>", &[0x93], "</text>")),
        ("latin1-name", latin1("ISO-8859-1", "<", &[0xe9], "/>")),
        ("shift-jis", latin1("Shift_JIS", "<text>", &[0x83, 0x41], "</text>")),
        // Two-byte characters of Shift_JIS that end in '[' or ']', which no name can hold and which could end "]]>".
        ("shift-jis-name", latin1("Shift_JIS", "<a", &[0x83, 0x5b], "/>")),
        ("shift-jis-brackets", latin1("Shift_JIS", "<text>", &[0x83, 0x5d], "]></text>")),
    ];
    for (name, text) in &drawn {
        let path = format!("{name}.svg");
        put(&dir, &path, text);
        let res = image(&s, &path, &[]);
        assert_eq!(res.status, 200, "{path}: {}", res.text());
        let info = &file(&s, &path, &[])["image"];
        assert!(info["refused"].is_null() && info["v"].is_string(), "{path}: {info}");
    }
}

// Browsers draw an SVG without an SVG it embeds that isn't well formed (in both, one that was to cover half the drawing
// left that half empty), so that is no reason to refuse the file. And what is refused for what it draws or embeds is
// refused for that, well formed or not.
#[test]
fn an_embedded_svg_that_isnt_well_formed_leaves_the_file_drawn() {
    let (dir, s) = repo();
    let broken = [
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><g></h></svg>",
        "<svg><rect/></svg>",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>&nbsp;</text></svg>",
    ];
    for (n, inner) in broken.iter().enumerate() {
        let path = format!("holder{n}.svg");
        put(&dir, &path, svg_doc(&image_of(&svg_url(inner))).as_bytes());
        svg_drawn(&s, &path);
    }
    put(&dir, "huge.svg", format!("{}<g>", svg_doc(&image_of(&png_url(&png(100_000, 100_000))))).as_bytes());
    embeds_too_much(&s, "huge.svg");
    put(&dir, "deep.svg", format!("{}<g>", nested(5, href)).as_bytes());
    too_complex(&s, "deep.svg");
}

// ESC is no character of XML. It switches ISO-2022-JP, which a document may declare, and whose two-byte characters can
// read as markup to a reader of bytes where a browser reads text: here, a comment that would hide the <use>s.
#[test]
fn an_svg_that_switches_encodings_is_refused() {
    let (dir, s) = repo();
    let deep = nested(5, href);
    let svg_end = deep.find('>').unwrap() + 1;
    let jis = format!("<?xml version=\"1.0\" encoding=\"ISO-2022-JP\"?>{}\x1b$B<!--\x1b(B{}", &deep[..svg_end], &deep[svg_end..]);
    put(&dir, "jis.svg", jis.as_bytes());
    too_complex(&s, "jis.svg");
}

// A <use> can get its href from an animation, which runs in an image too, and an animation can name the <use> it sets
// by an href of its own, so the copy may be made anywhere: what it draws isn't counted, and such a file is refused.
// Here each level's <use>s get theirs from a <set> (10^5 squares) or an <animate>'s values, or from <set>s outside
// every group. An animation of anything else, or one that sets an href to no element of the file, is let be.
#[test]
fn an_svg_whose_uses_get_their_href_from_an_animation_is_refused() {
    let (dir, s) = repo();
    let levels = |anim: &dyn Fn(u32) -> String| {
        let mut defs = "<rect id=\"l0\" width=\"1\" height=\"1\"/>".to_string();
        for n in 1..6 {
            defs += &format!("<g id=\"l{n}\">{}</g>", format!("<use>{}</use>", anim(n - 1)).repeat(10));
        }
        svg_doc(&format!("<defs>{defs}</defs><use href=\"#l5\"/>"))
    };
    let set = levels(&|n| format!("<set attributeName=\"href\" to=\"#l{n}\"/>"));
    put(&dir, "set.svg", set.as_bytes());
    too_complex(&s, "set.svg");
    let xlink = levels(&|n| format!("<set attributeName=\" xlink:href \" to=\"&#35;l{n}\"/>"));
    put(&dir, "xlink.svg", xlink.as_bytes());
    too_complex(&s, "xlink.svg");
    let values = levels(&|n| format!("<animate attributeName=\"href\" values=\"#;  #l{n} \" dur=\"1s\"/>"));
    put(&dir, "values.svg", values.as_bytes());
    too_complex(&s, "values.svg");
    let mut outside = String::new();
    let mut defs = "<rect id=\"l0\"/>".to_string();
    for n in 1..5 {
        defs += &format!("<g id=\"l{n}\">{}</g>", (0..10).map(|k| format!("<use id=\"u{n}-{k}\"/>")).collect::<String>());
        outside += &(0..10).map(|k| format!("<set href=\"#u{n}-{k}\" attributeName=\"href\" to=\"#l{}\"/>", n - 1)).collect::<String>();
    }
    put(&dir, "outside.svg", svg_doc(&format!("<defs>{defs}</defs><use href=\"#l4\"/>{outside}")).as_bytes());
    too_complex(&s, "outside.svg");

    let fill = levels(&|_| "<set attributeName=\"fill\" to=\"#f00\"/>".to_string());
    put(&dir, "fill.svg", fill.as_bytes());
    svg_drawn(&s, "fill.svg");
    let nowhere = levels(&|_| "<set attributeName=\"href\" to=\"other.svg#l1\"/><animate attributeName=\"href\" values=\"#\"/>".to_string());
    put(&dir, "nowhere.svg", nowhere.as_bytes());
    svg_drawn(&s, "nowhere.svg");
}

// ---------------------------------------------------------------- SVGs that embed images

// An <img> loads no other file, but it does load data: URLs, and design tools write pictures into the SVGs they export
// as <image>s of one. So an SVG's data: URLs are checked as files are: a raster image by its header, an SVG by what it
// draws, and what can't be checked is refused.

fn base64(data: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | ((b as u32) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() { DIGITS[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

// Every byte but a letter or a digit as %XX, which is safe in a quoted attribute, a style sheet and its strings.
fn percent(data: &[u8]) -> String {
    data.iter().map(|&b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn png_url(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64(png))
}
fn svg_url(text: &str) -> String {
    format!("data:image/svg+xml,{}", percent(text.as_bytes()))
}
fn svg_url64(text: &str) -> String {
    format!("data:image/svg+xml;base64,{}", base64(text.as_bytes()))
}
fn jpeg_url(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64(jpeg))
}
const XHTML: &str = "xmlns=\"http://www.w3.org/1999/xhtml\"";

fn image_of(url: &str) -> String {
    format!("<image width=\"10\" height=\"10\" href=\"{url}\"/>")
}

// A way of writing an image into an SVG, given the image, or given its data: URL.
type ImageForm = fn(&[u8]) -> String;
type UrlForm = fn(&str) -> String;

// An SVG refused for what it embeds, with no size (see too_complex).
fn embeds_too_much(s: &Server, path: &str) {
    let info = json!({ "type": "image/svg+xml", "refused": "embedded" });
    refused(s, path, info, 413, &format!("{path} embeds an image revue can't check, or images of over 50 million pixels in all"));
}

// A PNG of 10 × 10 is drawn and one whose header claims 100,000 × 100,000 is refused, as each would be on its own.
// Images under the limit each count together: a browser decodes all of them.
#[test]
fn an_svg_whose_embedded_images_take_too_many_pixels_is_refused() {
    let (dir, s) = repo();
    put(&dir, "small.svg", svg_doc(&image_of(&png_url(&png(10, 10)))).as_bytes());
    svg_drawn(&s, "small.svg");
    put(&dir, "huge.svg", svg_doc(&image_of(&png_url(&png(100_000, 100_000)))).as_bytes());
    embeds_too_much(&s, "huge.svg");

    // 49 million pixels are drawn, twice that is refused, and so is a million 51 times over, but not 50 times.
    let under = image_of(&png_url(&png(7_000, 7_000)));
    put(&dir, "under.svg", svg_doc(&under).as_bytes());
    svg_drawn(&s, "under.svg");
    put(&dir, "twice.svg", svg_doc(&under.repeat(2)).as_bytes());
    embeds_too_much(&s, "twice.svg");
    let million = image_of(&png_url(&png(1_000, 1_000)));
    put(&dir, "fifty.svg", svg_doc(&million.repeat(50)).as_bytes());
    svg_drawn(&s, "fifty.svg");
    put(&dir, "fifty-one.svg", svg_doc(&million.repeat(51)).as_bytes());
    embeds_too_much(&s, "fifty-one.svg");
    // An embedded SVG's images count with the file's.
    put(&dir, "within.svg", svg_doc(&format!("{under}{}", image_of(&svg_url(&svg_doc(&under))))).as_bytes());
    embeds_too_much(&s, "within.svg");
}

// An embedded image counts the pixels decoding it takes, as a file does, not the size its EXIF has it drawn at: two
// JPEGs of 49 million pixels each, drawn at 70 × 70, are too many, and one of 25 million drawn at 10,000 × 10,000 isn't.
#[test]
fn an_embedded_image_counts_the_pixels_it_stores_not_the_size_drawn() {
    let (dir, s) = repo();
    let small = jpeg_exif(7_000, 7_000, &resolution(7_200, 7_200, (70, 70)));
    put(&dir, "small.jpg", &small);
    shown(&s, "small.jpg", &small, "image/jpeg", (70, 70));
    put(&dir, "twice.svg", svg_doc(&image_of(&jpeg_url(&small)).repeat(2)).as_bytes());
    embeds_too_much(&s, "twice.svg");
    let large = jpeg_exif(5_000, 5_000, &resolution(36, 36, (10_000, 10_000)));
    put(&dir, "large.jpg", &large);
    shown(&s, "large.jpg", &large, "image/jpeg", (10_000, 10_000));
    put(&dir, "large.svg", svg_doc(&image_of(&jpeg_url(&large))).as_bytes());
    svg_drawn(&s, "large.svg");
}

// An embedded SVG draws its own elements: it is counted as the file is, whether percent-escaped or in base64, and what
// it draws is drawn again in each copy a <use> makes of a group around it.
#[test]
fn an_embedded_svg_is_counted_with_the_drawing() {
    let (dir, s) = repo();
    let deep = nested(5, href);
    put(&dir, "bomb.svg", svg_doc(&image_of(&svg_url(&deep))).as_bytes());
    too_complex(&s, "bomb.svg");
    put(&dir, "bomb64.svg", svg_doc(&image_of(&svg_url64(&deep))).as_bytes());
    too_complex(&s, "bomb64.svg");
    // 4 levels draw 24,687 elements, which the group around them holds with itself and the <image>: 24,689. Drawn
    // where it stands and once more, that is 49,381 with the root, <defs> and the <use>; drawn twice more, too many.
    let four = svg_doc(&format!("<defs><g id=\"a\">{}</g></defs><use href=\"#a\"/>", image_of(&svg_url(&nested(4, href)))));
    put(&dir, "once.svg", four.as_bytes());
    svg_drawn(&s, "once.svg");
    put(&dir, "copied.svg", four.replace("<use href=\"#a\"/>", "<use href=\"#a\"/><use href=\"#a\"/>").as_bytes());
    too_complex(&s, "copied.svg");
}

// An SVG in an SVG in an SVG is read three levels down, each level read again; one a level deeper is refused.
#[test]
fn svgs_embedded_over_three_levels_deep_are_refused() {
    let (dir, s) = repo();
    let mut doc = svg_doc("<rect/>");
    for _ in 0..3 {
        doc = svg_doc(&image_of(&svg_url64(&doc)));
    }
    put(&dir, "three.svg", doc.as_bytes());
    svg_drawn(&s, "three.svg");
    put(&dir, "four.svg", svg_doc(&image_of(&svg_url64(&doc))).as_bytes());
    embeds_too_much(&s, "four.svg");
}

// However a data: URL is written, and wherever an image can load one from, it is read as a browser reads it. Each form
// of a small PNG is drawn (so it was decoded) and the same form of one too large is refused.
#[test]
fn an_embedded_image_is_read_however_and_wherever_its_url_is_written() {
    let (dir, s) = repo();
    let forms: Vec<(&str, ImageForm)> = vec![
        // Percent-escaped instead of base64, with references for its characters, in any case with spaces around its
        // parts, its base64 on several lines, with a fragment, which is no part of the data, or without a type or
        // with another one, since a browser tells an image by its bytes.
        ("percent", |p| image_of(&format!("data:image/png,{}", percent(p)))),
        ("references", |p| image_of(&format!("&#100;ata&#x3A;image/png;base64,{}", base64(p)))),
        ("spaced", |p| image_of(&format!(" DATA:Image/PNG ; Base64 ,{} ", base64(p)))),
        ("lines", |p| image_of(&format!("data:image/png;base64,{}", base64(p).as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c)).collect::<Vec<_>>().join("\n  ")))),
        ("fragment", |p| image_of(&format!("{}#top", png_url(p)))),
        ("untyped", |p| image_of(&format!("data:;base64,{}", base64(p)))),
        ("text", |p| image_of(&format!("data:text/plain;base64,{}", base64(p)))),
        // Other attributes and elements: xlink:href, a filter's image, an animation's (to or one of its values), and an
        // HTML image in a <foreignObject>, by its src or a srcset's other candidate.
        ("xlink", |p| format!("<image xlink:href=\"{}\"/>", png_url(p))),
        ("filter", |p| format!("<filter id=\"f\"><feImage href=\"{}\"/></filter>", png_url(p))),
        ("set", |p| format!("<image href=\"#\"><set attributeName=\"href\" to=\"{}\"/></image>", png_url(p))),
        ("values", |p| format!("<image href=\"#\"><animate attributeName=\"href\" values=\"#;data:image/png,{}\" dur=\"1s\"/></image>", percent(p))),
        ("img", |p| format!("<foreignObject width=\"10\" height=\"10\"><img {XHTML} src=\"{}\"/></foreignObject>", png_url(p))),
        ("srcset", |p| format!("<foreignObject width=\"10\" height=\"10\"><img {XHTML} srcset=\"a.png 1x,{} 2x\"/></foreignObject>", png_url(p))),
    ];
    let (small, huge) = (png(10, 10), png(100_000, 100_000));
    for (name, form) in &forms {
        put(&dir, &format!("{name}.svg"), svg_doc(&form(&small)).as_bytes());
        svg_drawn(&s, &format!("{name}.svg"));
        put(&dir, &format!("{name}-huge.svg"), svg_doc(&form(&huge)).as_bytes());
        embeds_too_much(&s, &format!("{name}-huge.svg"));
    }
}

// CSS loads images too: a url() or a string (an image-set's) in a style attribute, a presentation attribute or a
// <style>, its CDATA included, with CSS's escapes, from a style sheet that is itself a data: URL (an @import's, an
// xml-stylesheet's), or in HTML in a <foreignObject>. A <style>'s sheet is its own text, not the elements' in it: here
// an element holds a quote that would hide the url() in a string, and another <style> in it has a sheet of its own.
#[test]
fn an_image_a_style_sheet_embeds_is_checked() {
    let (dir, s) = repo();
    let forms: Vec<(&str, UrlForm)> = vec![
        ("style", |u| svg_doc(&format!("<rect style=\"mask-image: url({u})\"/>"))),
        ("quoted", |u| svg_doc(&format!("<rect style='mask-image: url(\"{u}\")'/>"))),
        ("fill", |u| svg_doc(&format!("<rect fill=\"url({u})\"/>"))),
        ("sheet", |u| svg_doc(&format!("<style>rect {{ mask-image: url({u}) }}</style><rect/>"))),
        ("cdata", |u| svg_doc(&format!("<style><![CDATA[rect {{ mask-image: url('{u}') }}]]></style><rect/>"))),
        ("escaped", |u| svg_doc(&format!("<style>rect {{ mask-image: \\75 rl({u}) }}</style><rect/>"))),
        ("image-set", |u| svg_doc(&format!("<style>rect {{ mask-image: image-set(\"{u}\" 1x) }}</style><rect/>"))),
        ("own-text", |u| svg_doc(&format!("<style>a {{}}<x>\"</x>rect {{ mask-image: url({u}) }}\"</style><rect/>"))),
        ("nested", |u| svg_doc(&format!("<style>a {{ content: \"<style>rect {{ mask-image: url({u}) }}</style>\" }}</style><rect/>"))),
        ("import", |u| svg_doc(&format!("<style>@import \"data:text/css,{}\";</style><rect/>", percent(format!("rect {{ mask-image: url({u}) }}").as_bytes())))),
        ("pi", |u| format!("<?xml-stylesheet type=\"text/css\" href=\"data:text/css,{}\"?>\n{}", percent(format!("svg {{ mask-image: url({u}) }}").as_bytes()), svg_doc("<rect/>"))),
        ("html", |u| svg_doc(&format!("<foreignObject width=\"10\" height=\"10\"><div {XHTML} style=\"background: url({u})\"/></foreignObject>"))),
    ];
    let (small, huge) = (png_url(&png(10, 10)), png_url(&png(100_000, 100_000)));
    for (name, form) in &forms {
        put(&dir, &format!("{name}.svg"), form(&small).as_bytes());
        if *name == "escaped" {
            // Drawn, with no size: an escape in a style sheet could spell zoom, which Firefox applies.
            let info = &file(&s, &format!("{name}.svg"), &[])["image"];
            assert!(info["v"].is_string() && info["width"].is_null(), "{info}");
        } else {
            svg_drawn(&s, &format!("{name}.svg"));
        }
        put(&dir, &format!("{name}-huge.svg"), form(&huge).as_bytes());
        embeds_too_much(&s, &format!("{name}-huge.svg"));
    }
}

// What can't be checked is refused: a data: URL that can't be decoded, a raster image whose header can't be read, an
// image of a format revue doesn't read (by its type, or by its bytes whatever its type says), an SVG that isn't one
// revue reads, and a document of another kind, which could hold anything.
#[test]
fn an_embedded_image_that_cant_be_checked_is_refused() {
    let (dir, s) = repo();
    let cases = [
        ("no-comma.svg", image_of("data:image/png;base64")),
        ("no-comma-src.svg", format!("<foreignObject><img {XHTML} src=\"DATA:image/png\"/></foreignObject>")),
        ("bad-base64.svg", image_of("data:image/png;base64,iVBOR!w0K")),
        ("short-base64.svg", image_of("data:image/png;base64,iVBORw0KG")),
        ("no-size.svg", image_of(&png_url(&png(10, 10)[..16]))),
        ("bmp.svg", image_of("data:image/bmp;base64,Qk0AAAAA")),
        ("bmp-as-text.svg", image_of(&format!("data:text/plain;base64,{}", base64(b"BM\0\0\0\0")))),
        ("avif.svg", image_of(&format!("data:application/octet-stream;base64,{}", base64(b"\0\0\0\x1cftypavif")))),
        ("not-svg.svg", image_of(&svg_url("<html/>"))),
        ("laughs.svg", image_of(&svg_url("<!DOCTYPE svg [<!ENTITY a \"a\">]><svg/>"))),
        ("html.svg", format!("<foreignObject><iframe {XHTML} src=\"data:text/html,%3Cp%3E\"/></foreignObject>")),
    ];
    for (path, body) in &cases {
        put(&dir, path, svg_doc(body).as_bytes());
        embeds_too_much(&s, path);
    }
}

// What draws no image is let be: a font, text, a script (which an image never runs), and text that only looks like a
// data: URL, in an element's text or in an attribute that takes no URL (a design tool's label, an ARIA label). So is
// an animation's values list of data: URLs with ';base64' in them, which a browser cuts in two at the ';' as well.
#[test]
fn an_embedded_font_or_text_is_let_be() {
    let (dir, s) = repo();
    let font = base64(b"wOF2\0\x01\0\0\0\0\0\0");
    let small = png_url(&png(10, 10));
    let cases = [
        ("font.svg", format!("<style>@font-face {{ font-family: f; src: url(data:font/woff2;base64,{font}) format(\"woff2\") }}</style><text font-family=\"f\">f</text>")),
        ("text.svg", "<a href=\"data:text/plain,hello%2C%20world\"><rect/></a>".to_string()),
        ("script.svg", "<script href=\"data:text/javascript,alert(1)\"/>".to_string()),
        ("words.svg", "<text>data:image/png;base64,!!!</text>".to_string()),
        ("label.svg", "<g xmlns:inkscape=\"http://www.inkscape.org/namespaces/inkscape\" inkscape:label=\"Data: Q3 sales\"><rect/></g>".to_string()),
        ("aria.svg", "<g aria-label=\"DATA:none\"><rect/></g>".to_string()),
        ("values.svg", format!("<image href=\"#\"><animate attributeName=\"href\" values=\"{small};{small}\" dur=\"1s\"/></image>")),
    ];
    for (path, body) in &cases {
        put(&dir, path, svg_doc(body).as_bytes());
        svg_drawn(&s, path);
    }
}

// An image loads no frame and applies no XSLT style sheet, so what they hold isn't counted. This was checked in
// Chromium 156 and Firefox 157: SVGs like these were drawn as <img>s, read back from a canvas and a screenshot, and
// heavy ones timed against the same files with the frame or sheet made inert. Neither browser drew or processed an
// <iframe> (srcdoc or a data: URL), <object> or <embed> in a <foreignObject>, or an XSLT (by #id or a data: URL, of
// each type browsers take for one), though each did with the SVG opened as a document. WebKit 27.2 drew no frame
// either, and fails to load an SVG with an XSL style sheet as an image at all. So these are drawn, though the frame's
// document draws over 200,000 squares and the sheet would turn the file into a million. A data: URL in them is still
// checked as any is: a frame's HTML is refused, as a document of another kind.
#[test]
fn a_frame_or_an_xslt_style_sheet_is_let_be_since_an_image_loads_neither() {
    let (dir, s) = repo();
    let escaped = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
    let srcdoc = format!("<foreignObject width=\"10\" height=\"10\"><iframe {XHTML} srcdoc=\"{}\"/></foreignObject>", escaped(&nested(5, href)));
    put(&dir, "srcdoc.svg", svg_doc(&srcdoc).as_bytes());
    svg_drawn(&s, "srcdoc.svg");

    // A million squares, made by halving the count, so the templates call each other only 20 deep.
    let sheet = |id: &str| {
        format!(
            "<xsl:stylesheet {id} version=\"1.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\" xmlns=\"http://www.w3.org/2000/svg\">\
             <xsl:template name=\"squares\"><xsl:param name=\"n\"/><xsl:choose><xsl:when test=\"$n &gt; 1\">\
             <xsl:call-template name=\"squares\"><xsl:with-param name=\"n\" select=\"floor($n div 2)\"/></xsl:call-template>\
             <xsl:call-template name=\"squares\"><xsl:with-param name=\"n\" select=\"$n - floor($n div 2)\"/></xsl:call-template>\
             </xsl:when><xsl:otherwise><rect width=\"1\" height=\"1\"/></xsl:otherwise></xsl:choose></xsl:template>\
             <xsl:template match=\"/\"><svg viewBox=\"0 0 10 10\"><xsl:call-template name=\"squares\">\
             <xsl:with-param name=\"n\" select=\"1000000\"/></xsl:call-template></svg></xsl:template></xsl:stylesheet>"
        )
    };
    let styled = |kind: &str, href: &str, body: &str| format!("<?xml-stylesheet type=\"{kind}\" href=\"{href}\"?>\n{}", svg_doc(body));
    let inline = sheet("id=\"t\"");
    let cases = [
        ("xsl.svg", styled("text/xsl", "#t", &inline)),
        ("xslt.svg", styled("application/xslt+xml", "#t", &inline)),
        ("xml.svg", styled("text/xml", "#t", &inline)),
        ("application-xml.svg", styled("application/xml", "#t", &inline)),
        ("data.svg", styled("text/xsl", &format!("data:text/plain,{}", percent(sheet("").as_bytes())), "<rect/>")),
    ];
    for (path, text) in &cases {
        put(&dir, path, text.as_bytes());
        svg_drawn(&s, path);
    }
}

#[test]
fn an_image_whose_size_cant_be_read_is_refused() {
    let (dir, s) = repo();
    let no_frame = jpeg(10, 0, 0); // a height of 0 is given later, in the scan (DNL), which isn't read
    let mut junk = gif((5, 5), &[(0, 0, 5, 5)]);
    junk.pop();
    junk.extend([0x99, 0x3B]); // an unknown block where the next would start
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("cut.gif", b"GIF89a\0".to_vec(), "image/gif"),
        ("dnl.jpg", no_frame, "image/jpeg"),
        ("junk.gif", junk, "image/gif"),
        ("cut.png", png(4, 4)[..20].to_vec(), "image/png"),
        ("empty.ico", ico(&[]), "image/x-icon"),
        ("bad.webp", webp(b"VP8 ", &[0; 20]), "image/webp"),
    ];
    for (path, data, mime) in &cases {
        put(&dir, path, data);
        let error = format!("{path}: the size of this image can't be read");
        refused(&s, path, json!({ "type": mime, "refused": "header" }), 415, &error);
    }
    // A GIF cut short after a frame ends there, as it does for a browser.
    let mut cut = gif((5, 5), &[(0, 0, 5, 5), (0, 0, 9, 9)]);
    cut.truncate(cut.len() - 4);
    put(&dir, "short.gif", &cut);
    shown(&s, "short.gif", &cut, "image/gif", (9, 9));
}

#[test]
fn an_image_that_would_take_over_50_million_pixels_is_refused() {
    let (dir, s) = repo();
    let cases = vec![
        ("huge.png", png(10_000, 5_001), "image/png", (10_000, 5_001)),
        ("huge.jpg", jpeg(10_000, 6_000, 0), "image/jpeg", (10_000, 6_000)),
        ("huge.webp", webp_extended(16_000, 16_000), "image/webp", (16_000, 16_000)),
        // A tiny screen with one huge frame.
        ("frame.gif", gif((1, 1), &[(0, 0, 1, 1), (0, 0, 10_000, 10_000)]), "image/gif", (10_000, 10_000)),
        // An icon listed as 16 × 16 that is a huge PNG, or a bitmap whose header says it is huge.
        ("png.ico", ico(&[((16, 16), png(16, 16)), ((16, 16), png(10_000, 10_000))]), "image/x-icon", (10_000, 10_000)),
        ("bmp.ico", ico(&[((0, 0), bmp_icon(-10_000, 10_000))]), "image/x-icon", (10_000, 10_000)),
    ];
    for (path, data, mime, (w, h)) in &cases {
        put(&dir, path, data);
        let error = format!("{path} is too large to show ({w} × {h} pixels, over 50 million)");
        refused(&s, path, json!({ "type": mime, "width": w, "height": h, "refused": "pixels" }), 413, &error);
    }
    // 50 million exactly is shown.
    put(&dir, "most.png", &png(10_000, 5_000));
    shown(&s, "most.png", &png(10_000, 5_000), "image/png", (10_000, 5_000));
}

#[test]
fn images_over_the_text_limit_are_read_whole_and_over_20_mb_not_at_all() {
    let (dir, s) = repo();
    let padded = |head: Vec<u8>, len: usize| {
        let mut d = head;
        d.resize(len, 0);
        d
    };
    // Over the 2 MB /api/file shows as text, but images /api/image serves: from the working tree, its old side and a
    // commit. The GIF's frame past its screen comes after 3 MB of comment, so its size is read from all of it.
    let big = padded(png(100, 80), 3 << 20);
    let late = gif_with_comment((10, 10), 3 << 20, &[(0, 0, 10, 10), (0, 0, 70, 60)]);
    put(&dir, "big.png", &big);
    put(&dir, "late.gif", &late);
    commit_all(dir.path(), "big images");
    for view in [&[("mode", "working")][..], &[("mode", "working"), ("side", "old")], &[("mode", "commit"), ("ref", "HEAD")]] {
        let at = |path| [view, &[("path", path)]].concat();
        let res = s.get("/api/image", &at("big.png"));
        assert!(res.status == 200 && res.body == big, "{view:?}: {} {}", res.status, res.body.len());
        let v = res.header("etag").unwrap().trim_matches('"');
        let info = json!({ "binary": true, "size": big.len(), "image": { "type": "image/png", "width": 100, "height": 80, "v": v } });
        assert_eq!(s.body("/api/file", &at("big.png")), info, "{view:?}");
        let gif = &s.body("/api/file", &at("late.gif"))["image"];
        assert_eq!((&gif["type"], &gif["width"], &gif["height"]), (&json!("image/gif"), &json!(70), &json!(60)), "{view:?}");
    }

    // Over 20 MB, only the start is read, to tell it is an image.
    let huge = padded(png(100, 80), (20 << 20) + 1);
    put(&dir, "huge.png", &huge);
    s.reads();
    refused(&s, "huge.png", json!({ "type": "image/png", "refused": "bytes" }), 413, "huge.png is too large to show (over 20 MB)");
    let reads = s.read_sizes();
    assert!(!reads.is_empty() && reads.iter().all(|&(_, n)| n <= 8000), "read past the start of a file over 20 MB: {reads:?}");
    commit_all(dir.path(), "huge");
    let at = [("mode", "commit"), ("ref", "HEAD"), ("path", "huge.png")];
    assert_eq!(s.body("/api/file", &at)["image"], json!({ "type": "image/png", "refused": "bytes" }));
    assert_eq!(s.get("/api/image", &at).status, 413);
}

#[test]
fn the_answer_is_sandboxed_typed_and_revalidated() {
    let (dir, s) = repo();
    put(&dir, "a.png", &png(2, 2));
    let res = image(&s, "a.png", &[]);
    assert_eq!(res.status, 200);
    for (name, value) in [
        ("content-type", "image/png"),
        ("content-security-policy", "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox"),
        ("x-content-type-options", "nosniff"),
        ("cross-origin-resource-policy", "same-origin"),
        ("cache-control", "no-cache"),
    ] {
        assert_eq!(res.header(name), Some(value), "{name}");
    }
    let etag = res.header("etag").unwrap().to_string();
    let path = "/api/image?mode=working&path=a.png";
    let again = request(s.port, "GET", path, &[("if-none-match", &etag)], b"");
    assert_eq!((again.status, again.body.len()), (304, 0));
    assert_eq!(again.header("content-security-policy"), Some("default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox"));
    // Another image is another tag.
    rewrite(&dir, "a.png", &png(3, 3));
    let changed = request(s.port, "GET", path, &[("if-none-match", &etag)], b"");
    assert_eq!(changed.status, 200);
    assert_ne!(changed.header("etag"), Some(etag.as_str()));
    assert_eq!(changed.body, png(3, 3));
}

// The version follows the bytes, so an image redrawn at the same size in bytes and pixels is a new one, and the same
// bytes written again are the same.
#[test]
fn the_version_is_of_the_bytes() {
    let (dir, s) = repo();
    let v = |s: &Server| file(s, "a.png", &[])["image"]["v"].as_str().unwrap().to_string();
    let mut a = png(4, 4);
    a.extend([1, 2, 3]);
    put(&dir, "a.png", &a);
    let first = v(&s);
    *a.last_mut().unwrap() = 4;
    rewrite(&dir, "a.png", &a);
    let second = v(&s);
    assert_ne!(first, second);
    rewrite(&dir, "a.png", &a);
    assert_eq!(v(&s), second);
    assert_eq!(image(&s, "a.png", &[]).header("etag"), Some(format!("\"{second}\"").as_str()));
}

// The page asks /api/file about every image a markdown preview shows on every live update, so what it said of an
// image is kept until the image changes: asked again, it reads nothing, not even of an image over the 2 MB text
// limit, and nor does a revalidation of /api/image with the version it gave. A working file is known to have changed
// by its stat, so a rewrite at the same length is read once its mtime has moved, and one at another length at once.
#[test]
fn an_image_asked_about_again_is_not_read_again() {
    let (dir, s) = repo();
    let mut big = png(100, 80);
    big.resize(3 << 20, 0);
    put(&dir, "a.png", &png(4, 4));
    put(&dir, "big.png", &big);
    s.reads();
    let a = file(&s, "a.png", &[]);
    let b = file(&s, "big.png", &[]);
    let read = s.reads();
    assert!(["/a.png", "/big.png"].iter().all(|f| read.iter().any(|p| p.ends_with(f))), "{read:?}");
    assert_eq!(file(&s, "a.png", &[]), a);
    assert_eq!(file(&s, "big.png", &[]), b);
    assert_eq!(s.reads(), Vec::<String>::new(), "unchanged images were read again");
    for (path, info) in [("a.png", &a), ("big.png", &b)] {
        let etag = format!("\"{}\"", info["image"]["v"].as_str().unwrap());
        let at = [("mode", "working"), ("path", path)];
        assert_eq!(revalidate(&s, &at, &etag), (304, Some(etag.clone()), Some(IMAGE_CSP.to_string())), "{path}");
    }
    assert_eq!(s.reads(), Vec::<String>::new(), "revalidated images were read");

    // Other bytes at the same length, once the mtime moves: read again, with another version.
    rewrite(&dir, "a.png", &png(5, 5));
    let a5 = file(&s, "a.png", &[]);
    assert!(s.reads().iter().any(|p| p.ends_with("/a.png")), "a rewritten image wasn't read");
    assert_ne!(a5["image"]["v"], a["image"]["v"]);
    shown(&s, "a.png", &png(5, 5), "image/png", (5, 5));
    let old = format!("\"{}\"", a["image"]["v"].as_str().unwrap());
    assert_eq!(revalidate(&s, &[("mode", "working"), ("path", "a.png")], &old).0, 200);
    // At another length, with the mtime left as the write set it.
    big.resize((3 << 20) + 1, 0);
    put(&dir, "big.png", &big);
    assert_eq!(file(&s, "big.png", &[])["size"], big.len());
    shown(&s, "big.png", &big, "image/png", (100, 80));

    // Only images are kept: text, an SVG among them, and a binary file that isn't an image are read each time.
    put(&dir, "logo.svg", b"<svg width=\"2\" height=\"2\"></svg>\n");
    put(&dir, "data.bin", &[0, 1, 2]);
    for path in ["README.md", "logo.svg", "data.bin"] {
        file(&s, path, &[]);
        s.reads();
        file(&s, path, &[]);
        assert_eq!(s.reads().len(), 1, "{path} wasn't read again");
    }

    // An SVG's text is read each time, but what it is as an image is kept, so a revalidation isn't read either, and
    // keeps the attachment header. Other bytes at the same length are another version.
    let svg = |side: u8| format!("<svg {SVG_NS} width=\"{side}\" height=\"{side}\"/>\n").into_bytes();
    put(&dir, "icon.svg", &svg(2));
    let icon = file(&s, "icon.svg", &[]);
    assert_eq!(icon["image"]["width"], json!(2));
    let etag = format!("\"{}\"", icon["image"]["v"].as_str().unwrap());
    s.reads();
    assert_eq!(file(&s, "icon.svg", &[]), icon);
    let res = request(s.port, "GET", "/api/image?mode=working&path=icon.svg", &[("if-none-match", &etag)], b"");
    assert_eq!((res.status, res.header("etag"), res.header("content-disposition")), (304, Some(etag.as_str()), Some("attachment")));
    assert_eq!(s.reads().len(), 1, "the revalidated SVG was read");
    rewrite(&dir, "icon.svg", &svg(3));
    let icon3 = file(&s, "icon.svg", &[]);
    assert_eq!((icon3["image"]["width"].clone(), icon3["image"]["v"] == icon["image"]["v"]), (json!(3), false));
    let res = request(s.port, "GET", "/api/image?mode=working&path=icon.svg", &[("if-none-match", &etag)], b"");
    assert_eq!(res.status, 200);
}

// A commit's image is known by its blob, so what /api/file said of it holds for good, and for every view that names
// that blob: its commit, the old side of the next one, or the old side of the working tree.
#[test]
fn a_commits_image_is_known_by_its_blob() {
    let (dir, s) = repo();
    put(&dir, "a.png", &png(2, 2));
    commit_all(dir.path(), "two");
    put(&dir, "a.png", &png(3, 3));
    commit_all(dir.path(), "three");
    let at = |view: &[(&'static str, &'static str)]| [view, &[("path", "a.png")]].concat();
    let (two, three) = ([("mode", "commit"), ("ref", "HEAD~1")], [("mode", "commit"), ("ref", "HEAD")]);
    let mut infos = Vec::new();
    for (view, data, w) in [(&two, png(2, 2), 2), (&three, png(3, 3), 3)] {
        let res = s.get("/api/image", &at(view));
        assert!(res.status == 200 && res.body == data, "{view:?}: {} {}", res.status, res.text());
        let v = res.header("etag").unwrap().trim_matches('"');
        let info = json!({ "binary": true, "size": data.len(), "image": { "type": "image/png", "width": w, "height": w, "v": v } });
        assert_eq!(s.body("/api/file", &at(view)), info, "{view:?}");
        assert_eq!(s.body("/api/file", &at(view)), info, "{view:?}, asked again");
        infos.push(info);
    }
    assert_eq!(s.body("/api/file", &at(&[("mode", "commit"), ("ref", "HEAD"), ("side", "old")])), infos[0]);
    assert_eq!(s.body("/api/file", &at(&[("mode", "working"), ("side", "old")])), infos[1]);

    // With the blob's object gone from the repo, reading it fails, but what was said of it still comes, and a
    // revalidation of it still gets its 304: neither reads the blob.
    let blob = rev(dir.path(), "HEAD:a.png");
    std::fs::remove_file(dir.path().join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    assert_eq!(s.get("/api/image", &at(&three)).status, 500);
    assert_eq!(s.body("/api/file", &at(&three)), infos[1]);
    let etag = format!("\"{}\"", infos[1]["image"]["v"].as_str().unwrap());
    assert_eq!(revalidate(&s, &at(&three), &etag), (304, Some(etag.clone()), Some(IMAGE_CSP.to_string())));
}

#[test]
fn each_side_of_the_view_is_its_own_image() {
    let (dir, s) = repo();
    put(&dir, "a.png", &png(1, 1));
    commit_all(dir.path(), "one");
    put(&dir, "a.png", &png(2, 2));
    put(&dir, "new.png", &png(5, 5));
    commit_all(dir.path(), "two");
    put(&dir, "a.png", &png(3, 3));
    let body = |params: &[(&str, &str)]| {
        let mut p = vec![("path", "a.png")];
        p.extend_from_slice(params);
        let res = s.get("/api/image", &p);
        assert_eq!(res.status, 200, "{params:?}: {}", res.text());
        assert_eq!(s.body("/api/file", &p)["image"]["width"], json!(res.body[19]), "{params:?}");
        res.body
    };
    assert_eq!(body(&[("mode", "working")]), png(3, 3));
    assert_eq!(body(&[("mode", "working"), ("side", "old")]), png(2, 2));
    assert_eq!(body(&[("mode", "commit"), ("ref", "HEAD")]), png(2, 2));
    assert_eq!(body(&[("mode", "commit"), ("ref", "HEAD"), ("side", "old")]), png(1, 1));
    assert_eq!(body(&[("mode", "commit"), ("ref", "HEAD~1")]), png(1, 1));
    // Before it was added there is no image.
    let res = s.get("/api/image", &[("mode", "commit"), ("ref", "HEAD"), ("side", "old"), ("path", "new.png")]);
    assert_eq!(res.status, 404, "{}", res.text());
}

#[test]
fn paths_are_checked_as_for_any_file() {
    let (dir, s) = repo();
    let outside = TempDir::new("images-outside");
    std::fs::write(outside.path().join("x.png"), png(1, 1)).unwrap();
    std::fs::create_dir(dir.path().join("pics")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("x.png"), dir.path().join("out.png")).unwrap();
        put(&dir, "in.png", &png(4, 4));
        std::os::unix::fs::symlink("in.png", dir.path().join("link.png")).unwrap();
        let fifo = std::ffi::CString::new(dir.path().join("pipe.png").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    }
    let status = |path: &str| {
        let res = image(&s, path, &[]);
        (res.status, res.json()["error"].as_str().unwrap_or("").to_string())
    };
    assert_eq!(status("../x.png").0, 400);
    assert_eq!(status("pics"), (400, "pics is not a file".to_string()));
    assert_eq!(status("gone.png"), (404, "file not found: gone.png".to_string()));
    #[cfg(unix)]
    {
        assert_eq!(status("out.png"), (403, "out.png is a symlink to a location outside the repository".to_string()));
        assert_eq!(file(&s, "out.png", &[]), json!({ "outside": true }));
        // A FIFO is never opened: reading one would block.
        assert_eq!(status("pipe.png"), (400, "pipe.png is not a file".to_string()));
        // A link within the repo is its target, as the file view shows it.
        shown(&s, "link.png", &png(4, 4), "image/png", (4, 4));
    }
}

#[test]
fn a_plain_folder_serves_its_images_from_disk() {
    let dir = TempDir::new("images-folder");
    std::fs::write(dir.path().join("a.png"), png(6, 7)).unwrap();
    let s = Server::start(dir.path());
    assert_eq!(s.body("/api/info", &[])["git"], false);
    shown(&s, "a.png", &png(6, 7), "image/png", (6, 7));
    // A view's parameters are ignored there, as for /api/file.
    let res = image(&s, "a.png", &[("mode", "commit"), ("ref", "nope"), ("side", "old")]);
    assert_eq!((res.status, res.body), (200, png(6, 7)));
}
