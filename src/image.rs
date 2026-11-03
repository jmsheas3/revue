// Images revue shows: PNG, JPEG, GIF, WebP and ICO, and SVG. A repo's images are untrusted, so the server, not the
// browser, decides what one is: a raster image by its first bytes, never its name, and only in a format whose size in
// pixels it can read here. A few KB of PNG can claim 100,000 × 100,000 pixels, 40 GB once decoded, so an image over
// MAX_PIXELS is refused before a browser is ever handed it, and so is one whose size can't be read. SVG (svg) is a
// document that can hold script, shown only as an image (<img>), where browsers run no script and load no other file,
// and refused when it would draw over MAX_SVG_ELEMENTS elements, which a few hundred bytes of <use>s can, or when the
// images it holds as data: URLs, which an image does load, are more than these checks allow of a file, or when it isn't
// well-formed XML, of which browsers draw nothing (well_formed), with the reason.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

// The largest file /api/image serves.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
// The most pixels an image may take: 50 million, 200 MB decoded at 4 bytes each (a 48-megapixel photo fits).
pub const MAX_PIXELS: u64 = 50_000_000;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

// The type of the image `data` starts with, by its signature.
pub fn mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(PNG_SIGNATURE) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.get(..4) == Some(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if data.starts_with(&[0, 0, 1, 0]) {
        Some("image/x-icon")
    } else {
        None
    }
}

// Why an image isn't shown.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    Bytes,   // the file is over MAX_BYTES (an SVG: MAX_SVG_BYTES)
    Pixels,  // it would take over MAX_PIXELS
    Header,   // its size can't be read
    Complex,  // an SVG that would draw over MAX_SVG_ELEMENTS elements
    Embedded, // an SVG whose data: URLs hold images that can't be checked or take over MAX_PIXELS in all (data_url)
    Malformed(Malformed), // an SVG browsers draw nothing of, since it isn't well-formed XML (well_formed)
}

impl Refusal {
    pub fn name(&self) -> &'static str {
        match self {
            Refusal::Bytes => "bytes",
            Refusal::Pixels => "pixels",
            Refusal::Header => "header",
            Refusal::Complex => "complex",
            Refusal::Embedded => "embedded",
            Refusal::Malformed(_) => "malformed",
        }
    }
}

// What revue makes of an image file: its type, its sizes when the header gives them, and why it isn't shown, if it
// isn't. `size` is the one it is drawn at, and `pixels` the pixels it stores (turned as it is drawn), which decoding it
// takes, so MAX_PIXELS limits those (see Size); an SVG isn't decoded into pixels, so it has none.
pub struct Image {
    pub mime: &'static str,
    pub size: Option<(u32, u32)>,
    pub pixels: Option<(u32, u32)>,
    pub refused: Option<Refusal>,
}

impl Image {
    // The size to tell of it: the one it is drawn at, or for one refused for its pixels, those, which the limit
    // counted.
    pub fn reported_size(&self) -> Option<(u32, u32)> {
        if self.refused == Some(Refusal::Pixels) { self.pixels } else { self.size }
    }
}

// `data` is the whole file when `len` (its length) is at most MAX_BYTES, otherwise only its start. None when it isn't
// an image of one of these types.
pub fn inspect(data: &[u8], len: u64) -> Option<Image> {
    let mime = mime(data)?;
    let unread = |why| Some(Image { mime, size: None, pixels: None, refused: Some(why) });
    if len > MAX_BYTES {
        return unread(Refusal::Bytes);
    }
    let Some(Size { pixels, drawn }) = size(mime, data) else { return unread(Refusal::Header) };
    // Decoding takes its pixels as stored, whatever size it is drawn at.
    let refused = (pixels.0 as u64 * pixels.1 as u64 > MAX_PIXELS).then_some(Refusal::Pixels);
    Some(Image { mime, size: Some(drawn), pixels: Some(pixels), refused })
}

// The largest SVG served: the text limit, since a complex drawing takes time to draw however few its pixels.
pub const MAX_SVG_BYTES: u64 = 2 * 1024 * 1024;

// The most elements an SVG may draw, each <use> counted as the copy of what it names. Few bytes can draw many: a
// <use> names a group that holds <use>s of a group that holds <use>s, and so on, so 5 levels of 10 <use>s each, about
// a kilobyte, draw 100,000 elements, which took browsers 2 to 3 seconds, and 7 levels froze the tab for over 20.
pub const MAX_SVG_ELEMENTS: u64 = 50_000;

// An SVG: a file named .svg whose text, past a byte order mark, an XML declaration, comments and a doctype, starts
// with an <svg> element. Its size is the one browsers draw it at, from the root's width, height and viewBox, or with
// neither width nor height, its viewBox's (svg_size), and unknown otherwise, which is no reason to refuse it: it is
// drawn at the size it has. A doctype with an internal subset can define entities that expand without end (billion
// laughs), so such a file isn't one. One that would draw
// over MAX_SVG_ELEMENTS elements (svg_elements), or whose data: URLs hold more than revue would show (data_url), is
// refused, and so is one that isn't well-formed XML (well_formed), with no size, since browsers draw nothing of it.
// The limits on what it draws come first, as the one on its bytes does: a file refused for them is refused for that
// whether or not it is well formed. `data` is the whole file, or its start when `len` (its length) is over
// MAX_SVG_BYTES, which is enough to tell it is one to refuse.
pub const SVG: &str = "image/svg+xml";
pub fn svg(path: &str, data: &[u8], len: u64) -> Option<Image> {
    if !path.to_ascii_lowercase().ends_with(".svg") {
        return None;
    }
    let root = svg_root(utf8_start(data))?;
    let mime = SVG;
    if len > MAX_SVG_BYTES {
        return Some(Image { mime, size: None, pixels: None, refused: Some(Refusal::Bytes) });
    }
    // All of it is read, past bytes that aren't UTF-8 too (well_formed then refuses it, but the limits come first), and
    // from the start, since an xml-stylesheet before the root can link a style sheet.
    let mut embeds = Embeds::default();
    let refused = svg_elements(data, &mut embeds).err();
    if refused.is_none() {
        if let Err(why) = well_formed(data) {
            return Some(Image { mime, size: None, pixels: None, refused: Some(Refusal::Malformed(why)) });
        }
    }
    // A refused file was read only up to the refusal, so a style sheet or an animation may come after.
    let size = svg_size(root, embeds.styled || refused.is_some(), embeds.resized || refused.is_some());
    Some(Image { mime, size, pixels: None, refused })
}

// The start of `data` that is UTF-8: all of it, or up to a byte that isn't (a start may end inside a character).
fn utf8_start(data: &[u8]) -> &str {
    std::str::from_utf8(data).unwrap_or_else(|e| std::str::from_utf8(&data[..e.valid_up_to()]).unwrap_or_default())
}

// The text from the root <svg> element's start tag on, past what may come before it.
fn svg_root(text: &str) -> Option<&str> {
    let mut s = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("<?") {
            s = &rest[rest.find("?>")? + 2..];
        } else if let Some(rest) = s.strip_prefix("<!--") {
            s = &rest[rest.find("-->")? + 3..];
        } else if s.get(..9).is_some_and(|d| d.eq_ignore_ascii_case("<!doctype")) {
            let end = s.find('>')?;
            if s[..end].contains('[') {
                return None;
            }
            s = &s[end + 1..];
        } else {
            break;
        }
    }
    let after = s.strip_prefix("<svg")?;
    after.starts_with(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/').then_some(s)
}

// The size an <img> draws an SVG at, where Chromium 156 and Firefox 157 agree on it, from its root's start tag's
// attributes (the first of each name: a second is an error, where a browser draws nothing), read as svg_elements reads
// them, so that a name inside another's quoted value isn't taken for one. Where the two differ, or the server can't
// tell, it is unknown, and the page draws the image at the browser's own. `styled`: something besides the root's
// attributes may set its font size (a style sheet that could, or an animation), so em and rem are unknown; `resized`:
// something may change its size (an animation, or a style sheet that could zoom it), so it is unknown. A width and a
// height (svg_length) give it; one of them gives the other through the viewBox's proportions, and without a valid
// viewBox the other is a default (300 wide or 150 tall), so the size is unknown. A width or height that is a
// percentage, auto or no length at all is as good as missing, and a CSS width or height (a style attribute, a <style>)
// changes nothing, but zoom does in Firefox, not in Chromium, so a style attribute that may set it leaves the size
// unknown. With neither, browsers make a size up, 300 × 150 or a square, which is the size of no drawing, so revue
// takes the viewBox's instead, which draws an icon at its design size (the page never takes the browser's for an SVG).
// Each side is rounded as naturalWidth rounds it, and one of 0 once rounded, or over MAX_SVG_SIDE, is unknown.
fn svg_size(root: &str, styled: bool, resized: bool) -> Option<(u32, u32)> {
    if resized {
        return None;
    }
    let (mut width, mut height, mut view, mut font_size, mut style) = (None, None, None, None, None);
    for (name, value) in Attrs::new(root.as_bytes(), "<svg".len()) {
        let slot = match name {
            b"width" => &mut width,
            b"height" => &mut height,
            b"viewBox" => &mut view,
            b"font-size" => &mut font_size,
            b"style" => &mut style,
            _ => continue,
        };
        slot.get_or_insert_with(|| xml_value(value).into_owned());
    }
    if style.as_deref().is_some_and(zoom_sheet) {
        return None;
    }
    let em = if styled { None } else { root_font(font_size.as_deref(), style.as_deref()) };
    let length = |v: Option<Vec<u8>>| v.map_or(Length::Auto, |v| svg_length(&v, em));
    let view = view.as_deref().and_then(view_box);
    let (w, h) = match (length(width), length(height)) {
        (Length::Unknown, _) | (_, Length::Unknown) => return None,
        (Length::Px(w), Length::Px(h)) => (w, h),
        (Length::Px(w), Length::Auto) => view.map(|(vw, vh)| (w, w * vh / vw))?,
        (Length::Auto, Length::Px(h)) => view.map(|(vw, vh)| (h * vw / vh, h))?,
        (Length::Auto, Length::Auto) => view?,
    };
    let (w, h) = (w.round(), h.round());
    let fits = |n: f64| (1.0..=MAX_SVG_SIDE).contains(&n);
    (fits(w) && fits(h)).then_some((w as u32, h as u32))
}

// The largest side both browsers give an SVG as its natural size, in px: Chromium's (its layout units' limit, 2^25 px,
// less what single precision loses). Past it Chromium keeps to it and Firefox doesn't, though Firefox draws no side
// over 17,895,698 px.
const MAX_SVG_SIDE: f64 = 33_554_428.0;

// The root's font size, which em and rem are of, when nothing in the file sets it: the browsers' default.
const SVG_FONT_PX: f64 = 16.0;

// The largest root font size the browsers agree on, in px: Firefox rounds a larger one (9,000px is 8,992), and Chromium
// takes at most 10,000px.
const MAX_FONT_PX: f64 = 8192.0;

// A width or height of the root.
enum Length {
    Px(f64),
    Auto,    // no length the image has: missing, auto, a percentage, or not a length at all
    Unknown, // one the browsers read differently, or the server can't work out
}

// A root width or height, as both browsers read it: a number, in single precision as they keep it (17,895,697 is
// 17,895,696), with a unit (any case) whose size is known, or none for px, between spaces. em and rem are `em`, or
// unknown when the server can't tell the root's font size (`em` None). A percentage, auto or anything else that isn't a
// length (40., 40 px, 40deg) is as good as missing, as both ignore it. Unknown: what they read differently, a negative
// length (Chromium takes it for 0, Firefox ignores it) and a function, a comment or an escape (Chromium works out
// calc() and reads CSS's comments and escapes, Firefox ignores such a value); and what depends on what the server can't
// know, a unit of the font's (they differ by browser), the viewport's (0 in an image) or a container's.
fn svg_length(value: &[u8], em: Option<f64>) -> Length {
    let value = value.trim_ascii();
    if value.iter().any(|c| matches!(c, b'(' | b'\\' | b'/')) {
        return Length::Unknown;
    }
    let Some((n, end)) = number(value) else { return Length::Auto };
    let per = match unit_px(&value[end..], em) {
        Ok(per) => per,
        Err(Bad::Unknown) => return Length::Unknown,
        Err(Bad::Invalid) => return Length::Auto,
    };
    if n < 0.0 {
        return Length::Unknown;
    }
    Length::Px((n * per) as f32 as f64)
}

// Why a unit gives no size: the server can't know it, or it is no length unit.
enum Bad {
    Unknown,
    Invalid,
}

// How many px a unit (any case, or none for px) is: em and rem are `em`. Unknown for em and rem when `em` is None and
// for a unit relative to the font, the viewport or a container (relative_unit); Invalid for anything that is no length
// unit, a percentage included.
fn unit_px(unit: &[u8], em: Option<f64>) -> Result<f64, Bad> {
    let unit = std::str::from_utf8(unit).unwrap_or("?").to_ascii_lowercase();
    Ok(match unit.as_str() {
        "" | "px" => 1.0,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "q" => 96.0 / 101.6,
        "em" | "rem" => return em.ok_or(Bad::Unknown),
        u if relative_unit(u) => return Err(Bad::Unknown),
        _ => return Err(Bad::Invalid),
    })
}

// A CSS unit relative to the font (ex, ch, ic, cap, lh, and their root's: rex, ...), the viewport (vw, vh, vi, vb,
// vmin, vmax, and their small, large and dynamic forms: svw, ...) or a container (cqw, ...), in lower case.
fn relative_unit(unit: &str) -> bool {
    let font = unit.strip_prefix('r').unwrap_or(unit);
    let viewport = unit.strip_prefix(['s', 'l', 'd']).unwrap_or(unit);
    let sides = |rest: Option<&str>| matches!(rest, Some("w" | "h" | "i" | "b" | "min" | "max"));
    matches!(font, "ex" | "ch" | "ic" | "cap" | "lh") || sides(viewport.strip_prefix('v')) || sides(unit.strip_prefix("cq"))
}

