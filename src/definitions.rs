// Go to definition. git grep finds every line that looks like a definition of the clicked name; then the click
// site (receiver, the file's imports, the enclosing class/impl/function) scores those candidates so the right one
// can be opened directly. No parsing beyond regexes and brace/indent tracking, so it works at any revision and in
// any repo, and falls back to a picker when the evidence is a tie.
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use fancy_regex::{Captures, Regex};
use gix::ObjectId;
use serde_json::{json, Value};

use crate::git::{git_allow, http_error, Result, MAX_FILE_BYTES};

const MAX_ANALYZED_FILES: usize = 40;
const CONFIDENT_MARGIN: i64 = 25; // the best candidate must beat the next by this much to be opened directly

// ---------------------------------------------------------------- regex helpers
// The patterns are written as in JavaScript, where \w and \d are ASCII; Rust's are Unicode, so spell them out.
// (\b stays Unicode: fancy-regex can't switch Unicode off.) A `[` inside a class is literal in JS but nests in Rust.
fn js_re(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let mut in_class = false;
    let mut it = src.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('w') => out.push_str(if in_class { "A-Za-z0-9_" } else { "[A-Za-z0-9_]" }),
                Some('d') => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push_str("\\\\"),
            },
            '[' if !in_class => {
                in_class = true;
                out.push('[');
                if it.peek() == Some(&'^') {
                    out.push(it.next().unwrap());
                }
            }
            '[' | '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

fn compile(src: &str) -> Regex {
    Regex::new(&js_re(src)).unwrap_or_else(|e| panic!("bad pattern {src}: {e}"))
}

macro_rules! re {
    ($s:expr) => {{
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| compile($s))
    }};
}

fn test(r: &Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

fn exec<'t>(r: &Regex, s: &'t str) -> Option<Captures<'t>> {
    r.captures(s).ok().flatten()
}

// Every match, stopping at the first error: fancy-regex reports hitting its backtrack limit as an error without moving
// past that spot, so skipping errors would retry it forever.
fn all<'t>(r: &Regex, s: &'t str) -> Vec<Captures<'t>> {
    r.captures_iter(s).map_while(|c| c.ok()).collect()
}

fn group<'t>(c: &Captures<'t>, i: usize) -> Option<&'t str> {
    c.get(i).map(|m| m.as_str())
}

// JS String.replace with a non-global regex: only the first match.
fn replace_first(r: &Regex, s: &str, with: &str) -> String {
    match r.find(s).ok().flatten() {
        Some(m) => format!("{}{}{}", &s[..m.start()], with, &s[m.end()..]),
        None => s.to_string(),
    }
}

// Every match replaced by `with` (taken literally). Written out rather than Regex::replace_all, which panics when a
// pattern with lookaround hits its backtrack limit; here the rest of the text is just left as it is.
fn replace_all(r: &Regex, s: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in r.find_iter(s).map_while(|m| m.ok()) {
        out.push_str(&s[last..m.start()]);
        out.push_str(with);
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

// Leading whitespace length, counted like JS `l.length - l.trimStart().length`.
fn indent_of(l: &str) -> usize {
    l.chars().count() - l.trim_start().chars().count()
}

// ---------------------------------------------------------------- posix paths (lexical, always `/`-separated)
mod pp {
    pub fn normalize(p: &str) -> String {
        if p.is_empty() {
            return ".".into();
        }
        let abs = p.starts_with('/');
        let trailing = p.ends_with('/');
        let mut out: Vec<&str> = Vec::new();
        for seg in p.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !abs {
                    out.push("..");
                }
                continue;
            }
            out.push(seg);
        }
        let mut s = out.join("/");
        if s.is_empty() && !abs {
            s = ".".into();
        }
        if !s.is_empty() && trailing {
            s.push('/');
        }
        if abs {
            format!("/{s}")
        } else {
            s
        }
    }

    pub fn join(parts: &[&str]) -> String {
        let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return ".".into();
        }
        normalize(&parts.join("/"))
    }

    pub fn dirname(p: &str) -> String {
        if p.is_empty() {
            return ".".into();
        }
        let b = p.as_bytes();
        let has_root = b[0] == b'/';
        let mut end = None;
        let mut matched_slash = true;
        for i in (1..b.len()).rev() {
            if b[i] == b'/' {
                if !matched_slash {
                    end = Some(i);
                    break;
                }
            } else {
                matched_slash = false;
            }
        }
        match end {
            None => (if has_root { "/" } else { "." }).into(),
            Some(1) if has_root => "//".into(),
            Some(e) => p[..e].to_string(),
        }
    }

    pub fn basename<'a>(p: &'a str, ext: &str) -> &'a str {
        let trimmed = p.trim_end_matches('/');
        let base = trimmed.rsplit('/').next().unwrap_or("");
        if !ext.is_empty() && base != ext && base.ends_with(ext) {
            &base[..base.len() - ext.len()]
        } else {
            base
        }
    }
}

// A repo-relative path, or None when it climbs out of the repo.
fn norm(p: &str) -> Option<String> {
    let n = pp::normalize(p);
    if n.starts_with("../") || n == ".." {
        return None;
    }
    Some(n.strip_prefix("./").map(String::from).unwrap_or(n))
}

// ---------------------------------------------------------------- definition patterns
fn lang_of(p: &str) -> &'static str {
    let ext = match p.rfind('.') {
        Some(i) => p[i + 1..].to_lowercase(),
        None => String::new(),
    };
    match ext.as_str() {
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" | "vue" | "svelte" => "js",
        "py" | "pyi" => "py",
        "go" => "go",
        "rs" => "rs",
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ino" | "m" | "mm" | "java" | "cs" => "c",
        "sh" | "bash" | "zsh" => "sh",
        _ => "any",
    }
}

// Each entry is (kind, regex source); NAME (the escape for a private-use character, so it can't clash with anything
// a pattern means literally) stands for the escaped name as a whole word.
const NAME: &str = r"\u{E000}";

fn def_rules(lang: &str) -> &'static [(&'static str, &'static str)] {
    match lang {
        "js" => &[
            ("function", r"\bfunction\s*\*?\s*\u{E000}\s*[<(]"),
            ("class", r"\b(?:class|interface|enum|namespace)\s+\u{E000}"),
            ("type", r"\btype\s+\u{E000}\s*(?:<[^=]*>)?\s*="),
            ("function", r"\b(?:const|let|var)\s+\u{E000}\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|(?:\([^)]*\)|[\w$]+)\s*(?::[^=]+)?=>)"),
            ("variable", r"\b(?:const|let|var)\s+\u{E000}\s*(?::[^=]+)?="),
            // Methods: the line starts with the name and ends by opening the body. Skips calls that pass a callback,
            // like foo(() => {, describe('x', function () { and it('y', async function () {. The return type may be an
            // object type, `(): { a: number } {`.
            (
                "method",
                r#"^\s*(?:(?:export|default|public|private|protected|static|async|override|abstract|get|set)\s+)*\*?\s*\u{E000}\s*(?:<[^>]*>)?\s*\((?!\s*['"`])(?![^)]*=>)(?![^)]*\bfunction\b)[^;]*\)\s*(?::\s*(?:[^=;{]|\{[^{}]*\})+)?\{\s*(?:\/\/.*)?$"#,
            ),
            // A method whose parameters go on over the next lines, `name(` or `name(a: string,`: kept only if they
            // close into a body (definitions() checks, see opens_method), since a call split over lines starts the same.
            ("methodhead", r"^\s*(?:(?:export|default|public|private|protected|static|async|override|abstract|get|set)\s+)*\*?\s*\u{E000}\s*(?:<[^>]*>)?\s*\([^)]*$"),
            ("method", r"^\s*(?:(?:public|private|protected|static|readonly)\s+)*\u{E000}\s*[:=]\s*(?:async\s+)?(?:function\b|(?:\([^)]*\)|[\w$]+)\s*=>)"),
            ("variable", r"^\s*(?:(?:public|private|protected|static|readonly|declare)\s+)+\u{E000}\s*[?!]?\s*[:=]"),
            ("export", r"\b(?:module\.)?exports\.\u{E000}\s*="),
            // A TypeScript method signature, `name(args): T;`: kept only inside an interface or an abstract or declared
            // class (definitions() checks), since a call statement looks the same.
            ("signature", r"^\s*(?:(?:public|private|protected|static|abstract|readonly)\s+)*\u{E000}\s*\??\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::\s*[^;{=]+)?;\s*(?:\/\/.*)?$"),
        ],
        "py" => &[
            ("function", r"^\s*(?:async\s+)?def\s+\u{E000}\s*[(\[]"),
            ("class", r"^\s*class\s+\u{E000}\b"),
            ("variable", r"^\u{E000}\s*(?::[^=]+)?=(?!=)"),
            // Tuple assignment, `a, name, b = …`. Each name before `=` is followed by a comma or the `=`, so the lists
            // can't be split more than one way.
            ("variable", r"^\u{E000}\s*,\s*(?:[A-Za-z_]\w*\s*,\s*)*(?:[A-Za-z_]\w*\s*)?=(?!=)"),
            ("variable", r"^(?:[A-Za-z_]\w*\s*,\s*)+\u{E000}\s*(?:,\s*[A-Za-z_]\w*\s*)*,?\s*=(?!=)"),
            // Indented `name = …` / `name: T = …` / `name: T`: kept only if it turns out to sit directly in a class
            // body (checked in definitions(), which then makes it a variable owned by the class).
            ("attribute", r"^\s+\u{E000}\s*(?::[^=]+)?=(?!=)"),
            ("attribute", r"^\s+\u{E000}\s*:[^=]+$"),
        ],
        "go" => &[
            ("function", r"^func\s+(?:\([^)]*\)\s*)?\u{E000}\s*[(\[]"),
            ("type", r"^\s*type\s+\u{E000}\b"),
            ("type", r"^\s+\u{E000}\s+(?:struct|interface)\b"),
            ("variable", r"^(?:const|var)\s+\u{E000}\b"),
            // An item of a grouped `const (` / `var (` block: `Name`, `Name Type`, `Name = …`. Kept only when it really
            // sits in such a group (definitions() checks), since struct fields and assignments look the same.
            ("groupitem", r"^\s+\u{E000}(?:\s+[\w.*\[\]]+)?(?:\s*=.*)?$"),
        ],
        "rs" => &[
            ("function", r"\bfn\s+\u{E000}\s*[<(]"),
            ("type", r"\b(?:struct|enum|trait|type|union|mod)\s+\u{E000}\b"),
            ("macro", r"\bmacro_rules!\s*\u{E000}\b"),
            ("variable", r"\b(?:const|static)\s+(?:mut\s+)?\u{E000}\s*:"),
            // An enum variant (`Name,`, `Name(…)`, `Name { … }`) and a struct field (`pub name: T`). Kept only directly
            // in an enum or struct body (definitions() checks), since match arms, arguments and literals look the same.
            ("enumitem", r"^\s+\u{E000}\s*(?:[({,]|$)"),
            ("structitem", r"^\s+(?:pub(?:\([^)]*\))?\s+)?\u{E000}\s*:(?!:)"),
        ],
        "c" => &[
            ("class", r"\b(?:class|struct|interface|enum|union|record|namespace)\s+\u{E000}\b(?!\s*;)"),
            ("macro", r"^\s*#\s*define\s+\u{E000}\b"),
            ("type", r"\btypedef\b.*\b\u{E000}\s*;"),
            // Objective-C: typedef NS_ENUM(NSInteger, Name) {
            ("type", r"\btypedef\s+NS_(?:ENUM|OPTIONS)\s*\([^,)]*,\s*\u{E000}\s*\)"),
            ("function", r"^\s*[\w:]+::\u{E000}\s*\("),
            // Return type then name( with no ; afterwards (a declaration or call would have one). The leading
            // whitespace is atomic: the type class below matches whitespace too, and trying every split of a long
            // indent between the two made each long line cost milliseconds.
            ("function", r"^(?>\s*)(?!(?:return|else|new|throw|case|await|delete|sizeof|yield|goto)\b)[\w*&:<>,~\[\]\s]*[\w*&>\]]\s+[*&]*\s*(?:[\w:]+::)?\u{E000}\s*\([^;]*$"),
        ],
        "sh" => &[("function", r"^\s*(?:function\s+\u{E000}\b|\u{E000}\s*\(\s*\))")],
        _ => &[
            ("function", r"\b(?:function|def|defn|defun|fn|func|fun|sub|proc|macro)\s+[*&]?\u{E000}"),
            ("class", r"\b(?:class|struct|interface|trait|type|module|enum)\s+\u{E000}"),
            ("function", r"^\s*\u{E000}\s*\(\s*\)\s*\{"),
        ],
    }
}