// The number `s` starts with and where it ends: a sign, digits with a fraction or a fraction alone (40. isn't one), and
// an exponent when digits follow the e (4em is 4 em).
fn number(s: &[u8]) -> Option<(f64, usize)> {
    let digits = |from: usize| from + s[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut end = digits(usize::from(matches!(s.first(), Some(b'+' | b'-'))));
    if s.get(end) == Some(&b'.') && s.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = digits(end + 1);
    }
    if !s[..end].iter().any(u8::is_ascii_digit) {
        return None;
    }
    if matches!(s.get(end), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(s.get(end + 1), Some(b'+' | b'-')));
        if s.get(end + 1 + sign).is_some_and(u8::is_ascii_digit) {
            end = digits(end + 1 + sign);
        }
    }
    let n = std::str::from_utf8(&s[..end]).ok()?.parse().ok()?;
    Some((n, end))
}

// A viewBox's width and height when both browsers take it as valid: four numbers (each within single precision) apart
// by spaces, a comma or both, with a width and a height over 0. None for any other, numbers apart by nothing (before a
// sign or a '.' that starts the next) included, which Chromium takes for two and Firefox doesn't.
fn view_box(s: &[u8]) -> Option<(f64, f64)> {
    let mut at = spaces(s, 0);
    let mut n = [0.0; 4];
    for (k, slot) in n.iter_mut().enumerate() {
        let (x, len) = number(&s[at..])?;
        if !(x as f32).is_finite() {
            return None;
        }
        *slot = x;
        let next = spaces(s, at + len);
        let comma = k < 3 && s.get(next) == Some(&b',');
        if k < 3 && next == at + len && !comma {
            return None;
        }
        at = if comma { spaces(s, next + 1) } else { next };
    }
    let [_, _, w, h] = n;
    (at == s.len() && w > 0.0 && h > 0.0).then_some((w, h))
}

// ---------------------------------------------------------------- the root's font size

// The root's font size in px, which its em and rem are of, where the browsers agree on it: from the font-size
// declarations of its style attribute (style_font), the last of which wins, else its font-size attribute (font_px),
// else the default, 16px. None when the server can't tell, or it is over MAX_FONT_PX. 0 is a size too, which makes an
// em 0.
fn root_font(attribute: Option<&[u8]>, style: Option<&[u8]>) -> Option<f64> {
    let declared = match style {
        Some(style) => style_font(style)?,
        None => None,
    };
    let px = match (declared, attribute) {
        (Some(px), _) => px,
        (None, Some(attribute)) => font_px(attribute, true)?,
        (None, None) => SVG_FONT_PX,
    };
    (px <= MAX_FONT_PX).then_some(px)
}

// The font size the root's style attribute declares: Some(None) when it declares none, None when the server can't tell
// it as both browsers would. That is plain declarations (name: value, apart by ';'), with no string, block, function,
// comment, escape or !important, where the font shorthand and the all property are unknown, and so is a font-size the
// server can't read (font_px).
fn style_font(style: &[u8]) -> Option<Option<f64>> {
    let markup = |c: &u8| {
        matches!(c, b'"' | b'\'' | b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'\\' | b'/' | b'@' | b'!')
    };
    if style.iter().any(markup) {
        return None;
    }
    let mut px = None;
    for declaration in style.split(|&c| c == b';').map(<[u8]>::trim_ascii).filter(|d| !d.is_empty()) {
        let colon = declaration.iter().position(|&c| c == b':')?;
        let name = declaration[..colon].trim_ascii();
        if name.eq_ignore_ascii_case(b"font-size") {
            px = Some(font_px(&declaration[colon + 1..], false)?);
        } else if name.eq_ignore_ascii_case(b"font") || name.eq_ignore_ascii_case(b"all") {
            return None;
        }
    }
    Some(px)
}

// A font size as both browsers read one: a number that isn't negative with a unit whose size is known, em, rem and %
// being of the default, 16px (the root has no parent), or in the font-size attribute (`attribute`) none for px, where a
// declaration needs one (Chromium takes a bare number there too, Firefox ignores it). None for anything else, a
// keyword or a function included, which the server doesn't read.
fn font_px(value: &[u8], attribute: bool) -> Option<f64> {
    let value = value.trim_ascii();
    let (n, end) = number(value)?;
    let per = match &value[end..] {
        b"" if !attribute => return None,
        b"%" => SVG_FONT_PX / 100.0,
        unit => unit_px(unit, Some(SVG_FONT_PX)).ok()?,
    };
    (n >= 0.0).then_some((n * per) as f32 as f64)
}

// Whether `name` is one of `names`, in any case.
fn is_one_of(name: &[u8], names: &[&str]) -> bool {
    names.iter().any(|n| name.eq_ignore_ascii_case(n.as_bytes()))
}

// ---------------------------------------------------------------- how much an SVG draws

// Any count over MAX_SVG_ELEMENTS: what weigh gives for any more.
const OVER: u64 = MAX_SVG_ELEMENTS + 1;

// An element with an id, which a <use> can name, as svg_elements reads it (group 0 is the whole document): how many
// elements it holds, itself among them, the groups nearest within it, and the names its own <use>s refer to (those
// within an inner group are that group's).
#[derive(Default)]
struct Group {
    elements: u64,
    inner: Vec<usize>,
    uses: Vec<usize>,
}

// The ids and the names <use>s refer to, each given a number, and for each the groups with that id (one, unless the
// document gives two elements the same id; none for a name nothing has).
#[derive(Default)]
struct Names<'a> {
    index: HashMap<Cow<'a, [u8]>, usize>,
    groups: Vec<Vec<usize>>,
}

impl<'a> Names<'a> {
    fn of(&mut self, name: Cow<'a, [u8]>) -> usize {
        if let Some(&n) = self.index.get(name.as_ref()) {
            return n;
        }
        self.groups.push(Vec::new());
        self.index.insert(name, self.groups.len() - 1);
        self.groups.len() - 1
    }
}

// How many elements `text`, an SVG's markup, draws with every <use> drawn out and every SVG it embeds counted in, or
// why it is refused: Complex for over MAX_SVG_ELEMENTS, Embedded for what data_url refuses. A <use> draws a copy of the
// element it names (#id): that element and all it holds, where each <use> draws a copy in turn. So what a name draws
// (its weight) is the elements of the group with that id plus the weight of what each <use> in the group names, and
// the whole drawing is its elements plus the weight of what each of its <use>s names. A <use> inside a group is
// counted where it stands and in every copy of the group, which is what a browser builds. Where the group is only a
// definition (in <defs>) and isn't drawn where it stands, that counts its <use>s once too often: an over-count of at
// most about 2× when each level holds two or more <use>s, and more for a long chain of single ones, which errs on the
// side of refusing. An element whose attributes embed an SVG (data_url) draws that SVG's elements too, so they are
// counted as elements it holds, in every copy of it as well.
//
// The markup is read as an XML parser reads it, as far as this needs: tags and their attributes (quoted values may
// hold '>'), past comments, CDATA sections and processing instructions. A browser draws nothing of markup that isn't
// well formed: the file is refused for it after this count (well_formed), and an SVG it embeds is left out of the
// drawing, but counted here all the same. So where it isn't well formed this reads on and can only count more: a '<'
// that starts no tag is text, a stray end tag
// is skipped, one that closes an element left open closes that too, and what is still open at the end closes there.
// Its work grows with the text's length, not with what it draws, and it stops reading once there are over
// MAX_SVG_ELEMENTS elements. The style sheets it holds (a <style>'s own text, an xml-stylesheet's) are read for the
// data: URLs they name, and for whether they could set the root's font size (font_sheet) or zoom it (zoom_sheet), and
// an animation of the root's size or font size is noted (animation).
fn svg_elements(text: &[u8], embeds: &mut Embeds) -> Result<u64, Refusal> {
    // ESC switches ISO-2022-JP, a legacy encoding a document may declare, whose two-byte characters can read as markup
    // here that Firefox takes as text. Read as UTF-8 it is only a character XML doesn't allow, which well_formed
    // reports.
    if text.contains(&0x1B) && legacy_encoding(text) {
        return Err(Refusal::Complex);
    }
    let mut names = Names::default();
    let mut groups = vec![Group::default()];
    // The open elements, innermost last: each one's name, and if it is a group, which, with the count before it.
    let mut open = Vec::new();
    let mut open_named: HashMap<&[u8], usize> = HashMap::new(); // how many of them have each name
    let mut within = vec![0]; // the open groups, innermost last
    // The open <style>s, innermost last: where each is in `open`, and its style sheet so far. A sheet is the style's
    // own text (CDATA sections included), not that of the elements in it, and a <style> in a <style> has its own.
    let mut sheets: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut root_ids: Option<HashSet<_>> = None; // the root's ids, once it is read
    let mut count = 0;
    let mut i = 0;
    let mut text_from = 0; // where the text since the last markup starts
    while let Some(at) = text[i..].iter().position(|&c| c == b'<') {
        i += at;
        let s = &text[i..];
        if !s.starts_with(b"</") && tag_name(text, i + 1).is_empty() {
            i += 1; // a '<' that starts no markup is text
            continue;
        }
        if let Some(sheet) = own_sheet(&mut sheets, open.len()) {
            sheet.extend_from_slice(&xml_text(&text[text_from..i]));
        }
        if s.starts_with(b"<!--") {
            i = past(text, i + 4, b"-->");
        } else if s.starts_with(b"<![CDATA[") {
            let (cdata, end) = up_to(text, i + 9, b"]]>");
            if let Some(sheet) = own_sheet(&mut sheets, open.len()) {
                sheet.extend_from_slice(cdata);
            }
            i = end;
        } else if s.starts_with(b"<?") {
            let (pi, end) = up_to(text, i + 2, b"?>");
            i = end;
            // An xml-stylesheet's href names a style sheet. Its pseudo-attributes are read like a tag's, and on past a
            // '>', which ends no processing instruction.
            let target = tag_name(pi, 0);
            if target == b"xml-stylesheet" {
                let mut from = target.len();
                while from < pi.len() {
                    let mut attrs = Attrs::new(pi, from);
                    for (name, value) in attrs.by_ref() {
                        if name == b"href" {
                            count = limited(count + attr_embeds(name, value, embeds)?)?;
                        }
                    }
                    from = attrs.finish().0;
                }
            }
        } else if s.starts_with(b"<!") {
            i = past(text, i + 2, b">"); // a doctype, which has no place here
        } else if s.starts_with(b"</") {
            let name = tag_name(text, i + 2);
            i = past(text, i + 2, b">");
            if open_named.get(name).is_some_and(|&n| n > 0) {
                while let Some((was, group)) = open.pop() {
                    if let Some(n) = open_named.get_mut(was) {
                        *n -= 1;
                    }
                    count = closed(&mut sheets, open.len(), count, embeds)?;
                    shut(&mut groups, &mut within, group, count);
                    if was == name {
                        break;
                    }
                }
            }
        } else {
            let name = tag_name(text, i + 1);
            let tag = read_tag(text, i + 1 + name.len());
            match &root_ids {
                None => root_ids = Some(tag.ids.iter().map(|id| xml_value(id)).collect()),
                Some(root) if matches!(local(name), b"set" | b"animate") => {
                    animation(tag.animated, &tag.refs, root, open.len() == 1, embeds);
                }
                Some(_) => {}
            }
            i = tag.end;
            count = limited(count + 1)?;
            let outer = *within.last().unwrap_or(&0);
            if tag.animates_ref {
                return Err(Refusal::Complex);
            }
            let group = (!tag.ids.is_empty()).then(|| {
                let g = groups.len();
                groups.push(Group::default());
                groups[outer].inner.push(g);
                for id in &tag.ids {
                    let n = names.of(xml_value(id));
                    names.groups[n].push(g);
                }
                within.push(g);
                (g, count - 1)
            });
            if local(name) == b"use" {
                // A <use> with an id is a group, and its own <use> is in it.
                let at = *within.last().unwrap_or(&0);
                let mut targets = Vec::new();
                for r in &tag.refs {
                    let Some(ids) = use_target(r) else { return Err(Refusal::Complex) };
                    targets.extend(ids.into_iter().map(|id| names.of(id)));
                }
                // The same name twice (href and xlink:href alike) draws once; two names draw both, which counts more.
                targets.sort_unstable();
                targets.dedup();
                groups[at].uses.extend(targets);
            }
            for &(attr, value) in &tag.urls {
                count = limited(count + attr_embeds(attr, value, embeds)?)?;
            }
            if tag.empty {
                shut(&mut groups, &mut within, group, count);
            } else {
                if local(name) == b"style" {
                    sheets.push((open.len(), Vec::new()));
                }
                open.push((name, group));
                *open_named.entry(name).or_default() += 1;
            }
        }
        text_from = i;
    }
    if let Some(sheet) = own_sheet(&mut sheets, open.len()) {
        sheet.extend_from_slice(&xml_text(&text[text_from..]));
    }
    while let Some((_, group)) = open.pop() {
        count = closed(&mut sheets, open.len(), count, embeds)?;
        shut(&mut groups, &mut within, group, count);
    }
    groups[0].elements = count;
    limited(weigh(&groups, &names.groups))
}

// `count` if it is within MAX_SVG_ELEMENTS, else the refusal.
fn limited(count: u64) -> Result<u64, Refusal> {
    if count > MAX_SVG_ELEMENTS {
        Err(Refusal::Complex)
    } else {
        Ok(count)
    }
}

// The sheet of the <style> that text read now is in, with `open` elements open: the innermost element's, if it is one.
fn own_sheet(sheets: &mut [(usize, Vec<u8>)], open: usize) -> Option<&mut Vec<u8>> {
    sheets.last_mut().filter(|(at, _)| at + 1 == open).map(|(_, sheet)| sheet)
}

// An element closes, with `open` elements left open around it and `count` read so far; when it is a <style>, its style
// sheet is read, and the count comes to what that embeds as well.
fn closed(sheets: &mut Vec<(usize, Vec<u8>)>, open: usize, count: u64, embeds: &mut Embeds) -> Result<u64, Refusal> {
    match sheets.pop_if(|(at, _)| *at == open) {
        Some((_, sheet)) => {
            embeds.styled |= font_sheet(&sheet);
            embeds.resized |= zoom_sheet(&sheet);
            limited(count + css_embeds(&sheet, embeds)?)
        }
        None => Ok(count),
    }
}

// An animation element (<set> or <animate>) that may change the root's size or font size, and so leaves it unknown
// (`resized`) or its em unknown (`styled`): one whose attributeName (`attribute`, raw, as read_tag found it by its local
// name) is one of the root's width, height or viewBox, which Chromium animates (Firefox animates only the viewBox), or
// its font-size, or font, which Firefox animates, matched in any case, as Firefox matches it. It animates the root when
// one of its references (`refs`, read as a <use>'s) names the root by an id (`root`, a set, so the work stays linear),
// or when it is a child of the root (`child`) that names no other element: one with no reference animates its parent,
// and so in Chromium does one whose reference is empty.
fn animation(attribute: Option<&[u8]>, refs: &[&[u8]], root: &HashSet<Cow<'_, [u8]>>, child: bool, embeds: &mut Embeds) {
    let Some(attribute) = attribute else { return };
    let attribute = xml_value(attribute);
    let size = is_one_of(&attribute, &["width", "height", "viewBox"]);
    let font = is_one_of(&attribute, &["font-size", "font"]);
    let (mut names_root, mut names_other) = (false, false);
    for ids in refs.iter().filter_map(|r| use_target(r)).filter(|ids| !ids.is_empty()) {
        if ids.iter().any(|id| root.contains(id)) {
            names_root = true;
        } else {
            names_other = true;
        }
    }
    if names_root || (child && !names_other) {
        embeds.resized |= size;
        embeds.styled |= font;
    }
}

// An element closes, with `count` elements read so far: a group holds every element since its own start tag.
fn shut(groups: &mut [Group], within: &mut Vec<usize>, group: Option<(usize, u64)>, count: u64) {
    if let Some((g, before)) = group {
        groups[g].elements = count - before;
        within.pop();
    }
}

// What lies from `from` up to the first `end` after it, and where that `end` ends; all the rest of the text, and its
// end, when there is none.
fn up_to<'a>(text: &'a [u8], from: usize, end: &[u8]) -> (&'a [u8], usize) {
    let s = text.get(from..).unwrap_or_default();
    match s.windows(end.len()).position(|w| w == end) {
        Some(p) => (&s[..p], from + p + end.len()),
        None => (s, text.len()),
    }
}

// Past the first `end` from `from` on, or the end of the text when there is none.
fn past(text: &[u8], from: usize, end: &[u8]) -> usize {
    up_to(text, from, end).1
}

// The name a tag has at `from`, up to a space, '/' or '>'.
fn tag_name(text: &[u8], from: usize) -> &[u8] {
    let s = text.get(from..).unwrap_or_default();
    &s[..s.iter().position(|&c| c.is_ascii_whitespace() || c == b'/' || c == b'>').unwrap_or(s.len())]
}

// A name past its prefix: href for xlink:href, id for xml:id.
fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|&c| c == b':').next().unwrap_or(name)
}

// A start tag read from past its name: where it ends (past its '>', or the end of the text), whether it closes itself
// (/>), the raw values of its ids and references, and the attributes that may name a data: URL (urls: those with a
// ':' in some spelling, a character reference or a CSS escape). An attribute counts as an id or a reference by its
// local name, whatever its prefix: xml:id is an id, and xlink:href can be bound to any prefix. `animated` is the raw
// value of its first attributeName (an animation's), and `animates_ref` is set for an animation (<set>, <animate>) that
// makes an href name an element of this document (see animates_ref).
struct Tag<'a> {
    end: usize,
    empty: bool,
    ids: Vec<&'a [u8]>,
    refs: Vec<&'a [u8]>,
    urls: Vec<(&'a [u8], &'a [u8])>,
    animated: Option<&'a [u8]>,
    animates_ref: bool,
}

fn read_tag(text: &[u8], from: usize) -> Tag<'_> {
    let (mut ids, mut refs, mut urls) = (Vec::new(), Vec::new(), Vec::new());
    let (mut animated, mut anim_values) = (None, Vec::new());
    let mut attrs = Attrs::new(text, from);
    for (name, value) in attrs.by_ref() {
        match local(name) {
            b"id" => ids.push(value),
            b"href" => refs.push(value),
            b"attributeName" => {
                animated.get_or_insert(value);
            }
            b"to" | b"from" | b"by" | b"values" => anim_values.push(value),
            _ => {}
        }
        if value.iter().any(|&c| matches!(c, b':' | b'&' | b'\\')) {
            urls.push((name, value));
        }
    }
    let (end, empty) = attrs.finish();
    let animates_ref = animated.is_some_and(|a| animates_ref(a, &anim_values));
    Tag { end, empty, ids, refs, urls, animated, animates_ref }
}

// Whether an animation whose attributeName is `attribute` (raw) and whose to, from, by and values are `values` (raw)
// sets an href to a fragment of this document (#id). A <use> that gets its href that way draws a copy of that element
// as one with the href written out would, but the copy is made where the <use> stands, which may be anywhere (an
// animation's own href can name its target), so what it draws can't be counted here, and such a file is refused.
fn animates_ref(attribute: &[u8], values: &[&[u8]]) -> bool {
    let attribute = xml_value(attribute);
    if local(attribute.trim_ascii()) != b"href" {
        return false;
    }
    // A bare '#' names no element.
    let names = |v: &[u8]| url_of(Cow::Borrowed(v)).strip_prefix(b"#").is_some_and(|id| !id.is_empty());
    values.iter().any(|raw| xml_value(raw).split(|&c| c == b';').any(names))
}

// The attributes of a start tag from past its name, or a processing instruction's pseudo-attributes from past its
// target: each one's name and raw value, read as an XML parser reads them (a quoted value may hold '>'), with what is
// out of place ('=' or a quote with no name before it, a '/' not before '>') skipped. They end at the '>' or '/>' that
// ends the tag, or the end of the text.
struct Attrs<'a> {
    text: &'a [u8],
    at: usize,
    end: Option<(usize, bool)>,
}

impl<'a> Attrs<'a> {
    fn new(text: &'a [u8], from: usize) -> Self {
        Attrs { text, at: from, end: None }
    }

    // Where the tag ends, past its '>' (or the end of the text), and whether it closes itself (/>).
    fn finish(mut self) -> (usize, bool) {
        while self.next().is_some() {}
        self.end.unwrap_or((self.text.len(), false))
    }

    // The attribute whose name starts at `j`, with `at` moved past it; None for a name with no '=' after it, which is
    // skipped.
    fn attribute(&mut self, j: usize) -> Option<(&'a [u8], &'a [u8])> {
        let text = self.text;
        let space = |c: u8| c.is_ascii_whitespace();
        // The first byte from `k` on that `stop` is true of, or the end of the text.
        let find = |k: usize, stop: &dyn Fn(u8) -> bool| {
            text[k..].iter().position(|&c| stop(c)).map_or(text.len(), |p| k + p)
        };
        let name_end = find(j, &|c| space(c) || matches!(c, b'=' | b'>' | b'/' | b'"' | b'\''));
        self.at = name_end;
        let k = find(name_end, &|c| !space(c));
        if text.get(k) != Some(&b'=') {
            return None;
        }
        let k = find(k + 1, &|c| !space(c));
        let value = match text.get(k) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let end = find(k + 1, &|c| c == q);
                self.at = (end + 1).min(text.len());
                &text[k + 1..end]
            }
            _ => {
                self.at = find(k, &|c| space(c) || c == b'>');
                &text[k..self.at]
            }
        };
        Some((&text[j..name_end], value))
    }
}

impl<'a> Iterator for Attrs<'a> {
    type Item = (&'a [u8], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        while self.end.is_none() {
            let j = self.at;
            match self.text.get(j) {
                None => self.end = Some((self.text.len(), false)),
                Some(c) if c.is_ascii_whitespace() => self.at += 1,
                Some(b'>') => self.end = Some((j + 1, false)),
                Some(b'/') if self.text.get(j + 1) == Some(&b'>') => self.end = Some((j + 2, true)),
                Some(b'/' | b'=' | b'"' | b'\'') => self.at += 1, // out of place, and skipped
                Some(_) => {
                    if let Some(attr) = self.attribute(j) {
                        return Some(attr);
                    }
                }
            }
        }
        None
    }
}

// The ids a <use>'s reference (an href's raw value) may name in this document, as a browser reads it: the attribute's
// value (xml_value) as a URL (url_of), and its fragment, which is looked up percent-decoded (both ways, when that
// differs). A reference to another file names none: it can't load in an image. None for a data: URL, a document of its
// own, whose elements a copy would draw, and which isn't counted here.
fn use_target(raw: &[u8]) -> Option<Vec<Cow<'_, [u8]>>> {
    let url = url_of(xml_value(raw));
    if url.first() != Some(&b'#') {
        return (!is_data(&url)).then(Vec::new);
    }
    let id = match url {
        Cow::Borrowed(u) => Cow::Borrowed(&u[1..]),
        Cow::Owned(mut u) => {
            u.remove(0);
            Cow::Owned(u)
        }
    };
    Some(match percent_decoded(&id) {
        Some(decoded) if decoded != *id => vec![id, Cow::Owned(decoded)],
        _ => vec![id],
    })
}

// A URL as a browser reads one from a value: tabs and line ends dropped wherever they are, then spaces and controls at
// either end.
fn url_of(value: Cow<'_, [u8]>) -> Cow<'_, [u8]> {
    let value = if value.iter().any(|&c| matches!(c, b'\t' | b'\n' | b'\r')) {
        Cow::Owned(value.iter().copied().filter(|c| !matches!(c, b'\t' | b'\n' | b'\r')).collect())
    } else {
        value
    };
    let start = value.iter().position(|&c| c > b' ').unwrap_or(value.len());
    let end = value.iter().rposition(|&c| c > b' ').map_or(start, |p| p + 1);
    match value {
        Cow::Borrowed(v) => Cow::Borrowed(&v[start..end]),
        Cow::Owned(mut v) => {
            v.truncate(end);
            v.drain(..start);
            Cow::Owned(v)
        }
    }
}

fn is_data(url: &[u8]) -> bool {
    url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case(b"data:"))
}

// An attribute's value as an XML parser hands it over: tabs and line ends as spaces, and character references and the
// five named ones replaced.
fn xml_value(raw: &[u8]) -> Cow<'_, [u8]> {
    xml_chars(raw, true)
}

// Text as an XML parser hands it over: its references replaced.
fn xml_text(raw: &[u8]) -> Cow<'_, [u8]> {
    xml_chars(raw, false)
}