// Kinds a rule can only suggest: definitions() keeps them once the block structure confirms them (renaming them to the
// kind shown) and drops the rest. Python class attributes, Go grouped const/var items, Rust enum variants and struct
// fields, TypeScript method signatures and methods whose parameters span lines.
fn provisional(kind: &str) -> bool {
    matches!(kind, "attribute" | "groupitem" | "enumitem" | "structitem" | "signature" | "methodhead")
}

// How many lines a parameter list may span before a methodhead is given up on.
const MAX_PARAM_LINES: usize = 50;

// Does the parameter list opened on line `at` (0-based) close into a method body, `) {` or `): T {`? A call split over
// lines (callbacks and all) closes into `)`, `);` or `),` instead. The rule leaves only modifiers before the name, so
// the line's first `(` opens the list. Strings and comments aren't parsed, so a stray parenthesis in one only loses
// the method.
fn opens_method(lines: &[&str], at: usize) -> bool {
    let Some(open) = lines.get(at).and_then(|l| l.find('(')) else { return false };
    let (mut parens, mut braces) = (0, 0);
    for (k, l) in lines.iter().enumerate().skip(at).take(MAX_PARAM_LINES) {
        let l = if k == at { &l[open..] } else { l };
        for (i, c) in l.char_indices() {
            match c {
                '(' => parens += 1,
                ')' => {
                    parens -= 1;
                    if parens == 0 {
                        return test(re!(r"^\s*(?::\s*(?:[^=;{]|\{[^{}]*\})+)?\{\s*(?:\/\/.*)?$"), &l[i + 1..]);
                    }
                }
                '{' => braces += 1,
                '}' => braces -= 1,
                // Outside any braces (an object type has `;` inside them), the end of a statement: not a parameter list.
                ';' if braces == 0 => return false,
                _ => {}
            }
        }
    }
    false
}

fn noise_path() -> &'static Regex {
    re!(r"(^|\/)(vendor|third_party|node_modules|dist|build)\/")
}

// (kind, regex, whether the rule needs `name(`: such rules are skipped on lines without one, see has_call).
type Rules = Arc<Vec<(&'static str, Regex, bool)>>;

fn rules_for(lang: &'static str, name: &str) -> Rules {
    static CACHE: OnceLock<Mutex<HashMap<String, Rules>>> = OnceLock::new();
    let k = format!("{lang}\0{name}");
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(r) = cache.get(&k) {
        return r.clone();
    }
    if cache.len() > 500 {
        cache.clear();
    }
    let word = format!(r"(?<![\w$]){}(?![\w$])", name.replace('$', r"\$"));
    let rules: Vec<(&'static str, Regex, bool)> = def_rules(lang)
        .iter()
        .filter_map(|(kind, src)| {
            // Only a rule that names the name once, right before `(`: sh's has `function NAME` as an alternative.
            let call = src.matches(NAME).count() == 1 && src.contains(r"\u{E000}\s*\(");
            Regex::new(&js_re(&src.split(NAME).collect::<Vec<_>>().join(&word))).ok().map(|r| (*kind, r, call))
        })
        .collect();
    let rules = Arc::new(rules);
    cache.insert(k, rules.clone());
    rules
}

// Does `name` appear as a whole word followed by `(` (after optional whitespace)? A plain scan, so rules that need a
// call shape only run their regex on lines that could match.
fn has_call(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let mut from = 0;
    while let Some(i) = text[from..].find(name).map(|i| from + i) {
        let end = i + name.len();
        let mut j = end;
        while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
        }
        if (i == 0 || !word(b[i - 1])) && (end >= b.len() || !word(b[end])) && b.get(j) == Some(&b'(') {
            return true;
        }
        from = end;
    }
    false
}

// The kind of definition of `name` on this line, or None.
fn def_kind(p: &str, text: &str, name: &str) -> Option<&'static str> {
    // Every rule names the name, so a line without it can't match; this skips the regexes on nearly every line.
    if !text.contains(name) || text.encode_utf16().count() > 400 {
        return None;
    }
    let t = text.trim_start();
    // A line starting with `*` continues a block comment, unless a name follows: that's a JS generator method (*gen() {).
    let star_comment = t.starts_with('*') && !t[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$');
    if t.starts_with("//") || t.starts_with("/*") || star_comment || (t.starts_with('#') && !test(re!(r"^#\s*(define|\[)"), t)) {
        return None;
    }
    if test(re!(r"\.min\.[a-z]+$"), p) {
        return None;
    }
    let call = has_call(text, name);
    rules_for(lang_of(p), name).iter().find(|(_, r, needs_call)| (call || !needs_call) && test(r, text)).map(|(kind, _, _)| *kind)
}

// ---------------------------------------------------------------- the revision being viewed
type Text = Option<Rc<str>>;

fn text_of(buf: &[u8]) -> Text {
    if buf.len() as u64 > MAX_FILE_BYTES || buf[..buf.len().min(8000)].contains(&0) {
        return None;
    }
    Some(Rc::from(String::from_utf8_lossy(buf).as_ref()))
}

// File list and contents at `rev` (None = working tree), read once per request.
struct Repo<'a> {
    root: &'a Path,
    rev: Option<String>,
    // A plain folder: its listing is the file list (see Repo::folder), and whether that listing was cut short.
    folder: Option<bool>,
    list: Option<Vec<String>>,
    set: HashSet<String>,
    ids: HashMap<String, ObjectId>,
    cache: HashMap<String, Text>,
    go_mods: Option<Rc<Vec<GoMod>>>,
    crates: Option<Rc<Vec<Rc<Crate>>>>,
}

impl<'a> Repo<'a> {
    fn new(root: &'a Path, rev: Option<&str>) -> Self {
        Repo {
            root,
            rev: rev.map(String::from),
            folder: None,
            list: None,
            set: HashSet::new(),
            ids: HashMap::new(),
            cache: HashMap::new(),
            go_mods: None,
            crates: None,
        }
    }

    // A plain folder's working tree, with the files its listing (folder::walk) holds: nothing else is a candidate.
    fn folder(root: &'a Path, walk: crate::folder::Walk) -> Self {
        let mut repo = Repo::new(root, None);
        repo.folder = Some(walk.truncated);
        repo.set = walk.files.iter().cloned().collect();
        repo.list = Some(walk.files);
        repo
    }

    fn files(&mut self) -> &Vec<String> {
        if self.list.is_none() {
            let mut list = Vec::new();
            match &self.rev {
                None => {
                    if let Ok(out) = git_allow(self.root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"], &[0]) {
                        for p in out.split('\0').filter(|p| !p.is_empty()) {
                            if self.set.insert(p.to_string()) {
                                list.push(p.to_string());
                            }
                        }
                    }
                }
                Some(rev) => {
                    // ls-tree order: full paths sorted bytewise.
                    let mut entries = crate::repo::tree_files(self.root, rev).unwrap_or_default();
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    for (p, id) in entries {
                        if self.set.insert(p.clone()) {
                            self.ids.insert(p.clone(), id);
                            list.push(p);
                        }
                    }
                }
            }
            self.list = Some(list);
        }
        self.list.as_ref().unwrap()
    }

    fn has(&mut self, p: &str) -> bool {
        self.files();
        self.set.contains(p)
    }

    // Contents of the given files (only ones in the file list), path -> text.
    fn read_many(&mut self, paths: &[String]) -> HashMap<String, Text> {
        self.files();
        let mut want: Vec<String> = Vec::new();
        for p in paths {
            if self.set.contains(p) && !self.cache.contains_key(p) && !want.contains(p) {
                want.push(p.clone());
            }
        }
        if self.rev.is_none() {
            for p in want {
                // Only regular files: a FIFO or a device in the working tree would block the read forever. Checking
                // the size first also keeps a huge file from being read just to be turned away.
                let text = crate::paths::real_in_repo(self.root, &p)
                    .ok()
                    .flatten()
                    .filter(|real| std::fs::metadata(real).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES))
                    .and_then(|real| std::fs::read(real).ok())
                    .and_then(|buf| text_of(&buf));
                self.cache.insert(p, text);
            }
        } else if !want.is_empty() {
            let ids: Vec<ObjectId> = want.iter().map(|p| self.ids[p]).collect();
            for (p, data) in want.into_iter().zip(crate::repo::read_blobs(self.root, &ids)) {
                self.cache.insert(p, data.and_then(|d| text_of(&d)));
            }
        }
        paths.iter().map(|p| (p.clone(), self.cache.get(p).cloned().flatten())).collect()
    }

    fn read(&mut self, p: &str) -> Text {
        self.read_many(&[p.to_string()]).remove(p).flatten()
    }
}

// ---------------------------------------------------------------- blocks (scopes) in a file
// Every { } block (or Python def/class body) with its line span and the text that introduced it.
#[derive(Clone, Debug)]
struct Block {
    start: i64,
    end: i64,
    header: String,
    indent: usize,
}

fn block_tree(text: &str, lang: &str) -> Vec<Block> {
    if lang == "py" {
        py_blocks(text)
    } else {
        brace_blocks(text, lang)
    }
}

fn squash_header(head: &str) -> String {
    let h = replace_all(re!(r"\s+"), head, " ");
    let chars: Vec<char> = h.trim().chars().collect();
    chars[chars.len().saturating_sub(300)..].iter().collect()
}

// A char literal at t[i] (`'x'`, `'\n'`, `'\u{1F600}'`): its length, or None.
fn char_literal(t: &[char], i: usize) -> Option<usize> {
    let s = &t[i..t.len().min(i + 12)];
    let at = |k: usize| s.get(k).copied();
    if at(1) == Some('\\') {
        if at(2).is_some_and(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')) {
            let mut j = 3;
            while j < s.len() && j < 11 && s[j] != '\'' {
                j += 1;
            }
            if at(j) == Some('\'') {
                return Some(j + 1);
            }
        }
        return None;
    }
    if at(1).is_some_and(|c| !matches!(c, '\\' | '\'' | '\n')) && at(2) == Some('\'') {
        return Some(3);
    }
    None
}