// Characters with their references replaced, and in an attribute's value (`value`), tabs and line ends written as such
// as spaces (one written as a reference stays what it is). Anything else after a '&' is an error, where a browser
// stops, and is kept as written.
fn xml_chars(raw: &[u8], value: bool) -> Cow<'_, [u8]> {
    if !raw.iter().any(|&c| c == b'&' || (value && matches!(c, b'\t' | b'\n' | b'\r'))) {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'&' {
            if let Some((c, n)) = reference(&raw[i + 1..]) {
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                i += 1 + n;
                continue;
            }
        }
        match raw[i] {
            b'\r' if value && raw.get(i + 1) == Some(&b'\n') => {} // one line end, one space
            b'\t' | b'\n' | b'\r' if value => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    Cow::Owned(out)
}

// The character a reference past its '&' stands for (&#65; &#x41; &lt;), and its length with the ';'.
fn reference(s: &[u8]) -> Option<(char, usize)> {
    let end = s.iter().position(|&c| !c.is_ascii_alphanumeric() && c != b'#')?;
    if s[end] != b';' {
        return None;
    }
    let body = std::str::from_utf8(&s[..end]).ok()?;
    let c = match body {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "apos" => '\'',
        "quot" => '"',
        _ => match body.strip_prefix('#')? {
            hex if hex.starts_with('x') => char::from_u32(u32::from_str_radix(&hex[1..], 16).ok()?)?,
            dec => char::from_u32(dec.parse().ok()?)?,
        },
    };
    Some((c, end + 1))
}

// `s` with each %XX as the byte it stands for, when it has any.
fn percent_decoded(s: &[u8]) -> Option<Vec<u8>> {
    if !s.contains(&b'%') {
        return None;
    }
    let hex = |at: usize| s.get(at).and_then(|&c| (c as char).to_digit(16));
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let (b'%', Some(high), Some(low)) = (s[i], hex(i + 1), hex(i + 2)) {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- what an SVG embeds

// An image drawn as an <img> loads no other file, but it does load data: URLs, so an SVG can hold images of its own,
// as design tools write small pictures into the files they export. Each is checked as revue checks a file (data_url),
// at most MAX_NESTING levels below the file: an SVG the file embeds is one level down, one that SVG embeds two.
const MAX_NESTING: u32 = 3;

// What the data: URLs an SVG embeds come to so far, at every level: the pixels of the raster images among them, and how
// many levels down the document being read is (0 for the file itself). Also what may change the root's size besides
// its own attributes (svg_size), in the file itself, not an SVG it embeds: `styled`, a style sheet that could set the
// root's font size (font_sheet) or an animation of it, and `resized`, an animation of its size (animation).
#[derive(Default)]
struct Embeds {
    pixels: u64,
    depth: u32,
    styled: bool,
    resized: bool,
}

// What the data: URLs an attribute names draw (data_url), whatever its name, since an image can load from many: its
// value as a whole (an href, a src), or else the url()s and strings in it as CSS (a style attribute, fill="url(...)").
// A srcset lists URLs, and an animation's values lists values (one of which an <animate> can make an href), each read
// on its own. Reading an attribute that loads nothing as if it did can only refuse more, so that is done with any
// attribute, but a value that only starts like a data: URL and isn't one (data_of can't read it) is refused only in an
// attribute whose value can be a URL (URL_ATTRS): elsewhere it is text (inkscape:label="Data: sales"), which loads
// nothing. So is each of an animation's values: a browser splits the list at each ';' too, so a data: URL with a
// ';base64' in it is cut in two there and loads nothing either.
fn attr_embeds(name: &[u8], raw: &[u8], embeds: &mut Embeds) -> Result<u64, Refusal> {
    let value = xml_value(raw);
    let name = local(name);
    match name {
        b"srcset" | b"imagesrcset" => srcset_embeds(&value, embeds),
        b"values" => value.split(|&c| c == b';').try_fold(0, |n, item| limited(n + value_embeds(item, false, embeds)?)),
        _ => value_embeds(&value, URL_ATTRS.contains(&name), embeds),
    }
}

// The attributes (by local name) whose whole value a browser may load as a URL, in SVG and in the HTML a
// <foreignObject> holds, an animation's to, from and by included (an <animate> can make them an href).
const URL_ATTRS: [&[u8]; 20] = [
    b"href", b"src", b"poster", b"data", b"background", b"lowsrc", b"dynsrc", b"icon", b"longdesc", b"cite",
    b"manifest", b"codebase", b"archive", b"action", b"formaction", b"ping", b"to", b"from", b"by", b"base",
];

// What the data: URLs a value names draw: the value as a whole when it is one, else the url()s and strings in it as
// CSS. One that only starts like a data: URL and can't be read as one is refused when `url` (the value can be a URL),
// and otherwise read as CSS, as any other text is.
fn value_embeds(value: &[u8], url: bool, embeds: &mut Embeds) -> Result<u64, Refusal> {
    let whole = url_of(Cow::Borrowed(value));
    if is_data(&whole) && (url || data_of(&whole).is_some()) {
        data_url(&whole, embeds)
    } else {
        css_embeds(value, embeds)
    }
}

// The URLs of a srcset, as HTML splits one: each runs to a space, or ends with the commas there, and its descriptors
// (2x, 300w) run to a comma outside parentheses.
fn srcset_embeds(set: &[u8], embeds: &mut Embeds) -> Result<u64, Refusal> {
    let space = |c: u8| c.is_ascii_whitespace();
    let mut drawn = 0;
    let mut i = 0;
    loop {
        i += set[i..].iter().position(|&c| !space(c) && c != b',').unwrap_or(set.len() - i);
        if i == set.len() {
            return Ok(drawn);
        }
        let end = set[i..].iter().position(|&c| space(c)).map_or(set.len(), |p| i + p);
        let url = &set[i..end];
        let candidate = url.iter().rposition(|&c| c != b',').map_or(&url[..0], |p| &url[..=p]);
        if is_data(candidate) {
            drawn = limited(drawn + data_url(candidate, embeds)?)?;
        }
        i = end;
        if candidate.len() == url.len() {
            let mut parens = false;
            while let Some(&c) = set.get(i) {
                i += 1;
                match c {
                    b'(' => parens = true,
                    b')' => parens = false,
                    b',' if !parens => break,
                    _ => {}
                }
            }
        }
    }
}

// Checks a data: URL an SVG embeds (`url`, read as a URL: url_of), and gives how many elements it draws: an SVG's,
// counted as the file's are, with what it embeds in turn a level further down; anything else draws none.
//
// A raster image's header must be one inspect reads and finds within MAX_PIXELS, and the pixels of all of them, at
// every level, may come to MAX_PIXELS at most. Each counts once, where it is written: the copies <use>s make of it
// load the same URL, which a browser decodes once. A browser tells a raster's format by its bytes, not the type a URL
// gives, so it is checked whatever that says. A style sheet (from an @import, an xml-stylesheet or a link) is read a
// level down for the data: URLs it names, and for whether it could set the root's font size (font_sheet); what an
// SVG's own style sheets and animations do is its own root's. Refused (Embedded), since what they would draw can't be
// told here: a URL that can't be decoded, an image of a format revue doesn't read (by its type, or its bytes:
// other_image), and a document that is neither, such as HTML. Anything else, a font or text, is no image and draws
// none, so it is let be.
fn data_url(url: &[u8], embeds: &mut Embeds) -> Result<u64, Refusal> {
    let (kind, data) = data_of(url).ok_or(Refusal::Embedded)?;
    if kind == SVG.as_bytes() {
        if svg_root(utf8_start(&data)).is_none() {
            return Err(Refusal::Embedded);
        }
        // What its style sheets and animations do is its own root's.
        let file = (embeds.styled, embeds.resized);
        let drawn = deeper(embeds, |embeds| svg_elements(&data, embeds));
        (embeds.styled, embeds.resized) = file;
        return drawn;
    }
    if let Some(img) = inspect(&data, data.len() as u64) {
        // Counted on the pixels decoding it takes, not the size it is drawn at, which its EXIF can make tiny.
        let Some((w, h)) = img.pixels.filter(|_| img.refused.is_none()) else { return Err(Refusal::Embedded) };
        embeds.pixels += w as u64 * h as u64;
        return if embeds.pixels > MAX_PIXELS { Err(Refusal::Embedded) } else { Ok(0) };
    }
    let markup = kind.ends_with(b"xml") || kind == b"text/html" || kind == b"text/xsl";
    if kind.starts_with(b"image/") || other_image(&data) || markup {
        return Err(Refusal::Embedded);
    }
    if kind == b"text/css" {
        embeds.styled |= font_sheet(&data);
        embeds.resized |= zoom_sheet(&data);
        return deeper(embeds, |embeds| css_embeds(&data, embeds));
    }
    Ok(0)
}

// Reads a document a data: URL holds (an SVG, a style sheet) a level further down, past MAX_NESTING refused.
fn deeper(embeds: &mut Embeds, read: impl FnOnce(&mut Embeds) -> Result<u64, Refusal>) -> Result<u64, Refusal> {
    if embeds.depth == MAX_NESTING {
        return Err(Refusal::Embedded);
    }
    embeds.depth += 1;
    let drawn = read(embeds);
    embeds.depth -= 1;
    drawn
}

// Images browsers draw that revue doesn't read the size of, told by their first bytes as a browser tells them: BMP, a
// cursor, an ISO media file (AVIF, HEIF), JPEG XL, TIFF, JPEG 2000 and PDF (the last three in Safari).
fn other_image(d: &[u8]) -> bool {
    const SIGNATURES: [&[u8]; 9] = [
        b"BM",
        b"\0\0\x02\0",
        b"\xff\x0a",
        b"\0\0\0\x0cJXL ",
        b"II*\0",
        b"MM\0*",
        b"\0\0\0\x0cjP  ",
        b"\xff\x4f\xff\x51",
        b"%PDF",
    ];
    SIGNATURES.iter().any(|s| d.starts_with(s)) || d.get(4..8) == Some(b"ftyp")
}

// A data: URL's media type (lowercased, without its parameters: image/png) and its bytes, read as a browser's fetch
// reads one: a '#' starts its fragment, which is no part of the data, the type runs to the first ',', and the data
// after it is percent-decoded, then decoded as base64 when the type ends with ";base64" (forgiving-base64: spaces
// dropped, one or two '=' at the end). None when there is no ',' or the base64 isn't. `url` starts with "data:".
fn data_of(url: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let url = &url[5..];
    let url = &url[..url.iter().position(|&c| c == b'#').unwrap_or(url.len())];
    let comma = url.iter().position(|&c| c == b',')?;
    let mut kind = url[..comma].trim_ascii();
    let body = &url[comma + 1..];
    let body = percent_decoded(body).map_or(Cow::Borrowed(body), Cow::Owned);
    let data = match base64_kind(kind) {
        Some(rest) => {
            kind = rest;
            base64(&body)?
        }
        None => body.into_owned(),
    };
    let essence = kind.split(|&c| c == b';').next().unwrap_or_default();
    Some((essence.trim_ascii().to_ascii_lowercase(), data))
}

// A data: URL's media type without the ";base64" that ends it (a ';', any spaces, then base64 in any case), if it does.
fn base64_kind(kind: &[u8]) -> Option<&[u8]> {
    let rest = &kind[..kind.len().checked_sub(6).filter(|&n| kind[n..].eq_ignore_ascii_case(b"base64"))?];
    let semicolon = rest.iter().rposition(|&c| c != b' ')?;
    (rest[semicolon] == b';').then(|| &rest[..semicolon])
}

// Base64 as a browser decodes a data: URL's (forgiving-base64): spaces dropped, and one or two '=' at the end when the
// length is a multiple of 4. None for any other character, or a length that leaves a lone character over.
fn base64(s: &[u8]) -> Option<Vec<u8>> {
    let mut s: Vec<u8> = s.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    if s.len().is_multiple_of(4) {
        let pad = s.iter().rev().take(2).take_while(|&&c| c == b'=').count();
        s.truncate(s.len() - pad);
    }
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 2);
    let (mut bits, mut n) = (0u32, 0);
    for c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = ((bits << 6) | v as u32) & 0xFFFF;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------- style sheets

// What the data: URLs a style sheet names draw, each checked (data_url): a url()'s, or a string's, which is where a URL
// goes in an @import or an image-set(); a string that is no URL (content: "...") is checked too, which can only refuse
// more. The sheet is read as CSS reads it, as far as this needs: past comments, strings ending at their quote or a line
// end, and escapes decoded, a function's name included (\75 rl() is url()).
fn css_embeds(css: &[u8], embeds: &mut Embeds) -> Result<u64, Refusal> {
    let mut drawn = 0;
    let mut i = 0;
    while let Some(&c) = css.get(i) {
        let url = match c {
            b'/' if css.get(i + 1) == Some(&b'*') => {
                i = past(css, i + 2, b"*/");
                continue;
            }
            b'"' | b'\'' => css_string(css, &mut i),
            _ if is_name(c) || css_escape_at(css, i) => {
                let name = css_name(css, &mut i);
                if !name.eq_ignore_ascii_case(b"url") || css.get(i) != Some(&b'(') {
                    continue;
                }
                i += 1;
                i += css[i..].iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(css.len() - i);
                if matches!(css.get(i), Some(b'"' | b'\'')) {
                    continue; // url("..."), whose string is read next
                }
                css_url(css, &mut i)
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let url = url_of(Cow::Owned(url));
        if is_data(&url) {
            drawn = limited(drawn + data_url(&url, embeds)?)?;
        }
    }
    Ok(drawn)
}

// Whether a style sheet the file applies (a <style>'s, or a data: URL's: an xml-stylesheet's, a link's, an @import's)
// could set the root's font size: it says font anywhere, in any case (font-size, font, @keyframes of them), sets the
// all property (all, then a ':' past spaces and comments), or holds an escape, which could spell either. One that
// doesn't leaves the root's em and rem to its own attributes.
fn font_sheet(css: &[u8]) -> bool {
    if css.contains(&b'\\') || contains_ignore_case(css, b"font") {
        return true;
    }
    font_sheet_all(css)
}

// Whether a style sheet the file applies could zoom the root, which Firefox applies to the size it draws an SVG at and
// Chromium doesn't: it says zoom anywhere, in any case, or holds an escape, which could spell it.
fn zoom_sheet(css: &[u8]) -> bool {
    css.contains(&b'\\') || contains_ignore_case(css, b"zoom")
}

fn contains_ignore_case(s: &[u8], word: &[u8]) -> bool {
    s.windows(word.len()).any(|w| w.eq_ignore_ascii_case(word))
}

// Whether a style sheet sets the all property: all, then a ':' past spaces and comments.
fn font_sheet_all(css: &[u8]) -> bool {
    // Past the spaces and comments from `j` on. Each is read at most twice: here, and as the loop below goes on.
    let blank = |mut j: usize| loop {
        j += css[j..].iter().take_while(|c| c.is_ascii_whitespace()).count();
        if !css[j..].starts_with(b"/*") {
            return j;
        }
        j = past(css, j + 2, b"*/");
    };
    let mut i = 0;
    while i < css.len() {
        if css[i..].starts_with(b"/*") {
            i = past(css, i + 2, b"*/");
            continue;
        }
        let name = css[i..].iter().take_while(|&&c| is_name(c)).count();
        if name == 0 {
            i += 1;
            continue;
        }
        i += name;
        if css[i - name..i].eq_ignore_ascii_case(b"all") && css.get(blank(i)) == Some(&b':') {
            return true;
        }
    }
    false
}

fn is_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80
}

fn is_newline(c: u8) -> bool {
    matches!(c, b'\n' | b'\r' | b'\x0c')
}

// Whether a '\' at `i` starts an escape: one that isn't before a line end.
fn css_escape_at(css: &[u8], i: usize) -> bool {
    css[i] == b'\\' && !css.get(i + 1).copied().is_some_and(is_newline)
}

// The character an escape stands for, written to `out`, with `i` past its '\' and moved past it: up to 6 hex digits
// and one space after them, or any other character as itself.
fn css_escape(css: &[u8], i: &mut usize, out: &mut Vec<u8>) {
    let digits = css[*i..].iter().take(6).take_while(|c| c.is_ascii_hexdigit()).count();
    if digits == 0 {
        match css.get(*i) {
            Some(&c) => {
                out.push(c);
                *i += 1;
            }
            None => out.extend_from_slice("\u{fffd}".as_bytes()),
        }
        return;
    }
    let n = css[*i..*i + digits].iter().fold(0, |n, &c| n * 16 + (c as char).to_digit(16).unwrap_or(0));
    *i += digits;
    if css[*i..].starts_with(b"\r\n") {
        *i += 2;
    } else if css.get(*i).is_some_and(|c| c.is_ascii_whitespace()) {
        *i += 1;
    }
    let c = char::from_u32(n).filter(|&c| c != '\0').unwrap_or('\u{fffd}');
    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

// A name (an identifier, a function's name) at `i`, with its escapes decoded, and `i` moved past it.
fn css_name(css: &[u8], i: &mut usize) -> Vec<u8> {
    let mut name = Vec::new();
    while let Some(&c) = css.get(*i) {
        if is_name(c) {
            name.push(c);
            *i += 1;
        } else if css_escape_at(css, *i) {
            *i += 1;
            css_escape(css, i, &mut name);
        } else {
            break;
        }
    }
    name
}

// A string's value, `i` at its opening quote and moved past it to the closing one, or to a line end, where CSS ends a
// string that isn't closed (an escaped line end is no part of the string).
fn css_string(css: &[u8], i: &mut usize) -> Vec<u8> {
    let quote = css[*i];
    let mut s = Vec::new();
    *i += 1;
    while let Some(&c) = css.get(*i) {
        if is_newline(c) {
            break;
        }
        *i += 1;
        if c == quote {
            break;
        } else if c != b'\\' {
            s.push(c);
        } else if css[*i..].starts_with(b"\r\n") {
            *i += 2;
        } else if css.get(*i).copied().is_some_and(is_newline) {
            *i += 1;
        } else if *i < css.len() {
            css_escape(css, i, &mut s);
        }
    }
    s
}

// An unquoted url()'s value, `i` past the spaces after its '(' and moved past its ')'. A space, a quote, a '(' or a
// control inside it make it no URL to CSS, which loads nothing for it, but what came before is checked all the same,
// which can only refuse more; it is read on to its ')' (an escaped one doesn't end it).
fn css_url(css: &[u8], i: &mut usize) -> Vec<u8> {
    let mut url = Vec::new();
    let mut skipped = Vec::new();
    let mut ended = false;
    while let Some(&c) = css.get(*i) {
        *i += 1;
        if c == b')' {
            break;
        } else if css_escape_at(css, *i - 1) {
            css_escape(css, i, if ended { &mut skipped } else { &mut url });
        } else if c.is_ascii_whitespace() || matches!(c, b'"' | b'\'' | b'(' | b'\\' | 0x7f) || c < b' ' {
            ended = true;
        } else if !ended {
            url.push(c);
        }
    }
    url
}

// The count svg_elements gives, from the groups it read (0 the document) and the groups each name has: the document's
// elements plus what its <use>s draw (OVER for any more than MAX_SVG_ELEMENTS). What a group's <use>s draw is the
// weight of each name they refer to, plus what the <use>s of the groups within it draw, and a name's weight is the
// most any group with that id draws: its elements and what its <use>s draw. Each group and name is worked out once
// (memoized), by a walk with a stack of its own, since a document can nest deeper than a thread's stack. One met again
// while it is being worked out is a cycle, which would draw without end, so OVER. So is any part that comes to OVER,
// since it is part of the whole, so the walk ends there.
fn weigh(groups: &[Group], named: &[Vec<usize>]) -> u64 {
    #[derive(Clone, Copy)]
    enum Node {
        Group(usize),
        Name(usize),
    }
    // A group's Done holds what its <use>s draw, a name's its weight.
    #[derive(Clone, Copy)]
    enum State {
        New,
        Open,
        Done(u64),
    }
    let mut group_state = vec![State::New; groups.len()];
    let mut name_state = vec![State::New; named.len()];
    group_state[0] = State::Open;
    // The nodes being worked out, each with how many of the nodes it depends on are added in, and their total so far.
    let mut stack = vec![(Node::Group(0), 0, 0)];
    while let Some(&(node, k, total)) = stack.last() {
        let next = match node {
            Node::Group(g) => match groups[g].inner.get(k) {
                Some(&inner) => Some(Node::Group(inner)),
                None => groups[g].uses.get(k - groups[g].inner.len()).map(|&n| Node::Name(n)),
            },
            Node::Name(n) => named[n].get(k).map(|&g| Node::Group(g)),
        };
        let Some(next) = next else {
            stack.pop();
            match node {
                Node::Group(g) => group_state[g] = State::Done(total),
                Node::Name(n) => name_state[n] = State::Done(total),
            }
            continue;
        };
        let state = match next {
            Node::Group(g) => &mut group_state[g],
            Node::Name(n) => &mut name_state[n],
        };
        match *state {
            State::Open => return OVER,
            State::New => {
                *state = State::Open;
                stack.push((next, 0, 0));
            }
            State::Done(v) => {
                let total = match (node, next) {
                    (Node::Name(_), Node::Group(g)) => total.max(groups[g].elements + v),
                    _ => total + v,
                };
                if total > MAX_SVG_ELEMENTS {
                    return OVER;
                }
                let last = stack.len() - 1;
                stack[last] = (node, k + 1, total);
            }
        }
    }
    match group_state[0] {
        State::Done(drawn) => (groups[0].elements + drawn).min(OVER),
        _ => OVER,
    }
}

// ---------------------------------------------------------------- whether browsers draw an SVG at all

// Why browsers draw nothing of an SVG: a short reason, such as "</h> doesn't close <g>", and its line, from 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Malformed {
    pub reason: String,
    pub line: usize,
}

const SVG_NS: &[u8] = b"http://www.w3.org/2000/svg";
const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &[u8] = b"http://www.w3.org/2000/xmlns/";
const BOM: &[u8] = b"\xEF\xBB\xBF";

// How deep elements may be nested, the root counted: both browsers draw 5,000 and nothing of 5,001.
const MAX_DEPTH: usize = 5_000;

// A character of a legacy encoding (see utf8_label), which could be any: taken for U+10000, which may be anywhere a
// character may, in a name too.
const LEGACY: u32 = 0x1_0000;

// Whether browsers draw `text`, an SVG file, at all. Chromium 156 and Firefox 157 draw nothing of an SVG as an <img>
// when it isn't well-formed XML with namespaces, or its root isn't an <svg> in the SVG namespace, where the page could
// only say that it couldn't be loaded. So it is read as their parsers read it, and refused with the first error and its
// line where both draw nothing. Where they differ (each case was drawn in both), it is let be: a refusal hides the
// image in every browser.
//
// Refused, as both refuse it: an end tag that doesn't close the element open; an element left open; text, an element,
// a CDATA section or an end tag after the root ends (comments, processing instructions and spaces may follow it); a
// doctype after the root starts, or a second one; '<' or '</' not followed by a name; an end tag with more than its
// name; elements nested over MAX_DEPTH deep. An attribute repeated, without a quoted value, with no space before it, or
// with a '<' in its value, and a '/' not right before its tag's '>'. In text, a '&' or '<' that starts nothing, and
// "]]>". A reference to an entity but XML's five (a doctype naming a DTD defines none: browsers don't load it), and a
// character reference written otherwise than &#65; or &#x41;, or to a character XML doesn't allow. A comment with "--"
// in it; a comment, CDATA section or processing instruction not closed; a processing instruction without a target, or
// whose target isn't followed by a space; an XML declaration anywhere but the very start, written in another case
// (<?XML), without a version, of a version other than 1.x, with other pseudo-attributes than version, encoding and
// standalone in that order, or standalone other than yes or no. A doctype not written <!DOCTYPE, without a name, or
// without a quoted system ID where it needs one, or with a character a public ID can't have. A character XML doesn't
// allow (controls, U+FFFE, U+FFFF), written or referred to, and a name that starts with a character no name can start
// with or holds one no name can hold (XML's fifth edition). A name with more than one ':' or one at either end, a
// prefix used where it isn't declared (xlink:href without xmlns:xlink is a common one), a prefix undeclared
// (xmlns:x=""), xml bound to another namespace, xmlns declared or used on an element, the default namespace bound to
// xml's or xmlns's, two attributes of the same namespace and local name, and a root in no namespace or another than
// SVG's.
//
// Let be, since Chromium draws them and Firefox doesn't: a namespace declaration repeated (Chromium takes the first),
// XML 1.1 and later, an encoding name of any form, a doctype with no space after DOCTYPE or between its IDs, a control
// in its system ID (and Firefox draws a '<' or '&' there, where Chromium doesn't), a ':' anywhere in a processing
// instruction's target, a local name that starts with a character only a name's middle can have (a:1b), a prefix bound
// to xml's or xmlns's namespace, the characters the fifth edition lets names have but the fourth, which Firefox
// follows, doesn't, and a '<' in an attribute's value followed by '/' or '!', past which nothing is read; and an
// element with the prefix xml, which Firefox draws.
//
// The bytes must be UTF-8, unless the declaration names a legacy encoding (utf8_label), which Firefox decodes and
// Chromium doesn't: then only what is ASCII, the same in each, is checked. A byte order mark makes them UTF-8 whatever
// the declaration says, in both. The work grows with the text's length, with no recursion, and stops at the first error.
fn well_formed(text: &[u8]) -> Result<(), Malformed> {
    let (scopes, declared, open) = (HashMap::new(), Vec::new(), Vec::new());
    let mut xml = Xml { s: text, utf8: true, html_entities: false, scopes, declared, open };
    match xml.document() {
        Err(Stop::Malformed(why)) => Err(why),
        Ok(()) | Err(Stop::LetBe) => Ok(()),
    }
}

// Why the reading stops before the end: an error, or what is past it can't be told from here, as browsers read it
// differently from each other and from XML, so the file is let be.
enum Stop {
    Malformed(Malformed),
    LetBe,
}

// The doctype public IDs (exactly, in case too) under which Chromium and Firefox define HTML's named entities (&nbsp;,
// &eacute;, ...), as measured in Chromium 156 and Firefox 157, whatever the root's name and the system ID: Chromium
// takes HTML's own, Firefox those of its htmlmathml-f.ent. A name that is none of them is still an error in both, but
// revue doesn't carry either list, so under one of these doctypes a named entity is let be. XHTML Basic 1.1 isn't one.
const XHTML_PUBLIC_IDS: [&[u8]; 11] = [
    b"-//W3C//DTD XHTML 1.0 Transitional//EN",
    b"-//W3C//DTD XHTML 1.0 Strict//EN",
    b"-//W3C//DTD XHTML 1.0 Frameset//EN",
    b"-//W3C//DTD XHTML 1.1//EN",
    b"-//W3C//DTD XHTML Basic 1.0//EN",
    b"-//W3C//DTD XHTML 1.1 plus MathML 2.0//EN",
    b"-//W3C//DTD XHTML 1.1 plus MathML 2.0 plus SVG 1.1//EN",
    b"-//W3C//DTD MathML 2.0//EN",
    b"-//WAPFORUM//DTD XHTML Mobile 1.0//EN",
    b"-//WAPFORUM//DTD XHTML Mobile 1.1//EN",
    b"-//WAPFORUM//DTD XHTML Mobile 1.2//EN",
];

// Whether browsers read a document that declares the encoding `label` as UTF-8, as they read one that declares none.
// Chromium reads an SVG drawn as an image as UTF-8 whatever it declares. Firefox decodes it as it declares, except for
// these labels of the Encoding Standard (in any case, between spaces): UTF-8's, UTF-16's (which it reads as UTF-8, the
// bytes being 8-bit) and the replacement encoding's (of which it draws nothing). Any other is taken for a legacy
// encoding's. Both read a label of no encoding at all as UTF-8, but telling one from a legacy encoding's would take the
// standard's 200 labels, so that is let be too.
fn utf8_label(label: &[u8]) -> bool {
    const LABELS: [&[u8]; 21] = [
        b"unicode-1-1-utf-8",
        b"unicode11utf8",
        b"unicode20utf8",
        b"utf-8",
        b"utf8",
        b"x-unicode20utf8",
        b"unicodefffe",
        b"utf-16be",
        b"csunicode",
        b"iso-10646-ucs-2",
        b"ucs-2",
        b"unicode",
        b"unicodefeff",
        b"utf-16",
        b"utf-16le",
        b"csiso2022kr",
        b"hz-gb-2312",
        b"iso-2022-cn",
        b"iso-2022-cn-ext",
        b"iso-2022-kr",
        b"replacement",
    ];
    let label = label.trim_ascii();
    LABELS.iter().any(|l| l.eq_ignore_ascii_case(label))
}

// Whether `text` declares a legacy encoding (see utf8_label) in an XML declaration at its start, with no byte order
// mark, which makes it UTF-8 whatever it declares: svg_elements' look at it, before well_formed reads it in full.
fn legacy_encoding(text: &[u8]) -> bool {
    if text.starts_with(BOM) || !text.starts_with(b"<?xml") {
        return false;
    }
    let (decl, _) = up_to(text, 0, b"?>");
    let Some(at) = decl.windows(8).position(|w| w == b"encoding") else { return false };
    let i = spaces(decl, at + 8);
    if decl.get(i) != Some(&b'=') {
        return false;
    }
    let q = spaces(decl, i + 1);
    let Some(&quote @ (b'"' | b'\'')) = decl.get(q) else { return false };
    let value = &decl[q + 1..];
    !utf8_label(&value[..value.iter().position(|&c| c == quote).unwrap_or(value.len())])
}

// An SVG's markup as an XML parser reads it, for well_formed: its bytes, whether they are read as UTF-8 (else as a
// legacy encoding's), whether its doctype gives it HTML's entities, the namespaces each prefix is bound to ("" for the
// default namespace, where an empty one is none), innermost last, the prefixes the open elements declared, in order,
// and the open elements.
struct Xml<'a> {
    s: &'a [u8],
    utf8: bool,
    html_entities: bool,
    scopes: HashMap<&'a [u8], Vec<Cow<'a, [u8]>>>,
    declared: Vec<&'a [u8]>,
    open: Vec<Open<'a>>,
}

// An open element: where its start tag is, its name, and how many prefixes were declared before it.
struct Open<'a> {
    at: usize,
    name: &'a [u8],
    declared: usize,
}

// An attribute of a start tag: where it is, its name, and its value as written.
struct Attr<'a> {
    at: usize,
    name: &'a [u8],
    value: &'a [u8],
}

const NO_TAG: &str = "a '<' that starts no tag (in text, write &lt;)";
const NO_BANG: &str = "a '<!' that starts no comment or CDATA section";

impl<'a> Xml<'a> {
    fn document(&mut self) -> Result<(), Stop> {
        let s = self.s;
        let mut i = if s.starts_with(BOM) { BOM.len() } else { 0 };
        if let Some(end) = self.xml_decl(i)? {
            i = end;
        }
        // Before the root: spaces, comments, processing instructions and a doctype.
        let mut doctype = false;
        loop {
            i = spaces(s, i);
            let rest = &s[i..];
            if rest.starts_with(b"<!--") {
                i = self.comment(i)?;
            } else if rest.starts_with(b"<?") {
                i = self.pi(i)?;
            } else if rest.starts_with(b"<!DOCTYPE") && !doctype {
                (i, doctype) = (self.doctype(i)?, true);
            } else if rest.first() == Some(&b'<') && self.name_end(i + 1)? > i + 1 {
                break;
            } else {
                return Err(self.misplaced(i, "before the root element"));
            }
        }
        i = self.root(i)?;
        // After it: spaces, comments and processing instructions.
        loop {
            i = spaces(s, i);
            let rest = &s[i..];
            if rest.is_empty() {
                return Ok(());
            } else if rest.starts_with(b"<!--") {
                i = self.comment(i)?;
            } else if rest.starts_with(b"<?") {
                i = self.pi(i)?;
            } else {
                return Err(self.misplaced(i, "after the root element ends"));
            }
        }
    }

    // The root element, from its start tag at `at`, with all it holds: where it ends.
    fn root(&mut self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let mut i = self.start_tag(at)?;
        while let Some(open) = self.open.last() {
            let text_end = self.text(i)?;
            let rest = &s[text_end..];
            if rest.is_empty() {
                return Err(self.fail(open.at, format!("<{}> isn't closed", shown(open.name))));
            }
            i = if rest.starts_with(b"</") {
                self.end_tag(text_end)?
            } else if rest.starts_with(b"<!--") {
                self.comment(text_end)?
            } else if rest.starts_with(b"<![CDATA[") {
                self.cdata(text_end)?
            } else if rest.starts_with(b"<?") {
                self.pi(text_end)?
            } else if rest.get(..9).is_some_and(|d| d.eq_ignore_ascii_case(b"<!doctype")) {
                return Err(self.fail(text_end, "a doctype after the root element starts"));
            } else if rest.starts_with(b"<!") {
                return Err(self.fail(text_end, NO_BANG));
            } else {
                self.start_tag(text_end)?
            };
        }
        Ok(i)
    }

    // What is out of place at `i`, before or after the root element (`place`).
    fn misplaced(&self, i: usize, place: &str) -> Stop {
        let s = self.s;
        let rest = &s[i..];
        let reason = if rest.is_empty() {
            "no root element".to_string()
        } else if rest.get(..9).is_some_and(|d| d.eq_ignore_ascii_case(b"<!doctype")) {
            match (rest.starts_with(b"<!DOCTYPE"), place.starts_with("before")) {
                (false, _) => "a doctype is written <!DOCTYPE".to_string(),
                (true, true) => "a second doctype".to_string(),
                (true, false) => "a doctype after the root element starts".to_string(),
            }
        } else if rest.starts_with(b"<![CDATA[") {
            format!("a CDATA section {place}")
        } else if rest.starts_with(b"<!") {
            NO_BANG.to_string()
        } else if rest.starts_with(b"</") {
            format!("an end tag {place}")
        } else if rest[0] == b'<' {
            match self.name_end(i + 1) {
                Ok(end) if end > i + 1 => format!("<{}> {place}", shown(&s[i + 1..end])),
                Ok(_) => return self.no_name(i + 1, NO_TAG),
                Err(e) => return e,
            }
        } else {
            match self.char_at(i) {
                Ok((c, _)) if (0x21..0x7F).contains(&c) => format!("text {place}"),
                Ok((c, _)) => format!("{} {place}", self.desc(c)),
                Err(e) => return e,
            }
        };
        self.fail(i, reason)
    }

    // A start tag at `at` ('<' and a name): where it ends. The root's must be an <svg> in the SVG namespace.
    fn start_tag(&mut self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let name_end = self.name_end(at + 1)?;
        if name_end == at + 1 {
            return Err(self.no_name(at + 1, NO_TAG));
        }
        let name = &s[at + 1..name_end];
        self.qname(at + 1, name)?;
        if self.open.len() == MAX_DEPTH {
            return Err(self.fail(at, "elements nested over 5,000 deep"));
        }
        let (attrs, empty, end) = self.attributes(at, name_end)?;

        // Each name once, and the namespaces declared. A declaration repeated is let be: Chromium takes the first.
        let base = self.declared.len();
        let mut seen = HashSet::new();
        for a in &attrs {
            let first = attrs.len() < 2 || seen.insert(a.name);
            let prefix: &[u8] = match a.name.strip_prefix(b"xmlns") {
                Some(b"") => b"",
                Some(p) if p.starts_with(b":") => &p[1..],
                _ if first => continue,
                _ => return Err(self.fail(a.at, format!("<{}> has two {} attributes", shown(name), shown(a.name)))),
            };
            if !first {
                continue;
            }
            let uri = xml_value(a.value);
            let wrong = if prefix == b"xmlns" {
                Some("the prefix xmlns can't be declared".to_string())
            } else if prefix.is_empty() && (*uri == *XML_NS || *uri == *XMLNS_NS) {
                Some(format!("the default namespace can't be {}", shown(&uri)))
            } else if !prefix.is_empty() && uri.is_empty() {
                Some(format!("{}=\"\" can't undeclare a prefix", shown(a.name)))
            } else if prefix == b"xml" && *uri != *XML_NS {
                Some("the prefix xml can't be bound to another namespace".to_string())
            } else {
                None
            };
            if let Some(why) = wrong {
                return Err(self.fail(a.at, why));
            }
            self.scopes.entry(prefix).or_default().push(uri);
            self.declared.push(prefix);
        }

        // The element's namespace, and its attributes'.
        let (prefix, local) = split_name(name);
        let ns = match prefix {
            Some(b"xmlns") => {
                return Err(self.fail(at + 1, format!("<{}>: an element can't have the prefix xmlns", shown(name))));
            }
            Some(b"xml") => Some(XML_NS),
            Some(p) => Some(self.namespace(p).ok_or_else(|| self.undeclared(at + 1, p, None, name))?),
            None => self.namespace(b"").filter(|u| !u.is_empty()),
        };
        if self.open.is_empty() && (local != b"svg" || ns != Some(SVG_NS)) {
            let svg = String::from_utf8_lossy(SVG_NS);
            return Err(self.fail(at, match ns {
                _ if local != b"svg" => "the root element isn't <svg>".to_string(),
                None => format!("no xmlns=\"{svg}\" on the root <svg>"),
                Some(other) => format!("the root <svg> is in the namespace \"{}\", not \"{svg}\"", shown(other)),
            }));
        }
        let mut expanded = HashMap::new();
        for a in &attrs {
            let (Some(p), local) = split_name(a.name) else { continue };
            if p == b"xmlns" {
                continue;
            }
            let ns = match p {
                b"xml" => XML_NS,
                _ => self.namespace(p).ok_or_else(|| self.undeclared(a.at, p, Some(a.name), name))?,
            };
            if let Some(other) = expanded.insert((ns, local), a.name) {
                let why = format!("{} and {} on <{}> are the same attribute", shown(other), shown(a.name), shown(name));
                return Err(self.fail(a.at, why));
            }
        }

        if empty {
            self.close(base);
        } else {
            self.open.push(Open { at, name, declared: base });
        }
        Ok(end)
    }

    // The attributes of the start tag at `at`, from `from`, past its name: them, whether it closes itself (/>), and
    // where it ends.
    fn attributes(&self, at: usize, from: usize) -> Result<(Vec<Attr<'a>>, bool, usize), Stop> {
        let s = self.s;
        let elem = &s[at + 1..from];
        let tag = || shown(elem);
        let mut attrs = Vec::new();
        let mut i = from;
        loop {
            let k = spaces(s, i);
            match s.get(k) {
                None => return Err(self.fail(at, format!("the start tag of <{}> isn't ended by '>'", tag()))),
                Some(b'>') => return Ok((attrs, false, k + 1)),
                Some(b'/') if s.get(k + 1) == Some(&b'>') => return Ok((attrs, true, k + 2)),
                Some(b'/') => return Err(self.fail(k, format!("a '/' in <{}> that isn't right before its '>'", tag()))),
                Some(_) => {}
            }
            let end = self.name_end(k)?;
            if end == k {
                let (c, _) = self.char_at(k)?;
                if is_name_char(c) {
                    return Err(self.no_name(k, ""));
                }
                return Err(self.fail(k, format!("{} in <{}>, where an attribute's name should be", self.desc(c), tag())));
            }
            let attr = &s[k..end];
            if k == i {
                return Err(self.fail(k, format!("no space before {} in <{}>", shown(attr), tag())));
            }
            self.qname(k, attr)?;
            let eq = spaces(s, end);
            if s.get(eq) != Some(&b'=') {
                return Err(self.fail(k, format!("{} has no value", on(attr, elem))));
            }
            let q = spaces(s, eq + 1);
            let Some(&quote @ (b'"' | b'\'')) = s.get(q) else {
                return Err(self.fail(q, format!("the value of {} isn't quoted", on(attr, elem))));
            };
            let close = self.value(q, quote, attr, elem)?;
            attrs.push(Attr { at: k, name: attr, value: &s[q + 1..close] });
            i = close + 1;
        }
    }

    // The value of `attr` on <`elem`>, from its opening quote at `q`: where its closing quote is. A '<' in it is an
    // error, and Firefox draws nothing of a file with one, but Chromium draws one followed by '/' or '!' (</b, <!--x-->,
    // <![CDATA[x]]>, <!DOCTYPE x>), and reads on from there as XML doesn't, so the file is let be.
    fn value(&self, q: usize, quote: u8, attr: &[u8], elem: &[u8]) -> Result<usize, Stop> {
        let s = self.s;
        let unclosed = || self.fail(q, format!("the value of {} isn't closed", on(attr, elem)));
        let mut i = q + 1;
        loop {
            i = match s.get(i) {
                Some(&c) if c == quote => return Ok(i),
                None => return Err(unclosed()),
                // One left open runs on into the next tag, where a '<' is the first error.
                Some(b'<') if !s[i..].contains(&quote) => return Err(unclosed()),
                Some(b'<') if matches!(s.get(i + 1), Some(b'/' | b'!')) => return Err(Stop::LetBe),
                Some(b'<') => return Err(self.fail(i, format!("a '<' in the value of {}", on(attr, elem)))),
                Some(b'&') => self.reference(i)?,
                Some(_) => self.char(i)?,
            };
        }
    }

    // The error for the prefix `prefix` of <`elem`>, or of its attribute `attr`, which isn't declared where it is used.
    fn undeclared(&self, at: usize, prefix: &[u8], attr: Option<&[u8]>, elem: &[u8]) -> Stop {
        let of = match attr {
            Some(attr) => on(attr, elem),
            None => format!("<{}>", shown(elem)),
        };
        self.fail(at, format!("the prefix {} of {of} isn't declared", shown(prefix)))
    }

    // The namespace `prefix` is bound to where the reading is ("" for the default namespace).
    fn namespace(&self, prefix: &[u8]) -> Option<&[u8]> {
        self.scopes.get(prefix)?.last().map(|uri| uri.as_ref())
    }

    // Drops the namespaces declared since `base`, as their element closes.
    fn close(&mut self, base: usize) {
        for prefix in self.declared.drain(base..) {
            if let Some(uris) = self.scopes.get_mut(prefix) {
                uris.pop();
            }
        }
    }

    // An end tag at `at` ("</"): where it ends. It must close the element open.
    fn end_tag(&mut self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let end = self.name_end(at + 2)?;
        if end == at + 2 {
            return Err(self.no_name(at + 2, "a '</' that starts no end tag"));
        }
        let name = &s[at + 2..end];
        let k = spaces(s, end);
        match s.get(k) {
            Some(b'>') => {}
            None => return Err(self.fail(at, format!("the end tag </{} isn't ended by '>'", shown(name)))),
            Some(_) => return Err(self.fail(k, format!("the end tag </{}> holds more than its name", shown(name)))),
        }
        let Some(open) = self.open.pop() else { return Err(self.fail(at, format!("</{}> closes nothing", shown(name)))) };
        if open.name != name {
            return Err(self.fail(at, format!("</{}> doesn't close <{}>", shown(name), shown(open.name))));
        }
        self.close(open.declared);
        Ok(k + 1)
    }

    // Text from `i` up to the next markup or the end: where that is.
    fn text(&self, mut i: usize) -> Result<usize, Stop> {
        let s = self.s;
        while let Some(&c) = s.get(i) {
            i = match c {
                b'<' => break,
                b'&' => self.reference(i)?,
                b']' if s[i..].starts_with(b"]]>") => return Err(self.fail(i, "\"]]>\" in text")),
                _ => self.char(i)?,
            };
        }
        Ok(i)
    }

    // A reference at `at` ('&'): where it ends. It is to one of the five entities XML defines, or a character XML allows.
    fn reference(&self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        // As written, for the reason: up to its ';', or what could be part of it.
        let written = || {
            let n = s[at + 1..].iter().take(24).take_while(|&&c| c.is_ascii_alphanumeric() || c == b'#').count();
            let end = at + 1 + n + usize::from(s.get(at + 1 + n) == Some(&b';'));
            String::from_utf8_lossy(&s[at..end]).into_owned()
        };
        if s.get(at + 1) == Some(&b'#') {
            let (from, radix) = if s.get(at + 2) == Some(&b'x') { (at + 3, 16) } else { (at + 2, 10) };
            let n = s[from..].iter().take_while(|c| (**c as char).is_digit(radix)).count();
            if n == 0 || s.get(from + n) != Some(&b';') {
                return Err(self.fail(at, format!("{} isn't a character reference", written())));
            }
            let digit = |v: u32, &d: &u8| v.checked_mul(radix)?.checked_add((d as char).to_digit(radix)?);
            if !s[from..from + n].iter().try_fold(0, digit).is_some_and(is_xml_char) {
                return Err(self.fail(at, format!("{} is a character XML doesn't allow", written())));
            }
            return Ok(from + n + 1);
        }
        let end = self.name_end(at + 1)?;
        let name = &s[at + 1..end];
        if name.is_empty() {
            return Err(self.fail(at, "a '&' that starts no reference (in text, write &amp;)"));
        }
        if s.get(end) != Some(&b';') {
            return Err(self.fail(at, format!("&{} isn't ended by ';'", shown(name))));
        }
        if !self.html_entities && !matches!(name, b"lt" | b"gt" | b"amp" | b"apos" | b"quot") {
            return Err(self.fail(at, format!("&{}; isn't defined", shown(name))));
        }
        Ok(end + 1)
    }

    // A comment at `at` ("<!--"): where it ends. "--" may only end it.
    fn comment(&self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let mut i = at + 4;
        loop {
            if s[i..].starts_with(b"--") {
                return match s.get(i + 2) {
                    Some(b'>') => Ok(i + 3),
                    _ => Err(self.fail(i, "\"--\" in a comment")),
                };
            }
            if i == s.len() {
                return Err(self.fail(at, "a comment isn't closed"));
            }
            i = self.char(i)?;
        }
    }

    // A CDATA section at `at` ("<![CDATA["): where it ends.
    fn cdata(&self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let mut i = at + 9;
        loop {
            if s[i..].starts_with(b"]]>") {
                return Ok(i + 3);
            }
            if i == s.len() {
                return Err(self.fail(at, "a CDATA section isn't closed"));
            }
            i = self.char(i)?;
        }
    }

    // A processing instruction at `at` ("<?"): where it ends. Its target is a name, followed by a space unless it ends
    // there. The XML declaration is one only at the very start (xml_decl).
    fn pi(&self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let end = self.name_end(at + 2)?;
        if end == at + 2 {
            return Err(self.no_name(at + 2, "a processing instruction without a target"));
        }
        let target = &s[at + 2..end];
        if target.eq_ignore_ascii_case(b"xml") {
            return Err(self.fail(at, match target {
                b"xml" => "an XML declaration that isn't at the very start".to_string(),
                _ => format!("a processing instruction can't be named {}", shown(target)),
            }));
        }
        let mut i = end;
        match s.get(i) {
            Some(&c) if is_space(c) || s[i..].starts_with(b"?>") => {}
            None => return Err(self.fail(at, "a processing instruction isn't closed")),
            Some(_) => {
                let what = self.desc(self.char_at(i)?.0);
                return Err(self.fail(i, format!("{what} right after <?{}, where a space should be", shown(target))));
            }
        }
        loop {
            if s[i..].starts_with(b"?>") {
                return Ok(i + 2);
            }
            if i == s.len() {
                return Err(self.fail(at, "a processing instruction isn't closed"));
            }
            i = self.char(i)?;
        }
    }

    // The XML declaration, if there is one at `at`, the very start: where it ends. Its encoding says how the bytes are
    // read (utf8_label).
    fn xml_decl(&mut self, at: usize) -> Result<Option<usize>, Stop> {
        let s = self.s;
        if !s[at..].starts_with(b"<?xml") || self.name_end(at + 2)? != at + 5 {
            return Ok(None);
        }
        const NAMES: [&[u8]; 3] = [b"version", b"encoding", b"standalone"];
        let mut next = 0; // where in NAMES the next pseudo-attribute may be
        let mut i = at + 5;
        loop {
            let k = spaces(s, i);
            if s[k..].starts_with(b"?>") {
                if next == 0 {
                    return Err(self.fail(at, "an XML declaration without a version"));
                }
                return Ok(Some(k + 2));
            }
            let end = self.name_end(k)?;
            let name = &s[k..end];
            if name.is_empty() {
                if k == s.len() {
                    return Err(self.fail(at, "an XML declaration isn't closed"));
                }
                let (c, _) = self.char_at(k)?;
                return Err(self.fail(k, format!("{} in the XML declaration", self.desc(c))));
            }
            if k == i {
                return Err(self.fail(k, format!("no space before {} in the XML declaration", shown(name))));
            }
            let Some(pos) = NAMES.iter().position(|n| *n == name) else {
                return Err(self.fail(k, format!("{} isn't part of an XML declaration", shown(name))));
            };
            if next == 0 && pos > 0 {
                return Err(self.fail(at, "an XML declaration without a version"));
            }
            if pos < next {
                let why = format!("{} out of place in the XML declaration (version, encoding, standalone)", shown(name));
                return Err(self.fail(k, why));
            }
            let eq = spaces(s, end);
            if s.get(eq) != Some(&b'=') {
                return Err(self.fail(k, format!("{} in the XML declaration has no value", shown(name))));
            }
            let q = spaces(s, eq + 1);
            let Some(&quote @ (b'"' | b'\'')) = s.get(q) else {
                return Err(self.fail(q, format!("the {} in the XML declaration isn't quoted", shown(name))));
            };
            let close = match s[q + 1..].iter().position(|&c| c == quote || c == b'<' || c == b'>') {
                Some(n) if s[q + 1 + n] == quote => q + 1 + n,
                _ => return Err(self.fail(q, format!("the {} in the XML declaration isn't closed", shown(name)))),
            };
            let value = &s[q + 1..close];
            match pos {
                0 if !(value.len() > 2 && value.starts_with(b"1.") && value[2..].iter().all(u8::is_ascii_digit)) => {
                    return Err(self.fail(q, format!("version=\"{}\" isn't an XML version", shown(value))));
                }
                1 => self.utf8 = s.starts_with(BOM) || utf8_label(value),
                2 if value != b"yes" && value != b"no" => {
                    return Err(self.fail(q, format!("standalone=\"{}\" isn't yes or no", shown(value))));
                }
                _ => {}
            }
            next = pos + 1;
            i = close + 1;
        }
    }

    // A doctype at `at` ("<!DOCTYPE"): where it ends. One with an internal subset, which svg_root has taken for no SVG,
    // isn't read. A public ID of XHTML's or MathML's (XHTML_PUBLIC_IDS) gives the document HTML's entities.
    fn doctype(&mut self, at: usize) -> Result<usize, Stop> {
        let s = self.s;
        let from = spaces(s, at + 9);
        let end = self.name_end(from)?;
        if end == from {
            return Err(self.no_name(from, "a doctype without a name"));
        }
        let mut i = spaces(s, end);
        let public = s[i..].starts_with(b"PUBLIC");
        let id = public || s[i..].starts_with(b"SYSTEM");
        if id {
            i = spaces(s, i + 6);
            if public {
                let close = self.literal(i, true)?;
                self.html_entities = XHTML_PUBLIC_IDS.contains(&&s[i + 1..close - 1]);
                i = spaces(s, close);
            }
            i = spaces(s, self.literal(i, false)?);
        }
        match s.get(i) {
            Some(b'>') => Ok(i + 1),
            Some(b'[') => Err(Stop::LetBe),
            None => Err(self.fail(at, "a doctype isn't closed")),
            Some(_) => {
                let word = self.name_end(i)?;
                let what = if word > i { shown(&s[i..word]) } else { self.desc(self.char_at(i)?.0) };
                let expected = if id { "'>'" } else { "SYSTEM, PUBLIC or '>'" };
                Err(self.fail(i, format!("{what} in the doctype, where {expected} should be")))
            }
        }
    }

    // The doctype's public ID (`public`) or system ID, quoted, at `at`: where it ends. A public ID holds only the
    // characters XML allows it; a system ID any, as Chromium lets it hold a control and Firefox a '<' or '&'.
    fn literal(&self, at: usize, public: bool) -> Result<usize, Stop> {
        let s = self.s;
        let what = if public { "public ID" } else { "system ID" };
        let Some(&quote @ (b'"' | b'\'')) = s.get(at) else {
            return Err(self.fail(at, format!("the doctype has no quoted {what}")));
        };
        let mut i = at + 1;
        loop {
            match s.get(i) {
                None => return Err(self.fail(at, format!("the doctype's {what} isn't closed"))),
                Some(&c) if c == quote => return Ok(i + 1),
                Some(&c) if public && !is_pubid_char(c) => {
                    let (c, _) = self.char_at(i)?;
                    return Err(self.fail(i, format!("{} in the doctype's public ID", self.desc(c))));
                }
                Some(_) => i += self.char_at(i)?.1,
            }
        }
    }

    // A name of an element or an attribute: a local name, or a prefix and a local name apart by one ':'.
    fn qname(&self, at: usize, name: &[u8]) -> Result<(), Stop> {
        let why = if name.starts_with(b":") {
            "starts with ':'"
        } else if name.ends_with(b":") {
            "ends with ':'"
        } else if name.iter().filter(|&&c| c == b':').count() > 1 {
            "has more than one ':'"
        } else {
            return Ok(());
        };
        Err(self.fail(at, format!("the name {} {why}", shown(name))))
    }

    // Where the name at `at` ends: `at` when there is none.
    fn name_end(&self, at: usize) -> Result<usize, Stop> {
        let mut i = at;
        while i < self.s.len() {
            let (c, n) = self.char_at(i)?;
            if !(if i == at { is_name_start(c) } else { is_name_char(c) }) {
                break;
            }
            i += n;
        }
        Ok(i)
    }

    // Why there is no name at `i`: no name can start with the character there, or else `missing`.
    fn no_name(&self, i: usize, missing: &str) -> Stop {
        match self.s.get(i).map(|_| self.char_at(i)) {
            Some(Ok((c, _))) if is_name_char(c) => self.fail(i, format!("a name can't start with {}", self.desc(c))),
            Some(Err(e)) => e,
            _ => self.fail(i, missing),
        }
    }

    // Past the character at `i`, which must be one XML allows.
    fn char(&self, i: usize) -> Result<usize, Stop> {
        let (c, n) = self.char_at(i)?;
        if !is_xml_char(c) {
            return Err(self.fail(i, format!("{} isn't allowed in XML", self.desc(c))));
        }
        Ok(i + n)
    }

    // The character at `i` (before the end) and its length. In a legacy encoding, a byte that isn't ASCII is LEGACY,
    // and so is it with the byte after it when that could be the second of a two-byte character (Shift_JIS, Big5, GBK
    // and EUC-KR have them from 0x40 to 0x7E), which could otherwise be read as a ']' or a character no name can hold.
    fn char_at(&self, i: usize) -> Result<(u32, usize), Stop> {
        let s = self.s;
        if s[i] < 0x80 {
            return Ok((s[i] as u32, 1));
        }
        if !self.utf8 {
            return Ok((LEGACY, 1 + usize::from(s.get(i + 1).is_some_and(|c| (0x40..=0x7E).contains(c)))));
        }
        let start = &s[i..(i + 4).min(s.len())];
        let valid = match std::str::from_utf8(start) {
            Ok(text) => text,
            Err(e) => std::str::from_utf8(&start[..e.valid_up_to()]).unwrap_or_default(),
        };
        match valid.chars().next() {
            Some(c) => Ok((c as u32, c.len_utf8())),
            None => Err(self.fail(i, "bytes that aren't UTF-8")),
        }
    }

    // A character as a reason names it: 'x', or U+0001 for one that doesn't show.
    fn desc(&self, c: u32) -> String {
        match char::from_u32(c) {
            _ if c == LEGACY && !self.utf8 => "a character that isn't ASCII".to_string(),
            Some(c) if c.is_ascii_graphic() => format!("'{c}'"),
            _ => format!("U+{c:04X}"),
        }
    }

    // The error `reason` at `at`, with its line: lines end at "\n", "\r\n" or a "\r" alone, as XML ends them.
    fn fail(&self, at: usize, reason: impl Into<String>) -> Stop {
        let s = self.s;
        let ends = |&(k, &c): &(usize, &u8)| c == b'\n' || (c == b'\r' && s.get(k + 1) != Some(&b'\n'));
        let line = 1 + s[..at.min(s.len())].iter().enumerate().filter(ends).count();
        Stop::Malformed(Malformed { reason: reason.into(), line })
    }
}

// A name's prefix, if it has one, and its local name.
fn split_name(name: &[u8]) -> (Option<&[u8]>, &[u8]) {
    match name.iter().position(|&c| c == b':') {
        Some(colon) => (Some(&name[..colon]), &name[colon + 1..]),
        None => (None, name),
    }
}

// An attribute as a reason names it: width on <rect>.
fn on(attr: &[u8], elem: &[u8]) -> String {
    format!("{} on <{}>", shown(attr), shown(elem))
}

// A name or a value as a reason shows it: at most 40 characters of it.
fn shown(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    match text.char_indices().nth(40) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.into_owned(),
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

// Past the spaces (XML's: space, tab, line ends) from `i` on.
fn spaces(s: &[u8], i: usize) -> usize {
    i + s[i..].iter().take_while(|&&c| is_space(c)).count()
}

// A character XML 1.0 allows.
fn is_xml_char(c: u32) -> bool {
    matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x1_0000..=0x10_FFFF)
}

// A character a name may start with, and one it may hold, as XML 1.0's fifth edition has them.
fn is_name_start(c: u32) -> bool {
    matches!(c,
        0x3A | 0x41..=0x5A | 0x5F | 0x61..=0x7A | 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D
        | 0x37F..=0x1FFF | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF | 0xF900..=0xFDCF
        | 0xFDF0..=0xFFFD | 0x1_0000..=0xE_FFFF)
}

fn is_name_char(c: u32) -> bool {
    is_name_start(c) || matches!(c, 0x2D | 0x2E | 0x30..=0x39 | 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
}

// A character a doctype's public ID may hold.
fn is_pubid_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b" \r\n-'()+,./:=?;!*#@$_%".contains(&c)
}

// An image's width and height: in pixels as stored (`pixels`, though turned as it is drawn), and as it is drawn.
struct Size {
    pixels: (u32, u32),
    drawn: (u32, u32),
}

// The size of the whole file `data` of type `mime`, or None when its header can't be read. Where a format's parts can
// each be larger than the size it states (a GIF's frames, an ICO's icons), the largest of them. A JPEG or a PNG is
// drawn turned as its EXIF orientation says (see Exif::turns), and a JPEG at the size its EXIF resolution gives (see
// Exif::density_size).
fn size(mime: &str, data: &[u8]) -> Option<Size> {
    let as_stored = |size| Size { pixels: size, drawn: size };
    match mime {
        "image/png" => png(data).map(|size| exif_size(size, png_exif(data), false)),
        "image/jpeg" => jpeg(data),
        "image/gif" => gif(data).map(as_stored),
        "image/webp" => webp(data).map(as_stored),
        "image/x-icon" => ico(data).map(as_stored),
        _ => None,
    }
}

// The size of an image stored at `stored` whose EXIF is `exif` (the TIFF structure an EXIF block holds): turned as its
// orientation says, and with `density` (a JPEG), drawn at the size its resolution gives, before it is turned.
fn exif_size(stored: (u32, u32), exif: Option<&[u8]>, density: bool) -> Size {
    let exif = exif.map(Exif::read).unwrap_or_default();
    let scaled = if density { exif.density_size(stored) } else { None };
    let turned = |(w, h)| if exif.turns() { (h, w) } else { (w, h) };
    Size { pixels: turned(stored), drawn: turned(scaled.unwrap_or(stored)) }
}

fn bytes<const N: usize>(d: &[u8], at: usize) -> Option<[u8; N]> {
    d.get(at..at.checked_add(N)?)?.try_into().ok()
}
fn be16(d: &[u8], at: usize) -> Option<u32> {
    bytes(d, at).map(|b| u16::from_be_bytes(b) as u32)
}
fn be32(d: &[u8], at: usize) -> Option<u32> {
    bytes(d, at).map(u32::from_be_bytes)
}
fn le16(d: &[u8], at: usize) -> Option<u32> {
    bytes(d, at).map(|b| u16::from_le_bytes(b) as u32)
}
fn le24(d: &[u8], at: usize) -> Option<u32> {
    bytes::<3>(d, at).map(|[a, b, c]| u32::from_le_bytes([a, b, c, 0]))
}
fn le32(d: &[u8], at: usize) -> Option<u32> {
    bytes(d, at).map(u32::from_le_bytes)
}

fn nonzero(w: u32, h: u32) -> Option<(u32, u32)> {
    (w > 0 && h > 0).then_some((w, h))
}

// The first chunk is IHDR: its length and type, then the width and height, as stored. An APNG's frames lie within that
// size.
fn png(d: &[u8]) -> Option<(u32, u32)> {
    if d.get(12..16)? != b"IHDR" {
        return None;
    }
    nonzero(be32(d, 16)?, be32(d, 20)?)
}

// The data of the first eXIf chunk, a TIFF header and its directories, among the chunks after IHDR. Browsers read it
// only before the image data (IDAT).
fn png_exif(d: &[u8]) -> Option<&[u8]> {
    let mut i = 8;
    loop {
        // Each chunk: the length of its data, its type, the data, then a CRC.
        let len = be32(d, i)? as usize;
        let data = i.checked_add(8)?;
        match d.get(i + 4..data)? {
            b"eXIf" => return d.get(data..data.checked_add(len)?),
            b"IDAT" | b"IEND" => return None,
            _ => i = data.checked_add(len)?.checked_add(4)?,
        }
    }
}

// A TIFF structure, as an EXIF block holds one: the byte order (II little endian, MM big), 42, and the offset of the
// first directory (IFD0) from the start. A directory is a count of 12-byte entries, each a tag, a type, a count and 4
// bytes that hold a value that fits in them, or else the offset of one that doesn't.
struct Tiff<'a> {
    d: &'a [u8],
    big: bool,
}

// A directory's entry: its tag, type and count, and the offset of its 4 bytes of value.
struct Entry {
    tag: u32,
    kind: u32,
    count: u32,
    value: usize,
}

impl<'a> Tiff<'a> {
    // The structure, and the offset of its first directory.
    fn read(d: &'a [u8]) -> Option<(Tiff<'a>, usize)> {
        let big = match d.get(..2)? {
            b"II" => false,
            b"MM" => true,
            _ => return None,
        };
        let tiff = Tiff { d, big };
        if tiff.u16(2)? != 42 {
            return None;
        }
        let ifd = tiff.u32(4)? as usize;
        Some((tiff, ifd))
    }
    fn u16(&self, at: usize) -> Option<u32> {
        if self.big { be16(self.d, at) } else { le16(self.d, at) }
    }
    fn u32(&self, at: usize) -> Option<u32> {
        if self.big { be32(self.d, at) } else { le32(self.d, at) }
    }
    // The RATIONAL whose offset a value's 4 bytes hold, in single precision as Chromium reads it, with a denominator of
    // 0 read as 0, as it reads one; None when it lies past the end.
    fn rational(&self, value: usize) -> Option<f32> {
        let at = self.u32(value)? as usize;
        let (n, d) = (self.u32(at)?, self.u32(at.checked_add(4)?)?);
        Some(if d == 0 { 0.0 } else { n as f32 / d as f32 })
    }
    // The entries of the directory at `ifd`: as many as its count says, but only those whose 12 bytes all lie in the
    // structure, as Chromium reads them, so a directory cut short ends at its last whole entry.
    fn entries(&self, ifd: usize) -> Option<impl Iterator<Item = Entry> + '_> {
        let count = self.u16(ifd)? as usize;
        // The count could be read, so the entries start within the structure.
        let whole = (self.d.len() - ifd - 2) / 12;
        Some((0..count.min(whole)).filter_map(move |n| {
            let at = ifd + 2 + 12 * n;
            Some(Entry { tag: self.u16(at)?, kind: self.u16(at + 2)?, count: self.u32(at + 4)?, value: at + 8 })
        }))
    }
}