fn brace_blocks(text: &str, lang: &str) -> Vec<Block> {
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let mut blocks = Vec::new();
    let mut stack: Vec<Block> = Vec::new();
    let (mut line, mut head, mut i) = (1i64, String::new(), 0usize);
    let char_lit = lang == "rs" || lang == "c" || lang == "go";
    while i < n {
        let c = t[i];
        let d = t.get(i + 1).copied();
        if c == '\n' {
            line += 1;
            head.push(' ');
            i += 1;
            continue;
        }
        if (c == '/' && d == Some('/')) || (c == '#' && lang == "sh") {
            while i < n && t[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && d == Some('*') {
            i += 2;
            while i < n && !(t[i] == '*' && t.get(i + 1) == Some(&'/')) {
                if t[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' || c == '`' || (c == '\'' && !char_lit) {
            i += 1;
            while i < n && t[i] != c {
                if t[i] == '\\' {
                    i += 1;
                } else if t[i] == '\n' {
                    line += 1;
                    if c != '`' {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
            head.push_str("\"\"");
            continue;
        }
        if c == '\'' && char_lit {
            if let Some(len) = char_literal(&t, i) {
                i += len;
                continue;
            }
        }
        if c == '{' {
            stack.push(Block { start: line, end: 0, header: squash_header(&head), indent: 0 });
            head.clear();
            i += 1;
            continue;
        }
        if c == '}' {
            if let Some(mut b) = stack.pop() {
                b.end = line;
                blocks.push(b);
            }
            head.clear();
            i += 1;
            continue;
        }
        if c == ';' {
            head.clear();
            i += 1;
            continue;
        }
        head.push(c);
        i += 1;
    }
    for mut b in stack {
        b.end = line;
        blocks.push(b);
    }
    blocks
}

// Indentation of the first statement in a Python block's body, or None.
fn py_body_indent(lines: &[&str], block: &Block) -> Option<usize> {
    let mut i = block.start.max(0) as usize;
    while i < lines.len() && (i as i64) < block.end {
        let t = lines[i].trim();
        if !t.is_empty() && !t.starts_with('#') {
            return Some(indent_of(lines[i]));
        }
        i += 1;
    }
    None
}

fn py_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut stack: Vec<Block> = Vec::new();
    let mut last = 0i64;
    for (idx, l) in text.split('\n').enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = indent_of(l);
        let line = idx as i64 + 1;
        while stack.last().is_some_and(|b| b.indent >= indent) {
            let mut b = stack.pop().unwrap();
            b.end = last;
            blocks.push(b);
        }
        if test(re!(r"^(?:async\s+def|def|class)\b"), t) {
            stack.push(Block { start: line, end: 0, header: t.to_string(), indent });
        }
        last = line;
    }
    for mut b in stack {
        b.end = last;
        blocks.push(b);
    }
    blocks
}

// Blocks that contain `line`, innermost first. A block opened on the definition's own line is its body, not its
// container, so only blocks that start strictly before count.
fn enclosing(blocks: &[Block], line: i64) -> Vec<&Block> {
    let mut v: Vec<&Block> = blocks.iter().filter(|b| b.start < line && b.end >= line).collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.start));
    v
}

fn enclosing_click(blocks: &[Block], line: i64) -> Vec<&Block> {
    let mut v: Vec<&Block> = blocks.iter().filter(|b| b.start <= line && b.end >= line).collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.start));
    v
}