// What an image's EXIF says of the way it is drawn, read as Chromium reads it: Orientation (tag 0x112, a SHORT from 1
// to 8), ResolutionUnit (0x128, a SHORT), XResolution and YResolution (0x11A and 0x11B, each a RATIONAL: the offset of
// two LONGs, a numerator and a denominator) and PixelXDimension and PixelYDimension (0xA002 and 0xA003, a SHORT or a
// LONG). Each is one value, and is taken from the first entry for it that has a type and a value it takes and can be
// read, so an orientation of 9, or one written as a LONG, is passed over for one after it (Firefox stops reading
// there), while a resolution of 0 is read, and so leaves the image as stored. They are read in the first directory and
// in the Exif directory, which it points to (tag 0x8769, one LONG offset) and which is read at that entry, so that its
// tags come before those of the first directory's entries after it.
#[derive(Default)]
struct Exif {
    orientation: Option<u32>,
    unit: Option<u32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<u32>,
    height: Option<u32>,
}

// Chromium gives no side of an image's natural size more than 2^25 - 1 pixels (33,554,431, the most whole pixels its
// layout units hold), so a density-corrected size larger than that is drawn at that on that side.
const MAX_SIDE: u32 = (1 << 25) - 1;

impl Exif {
    // `tiff` is the TIFF structure an EXIF block holds. One that can't be read says nothing, so the image is drawn as
    // stored, never refused.
    fn read(tiff: &[u8]) -> Exif {
        let mut exif = Exif::default();
        if let Some((tiff, ifd)) = Tiff::read(tiff) {
            exif.directory(&tiff, ifd, true);
        }
        exif
    }

    // The tags in the directory at `ifd`, and in the first one's case, in the Exif directory it points to. Chromium
    // follows only the first such pointer, even one to nothing it can read, and none in the Exif directory, so at most
    // two directories of at most 65,535 entries each are read. Following each pointer would make the work quadratic
    // in the block's length, since a pointer may name the directory it is in: a PNG's eXIf can hold 65,534 of them.
    fn directory(&mut self, tiff: &Tiff, ifd: usize, first: bool) {
        let Some(entries) = tiff.entries(ifd) else { return };
        let mut exif_dir = first;
        for Entry { tag, kind, count, value } in entries {
            if count != 1 {
                continue;
            }
            match (tag, kind) {
                (0x112, 3) => {
                    self.orientation = self.orientation.or_else(|| tiff.u16(value).filter(|o| (1..=8).contains(o)))
                }
                (0x128, 3) => self.unit = self.unit.or_else(|| tiff.u16(value)),
                (0x11A, 5) => self.x = self.x.or_else(|| tiff.rational(value)),
                (0x11B, 5) => self.y = self.y.or_else(|| tiff.rational(value)),
                (0xA002, 3) => self.width = self.width.or_else(|| tiff.u16(value)),
                (0xA002, 4) => self.width = self.width.or_else(|| tiff.u32(value)),
                (0xA003, 3) => self.height = self.height.or_else(|| tiff.u16(value)),
                (0xA003, 4) => self.height = self.height.or_else(|| tiff.u32(value)),
                (0x8769, 4) if exif_dir => {
                    exif_dir = false;
                    if let Some(at) = tiff.u32(value) {
                        self.directory(tiff, at as usize, false);
                    }
                }
                _ => {}
            }
        }
    }