fn strip_generics(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        if c == '<' {
            depth += 1;
        } else if c == '>' && depth > 0 {
            depth -= 1;
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

// If the block is a class-like container (class, interface, Rust impl/trait), the type it belongs to.
fn owner_of(block: Option<&Block>, lang: &str) -> Option<String> {
    let block = block?;
    let h = replace_first(re!(r"^(?:#\[[^\]]*\]\s*)+"), &block.header, "");
    let cap1 = |r: &Regex, s: &str| exec(r, s).and_then(|m| group(&m, 1).map(String::from));
    match lang {
        "rs" => {
            if let Some(t) = cap1(re!(r"^(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+(\w+)"), &h) {
                return Some(t);
            }
            if !test(re!(r"^(?:unsafe\s+)?impl\b"), &h) {
                return None;
            }
            let stripped = strip_generics(&replace_first(re!(r"^(?:unsafe\s+)?impl"), &h, ""));
            let mut t: String = match re!(r"\bwhere\b").find(&stripped).ok().flatten() {
                Some(m) => stripped[..m.start()].to_string(),
                None => stripped,
            };
            if test(re!(r"\sfor\s"), &format!(" {t}")) {
                if let Some(m) = re!(r"\bfor\s").find(&t).ok().flatten() {
                    t = t[m.end()..].to_string();
                }
            }
            cap1(re!(r"([A-Za-z_]\w*)\s*$"), t.trim())
        }
        "py" => cap1(re!(r"^class\s+(\w+)"), &h),
        "js" => cap1(re!(r"\b(?:class|interface)\s+([\w$]+)"), &h),
        "c" => cap1(re!(r"^[^(]*\b(?:class|struct|interface|record|enum|union)\s+(\w+)[^(]*$"), &h),
        _ => None,
    }
}

// Go methods: `func (s *Server) Name(` -> var `s`, type `Server`.
struct GoRecv {
    var: Option<String>,
    typ: String,
}

fn go_receiver(text: &str) -> Option<GoRecv> {
    let m = exec(re!(r"^func\s*\(\s*(?:(\w+)\s+)?\*?\s*([A-Za-z_]\w*)"), text.trim())?;
    Some(GoRecv { var: group(&m, 1).map(String::from), typ: group(&m, 2)?.to_string() })
}

// ---------------------------------------------------------------- the click site
// What surrounds the clicked name: the receiver chain before it (a.b.name, Type::name, this.name) and hints after.
struct Site {
    line: i64,
    chain: Vec<String>,
    sep: Option<&'static str>,
    is_new: bool,
    is_macro: bool,
}

fn click_site(text: &str, line_no: i64, col: i64, name: &str) -> Option<Site> {
    if line_no < 1 {
        return None;
    }
    let l: Vec<char> = text.split('\n').nth(line_no as usize - 1)?.chars().collect();
    let nm: Vec<char> = name.chars().collect();
    let word = |at: usize| {
        at + nm.len() <= l.len()
            && l[at..at + nm.len()] == nm[..]
            && !(at > 0 && is_word_char(l[at - 1]))
            && !l.get(at + nm.len()).is_some_and(|c| is_word_char(*c))
    };
    // The page sends `col` as a UTF-16 offset (a JavaScript string index); find the char it points at. Characters
    // outside the BMP take two units, so after an emoji the two counts differ.
    let mut units = 0i64;
    let clicked = (0..=l.len()).find(|&i| {
        let here = units == col;
        units += l.get(i).map_or(0, |c| c.len_utf16() as i64);
        here
    });
    let at = match clicked.filter(|&i| word(i)) {
        Some(i) => i,
        None => (0..l.len()).find(|&i| word(i))?,
    };
    let mut rest: String = l[..at].iter().collect::<String>().trim_end().to_string();
    let after: String = l[at + nm.len()..].iter().collect();
    let mut chain: Vec<String> = Vec::new();
    let mut sep: Option<&'static str> = None;
    while let Some(s) = exec(re!(r"(\?\.|\.|::|->)\s*$"), &rest) {
        if rest.ends_with("..") {
            break;
        }
        if sep.is_none() {
            sep = Some(if group(&s, 1) == Some("::") { "::" } else { "." });
        }
        let start = s.get(0).unwrap().start();
        rest = rest[..start].trim_end().to_string();
        match re!(r"[A-Za-z_$][\w$]*$").find(&rest).ok().flatten() {
            None => {
                chain.insert(0, "?".into());
                break;
            }
            Some(id) => {
                chain.insert(0, id.as_str().to_string());
                rest.truncate(id.start());
            }
        }
    }
    Some(Site {
        line: line_no,
        is_new: chain.is_empty() && test(re!(r"\bnew\s*$"), &rest),
        is_macro: test(re!(r"^!\s*[([{]"), &after),
        chain,
        sep,
    })
}

// ---------------------------------------------------------------- import resolution
// Each resolver returns targets: "the definition of `name` in `path` is what the click refers to" (`line` when it is
// already known), plus how the receiver was understood.
#[derive(Clone)]
struct Target {
    path: String,
    name: Option<String>,
    line: Option<i64>,
    weight: i64,
    rs_mod: Option<RsMod>,
}

fn target(path: &str, name: &str, weight: i64) -> Target {
    Target { path: path.to_string(), name: Some(name.to_string()), line: None, weight, rs_mod: None }
}

#[derive(Default)]
struct Resolved {
    targets: Vec<Target>,
    module: bool,
    typ: Option<String>,
}

fn targets_only(targets: Vec<Target>) -> Resolved {
    Resolved { targets, ..Default::default() }
}

fn first_existing(repo: &mut Repo, list: &[Option<String>]) -> Option<String> {
    list.iter().flatten().find(|p| repo.has(p)).cloned()
}

// ---- JS / TS
// The clause of an import/export ... from: names, braces and commas, and comments between them (stripped before
// parsing). A block comment can't run past its first */.
const JS_CLAUSE: &str = r"((?:[\w$\s{},*]|\/\/[^\n]*\n|\/\*(?:[^*]|\*(?!\/))*\*\/)+?)";
const JS_EXTS: [&str; 11] = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".d.ts", ".vue", ".svelte"];

fn js_import_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!(r#"\bimport\s+{JS_CLAUSE}\s+from\s*['"]([^'"\n]+)['"]"#)))
}

fn js_export_from_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!(r#"\bexport\s+{JS_CLAUSE}\s+from\s*['"]([^'"\n]+)['"]"#)))
}

fn js_clause(s: &str) -> String {
    let s = replace_all(re!(r"\/\/[^\n]*|\/\*[\s\S]*?\*\/"), s, " ");
    replace_first(re!(r"^type\s+"), s.trim(), "")
}

// JS String.replace(string, ''): the first occurrence only.
fn remove_first(s: &str, what: &str) -> String {
    s.replacen(what, "", 1)
}

struct JsBind {
    spec: String,
    imported: String,
}

fn js_bindings(text: &str) -> HashMap<String, JsBind> {
    let mut b: HashMap<String, JsBind> = HashMap::new();
    let item_re = re!(r"^\s*(?:type\s+)?([\w$]+)(?:\s+as\s+([\w$]+))?\s*$");
    for m in all(js_import_re(), text) {
        let mut clause = js_clause(group(&m, 1).unwrap_or(""));
        let spec = group(&m, 2).unwrap_or("").to_string();
        if let Some(braces) = exec(re!(r"\{([^}]*)\}"), &clause) {
            for it in group(&braces, 1).unwrap_or("").split(',') {
                if let Some(x) = exec(item_re, it) {
                    let imported = group(&x, 1).unwrap().to_string();
                    b.insert(group(&x, 2).unwrap_or(&imported).to_string(), JsBind { spec: spec.clone(), imported });
                }
            }
            let whole = braces.get(0).unwrap().as_str().to_string();
            clause = remove_first(&clause, &whole);
        }
        if let Some(star) = exec(re!(r"\*\s*as\s+([\w$]+)"), &clause) {
            b.insert(group(&star, 1).unwrap().to_string(), JsBind { spec: spec.clone(), imported: "*".into() });
            let whole = star.get(0).unwrap().as_str().to_string();
            clause = remove_first(&clause, &whole);
        }
        if let Some(def) = exec(re!(r"^\s*([\w$]+)"), &clause) {
            b.insert(group(&def, 1).unwrap().to_string(), JsBind { spec: spec.clone(), imported: "default".into() });
        }
    }
    let req = re!(r#"\b(?:const|let|var)\s+([\w$]+|\{[^}]*\})\s*=\s*require\(\s*['"]([^'"\n]+)['"]\s*\)(?:\.([\w$]+))?"#);
    for m in all(req, text) {
        let lhs = group(&m, 1).unwrap_or("");
        let spec = group(&m, 2).unwrap_or("").to_string();
        if lhs.starts_with('{') {
            for it in lhs[1..lhs.len() - 1].split(',') {
                if let Some(x) = exec(re!(r"^\s*([\w$]+)(?:\s*:\s*([\w$]+))?\s*$"), it) {
                    let imported = group(&x, 1).unwrap().to_string();
                    b.insert(group(&x, 2).unwrap_or(&imported).to_string(), JsBind { spec: spec.clone(), imported });
                }
            }
        } else {
            b.insert(lhs.to_string(), JsBind { spec, imported: group(&m, 3).unwrap_or("*").to_string() });
        }
    }
    b
}

struct Reexport {
    spec: Option<String>,
    exported: String,
    imported: String,
}

fn js_reexports(text: &str) -> Vec<Reexport> {
    let mut out = Vec::new();
    for m in all(js_export_from_re(), text) {
        let clause = js_clause(group(&m, 1).unwrap_or(""));
        let spec = group(&m, 2).map(String::from);
        if let Some(braces) = exec(re!(r"\{([^}]*)\}"), &clause) {
            for it in group(&braces, 1).unwrap_or("").split(',') {
                if let Some(x) = exec(re!(r"^\s*(?:type\s+)?([\w$]+)(?:\s+as\s+([\w$]+))?\s*$"), it) {
                    let imported = group(&x, 1).unwrap().to_string();
                    out.push(Reexport { spec: spec.clone(), exported: group(&x, 2).unwrap_or(&imported).to_string(), imported });
                }
            }
        } else if clause.trim() == "*" {
            out.push(Reexport { spec, exported: "*".into(), imported: "*".into() });
        }
    }
    // export { a as b } with no `from`: a local alias
    for m in all(re!(r"\bexport\s*\{([^}]*)\}\s*(?!\s*from)"), text) {
        for it in js_clause(group(&m, 1).unwrap_or("")).split(',') {
            if let Some(x) = exec(re!(r"^\s*([\w$]+)\s+as\s+([\w$]+)\s*$"), it) {
                out.push(Reexport { spec: None, exported: group(&x, 2).unwrap().into(), imported: group(&x, 1).unwrap().into() });
            }
        }
    }
    out
}

fn js_resolve_spec(repo: &mut Repo, from_file: &str, spec: &str) -> Option<String> {
    let bases = if spec.starts_with('.') {
        vec![norm(&pp::join(&[&pp::dirname(from_file), spec]))]
    } else if let Some(abs) = spec.strip_prefix('/') {
        vec![norm(abs)]
    } else {
        vec![norm(spec), norm(&format!("src/{spec}"))] // baseUrl-style paths; packages are not in the repo anyway
    };
    let mut list: Vec<Option<String>> = Vec::new();
    for base in bases.into_iter().flatten() {
        let no_ext = replace_first(re!(r"\.(?:m|c)?jsx?$"), &base, "");
        list.push(Some(base.clone()));
        list.extend(JS_EXTS.iter().map(|e| Some(format!("{base}{e}"))));
        list.extend(JS_EXTS.iter().map(|e| Some(format!("{no_ext}{e}"))));
        list.extend(JS_EXTS.iter().map(|e| Some(format!("{base}/index{e}"))));
    }
    first_existing(repo, &list)
}

// The definition the default export of a file refers to: (line, name, alias).
fn js_default_export(text: &str) -> Option<(i64, Option<String>, bool)> {
    for (i, l) in text.split('\n').enumerate() {
        let line = i as i64 + 1;
        if let Some(m) = exec(re!(r"^\s*export\s+default\s+(?:abstract\s+)?(?:async\s+)?(?:function\s*\*?|class)\s*([\w$]+)?"), l) {
            return Some((line, group(&m, 1).map(String::from), false));
        }
        if let Some(m) = exec(re!(r"^\s*export\s+default\s+([\w$]+)\s*;?\s*$"), l) {
            return Some((line, group(&m, 1).map(String::from), true));
        }
        if let Some(m) = exec(
            re!(r"^\s*module\.exports\s*=\s*(?:(?:async\s+)?function\s*\*?\s*([\w$]+)?|class\s+([\w$]+)|([\w$]+)\s*;?\s*$)"),
            l,
        ) {
            let name = group(&m, 1).or(group(&m, 2)).or(group(&m, 3)).map(String::from);
            return Some((line, name, group(&m, 3).is_some()));
        }
    }
    None
}

fn js_targets(repo: &mut Repo, from: &str, text: &str, site: &Site, name: &str) -> Resolved {
    let b = js_bindings(text);
    let (bind, wanted) = if site.chain.is_empty() {
        let bind = b.get(name);
        (bind, bind.map(|x| if x.imported == "*" { "default".to_string() } else { x.imported.clone() }))
    } else if site.chain.len() == 1 && site.sep == Some(".") {
        let bind = b.get(&site.chain[0]).filter(|x| x.imported == "*" || x.imported == "default");
        (bind, Some(name.to_string()))
    } else {
        (None, None)
    };
    let Some(bind) = bind else { return Resolved::default() };
    let module = !site.chain.is_empty();
    let Some(file) = js_resolve_spec(repo, from, &bind.spec) else { return Resolved { module, ..Default::default() } };
    Resolved {
        targets: vec![Target { path: file, name: wanted, line: None, weight: 100, rs_mod: None }],
        module,
        typ: None,
    }
}

// A target file that doesn't define the name itself may pass it on.
fn js_follow(repo: &mut Repo, file: &str, text: &str, name: &str) -> Vec<Target> {
    if name == "default" {
        let Some((line, dname, alias)) = js_default_export(text) else { return vec![] };
        return if alias {
            vec![Target { path: file.into(), name: dname, line: None, weight: 100, rs_mod: None }]
        } else {
            let n = dname.unwrap_or_else(|| "default".into());
            vec![Target { path: file.into(), name: Some(n), line: Some(line), weight: 100, rs_mod: None }]
        };
    }
    let mut out = Vec::new();
    if let Some(b) = js_bindings(text).remove(name) {
        if let Some(f) = js_resolve_spec(repo, file, &b.spec) {
            out.push(target(&f, if b.imported == "*" { "default" } else { &b.imported }, 100));
        }
    }
    for r in js_reexports(text) {
        if r.exported != name && r.exported != "*" {
            continue;
        }
        let t = match &r.spec {
            Some(spec) => js_resolve_spec(repo, file, spec),
            None => Some(file.to_string()),
        };
        if let Some(t) = t {
            out.push(target(&t, if r.exported == "*" { name } else { &r.imported }, 100));
        }
    }
    out
}

// ---- Python
struct PyBind {
    module: String,
    attr: Option<String>,
    alias: bool,
}

struct PyBindings {
    binds: HashMap<String, PyBind>,
    stars: Vec<String>,
}

fn py_bindings(text: &str) -> PyBindings {
    let mut b = PyBindings { binds: HashMap::new(), stars: Vec::new() };
    let src = text.replace("\\\n", " ");
    for m in all(re!(r"(?m)^[ \t]*from\s+(\.*)([\w.]*)\s+import\s+(\([^)]*\)|[^\n#;]+)"), &src) {
        let module = format!("{}{}", group(&m, 1).unwrap_or(""), group(&m, 2).unwrap_or(""));
        for it in group(&m, 3).unwrap_or("").replace(['(', ')'], "").split(',') {
            let Some(x) = exec(re!(r"^\s*(\w+|\*)(?:\s+as\s+(\w+))?\s*$"), it) else { continue };
            let n = group(&x, 1).unwrap();
            if n == "*" {
                b.stars.push(module.clone());
            } else {
                b.binds.insert(
                    group(&x, 2).unwrap_or(n).to_string(),
                    PyBind { module: module.clone(), attr: Some(n.to_string()), alias: false },
                );
            }
        }
    }
    for m in all(re!(r"(?m)^[ \t]*import\s+([^\n#;]+)"), &src) {
        for it in group(&m, 1).unwrap_or("").split(',') {
            let Some(x) = exec(re!(r"^\s*([\w.]+)(?:\s+as\s+(\w+))?\s*$"), it) else { continue };
            let module = group(&x, 1).unwrap();
            match group(&x, 2) {
                Some(alias) => {
                    b.binds.insert(alias.to_string(), PyBind { module: module.to_string(), attr: None, alias: true });
                }
                None => {
                    let top = module.split('.').next().unwrap_or("").to_string();
                    b.binds.insert(top.clone(), PyBind { module: top, attr: None, alias: false });
                }
            }
        }
    }
    b
}

fn py_module_file(repo: &mut Repo, from_file: &str, module: &str) -> Option<String> {
    let dots = module.chars().take_while(|c| *c == '.').count();
    let segs: Vec<&str> = module[dots..].split('.').filter(|s| !s.is_empty()).collect();
    let roots: Vec<String> = if dots > 0 {
        let mut d = pp::dirname(from_file);
        for _ in 1..dots {
            d = pp::dirname(&d);
        }
        vec![d]
    } else {
        let mut roots = Vec::new();
        let mut d = pp::dirname(from_file);
        while d != "." && d != "/" {
            roots.push(d.clone());
            d = pp::dirname(&d);
        }
        roots.push(".".into());
        roots.push("src".into());
        roots
    };
    for r in roots {
        let mut parts = vec![r.as_str()];
        parts.extend(segs.iter().copied());
        let Some(base) = norm(&pp::join(&parts)) else { continue };
        let list: Vec<Option<String>> = if segs.is_empty() {
            vec![Some(format!("{base}/__init__.py"))]
        } else {
            ["py", "pyi"]
                .iter()
                .map(|e| Some(format!("{base}.{e}")))
                .chain(["py", "pyi"].iter().map(|e| Some(format!("{base}/__init__.{e}"))))
                .collect()
        };
        if let Some(f) = first_existing(repo, &list) {
            return Some(f);
        }
    }
    None
}

// Split a dotted module path at dots that follow something other than a dot (leading dots stay with the first part).
fn split_dotted(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c == '.' && prev.is_some_and(|p| p != '.') {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        prev = Some(c);
    }
    parts.push(cur);
    parts
}

fn py_targets(repo: &mut Repo, from: &str, text: &str, site: &Site, name: &str) -> Resolved {
    let b = py_bindings(text);
    if site.chain.is_empty() {
        if let Some(bind) = b.binds.get(name) {
            if let Some(attr) = &bind.attr {
                return match py_module_file(repo, from, &bind.module) {
                    Some(f) => targets_only(vec![target(&f, attr, 100)]),
                    None => Resolved::default(),
                };
            }
        }
        let mut out = Vec::new();
        for module in &b.stars {
            if let Some(f) = py_module_file(repo, from, module) {
                out.push(target(&f, name, 60));
            }
        }
        return targets_only(out);
    }
    if site.sep != Some(".") || site.chain.iter().any(|c| c == "?") {
        return Resolved::default();
    }
    let Some(bind) = b.binds.get(&site.chain[0]) else { return Resolved::default() };
    // The dotted chain names a module, possibly followed by a class: m.Class.method
    let full: Vec<String> = if let Some(attr) = &bind.attr {
        let dot = if bind.module.ends_with('.') { "" } else { "." };
        std::iter::once(format!("{}{dot}{attr}", bind.module)).chain(site.chain[1..].iter().cloned()).collect()
    } else if bind.alias {
        std::iter::once(bind.module.clone()).chain(site.chain[1..].iter().cloned()).collect()
    } else {
        vec![site.chain.join(".")]
    };
    let dotted = re!(r"^(\.+)\.").replace(&full.join("."), "$1").into_owned();
    let parts = split_dotted(&dotted);
    for k in (1..=parts.len()).rev() {
        let Some(f) = py_module_file(repo, from, &parts[..k].join(".")) else { continue };
        let rest = &parts[k..];
        if rest.is_empty() {
            return Resolved { targets: vec![target(&f, name, 100)], module: true, typ: None };
        }
        if rest.len() == 1 {
            return Resolved { targets: vec![target(&f, name, 60)], module: false, typ: Some(rest[0].clone()) };
        }
        break;
    }
    if let Some(attr) = &bind.attr {
        // from x import Class; Class.method
        if let Some(f) = py_module_file(repo, from, &bind.module) {
            if site.chain.len() == 1 {
                return Resolved { targets: vec![target(&f, name, 60)], module: false, typ: Some(attr.clone()) };
            }
        }
    }
    Resolved::default()
}

fn py_follow(repo: &mut Repo, file: &str, text: &str, name: &str) -> Vec<Target> {
    let b = py_bindings(text);
    let mut out = Vec::new();
    if let Some(bind) = b.binds.get(name) {
        if let Some(attr) = &bind.attr {
            if let Some(f) = py_module_file(repo, file, &bind.module) {
                out.push(target(&f, attr, 100));
            }
        }
    }
    for module in &b.stars {
        if let Some(f) = py_module_file(repo, file, module) {
            out.push(target(&f, name, 100));
        }
    }
    out
}

// ---- Go
struct GoMod {
    dir: String,
    module: String,
}

fn go_modules(repo: &mut Repo) -> Rc<Vec<GoMod>> {
    if let Some(m) = &repo.go_mods {
        return m.clone();
    }
    let mods: Vec<String> = repo.files().iter().filter(|p| *p == "go.mod" || p.ends_with("/go.mod")).cloned().collect();
    let texts = repo.read_many(&mods);
    let list: Vec<GoMod> = mods
        .iter()
        .filter_map(|p| {
            let text = texts.get(p).cloned().flatten();
            let m = exec(re!(r"(?m)^module\s+(\S+)"), text.as_deref().unwrap_or(""))?;
            Some(GoMod { dir: pp::dirname(p), module: group(&m, 1)?.to_string() })
        })
        .collect();
    let list = Rc::new(list);
    repo.go_mods = Some(list.clone());
    list
}

fn go_package_files(repo: &mut Repo, dir: Option<&str>, from_file: &str) -> Vec<String> {
    let test_file = from_file.ends_with("_test.go");
    let Some(dir) = dir else { return vec![] };
    repo.files()
        .iter()
        .filter(|p| pp::dirname(p) == dir && p.ends_with(".go") && (test_file || !p.ends_with("_test.go")))
        .cloned()
        .collect()
}

fn go_targets(repo: &mut Repo, from: &str, text: &str, site: &Site, name: &str) -> Resolved {
    if site.chain.is_empty() {
        // Bare identifiers: anything declared at package level in the same directory.
        let files = go_package_files(repo, Some(&pp::dirname(from)), from);
        return targets_only(files.iter().map(|p| target(p, name, 60)).collect());
    }
    if site.chain.len() != 1 || site.sep != Some(".") {
        return Resolved::default();
    }
    let mut imports: Vec<(Option<String>, String)> = Vec::new();
    for m in all(re!(r#"(?m)^import\s+(?:([\w.]+)\s+)?"([^"]+)""#), text) {
        imports.push((group(&m, 1).map(String::from), group(&m, 2).unwrap_or("").to_string()));
    }
    for blk in all(re!(r"(?m)^import\s*\(([\s\S]*?)^\)"), text) {
        for m in all(re!(r#"(?m)^\s*(?:([\w.]+)\s+)?"([^"]+)""#), group(&blk, 1).unwrap_or("")) {
            imports.push((group(&m, 1).map(String::from), group(&m, 2).unwrap_or("").to_string()));
        }
    }
    let mods = go_modules(repo);
    for (alias, path) in imports {
        let mut matching: Vec<&GoMod> =
            mods.iter().filter(|m| path == m.module || path.starts_with(&format!("{}/", m.module))).collect();
        matching.sort_by_key(|a| std::cmp::Reverse(a.module.len()));
        let Some(module) = matching.first() else { continue };
        let dir = norm(&pp::join(&[&module.dir, &path[module.module.len()..]]));
        let files = go_package_files(repo, dir.as_deref(), "");
        if files.is_empty() {
            continue;
        }
        let pkg = match alias {
            Some(a) => Some(a),
            None => {
                let text = repo.read(&files[0]);
                exec(re!(r"(?m)^package\s+(\w+)"), text.as_deref().unwrap_or("")).and_then(|m| group(&m, 1).map(String::from))
            }
        };
        if pkg.as_deref() == Some(site.chain[0].as_str()) {
            return Resolved { targets: files.iter().map(|p| target(p, name, 100)).collect(), module: true, typ: None };
        }
    }
    Resolved::default()
}

// ---- Rust
struct Crate {
    name: String,
    dir: String,
    lib: Option<String>,
    roots: Vec<String>,
    aliases: HashMap<String, String>,
}

// A module: the file that holds it (an inline `mod x { }` keeps its parent's file), its crate and crate root, and
// its path from that root.
#[derive(Clone)]
struct RsMod {
    file: String,
    mod_root: bool,
    krate: Rc<Crate>,
    root: String,
    path: Vec<String>,
}

fn rust_crates(repo: &mut Repo) -> Rc<Vec<Rc<Crate>>> {
    if let Some(c) = &repo.crates {
        return c.clone();
    }
    let tomls: Vec<String> = repo.files().iter().filter(|p| *p == "Cargo.toml" || p.ends_with("/Cargo.toml")).cloned().collect();
    let texts = repo.read_many(&tomls);
    let mut crates = Vec::new();
    for t in &tomls {
        let dir = pp::dirname(t);
        let at = |rel: &str| norm(&pp::join(&[&dir, rel]));
        let mut section = String::new();
        let (mut pkg_name, mut lib_name, mut lib_path): (Option<String>, Option<String>, Option<String>) = (None, None, None);
        let mut bins: Vec<Option<String>> = Vec::new(); // each [[bin]]'s path
        let mut aliases = HashMap::new();
        let text = texts.get(t).cloned().flatten();
        for raw in text.as_deref().unwrap_or("").split('\n') {
            let stripped = replace_first(re!(r"#.*$"), raw, "");
            let l = stripped.trim();
            if let Some(m) = exec(re!(r"^\[\[?\s*([^\]]+?)\s*\]\]?$"), l) {
                section = group(&m, 1).unwrap_or("").to_string();
                if section == "bin" {
                    bins.push(None);
                }
                continue;
            }
            let Some(m) = exec(re!(r"^([\w-]+)\s*=\s*(.*)$"), l) else { continue };
            let key = group(&m, 1).unwrap_or("");
            let value = group(&m, 2).unwrap_or("");
            let s = exec(re!(r#"^"([^"]*)""#), value).and_then(|c| group(&c, 1).map(String::from));
            if section == "package" && key == "name" {
                pkg_name = s;
            } else if section == "lib" && key == "name" {
                lib_name = s;
            } else if section == "lib" && key == "path" {
                lib_path = s;
            } else if section == "bin" && key == "path" && !bins.is_empty() {
                *bins.last_mut().unwrap() = s;
            } else if section.ends_with("dependencies") {
                if let Some(pk) = exec(re!(r#"\bpackage\s*=\s*"([^"]+)""#), value) {
                    aliases.insert(key.replace('-', "_"), group(&pk, 1).unwrap_or("").replace('-', "_"));
                }
            }
        }
        let Some(pkg_name) = pkg_name.filter(|n| !n.is_empty()) else { continue };
        let lib = match lib_path.filter(|p| !p.is_empty()) {
            Some(p) => at(&p),
            None => at("src/lib.rs").filter(|p| repo.has(p)),
        };
        let mut roots: Vec<String> = Vec::new();
        let candidates = std::iter::once(lib.clone())
            .chain(bins.iter().map(|b| b.as_deref().filter(|p| !p.is_empty()).and_then(at)))
            .chain(std::iter::once(at("src/main.rs")));
        for p in candidates.flatten() {
            if repo.has(&p) {
                roots.push(p);
            }
        }
        let prefix = if dir == "." { String::new() } else { format!("{dir}/") };
        let extra = re!(r"^src\/bin\/[^/]+\.rs$|^src\/bin\/[^/]+\/main\.rs$|^(tests|examples|benches)\/[^/]+\.rs$|^(tests|examples|benches)\/[^/]+\/main\.rs$");
        for p in repo.files() {
            if !p.starts_with(&prefix) || !p.ends_with(".rs") {
                continue;
            }
            let rel = if dir == "." { p.as_str() } else { &p[dir.len() + 1..] };
            if test(extra, rel) {
                roots.push(p.clone());
            }
        }
        let name = lib_name.filter(|n| !n.is_empty()).unwrap_or(pkg_name).replace('-', "_");
        crates.push(Rc::new(Crate { name, dir, lib, roots, aliases }));
    }
    let crates = Rc::new(crates);
    repo.crates = Some(crates.clone());
    crates
}

fn is_mod_root(file: &str, krate: &Crate) -> bool {
    krate.roots.iter().any(|r| r == file) || pp::basename(file, "") == "mod.rs"
}

// The crate a file belongs to and that crate's root file for it.
fn rust_crate_of(repo: &mut Repo, file: &str) -> Option<(Rc<Crate>, String)> {
    let crates = rust_crates(repo);
    let mut matching: Vec<&Rc<Crate>> = crates.iter().filter(|c| c.dir == "." || file.starts_with(&format!("{}/", c.dir))).collect();
    matching.sort_by_key(|a| std::cmp::Reverse(a.dir.len()));
    let krate = (*matching.first()?).clone();
    // The root whose directory holds the file most closely (src/bin/x.rs, tests/y.rs, else lib or main).
    let mut roots: Vec<&String> =
        krate.roots.iter().filter(|r| *r == file || file.starts_with(&format!("{}/", pp::dirname(r)))).collect();
    roots.sort_by(|a, b| {
        (*b == file)
            .cmp(&(*a == file))
            .then(pp::dirname(b).len().cmp(&pp::dirname(a).len()))
            .then((Some(b.as_str()) == krate.lib.as_deref()).cmp(&(Some(a.as_str()) == krate.lib.as_deref())))
    });
    let root = (*roots.first()?).clone();
    Some((krate, root))
}

struct RustUses {
    bind: HashMap<String, Vec<String>>,
    globs: Vec<Vec<String>>,
}

// Repo files are untrusted, so a `use` tree is walked with an explicit stack (a {{{…}}} nest thousands deep used to
// overflow the server's stack), groups nested deeper than this are ignored, and so are absurdly long statements.
const MAX_USE_DEPTH: usize = 32;
const MAX_USE_LEN: usize = 4096;

fn rust_uses(text: &str) -> RustUses {
    // One `use` tree, depth first and in order, so a later binding of a name wins as it would when written out.
    fn walk(u: &mut RustUses, tree: &str) {
        let mut stack: Vec<(Vec<String>, &str, usize)> = vec![(Vec::new(), tree, 0)];
        while let Some((prefix, s, depth)) = stack.pop() {
            let s = s.trim();
            let s = s.strip_prefix("::").unwrap_or(s);
            if s.is_empty() {
                continue;
            }
            if s.ends_with('}') && s.contains('{') {
                if depth >= MAX_USE_DEPTH {
                    continue;
                }
                let idx = s.find('{').unwrap();
                let mut base: Vec<String> = prefix;
                base.extend(s[..idx].split("::").map(|x| x.trim()).filter(|x| !x.is_empty()).map(String::from));
                let inner = &s[idx + 1..s.len() - 1];
                // Split at top-level commas, as slices of `inner`; push them reversed so they pop in order.
                let mut pieces = Vec::new();
                let (mut level, mut start) = (0i64, 0);
                for (i, c) in inner.char_indices() {
                    match c {
                        '{' => level += 1,
                        '}' => level -= 1,
                        ',' if level == 0 => {
                            pieces.push(&inner[start..i]);
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
                pieces.push(&inner[start..]);
                for p in pieces.into_iter().rev() {
                    stack.push((base.clone(), p, depth + 1));
                }
                continue;
            }
            let mut pieces = re!(r"\s+as\s+").split(s).filter_map(|p| p.ok());
            let p = pieces.next().unwrap_or("");
            let alias = pieces.next();
            let mut segs: Vec<String> = prefix;
            segs.extend(p.split("::").map(|x| x.trim()).filter(|x| !x.is_empty()).map(String::from));
            if segs.last().is_some_and(|l| l == "*") {
                segs.pop();
                u.globs.push(segs);
                continue;
            }
            if segs.last().is_some_and(|l| l == "self") {
                segs.pop();
            }
            if segs.is_empty() || alias.is_some_and(|a| a.trim() == "_") {
                continue;
            }
            let key = match alias.filter(|a| !a.is_empty()) {
                Some(a) => a.trim().to_string(),
                None => segs.last().unwrap().clone(),
            };
            u.bind.insert(key, segs);
        }
    }
    let mut u = RustUses { bind: HashMap::new(), globs: Vec::new() };
    for m in all(re!(r"(?:^|[;{}\s])(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);"), text) {
        let tree = group(&m, 1).unwrap_or("");
        if tree.len() > MAX_USE_LEN {
            continue;
        }
        walk(&mut u, &replace_all(re!(r"\s+"), tree, " "));
    }
    u
}

// A child module `seg` of `module`, or None.
fn rust_child(repo: &mut Repo, module: &RsMod, seg: &str) -> Option<RsMod> {
    let text = repo.read(&module.file)?;
    let dir = if module.mod_root {
        pp::dirname(&module.file)
    } else {
        pp::join(&[&pp::dirname(&module.file), pp::basename(&module.file, ".rs")])
    };
    let mut path = module.path.clone();
    path.push(seg.to_string());
    let at = |file: String, mod_root: bool| RsMod { file, mod_root, krate: module.krate.clone(), root: module.root.clone(), path: path.clone() };
    let seg_re = fancy_regex::escape(seg);
    let attr_re = Regex::new(&js_re(&format!(r#"#\[path\s*=\s*"([^"]+)"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+{seg_re}\s*;"#))).ok()?;
    if let Some(attr) = exec(&attr_re, &text) {
        let f = norm(&pp::join(&[&dir, group(&attr, 1).unwrap_or("")]))?;
        return repo.has(&f).then(|| at(f, true));
    }
    let inline_re = Regex::new(&js_re(&format!(r"\bmod\s+{seg_re}\s*\{{"))).ok()?;
    if test(&inline_re, &text) {
        return Some(at(module.file.clone(), module.mod_root));
    }
    let flat = norm(&pp::join(&[&dir, &format!("{seg}.rs")]));
    let nested = norm(&pp::join(&[&dir, seg, "mod.rs"]));
    if let Some(f) = flat.filter(|f| repo.has(f)) {
        return Some(at(f, false));
    }
    if let Some(f) = nested.filter(|f| repo.has(f)) {
        return Some(at(f, true));
    }
    None
}

fn rust_mod_at(repo: &mut Repo, krate: &Rc<Crate>, root: &str, segs: &[String]) -> Option<RsMod> {
    let mut module = RsMod { file: root.to_string(), mod_root: true, krate: krate.clone(), root: root.to_string(), path: vec![] };
    for s in segs {
        module = rust_child(repo, &module, s)?;
    }
    Some(module)
}

fn rust_mod_path(file: &str, root: &str) -> Option<Vec<String>> {
    if file == root {
        return Some(vec![]);
    }
    let dir = pp::dirname(root);
    let rel = if dir == "." {
        file
    } else {
        file.strip_prefix(&format!("{dir}/"))?
    };
    if rel.is_empty() {
        return None;
    }
    let rel = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut segs: Vec<String> = rel.split('/').map(String::from).collect();
    if segs.last().is_some_and(|l| l == "mod") {
        segs.pop();
    }
    Some(segs)
}

// Resolve a path (as written in code or a `use`) seen from `module`: the module it reaches and what's left.
fn rust_resolve_path(repo: &mut Repo, module: &RsMod, segs: &[String], depth: usize) -> Option<(RsMod, Vec<String>)> {
    if segs.is_empty() || depth > 4 {
        return None;
    }
    let crates = rust_crates(repo);
    let head = segs[0].as_str();
    let mut i = 1;
    let mut cur = match head {
        "crate" => rust_mod_at(repo, &module.krate, &module.root, &[])?,
        "self" => module.clone(),
        "super" => {
            let mut up = module.path.clone();
            i = 0;
            while segs.get(i).is_some_and(|s| s == "super") {
                up.pop();
                i += 1;
            }
            rust_mod_at(repo, &module.krate, &module.root, &up)?
        }
        _ => {
            let real = module.krate.aliases.get(head).map(String::as_str).unwrap_or(head);
            match crates.iter().find(|c| c.lib.is_some() && c.name == real) {
                Some(other) => {
                    let lib = other.lib.clone().unwrap();
                    RsMod { file: lib.clone(), mod_root: true, krate: other.clone(), root: lib, path: vec![] }
                }
                None => {
                    let text = repo.read(&module.file);
                    let uses = rust_uses(text.as_deref().unwrap_or(""));
                    if let Some(b) = uses.bind.get(head) {
                        if !(b.len() == 1 && b[0] == head) {
                            let mut next = b.clone();
                            next.extend(segs[1..].iter().cloned());
                            return rust_resolve_path(repo, module, &next, depth + 1);
                        }
                    }
                    rust_child(repo, module, head)?
                }
            }
        }
    };
    while i < segs.len() {
        let Some(next) = rust_child(repo, &cur, &segs[i]) else { break };
        cur = next;
        i += 1;
    }
    Some((cur, segs[i..].to_vec()))
}

fn rust_click_module(repo: &mut Repo, from: &str) -> Option<RsMod> {
    let (krate, root) = rust_crate_of(repo, from)?;
    Some(RsMod {
        file: from.to_string(),
        mod_root: is_mod_root(from, &krate),
        path: rust_mod_path(from, &root).unwrap_or_default(),
        krate,
        root,
    })
}

fn rs_target(m: &RsMod, name: &str, weight: i64) -> Target {
    Target { path: m.file.clone(), name: Some(name.to_string()), line: None, weight, rs_mod: Some(m.clone()) }
}

// From a resolved `use` path ending in the item's name: the target for that item, and the type it sits in.
fn rust_target_for(repo: &mut Repo, module: &RsMod, full: &[String], weight: i64) -> Option<(Target, Option<String>)> {
    let (m, rest) = rust_resolve_path(repo, module, &full[..full.len().saturating_sub(1)], 0)?;
    let last = full.last()?;
    match rest.len() {
        0 => Some((rs_target(&m, last, weight), None)),
        1 => Some((rs_target(&m, last, 50), Some(rest[0].clone()))),
        _ => None,
    }
}

fn rs_targets(repo: &mut Repo, from: &str, text: &str, site: &Site, name: &str) -> Resolved {
    let Some(module) = rust_click_module(repo, from) else { return Resolved::default() };
    let uses = rust_uses(text);
    if site.chain.is_empty() {
        if let Some(full) = uses.bind.get(name) {
            return match rust_target_for(repo, &module, full, 100) {
                Some((t, typ)) => Resolved { targets: vec![t], module: false, typ },
                None => Resolved::default(),
            };
        }
        let mut out = Vec::new();
        for g in &uses.globs {
            if let Some((m, rest)) = rust_resolve_path(repo, &module, g, 0) {
                if rest.is_empty() {
                    out.push(rs_target(&m, name, 60));
                }
            }
        }
        return targets_only(out);
    }
    if site.sep != Some("::") || site.chain.iter().any(|c| c == "?") {
        return Resolved::default();
    }
    if site.chain.len() == 1 && site.chain[0] == "Self" {
        return Resolved::default();
    }
    let Some((m, rest)) = rust_resolve_path(repo, &module, &site.chain, 0) else { return Resolved::default() };
    match rest.len() {
        0 => Resolved { targets: vec![rs_target(&m, name, 100)], module: true, typ: None },
        1 => Resolved { targets: vec![rs_target(&m, name, 40)], module: false, typ: Some(rest[0].clone()) },
        _ => Resolved::default(),
    }
}

// `pub use` re-exports in a module that doesn't define the name itself.
fn rs_follow(repo: &mut Repo, file: &str, text: &str, name: &str, t: &Target) -> Vec<Target> {
    let module = match &t.rs_mod {
        Some(m) => m.clone(),
        None => match rust_click_module(repo, file) {
            Some(m) => m,
            None => return vec![],
        },
    };
    let uses = rust_uses(text);
    let mut out = Vec::new();
    if let Some(full) = uses.bind.get(name) {
        if let Some((t, _)) = rust_target_for(repo, &module, full, 100) {
            out.push(t);
        }
    }
    for g in &uses.globs {
        if let Some((m, rest)) = rust_resolve_path(repo, &module, g, 0) {
            if rest.is_empty() {
                out.push(rs_target(&m, name, 100));
            }
        }
    }
    out
}

// The languages with a resolver.
fn resolver_targets(lang: &str, repo: &mut Repo, from: &str, text: &str, site: &Site, name: &str) -> Option<Resolved> {
    Some(match lang {
        "js" => js_targets(repo, from, text, site, name),
        "py" => py_targets(repo, from, text, site, name),
        "go" => go_targets(repo, from, text, site, name),
        "rs" => rs_targets(repo, from, text, site, name),
        _ => return None,
    })
}

fn resolver_follow(lang: &str, repo: &mut Repo, file: &str, text: &str, name: &str, t: &Target) -> Vec<Target> {
    match lang {
        "js" => js_follow(repo, file, text, name),
        "py" => py_follow(repo, file, text, name),
        "rs" => rs_follow(repo, file, text, name, t),
        _ => vec![],
    }
}

// ---------------------------------------------------------------- candidates
#[derive(Clone)]
struct Cand {
    path: String,
    line: i64,
    text: String,
    kind: &'static str,
    weight: i64,
    owner: Option<String>,
    visible: Option<bool>,
    depth: i64,
    score: i64,
}

fn cand(path: &str, line: i64, text: &str, kind: &'static str) -> Cand {
    Cand { path: path.into(), line, text: text.trim().into(), kind, weight: 0, owner: None, visible: None, depth: 0, score: 0 }
}

const MAX_GREP_LINES: usize = 20000;

// What git grep saw: every file with a hit on the name (definition or not), and whether that list is complete.
struct Hits {
    name: String,
    paths: HashSet<String>,
    complete: bool,
}

impl Hits {
    // Whether a file can be skipped when looking for `name` in it: grep saw every hit and none was in that file.
    fn absent(&self, path: &str, name: &str) -> bool {
        self.complete && name == self.name && !self.paths.contains(path)
    }
}

// Lines that may define `name`, from git grep. The output is read as it streams, and the search stopped once
// MAX_GREP_LINES hits have been seen, so a common name in a huge repo costs bounded time and memory. A plain folder is
// searched with --no-index (which works outside a repo and doesn't look at .gitignore or into .git) and only its
// listed files count; once its listing was cut short, git would still search everything, so the listed files are
// searched in process instead (grep_listed).
fn grep_defs(repo: &Repo, name: &str) -> Result<(Vec<Cand>, Hits)> {
    use std::io::BufRead;
    if repo.folder == Some(true) {
        return Ok(grep_listed(repo, name));
    }
    let mut args = vec!["grep", "-n", "-I", "-w", "-F", "-z", "--max-count=200", "-e", name];
    match &repo.rev {
        None if repo.folder.is_some() => args.push("--no-index"),
        None => args.push("--untracked"),
        Some(rev) => args.push(rev),
    }
    args.push("--");
    let mut child = crate::git::command(repo.root)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| crate::git::internal(format!("git: {e}")))?;
    let mut stderr = child.stderr.take().unwrap();
    let errors = match crate::git::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    }) {
        Ok(h) => h,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(crate::git::no_thread(e));
        }
    };
    let prefix = repo.rev.as_ref().map(|r| format!("{r}:")).unwrap_or_default();
    let mut found = Vec::new();
    let mut hits = Hits { name: name.to_string(), paths: HashSet::new(), complete: true };
    let mut scanned = 0;
    let mut rec: Vec<u8> = Vec::new();
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    // Records are path\0line\0text\n. A path may hold a newline (never a NUL), so a piece with fewer than three
    // fields is the start of a record that goes on after the next newline. Anything else that doesn't parse is skipped.
    loop {
        match out.read_until(b'\n', &mut rec) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if rec.iter().filter(|&&b| b == 0).count() < 2 && rec.last() == Some(&b'\n') {
            continue; // a newline inside the path: the record goes on
        }
        let r = String::from_utf8_lossy(rec.strip_suffix(b"\n").unwrap_or(&rec)).into_owned();
        rec.clear();
        let f: Vec<&str> = r.split('\0').collect();
        if f.len() < 3 {
            continue;
        }
        scanned += 1;
        if scanned > MAX_GREP_LINES {
            hits.complete = false;
            break;
        }
        let (p0, line_no) = (f[0], f[1]);
        let text = f[2..].join("\0");
        if !p0.starts_with(&prefix) || line_no.is_empty() || !line_no.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let p = &p0[prefix.len()..];
        if repo.folder.is_some() && !repo.set.contains(p) {
            continue; // past a plain folder's listing
        }
        hits.paths.insert(p.to_string());
        if let Some(kind) = def_kind(p, &text, name) {
            found.push(cand(p, line_no.parse().unwrap_or(0), &text, kind));
        }
    }
    if !hits.complete {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| crate::git::internal(format!("git: {e}")))?;
    let errors = errors.join().unwrap_or_default();
    // Exit 1 is "no match"; a killed grep was stopped on purpose.
    if hits.complete && !matches!(status.code(), Some(0) | Some(1)) {
        return Err(crate::git::git_error(errors.trim().to_string()));
    }
    Ok((found, hits))
}

// The hits git grep -n -I -w -F --max-count=200 would give in a plain folder's listed files, in path order: binary
// files (a NUL in the first 8000 bytes), files over MAX_FILE_BYTES (which Repo never reads either) and anything that
// isn't a regular file inside the folder are skipped. Files are read MAX_GREP_READERS at a time, and the search stops
// at MAX_GREP_LINES hits like git's.
fn grep_listed(repo: &Repo, name: &str) -> (Vec<Cand>, Hits) {
    let list = repo.list.as_deref().unwrap_or_default();
    let mut found = Vec::new();
    let mut hits = Hits { name: name.to_string(), paths: HashSet::new(), complete: true };
    let mut scanned = 0;
    // In batches, so a common name stops the search early instead of reading every file first.
    'files: for batch in list.chunks(MAX_GREP_READERS * 8) {
        let lines = crate::server::pool(batch, MAX_GREP_READERS, |p| word_lines(repo.root, p, name));
        for (p, lines) in batch.iter().zip(lines) {
            if !lines.is_empty() {
                hits.paths.insert(p.clone());
            }
            for (line_no, text) in lines {
                scanned += 1;
                if scanned > MAX_GREP_LINES {
                    hits.complete = false;
                    break 'files;
                }
                if let Some(kind) = def_kind(p, &text, name) {
                    found.push(cand(p, line_no, &text, kind));
                }
            }
        }
    }
    (found, hits)
}

const MAX_GREP_READERS: usize = 16;

// The lines of a working-tree file holding `name` as a whole word, as git grep -w -F matches it: an occurrence with no
// word character ([A-Za-z0-9_]) right before or after it; the first 200 of them.
fn word_lines(root: &Path, p: &str, name: &str) -> Vec<(i64, String)> {
    let Some(buf) = crate::paths::real_in_repo(root, p)
        .ok()
        .flatten()
        .filter(|real| std::fs::metadata(real).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES))
        .and_then(|real| std::fs::read(real).ok())
    else {
        return Vec::new();
    };
    if buf[..buf.len().min(8000)].contains(&0) {
        return Vec::new();
    }
    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let needle = name.as_bytes();
    let mut out = Vec::new();
    for (i, line) in buf.split(|&b| b == b'\n').enumerate() {
        let hit = (0..line.len().saturating_sub(needle.len() - 1)).any(|at| {
            line[at..].starts_with(needle) && !word(at.checked_sub(1).and_then(|b| line.get(b))) && !word(line.get(at + needle.len()))
        });
        if hit {
            out.push((i as i64 + 1, String::from_utf8_lossy(line).into_owned()));
            if out.len() == 200 {
                break;
            }
        }
    }
    out
}

// Definitions of `name` in one file's text.
fn defs_in_text(p: &str, text: &str, name: &str) -> Vec<Cand> {
    text.split('\n')
        .enumerate()
        .filter_map(|(i, l)| def_kind(p, l, name).map(|kind| cand(p, i as i64 + 1, l, kind)))
        .collect()
}

// At most this many target files are read per round of resolve_targets.
const MAX_TARGET_FILES: usize = 100;

// Follow resolver targets (and re-exports, a few levels deep) to concrete definitions. A target file that grep saw no
// hit in can't define the name, or pass it on under that name, so it isn't read: a bare Go name targets every file of
// its package, and that used to mean reading and scanning all of them.
fn resolve_targets(repo: &mut Repo, lang: &str, targets: Vec<Target>, hits: &Hits) -> Vec<Cand> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = targets;
    let mut depth = 0;
    while depth < 4 && !queue.is_empty() {
        queue.retain(|t| t.line.is_some() || !t.name.as_deref().is_some_and(|n| hits.absent(&t.path, n)));
        queue.truncate(MAX_TARGET_FILES);
        let paths: Vec<String> = queue.iter().map(|t| t.path.clone()).collect();
        let texts = repo.read_many(&paths);
        let mut next = Vec::new();
        for t in &queue {
            let key = format!("{}\0{}\0{}", t.path, t.name.as_deref().unwrap_or("undefined"), t.line.map(|l| l.to_string()).unwrap_or_default());
            if seen.contains(&key) {
                continue;
            }
            let Some(name) = t.name.as_deref().filter(|n| !n.is_empty()) else { continue };
            seen.insert(key);
            let Some(text) = texts.get(&t.path).cloned().flatten() else { continue };
            if let Some(line) = t.line {
                let l = text.split('\n').nth((line - 1).max(0) as usize).unwrap_or("");
                let mut c = cand(&t.path, line, l, "export");
                c.weight = t.weight;
                found.push(c);
                continue;
            }
            let defs = if name == "default" { vec![] } else { defs_in_text(&t.path, &text, name) };
            let only_attributes = defs.iter().all(|d| provisional(d.kind));
            for mut d in defs {
                d.weight = t.weight;
                found.push(d);
            }
            // A provisional match (an indented Python assignment, say) may turn out not to count, so it doesn't stop the search.
            if only_attributes {
                for mut n in resolver_follow(lang, repo, &t.path, &text, name, t) {
                    n.weight = n.weight.min(t.weight);
                    next.push(n);
                }
            }
        }
        queue = next;
        depth += 1;
    }
    found
}

// ---------------------------------------------------------------- scoring
#[derive(PartialEq)]
enum Recv {
    None,
    Self_,
    Type,
    Expr,
    Module,
}

struct Ctx<'a> {
    from: &'a str,
    lang: &'static str,
    recv: Recv,
    self_type: Option<String>,
    target_type: Option<String>,
    is_new: bool,
    is_macro: bool,
}

fn score(c: &Cand, ctx: &Ctx) -> i64 {
    let mut s = c.weight;
    let same = c.path == ctx.from;
    let member = c.owner.is_some() || c.kind == "method";
    match ctx.recv {
        Recv::None => {
            if member {
                s += if ctx.lang == "c" {
                    if c.owner.is_some() && c.owner == ctx.self_type {
                        40
                    } else {
                        -10
                    }
                } else {
                    -50
                };
            } else if same && c.visible.is_some() {
                s += if c.visible == Some(true) { 40 + 30 * c.depth } else { -40 };
            }
        }
        Recv::Self_ | Recv::Type => {
            if c.owner.is_some() && ctx.target_type.is_some() && c.owner == ctx.target_type {
                s += 60 + if same { 5 } else { 0 };
            } else {
                s += if member { -10 } else { -40 };
            }
        }
        Recv::Expr => s += if member { 20 } else { -20 },
        Recv::Module => {
            if c.weight == 0 {
                s -= 20;
            }
        }
    }
    if ctx.is_new {
        s += if c.kind == "class" || c.kind == "type" { 20 } else { -10 };
    }
    if ctx.lang == "rs" {
        s += if ctx.is_macro {
            if c.kind == "macro" {
                60
            } else {
                -40
            }
        } else if c.kind == "macro" {
            -40
        } else {
            0
        };
    }
    if !same {
        s += if test(noise_path(), &c.path) {
            -40
        } else if pp::dirname(&c.path) == pp::dirname(ctx.from) {
            10
        } else {
            0
        };
    }
    if c.kind == "variable" || c.kind == "export" {
        s -= 5;
    }
    s
}

fn out_json(c: &Cand) -> Value {
    json!({ "path": c.path, "line": c.line, "text": c.text, "kind": c.kind, "score": c.score })
}

// ---------------------------------------------------------------- entry point
// `from` is the path of the clicked file, `line`/`col` the click; `rev` the side of the range that was clicked
// (None = working tree). `folder` is a plain folder's listing, in place of the repo's files.
pub fn definitions(root: &Path, rev: Option<&str>, folder: Option<crate::folder::Walk>, name: Option<&str>, from: &str, line: i64, col: i64) -> Result<Value> {
    let name = match name {
        Some(n) if test(re!(r"^[A-Za-z_$][\w$]{0,99}$"), n) => n,
        _ => return Err(http_error(400, "bad name")),
    };
    let mut repo = match folder {
        Some(walk) => Repo::folder(root, walk),
        None => Repo::new(root, rev),
    };
    let from_lang = if from.is_empty() { "any" } else { lang_of(from) };

    let mut all_cands: Vec<Cand> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut add = |d: Cand, all_cands: &mut Vec<Cand>| {
        let k = format!("{}:{}", d.path, d.line);
        match index.get(&k) {
            None => {
                index.insert(k, all_cands.len());
                all_cands.push(d);
            }
            Some(&i) => all_cands[i].weight = all_cands[i].weight.max(d.weight),
        }
    };
    // A plain folder needs git only for this, and git may not be installed there: then there is nothing to go to.
    let (grepped, hits) = match grep_defs(&repo, name) {
        Err(_) if repo.folder.is_some() => (Vec::new(), Hits { name: name.to_string(), paths: HashSet::new(), complete: false }),
        r => r?,
    };
    for d in grepped {
        add(d, &mut all_cands);
    }

    // The click site, and what the clicked file's imports say the name refers to.
    let mut ctx = Ctx { from, lang: from_lang, recv: Recv::None, self_type: None, target_type: None, is_new: false, is_macro: false };
    let from_text = if !from.is_empty() && line != 0 { repo.read(from) } else { None };
    let site = from_text.as_ref().and_then(|t| click_site(t, line, col, name));
    let mut from_blocks: Option<Vec<Block>> = None;
    if let (Some(site), Some(from_text)) = (&site, &from_text) {
        ctx.is_new = site.is_new;
        ctx.is_macro = site.is_macro;
        let blocks = block_tree(from_text, from_lang);
        {
            let around = enclosing_click(&blocks, site.line);
            ctx.self_type = around.iter().find_map(|b| owner_of(Some(b), from_lang));
            let go_recv = if from_lang == "go" {
                around.iter().find(|b| test(re!(r"^func\s*\("), &b.header)).and_then(|b| go_receiver(&b.header))
            } else {
                None
            };
            let ch = &site.chain;
            let is_go_var = |c: &str| go_recv.as_ref().is_some_and(|g| g.var.as_deref() == Some(c));
            if ch.is_empty() {
                ctx.recv = Recv::None;
            } else if ch.len() == 1 && (["this", "self", "Self", "cls"].contains(&ch[0].as_str()) || is_go_var(&ch[0])) {
                ctx.recv = Recv::Self_;
                ctx.target_type = if is_go_var(&ch[0]) { go_recv.as_ref().map(|g| g.typ.clone()) } else { ctx.self_type.clone() };
            } else if ch.last().unwrap() != "?" && ch.last().unwrap().starts_with(|c: char| c.is_ascii_uppercase()) && from_lang != "go" {
                ctx.recv = Recv::Type;
                ctx.target_type = ch.last().cloned();
            } else {
                ctx.recv = Recv::Expr;
            }
        }
        // Resolution is best effort; the grep candidates still stand.
        if let Some(res) = resolver_targets(from_lang, &mut repo, from, from_text, site, name) {
            if res.module {
                ctx.recv = Recv::Module;
            }
            if let Some(t) = res.typ {
                ctx.recv = Recv::Type;
                ctx.target_type = Some(t);
            }
            for d in resolve_targets(&mut repo, from_lang, res.targets, &hits) {
                add(d, &mut all_cands);
            }
        }
        from_blocks = Some(blocks);
    }

    // Where each candidate sits: inside a class/impl (a member of that type), or a scope that may hide it. Only the
    // first MAX_ANALYZED_FILES files are read, so put the clicked file and the files the imports point to first,
    // then the rest in grep order.
    let mut cands = all_cands;
    // The clicked line itself is where the name is, not where to go.
    if site.is_some() {
        cands.retain(|c| !(c.path == from && c.line == line));
    }
    let mut files: Vec<String> = Vec::new();
    let mut listed: HashSet<&str> = HashSet::new();
    for p in cands
        .iter()
        .filter(|c| c.path == from)
        .chain(cands.iter().filter(|c| c.weight > 0))
        .chain(cands.iter())
        .map(|c| c.path.as_str())
    {
        if listed.insert(p) {
            files.push(p.to_string());
        }
    }
    files.truncate(MAX_ANALYZED_FILES);
    let texts = repo.read_many(&files);
    for f in &files {
        let Some(text) = texts.get(f).cloned().flatten() else { continue };
        let lang = lang_of(f);
        let own;
        let blocks: &Vec<Block> = match (&from_blocks, f == from) {
            (Some(b), true) => b,
            _ => {
                own = block_tree(&text, lang);
                &own
            }
        };
        let lines: Vec<&str> = text.split('\n').collect();
        for c in cands.iter_mut().filter(|c| &c.path == f) {
            let around = enclosing(blocks, c.line);
            let owner = owner_of(around.first().copied(), lang);
            let header = around.first().map(|b| b.header.as_str()).unwrap_or("");
            match c.kind {
                "attribute" => {
                    // A Python class attribute: one level into a class body, not in a method or a call's arguments.
                    let l = lines.get((c.line - 1).max(0) as usize).copied().unwrap_or("");
                    if lang == "py" && owner.is_some() && Some(indent_of(l)) == py_body_indent(&lines, around[0]) {
                        c.kind = "variable";
                    } else {
                        continue;
                    }
                }
                "groupitem" => {
                    // A Go const/var group item: the nearest line above that starts in column 0 opens the group.
                    let opener = lines[..(c.line - 1).max(0) as usize]
                        .iter()
                        .rev()
                        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//") && !l.starts_with([' ', '\t']));
                    if lang == "go" && opener.is_some_and(|l| test(re!(r"^(?:const|var)\s*\("), l)) {
                        c.kind = "variable";
                    } else {
                        continue;
                    }
                }
                "enumitem" | "structitem" => {
                    // A Rust variant or field: directly in an enum (for a variant) or struct/union (for a field) body,
                    // which is also the type it belongs to.
                    let (keyword, kind) = if c.kind == "enumitem" { ("enum", "variant") } else { ("struct|union", "field") };
                    let re = Regex::new(&format!(r"\b(?:{keyword})\s+([A-Za-z_][A-Za-z0-9_]*)")).unwrap();
                    match exec(&re, header).and_then(|m| group(&m, 1).map(String::from)) {
                        Some(t) if lang == "rs" => {
                            c.kind = kind;
                            c.owner = Some(t);
                        }
                        _ => continue,
                    }
                }
                "methodhead" => {
                    if lang == "js" && opens_method(&lines, (c.line - 1).max(0) as usize) {
                        c.kind = "method";
                    } else {
                        continue;
                    }
                }
                "signature" => {
                    // A TypeScript method signature: only an interface or an abstract or declared class has bodiless ones.
                    if lang == "js" && test(re!(r"\binterface\s|\babstract\s+class\s|\bdeclare\s+(?:abstract\s+)?class\s"), header) {
                        c.kind = "method";
                    } else {
                        continue;
                    }
                }
                _ => {}
            }
            if let Some(o) = &owner {
                c.owner = Some(o.clone());
                if c.kind == "function" {
                    c.kind = "method";
                }
            }
            if lang == "go" && test(re!(r"^func\s*\("), &c.text) {
                c.kind = "method";
                c.owner = go_receiver(&c.text).map(|g| g.typ);
            }
            if lang == "c" {
                if let Ok(r) = Regex::new(&js_re(&format!(r"(\w+)::{name}\s*\("))) {
                    if let Some(m) = exec(&r, &c.text) {
                        c.owner = group(&m, 1).map(String::from);
                    }
                }
            }
            if let (true, Some(site), None) = (f == from, &site, &owner) {
                c.visible = Some(around.is_empty() || (around[0].end >= site.line && around[0].start <= site.line));
                c.depth = around.len() as i64;
            }
        }
    }

    cands.retain(|c| !provisional(c.kind)); // unconfirmed (or not analyzed) provisional matches
    for c in cands.iter_mut() {
        c.score = score(c, &ctx);
    }
    cands.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)).then(a.line.cmp(&b.line)));
    cands.truncate(50);
    if cands.is_empty() {
        return Ok(json!({ "jump": null, "candidates": [] }));
    }
    let best = cands[0].score;
    let close: Vec<&Cand> = cands.iter().filter(|c| c.score >= best - CONFIDENT_MARGIN).collect();
    // A lone candidate that scores below zero is a poor match (a method for a bare call, a noise path): show it in
    // the picker instead of opening it.
    Ok(if close.len() == 1 && best >= 0 {
        json!({ "jump": out_json(&cands[0]), "candidates": cands.iter().map(out_json).collect::<Vec<_>>() })
    } else {
        json!({ "jump": null, "candidates": close.into_iter().map(out_json).collect::<Vec<_>>() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_compiles() {
        for lang in ["js", "py", "go", "rs", "c", "sh", "any"] {
            for (_, src) in def_rules(lang) {
                let word = r"(?<![\w$])foo(?![\w$])";
                Regex::new(&js_re(&src.split(NAME).collect::<Vec<_>>().join(word))).unwrap();
            }
        }
        js_import_re();
        js_export_from_re();
    }

    #[test]
    fn deep_use_trees_are_walked_without_recursion() {
        // 2000 levels fits under MAX_USE_LEN, so this goes through the walk (and its depth cap), not the length check.
        let deep = format!("use {}x{};", "{".repeat(2000), "}".repeat(2000));
        assert!(deep.len() < MAX_USE_LEN);
        rust_uses(&deep);
        let u = rust_uses("use a::{b, c::{d as e, *}, f::self};");
        assert_eq!(u.bind["b"], ["a", "b"]);
        assert_eq!(u.bind["e"], ["a", "c", "d"]);
        assert_eq!(u.bind["f"], ["a", "f"]);
        assert_eq!(u.globs, [vec!["a".to_string(), "c".to_string()]]);
    }

    #[test]
    fn click_columns_are_utf16_offsets() {
        // Three astral characters (two UTF-16 units each) before the click: the bare foo is at char 28, unit 31.
        let line = "const s = \"\u{1F600}\u{1F600}\u{1F600}\"; obj.foo(); foo();";
        let site = click_site(line, 1, 31, "foo").unwrap();
        assert!(site.chain.is_empty(), "clicked obj.foo instead of the bare foo: {:?}", site.chain);
        let site = click_site(line, 1, 24, "foo").unwrap();
        assert_eq!(site.chain, ["obj"]);
    }

    #[test]
    fn posix_paths() {
        assert_eq!(pp::dirname("a/b.js"), "a");
        assert_eq!(pp::dirname("b.js"), ".");
        assert_eq!(pp::join(&["src", "./a"]), "src/a");
        assert_eq!(pp::join(&[".", "/server"]), "server");
        assert_eq!(norm("../x"), None);
        assert_eq!(norm("a/../b"), Some("b".into()));
        assert_eq!(split_dotted("..a.b"), vec!["..a", "b"]);
    }
}