    // Browsers draw a JPEG or a PNG turned and flipped as its EXIF orientation says (CSS's image-orientation:
    // from-image), though not a WebP. Orientations 5 to 8 turn it a quarter turn, so it is drawn with its width and
    // height swapped; 1 to 4 leave them as stored.
    fn turns(&self) -> bool {
        self.orientation.is_some_and(|o| (5..=8).contains(&o))
    }

    // The size browsers draw a JPEG stored at `(w, h)` at for the resolution its EXIF gives (its density-corrected
    // size), before it is turned, or None when its tags give none. In inches (ResolutionUnit 2), at a resolution other
    // than 72 dots per inch, it is drawn at 72 / that of its size, each side rounded, but only when PixelXDimension and
    // PixelYDimension are that size, so that a resolution alone, which cameras and editors write for print, changes
    // nothing. A PNG's (eXIf) is left alone, as Chromium leaves it; Firefox applies it too. Nor do browsers apply a
    // PNG's pHYs or a JPEG's JFIF density. Tags that can't be read, a resolution of 0, or tags that give another size
    // leave it as stored.
    fn density_size(&self, (w, h): (u32, u32)) -> Option<(u32, u32)> {
        let (x, y, width, height) = (self.x?, self.y?, self.width?, self.height?);
        // Worked out as Chromium does: in single precision, so that the sizes that round to it are the ones it takes,
        // and in ints, so that a size past the largest one is the largest, and pixel dimensions past it are none.
        let int = i32::MAX as u64;
        let at = |n: u32, dpi: f32| (((n as u64 * 72) as f32 / dpi).round() as u64).min(int);
        let fits = |n: u32| (1..=int).contains(&(n as u64));
        let ok = self.unit == Some(2) && x > 0.0 && y > 0.0 && fits(width) && fits(height);
        let ok = ok && at(w, x) == width as u64 && at(h, y) == height as u64;
        ok.then_some((width.min(MAX_SIDE), height.min(MAX_SIDE)))
    }
}

// Segments up to the frame header (SOF), which holds the height and the width. Each segment before it says how long it
// is, so the metadata (EXIF, ICC profiles, an embedded thumbnail with a frame header of its own) is skipped whole,
// except that the first EXIF segment (APP1, whose data is "Exif\0\0" and a TIFF structure) gives the orientation and
// the resolution (see exif_size). A height of 0 is given later in the scan (DNL), which isn't read, so that image is
// refused.
fn jpeg(d: &[u8]) -> Option<Size> {
    let mut i = 2;
    let mut exif = None;
    loop {
        if *d.get(i)? != 0xFF {
            return None;
        }
        // A marker may be padded with any number of 0xFF bytes.
        while *d.get(i)? == 0xFF {
            i += 1;
        }
        let marker = *d.get(i)?;
        i += 1;
        match marker {
            0x01 | 0xD0..=0xD8 => {} // no length
            0xD9 | 0xDA => return None, // the end, or the scan, before any frame header
            // SOF0 to SOF15, but for DHT, JPG and DAC: length, precision, height, width.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                return nonzero(be16(d, i + 5)?, be16(d, i + 3)?).map(|size| exif_size(size, exif, true));
            }
            _ => {
                let len = be16(d, i)? as usize;
                if len < 2 {
                    return None;
                }
                if marker == 0xE1 && exif.is_none() {
                    exif = d.get(i + 2..i + len).and_then(|s| s.strip_prefix(b"Exif\0\0"));
                }
                i += len;
            }
        }
    }
}

// The screen size, and every frame's own: a frame may reach past the screen, and browsers make room for it. A file
// cut short ends where it is cut, as it does for a browser, but an unknown block is refused.
fn gif(d: &[u8]) -> Option<(u32, u32)> {
    let (mut w, mut h) = (le16(d, 6)?, le16(d, 8)?);
    let color_table = |flags: u8| if flags & 0x80 != 0 { 3 << ((flags & 7) + 1) } else { 0 };
    let mut i = 13 + color_table(*d.get(10)?);
    loop {
        match d.get(i) {
            None | Some(0x3B) => break, // the end
            Some(0x21) => match sub_blocks(d, i + 2) {
                Some(next) => i = next,
                None => break,
            },
            Some(0x2C) => {
                let (Some(left), Some(top), Some(fw), Some(fh), Some(&flags)) =
                    (le16(d, i + 1), le16(d, i + 3), le16(d, i + 5), le16(d, i + 7), d.get(i + 9))
                else {
                    break;
                };
                w = w.max(left + fw);
                h = h.max(top + fh);
                // The local color table, the LZW code size, then the data.
                match sub_blocks(d, i + 10 + color_table(flags) + 1) {
                    Some(next) => i = next,
                    None => break,
                }
            }
            Some(_) => return None,
        }
    }
    nonzero(w, h)
}

// Past a chain of data sub-blocks (a length byte, then that many bytes), which ends with an empty one.
fn sub_blocks(d: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let n = *d.get(i)? as usize;
        i += 1;
        if n == 0 {
            return Some(i);
        }
        i += n;
    }
}

// The first chunk: a lossy key frame (VP8), a lossless image (VP8L) or, for the extended format (VP8X: animation,
// alpha, metadata), the canvas, which every frame lies within.
fn webp(d: &[u8]) -> Option<(u32, u32)> {
    match d.get(12..16)? {
        // The frame tag, the start code, then 14 bits each of width and height (the top two bits are a scale).
        b"VP8 " => {
            if d.get(23..26)? != [0x9D, 0x01, 0x2A] {
                return None;
            }
            nonzero(le16(d, 26)? & 0x3FFF, le16(d, 28)? & 0x3FFF)
        }
        // A signature byte, then 14 bits each of width - 1 and height - 1.
        b"VP8L" => {
            if *d.get(20)? != 0x2F {
                return None;
            }
            let bits = le32(d, 21)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        // Flags and 3 reserved bytes, then 24 bits each of width - 1 and height - 1.
        b"VP8X" => Some((le24(d, 24)? + 1, le24(d, 27)? + 1)),
        _ => None,
    }
}

// A directory of icons, each a PNG or a bitmap without its file header. Browsers pick one to draw, so each must fit,
// and each is as large as the larger of what the directory says and what its own header says (a bitmap's height
// counts its mask too, so it is halved). The size given is the largest icon's.
fn ico(d: &[u8]) -> Option<(u32, u32)> {
    let count = le16(d, 4)? as usize;
    let mut largest: Option<(u32, u32)> = None;
    for n in 0..count {
        let entry = d.get(6 + 16 * n..6 + 16 * (n + 1))?;
        let listed = |b: u8| if b == 0 { 256 } else { b as u32 };
        let icon = d.get(le32(entry, 12)? as usize..)?;
        let (w, h) = if icon.starts_with(PNG_SIGNATURE) {
            png(icon)?
        } else {
            let (w, h) = (le32(icon, 4)? as i32, le32(icon, 8)? as i32);
            (w.unsigned_abs(), h.unsigned_abs() / 2)
        };
        let (w, h) = nonzero(w.max(listed(entry[0])), h.max(listed(entry[1])))?;
        if largest.is_none_or(|(lw, lh)| w as u64 * h as u64 > lw as u64 * lh as u64) {
            largest = Some((w, h));
        }
    }
    largest
}
