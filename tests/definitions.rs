// Go-to-definition accuracy. Builds a throwaway repo with same-named definitions in several places and checks that
// each click lands on the right one. The fixture is large, so one test runs every case against one server and
// reports all the failures together.
mod common;
use common::*;

fn files() -> Vec<(String, String)> {
    let j = |l: &[&str]| l.join("\n");
    let mut f: Vec<(&str, String)> = vec![
        // ---- JS / TS
        ("src/a.js", "export function render() {}\n".into()),
        ("src/b.js", "export function render() {}\n".into()),
        ("src/lib/index.js", "export { tool as t2, tool } from './tools';\nexport default function helper() {}\n".into()),
        ("src/lib/tools.ts", "export function tool(): void {}\n".into()),
        ("src/other/tools.js", "export function tool() {}\nexport function helper() {}\n".into()),
        ("src/util.js", "function fmt() {}\nmodule.exports = { fmt };\n".into()),
        ("src/other/fmt.js", "function fmt() {}\n".into()),
        (
            "src/use.js",
            j(&[
                "import { render } from './a';",
                "import * as B from './b';",
                "import helper, { tool as t } from './lib/index.js';",
                "const util = require('./util');",
                "const value = 2;",
                "class Widget {",
                "  draw() {",
                "    return 1;",
                "  }",
                "  paint() {",
                "    this.draw();",
                "  }",
                "}",
                "class Other {",
                "  draw() {",
                "    return 2;",
                "  }",
                "}",
                "function shadow() {",
                "  const value = 1;",
                "  return value + 1;",
                "}",
                "render();",
                "B.render();",
                "helper();",
                "t();",
                "util.fmt();",
                "export const total = value * 3;",
                "",
            ]),
        ),
        (
            "src/multi.js",
            j(&[
                "import {",
                "  tool, // the one in other/, not lib/ {",
                "  /* } */ helper /* also from there */",
                "} from './other/tools'; // http://example.com/x",
                "tool();",
                "helper();",
                "",
            ]),
        ),
        ("src/re.js", "export {\n  fmt, // re-exported from other/\n} from './other/fmt';\n".into()),
        ("src/use_re.js", "import { fmt } from './re';\nfmt();\n".into()),
        // ---- Python
        ("pkg/__init__.py", "from .core import run\n".into()),
        (
            "pkg/core.py",
            j(&["def run():", "    pass", "", "class Engine:", "    def start(self):", "        pass", "", "    def go(self):", "        self.start()", ""]),
        ),
        ("pkg/alt.py", j(&["def run():", "    pass", "", "class Car:", "    def start(self):", "        pass", ""])),
        ("main.py", j(&["from pkg import run", "import pkg.alt as alt", "from pkg.core import Engine", "", "run()", "alt.run()", "Engine.start(None)", ""])),
        (
            "cfg/config.py",
            j(&[
                "class Config:",
                "    timeout = 5",
                "    retries: int = 3",
                "    label: str",
                "    timeout == 4",
                "",
                "    def check(self):",
                "        timeout = 1",
                "        return self.timeout + self.retries + len(self.label)",
                "",
                "    def call(self):",
                "        run(",
                "            timeout=2,",
                "        )",
                "",
            ]),
        ),
        ("cfg/other.py", "class Other:\n    retries = 9\n".into()),
        ("cfg/use.py", "from cfg.config import Config\n\nConfig.retries\n".into()),
        // A file name with a newline in it sorts before the file that matters
        ("wz/a\nb.py", "def zap():\n    pass\n".into()),
        ("wz/thing.py", "class Thing:\n    def zap(self):\n        pass\n".into()),
        ("wz/tools.py", "def zap():\n    pass\n".into()),
        ("wz/use.py", "def f(obj):\n    obj.zap()\n".into()),
        ("gz/a\nb.go", "package gz\n\nfunc other() {}\n".into()),
        ("gz/helper.go", "package gz\n\nfunc zip() {}\n".into()),
        ("gz/use.go", "package gz\n\nfunc f() {\n\tzip()\n}\n".into()),
        ("hz/zip.go", "package hz\n\nfunc zip() {}\n".into()),
        // ---- Go
        ("go.mod", "module example.com/app\n\ngo 1.22\n".into()),
        (
            "server/server.go",
            j(&[
                "package server",
                "",
                "type Server struct{}",
                "",
                "func (s *Server) Start() {}",
                "",
                "func (s *Server) Run() {",
                "\ts.Start()",
                "}",
                "",
                "func New() *Server { return &Server{} }",
                "",
            ]),
        ),
        (
            "client/client.go",
            j(&["package client", "", "type Client struct{}", "", "func (c *Client) Start() {}", "", "func New() *Client { return nil }", "", "func helper() {}", ""]),
        ),
        ("client/util.go", "package client\n\nfunc use() {\n\thelper()\n}\n".into()),
        (
            "main.go",
            j(&["package main", "", "import (", "\tsrv \"example.com/app/server\"", "\t\"example.com/app/client\"", ")", "", "func main() {", "\tsrv.New()", "\tclient.New()", "}", ""]),
        ),
        // ---- Rust (a two-crate workspace)
        ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n".into()),
        ("core/Cargo.toml", "[package]\nname = \"my-core\"\nversion = \"0.1.0\"\n".into()),
        (
            "core/src/lib.rs",
            j(&["pub mod shapes;", "pub use shapes::area;", "", "#[macro_export]", "macro_rules! square {", "    ($x:expr) => { $x * $x };", "}", ""]),
        ),
        (
            "core/src/shapes.rs",
            j(&[
                "pub struct Circle {",
                "    pub r: f64,",
                "}",
                "",
                "impl Circle {",
                "    pub fn new(r: f64) -> Self {",
                "        Circle { r }",
                "    }",
                "",
                "    pub fn area(&self) -> f64 {",
                "        3.14 * self.r * self.r",
                "    }",
                "",
                "    pub fn grow(&mut self) {",
                "        let _a = self.area();",
                "    }",
                "}",
                "",
                "pub fn area(c: &Circle) -> f64 {",
                "    c.area()",
                "}",
                "",
            ]),
        ),
        ("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmy-core = { path = \"../core\" }\n".into()),
        (
            "app/src/main.rs",
            j(&[
                "mod geo;",
                "use my_core::shapes::Circle;",
                "use my_core::area;",
                "",
                "fn main() {",
                "    let c = Circle::new(1.0);",
                "    area(&c);",
                "    geo::area();",
                "    let _n = my_core::square!(2);",
                "}",
                "",
            ]),
        ),
        ("app/src/geo.rs", "pub fn area() -> f64 {\n    0.0\n}\n\npub fn new() {}\n".into()),
        ("app/src/other.rs", "macro_rules! square {\n    () => {};\n}\n".into()),
        // ---- Objective-C (the NS_ENUM rule has literal capital Ns in its pattern)
        ("ui/shade.h", "typedef NS_ENUM(NSInteger, Shade) {\n  ShadeDark,\n};\n".into()),
        ("ui/view.m", "static Shade current = ShadeDark;\n".into()),
        // ---- More files define the name than get analyzed; the imported one sorts last
        ("zz/gadget.py", "class Gadget:\n    def spin(self):\n        pass\n".into()),
        ("use_gadget.py", "from zz.gadget import Gadget\n\nGadget.spin(None)\n".into()),
        // ---- The page sends columns as UTF-16 offsets; astral characters before the click make them differ
        ("src/emoji.js", j(&["function foo() {}", "class A {", "  foo() {", "  }", "}", "const s = \"\u{1F600}\u{1F600}\u{1F600}\"; obj.foo(); foo();", ""])),
        // ---- Test-runner calls look like methods but define nothing
        ("src/spec.js", j(&["describe('suite', function () {", "  it('works', function () {", "  });", "});", ""])),
        // ---- Generator methods
        ("src/gen.js", j(&["class Seq {", "  *gen() {", "    yield 1;", "  }", "  run() {", "    return this.gen();", "  }", "}", ""])),
        // ---- TypeScript interface method signatures (a call at the start of a line is not one)
        (
            "src/shape.ts",
            j(&["export interface Shape {", "  perimeter(scale: number): number;", "}", "", "export function total(s: Shape): number {", "  perimeter(1);", "  return s.perimeter(2);", "}", ""]),
        ),
        // ---- TypeScript methods with parameters over several lines or an object return type (a call split over lines,
        // callback and all, is not one)
        (
            "src/store.ts",
            j(&[
                "export class Store {",
                "  load(",
                "    key: string,",
                "    { fresh }: { fresh: boolean; ttl?: number },",
                "  ): Promise<{ value: string }> {",
                "    return this.get(key, fresh);",
                "  }",
                "  shape(): { a: number } {",
                "    return { a: 1 };",
                "  }",
                "  run() {",
                "    this.load('k', { fresh: true });",
                "    this.shape();",
                "    notify(",
                "      1,",
                "    );",
                "    notify(",
                "      () => {",
                "        return 2;",
                "      },",
                "    )",
                "  }",
                "}",
                "export function notify(n: unknown) {}",
                "notify(3);",
                "",
            ]),
        ),
        // ---- Go grouped declarations (a struct field with the same name is not one)
        (
            "colors/colors.go",
            j(&["package colors", "", "type Color int", "", "const (", "\tRed Color = iota", "\tGreen", ")", "", "var (", "\tDefault = Red", ")", "", "type Pen struct {", "\tGreen bool", "}", ""]),
        ),
        ("cmd/paint.go", j(&["package main", "", "import \"example.com/app/colors\"", "", "func paint() {", "\t_ = colors.Green", "\t_ = colors.Default", "}", ""])),
        // ---- Rust enum variants and struct fields (a struct literal's fields are not either)
        (
            "core/src/kinds.rs",
            j(&[
                "pub enum Shape {",
                "    Circle,",
                "    Square(f64),",
                "}",
                "",
                "pub struct Point {",
                "    pub x: f64,",
                "    y: f64,",
                "}",
                "",
                "pub fn pick() -> Shape {",
                "    Shape::Circle",
                "}",
                "",
                "pub fn px(p: &Point) -> f64 {",
                "    let q = Point {",
                "        x: 1.0,",
                "        y: 2.0,",
                "    };",
                "    p.x + q.y",
                "}",
                "",
            ]),
        ),
        // ---- Python tuple assignment
        ("cfg/pair.py", "left, right = 1, 2\n".into()),
        ("cfg/use_pair.py", "from cfg.pair import right\n\nprint(right)\n".into()),
    ];
    // Decoys that look just like the target (same class name) and sort before it.
    let decoys: Vec<String> = (0..45).map(|i| format!("dec/d{i:02}.py")).collect();
    let mut out: Vec<(String, String)> = f.drain(..).map(|(p, t)| (p.to_string(), t)).collect();
    out.extend(decoys.into_iter().map(|p| (p, "class Gadget:\n    def spin(self):\n        pass\n".to_string())));
    out
}

struct Fixture {
    dir: TempDir,
    s: Server,
    // A plain folder (no repo): only the working tree can be clicked.
    folder: bool,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = TempDir::new("def");
        for (p, text) in files() {
            write(dir.path(), &p, text);
        }
        git(dir.path(), &["init", "-q"]);
        commit_all(dir.path(), "fixtures");
        // Working-tree edit: render moves down two lines, so HEAD and the working tree disagree on its line.
        write(dir.path(), "src/a.js", "// moved\n\nexport function render() {}\n");
        let s = Server::start(dir.path());
        Fixture { dir, s, folder: false }
    }

    // The same files in a plain folder, as the working tree above has them. `truncated` caps the listing one file
    // short, at a file deeper than every other, so all the fixtures are still listed but the in-process search
    // (grep_listed) runs instead of git grep --no-index.
    fn folder(truncated: bool) -> Fixture {
        let dir = TempDir::new("def-folder");
        let files = files();
        for (p, text) in &files {
            write(dir.path(), p, text);
        }
        write(dir.path(), "src/a.js", "// moved\n\nexport function render() {}\n");
        let mut env = Vec::new();
        let cap = files.len().to_string();
        if truncated {
            write(dir.path(), &format!("{}padding.js", "deep/".repeat(20)), "export function render() {}\n");
            env.push(("REVUE_TEST_MAX_FILES", cap.as_str()));
        }
        let s = Server::try_start(dir.path(), &[], &env).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr));
        assert_eq!(s.body("/api/tree", &[])["truncated"], truncated);
        Fixture { dir, s, folder: true }
    }

    // The revisions a case can click at.
    fn revs(&self) -> &'static [Option<&'static str>] {
        if self.folder {
            &[None]
        } else {
            &[Some("HEAD"), None]
        }
    }

    // GET /api/def on the new side of a view: rev None is the working tree, Some("HEAD") that commit.
    fn definitions(&self, rev: Option<&str>, params: &[(&str, &str)]) -> Result<Value, String> {
        let mut p: Vec<(&str, &str)> = match rev {
            None => vec![("mode", "working")],
            Some(r) => vec![("mode", "commit"), ("ref", r)],
        };
        p.extend_from_slice(params);
        let (status, body) = self.s.json("/api/def", &p);
        if status != 200 {
            return Err(format!("status {status}: {body}"));
        }
        Ok(body)
    }

    // Click on `name` inside the first line of `from` that contains `needle`.
    fn click(&self, from: &str, needle: &str, name: &str, rev: Option<&str>) -> Result<Value, String> {
        let text = match rev {
            None => read(self.dir.path(), from),
            Some(r) => git(self.dir.path(), &["show", &format!("{r}:{from}")]),
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let i = lines.iter().position(|l| l.contains(needle)).ok_or(format!("needle not found: {needle}"))?;
        let at = lines[i].find(needle).unwrap();
        let byte = at + lines[i][at..].find(name).ok_or(format!("{name} not in {needle}"))?;
        // The page counts columns in UTF-16 units (JavaScript string indexes), so send it the same way.
        let col = lines[i][..byte].encode_utf16().count();
        let (line, col) = ((i + 1).to_string(), col.to_string());
        self.definitions(rev, &[("side", "new"), ("name", name), ("from", from), ("line", &line), ("col", &col)])
    }
}

fn expect_jump(res: &Value, file: &str, line_text: &str) -> Result<(), String> {
    let jump = &res["jump"];
    if jump.is_null() {
        let picks: Vec<String> = res["candidates"].as_array().unwrap().iter().map(|c| format!("{}:{} {}", c["path"].as_str().unwrap(), c["line"], c["score"])).collect();
        return Err(format!("expected a direct jump, got a picker: {picks:?}"));
    }
    if jump["path"] != file {
        return Err(format!("jumped to {}, expected {file}", jump["path"]));
    }
    let text = jump["text"].as_str().unwrap();
    if !text.contains(line_text) {
        return Err(format!("landed on \"{text}\", expected \"{line_text}\""));
    }
    Ok(())
}

fn jump_line(res: &Value, want: u64) -> Result<(), String> {
    match res["jump"]["line"].as_u64() {
        Some(l) if l == want => Ok(()),
        other => Err(format!("jumped to line {other:?}, expected {want}")),
    }
}

type Case = (&'static str, fn(&Fixture) -> Result<(), String>);

const CASES: &[Case] = &[
    ("js: named import picks the imported file", |f| expect_jump(&f.click("src/use.js", "render();", "render", None)?, "src/a.js", "function render")),
    ("js: namespace import", |f| expect_jump(&f.click("src/use.js", "B.render", "render", None)?, "src/b.js", "function render")),
    ("js: default import", |f| expect_jump(&f.click("src/use.js", "helper();", "helper", None)?, "src/lib/index.js", "export default function helper")),
    ("js: aliased import through a re-export", |f| expect_jump(&f.click("src/use.js", "t();", "t", None)?, "src/lib/tools.ts", "function tool")),
    ("js: require then member", |f| expect_jump(&f.click("src/use.js", "util.fmt", "fmt", None)?, "src/util.js", "function fmt")),
    ("js: this.method stays in the enclosing class", |f| {
        let res = f.click("src/use.js", "this.draw", "draw", None)?;
        expect_jump(&res, "src/use.js", "draw()")?;
        jump_line(&res, 7)
    }),
    ("js: local variable shadows the top-level one", |f| expect_jump(&f.click("src/use.js", "return value + 1", "value", None)?, "src/use.js", "const value = 1")),
    ("js: top-level use sees the top-level variable", |f| expect_jump(&f.click("src/use.js", "total = value", "value", None)?, "src/use.js", "const value = 2")),
    ("js: comments inside a multi-line import", |f| {
        expect_jump(&f.click("src/multi.js", "tool();", "tool", None)?, "src/other/tools.js", "function tool")?;
        expect_jump(&f.click("src/multi.js", "helper();", "helper", None)?, "src/other/tools.js", "function helper")
    }),
    ("js: comments inside a multi-line re-export", |f| expect_jump(&f.click("src/use_re.js", "fmt();", "fmt", None)?, "src/other/fmt.js", "function fmt")),
    ("js: works at an older revision", |f| {
        if f.folder {
            return Ok(());
        }
        let head = f.click("src/use.js", "render();", "render", Some("HEAD"))?;
        expect_jump(&head, "src/a.js", "function render")?;
        jump_line(&head, 1)?;
        jump_line(&f.click("src/use.js", "render();", "render", None)?, 3)
    }),
    ("py: from-import through a package __init__", |f| expect_jump(&f.click("main.py", "run()", "run", None)?, "pkg/core.py", "def run")),
    ("py: aliased module import", |f| expect_jump(&f.click("main.py", "alt.run", "run", None)?, "pkg/alt.py", "def run")),
    ("py: Class.method", |f| expect_jump(&f.click("main.py", "Engine.start", "start", None)?, "pkg/core.py", "def start")),
    ("py: self.method stays in the class", |f| expect_jump(&f.click("pkg/core.py", "self.start", "start", None)?, "pkg/core.py", "def start")),
    ("py: self.attr jumps to the class attribute, not a local or keyword argument", |f| {
        let res = f.click("cfg/config.py", "self.timeout", "timeout", None)?;
        expect_jump(&res, "cfg/config.py", "timeout = 5")?;
        let here: Vec<u64> = res["candidates"].as_array().unwrap().iter().filter(|c| c["path"] == "cfg/config.py").map(|c| c["line"].as_u64().unwrap()).collect();
        if here != [2] {
            return Err(format!("cfg/config.py candidates on lines {here:?}, expected [2]"));
        }
        Ok(())
    }),
    ("py: annotated class attribute", |f| expect_jump(&f.click("cfg/config.py", "self.label", "label", None)?, "cfg/config.py", "label: str")),
    ("py: Cls.attr through an import", |f| expect_jump(&f.click("cfg/use.py", "Config.retries", "retries", None)?, "cfg/config.py", "retries: int = 3")),
    ("a file name with a newline in it does not upset the others", |f| {
        // obj.zap() prefers a method, which only the owner check (reading wz/thing.py) can tell.
        for &rev in f.revs() {
            expect_jump(&f.click("wz/use.py", "obj.zap", "zap", rev)?, "wz/thing.py", "def zap(self)")?;
        }
        // The Go resolver reads every file of the package, the oddly named one included.
        for &rev in f.revs() {
            expect_jump(&f.click("gz/use.go", "zip()", "zip", rev)?, "gz/helper.go", "func zip")?;
        }
        Ok(())
    }),
    ("go: aliased package import", |f| expect_jump(&f.click("main.go", "srv.New", "New", None)?, "server/server.go", "func New")),
    ("go: package import", |f| expect_jump(&f.click("main.go", "client.New", "New", None)?, "client/client.go", "func New")),
    ("go: receiver method", |f| expect_jump(&f.click("server/server.go", "s.Start", "Start", None)?, "server/server.go", "func (s *Server) Start")),
    ("go: same package, other file", |f| expect_jump(&f.click("client/util.go", "helper()", "helper", None)?, "client/client.go", "func helper")),
    ("rs: Type::assoc through a use from another workspace crate", |f| {
        expect_jump(&f.click("app/src/main.rs", "Circle::new", "new", None)?, "core/src/shapes.rs", "pub fn new")
    }),
    ("rs: function re-exported with pub use", |f| expect_jump(&f.click("app/src/main.rs", "area(&c)", "area", None)?, "core/src/shapes.rs", "pub fn area(c")),
    ("rs: child module path", |f| expect_jump(&f.click("app/src/main.rs", "geo::area", "area", None)?, "app/src/geo.rs", "pub fn area")),
    ("rs: self.method picks the impl method", |f| expect_jump(&f.click("core/src/shapes.rs", "self.area()", "area", None)?, "core/src/shapes.rs", "pub fn area(&self")),
    ("rs: crate-qualified macro", |f| expect_jump(&f.click("app/src/main.rs", "square!", "square", None)?, "core/src/lib.rs", "macro_rules! square")),
    ("c: typedef NS_ENUM (a rule with a literal N in it)", |f| expect_jump(&f.click("ui/view.m", "Shade current", "Shade", None)?, "ui/shade.h", "NS_ENUM")),
    ("analysis cap: the imported file is analyzed even when it sorts past the cap", |f| {
        expect_jump(&f.click("use_gadget.py", "Gadget.spin", "spin", None)?, "zz/gadget.py", "def spin")
    }),
    ("columns are UTF-16 offsets, as the page sends them", |f| expect_jump(&f.click("src/emoji.js", "; foo();", "foo", None)?, "src/emoji.js", "function foo")),
    ("js: test-runner calls are not definitions", |f| {
        for name in ["describe", "it"] {
            let res = f.definitions(None, &[("name", name)])?;
            let found = res["candidates"].as_array().unwrap();
            if !found.is_empty() {
                return Err(format!("{name}: {} candidates: {}", found.len(), res["candidates"]));
            }
        }
        Ok(())
    }),
    ("clicking a definition's own name doesn't jump to it, or to a poor match", |f| {
        // function foo is the clicked line; the only other candidate, method A.foo, scores below zero.
        let res = f.click("src/emoji.js", "function foo", "foo", None)?;
        if !res["jump"].is_null() {
            return Err(format!("expected no jump, got {}", res["jump"]));
        }
        Ok(())
    }),
    ("js: generator method", |f| expect_jump(&f.click("src/gen.js", "this.gen", "gen", None)?, "src/gen.js", "*gen()")),
    ("ts: interface method signature", |f| {
        let res = f.click("src/shape.ts", "s.perimeter", "perimeter", None)?;
        expect_jump(&res, "src/shape.ts", "perimeter(scale: number): number;")?;
        jump_line(&res, 2)
    }),
    ("ts: method with parameters over several lines", |f| {
        let res = f.click("src/store.ts", "this.load", "load", None)?;
        expect_jump(&res, "src/store.ts", "load(")?;
        jump_line(&res, 2)
    }),
    ("ts: method returning an object type", |f| expect_jump(&f.click("src/store.ts", "this.shape", "shape", None)?, "src/store.ts", "shape(): { a: number } {")),
    ("ts: a call split over lines is not a method", |f| {
        let res = f.click("src/store.ts", "notify(3)", "notify", None)?;
        expect_jump(&res, "src/store.ts", "export function notify")?;
        let n = res["candidates"].as_array().unwrap().len();
        if n != 1 {
            return Err(format!("expected only the function, got {n} candidates: {}", res["candidates"]));
        }
        Ok(())
    }),
    ("go: grouped const and var declarations", |f| {
        let res = f.click("cmd/paint.go", "colors.Green", "Green", None)?;
        expect_jump(&res, "colors/colors.go", "Green")?;
        jump_line(&res, 7)?;
        expect_jump(&f.click("cmd/paint.go", "colors.Default", "Default", None)?, "colors/colors.go", "Default = Red")
    }),
    ("rs: enum variant", |f| expect_jump(&f.click("core/src/kinds.rs", "Shape::Circle", "Circle", None)?, "core/src/kinds.rs", "Circle,")),
    ("rs: struct field", |f| {
        let res = f.click("core/src/kinds.rs", "p.x", "x", None)?;
        expect_jump(&res, "core/src/kinds.rs", "pub x: f64")?;
        jump_line(&res, 7)
    }),
    ("py: tuple assignment", |f| expect_jump(&f.click("cfg/use_pair.py", "print(right)", "right", None)?, "cfg/pair.py", "left, right = 1, 2")),
    ("unrelated names still fall back to a picker", |f| {
        // Two top-level definitions of `tool` with nothing in the click site to tell them apart.
        let res = f.definitions(None, &[("name", "tool")])?;
        if !res["jump"].is_null() {
            return Err(format!("expected a picker, got a jump to {}", res["jump"]));
        }
        let n = res["candidates"].as_array().unwrap().len();
        if n < 2 {
            return Err(format!("only {n} candidates"));
        }
        Ok(())
    }),
];

fn run_cases(f: &Fixture) {
    let failures: Vec<String> = CASES.iter().filter_map(|(name, case)| case(f).err().map(|e| format!("{name}: {e}"))).collect();
    assert!(failures.is_empty(), "{} of {} cases failed:\n  {}", failures.len(), CASES.len(), failures.join("\n  "));
}

#[test]
fn every_click_lands_on_the_right_definition() {
    run_cases(&Fixture::new());
}

// A plain folder is searched with git grep --no-index, or in process once its listing was cut short; both land where
// a repo's working tree does, with the same candidates.
#[test]
fn every_click_lands_on_the_right_definition_in_a_plain_folder() {
    let (git, listed) = (Fixture::folder(false), Fixture::folder(true));
    run_cases(&git);
    run_cases(&listed);
    for name in ["render", "tool", "helper", "fmt", "draw", "value", "run", "start", "New", "area", "zap", "zip", "Circle", "x", "foo", "Shade"] {
        let a = git.definitions(None, &[("name", name)]).unwrap();
        let b = listed.definitions(None, &[("name", name)]).unwrap();
        assert_eq!(a, b, "{name}: git grep --no-index and the in-process search disagree");
        assert!(!a["candidates"].as_array().unwrap().is_empty(), "{name}: no candidates");
    }
}

// A plain folder with these files, listed in full or (`cap`) cut short.
fn folder_with(files: &[(&str, &str)], cap: Option<usize>) -> (TempDir, Server) {
    let dir = TempDir::new("def-folder-edge");
    for (p, text) in files {
        write(dir.path(), p, text);
    }
    let cap = cap.map(|c| c.to_string());
    let env: Vec<(&str, &str)> = cap.iter().map(|c| ("REVUE_TEST_MAX_FILES", c.as_str())).collect();
    let s = Server::try_start(dir.path(), &[], &env).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr));
    (dir, s)
}

fn candidates(s: &Server, name: &str) -> Vec<String> {
    let (status, body) = s.json("/api/def", &[("name", name)]);
    assert_eq!(status, 200, "{body}");
    body["candidates"].as_array().unwrap().iter().map(|c| format!("{}:{}", c["path"].as_str().unwrap(), c["line"])).collect()
}

// The in-process search matches whole words as git grep -w does, skips binary files and files over 2 MB, and keeps at
// most 200 lines per file (so a definition after 200 uses isn't seen); git grep --no-index gives the same, apart from
// the large file, which git reads.
#[test]
fn the_in_process_search_matches_words_as_git_grep_does() {
    let many = format!("{}function foo() {{}}\n", "foo();\n".repeat(200));
    let big = format!("function foo() {{}}\n{}", "x".repeat(2 * 1024 * 1024));
    let files = [
        ("a.js", "function foo_bar() {}\nfunction xfoo() {}\nfunction foo() {}\nconst $foo = 1, foo2 = 2;\n"),
        ("b.js", "function foo() {}\0\n"),
        ("c.js", many.as_str()),
        ("d.js", big.as_str()),
        ("e.js", "let foo = 1;\n"),
        ("deep/deeper/never.js", "function foo() {}\n"),
    ];
    let (_d, listed) = folder_with(&files, Some(5));
    let (_d2, git) = folder_with(&files[..5], None);
    let found = candidates(&listed, "foo");
    let want = ["a.js:3".to_string(), "e.js:1".to_string()];
    assert_eq!(found, want, "whole words only, no binary file, 200 lines of c.js, nothing past the listing or over 2 MB");
    let mut from_git = candidates(&git, "foo");
    assert!(from_git.contains(&"d.js:1".to_string()), "git grep reads files over 2 MB: {from_git:?}");
    from_git.retain(|c| c != "d.js:1");
    assert_eq!(from_git, want);
}

// Only the listing's files are candidates: never one the cut-short walk didn't reach, nor one git grep finds past a
// directory link the walk doesn't follow.
#[cfg(unix)]
#[test]
fn only_listed_files_are_candidates_in_a_plain_folder() {
    let (dir, listed) = folder_with(&[("a.js", "function foo() {}\n"), ("sub/b.js", "function foo() {}\n")], Some(1));
    assert_eq!(candidates(&listed, "foo"), ["a.js:1"]);
    let outside = TempDir::new("def-folder-outside");
    write(outside.path(), "c.js", "function foo() {}\n");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let (_d, git) = folder_with(&[("a.js", "function foo() {}\n")], None);
    std::os::unix::fs::symlink(outside.path(), _d.path().join("link")).unwrap();
    assert_eq!(candidates(&git, "foo"), ["a.js:1"]);
}

// Without git on the PATH, a folder cut short still resolves (it is searched in process), and a complete one finds
// nothing instead of failing.
#[cfg(unix)]
#[test]
fn a_plain_folder_without_git_still_answers() {
    let files = [("a.js", "function foo() {}\n"), ("sub/b.js", "function foo() {}\n")];
    let start = |cap: Option<&str>| {
        let dir = TempDir::new("def-nogit");
        for (p, text) in &files {
            write(dir.path(), p, text);
        }
        let mut env = vec![("PATH", "/nonexistent")];
        env.extend(cap.map(|c| ("REVUE_TEST_MAX_FILES", c)));
        let s = Server::try_start(dir.path(), &[], &env).unwrap_or_else(|e| panic!("revue exited with {:?}: {}", e.code, e.stderr));
        (dir, s)
    };
    let (_d, cut) = start(Some("1"));
    assert_eq!(candidates(&cut, "foo"), ["a.js:1"]);
    let (_d2, full) = start(None);
    assert_eq!(candidates(&full, "foo"), Vec::<String>::new());
}

// ---------------------------------------------------------------- inputs that used to crash or hang the server
// GET /api/def within `secs`, and the server still answers afterwards.
fn def_within(s: &Server, params: &[(&str, &str)], secs: u64) -> Result<Value, String> {
    let path = format!("/api/def?{}", qs(params));
    let port = s.port;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(request(port, "GET", &path, &[], b"").json());
    });
    let res = match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(v) => Ok(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("no answer within {secs} s")),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the request failed (did the server die?)".into()),
    };
    if std::net::TcpStream::connect(("127.0.0.1", s.port)).is_err() {
        return Err("the server is gone".into());
    }
    res
}

// A working tree with these files and no commits, and a server on it.
fn repo_with(files: &[(String, String)]) -> (TempDir, Server) {
    let dir = TempDir::new("def-edge");
    for (p, text) in files {
        write(dir.path(), p, text);
    }
    init(dir.path());
    let s = Server::start(dir.path());
    (dir, s)
}

#[test]
fn a_deeply_nested_rust_use_does_not_crash_the_server() {
    let deep = format!("fn foo() {{}}\nuse {}foo{};\n", "{".repeat(3000), "}".repeat(3000));
    let (_dir, s) = repo_with(&[
        ("Cargo.toml".into(), "[package]\nname = \"x\"\nversion = \"0.1.0\"\n".into()),
        ("src/main.rs".into(), "fn main() {}\n".into()),
        ("src/deep.rs".into(), deep),
    ]);
    let col = (4 + 3000).to_string();
    def_within(&s, &[("mode", "working"), ("name", "foo"), ("from", "src/deep.rs"), ("line", "2"), ("col", &col)], 20).unwrap();
}

#[test]
fn a_regex_at_its_backtrack_limit_does_not_hang() {
    let (_dir, s) = repo_with(&[("src/big.js".into(), format!("import {}\n", "a ".repeat(120_000)))]);
    def_within(&s, &[("mode", "working"), ("name", "a"), ("from", "src/big.js"), ("line", "1"), ("col", "7")], 20).unwrap();
}

// 2,000 lines that each name foo after 380 spaces. A debug build, which the tests run, takes about 1.3 s for them on a
// fast machine and several times that on a CI runner that is running the other tests too, so 5 s failed there now
// and then. The crawl this guards against (the C function rule trying every split of a long indent, on every line)
// took about two minutes on the same machine.
#[test]
fn long_indented_c_lines_stay_fast() {
    let line = format!("{}a b c d foo x;\n", " ".repeat(380));
    let files: Vec<(String, String)> = (0..20).map(|i| (format!("c/f{i}.c"), line.repeat(100))).collect();
    let (_dir, s) = repo_with(&files);
    def_within(&s, &[("mode", "working"), ("name", "foo")], 20).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_in_the_working_tree_does_not_hang() {
    // A tracked file replaced by a FIFO is still in the file list; reading it would block forever.
    let (dir, s) = repo_with(&[("src/x.js".into(), "foo();\n".into()), ("src/def.js".into(), "export function foo() {}\n".into())]);
    commit_all(dir.path(), "files");
    let x = dir.path().join("src/x.js");
    std::fs::remove_file(&x).unwrap();
    let c = std::ffi::CString::new(x.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    let res = def_within(&s, &[("mode", "working"), ("name", "foo"), ("from", "src/x.js"), ("line", "1"), ("col", "0")], 5).unwrap();
    assert_eq!(res["jump"]["path"], "src/def.js", "{res}");
}

#[test]
fn a_bare_go_name_in_a_big_package_stays_fast() {
    let filler: String = (0..1000).map(|i| format!("var v{i} = {i}\n")).collect();
    let mut files: Vec<(String, String)> = (0..200).map(|i| (format!("big/f{i:03}.go"), format!("package big\n\n{filler}"))).collect();
    files.push(("big/target.go".into(), "package big\n\nfunc target() {}\n".into()));
    files.push(("big/use.go".into(), "package big\n\nfunc use() {\n\ttarget()\n}\n".into()));
    let (_dir, s) = repo_with(&files);
    // Before the bound this took over 2 s in a debug build, rescanning every line of all 200 files.
    let res = def_within(&s, &[("mode", "working"), ("name", "target"), ("from", "big/use.go"), ("line", "4"), ("col", "1")], 1).unwrap();
    assert_eq!(res["jump"]["path"], "big/target.go", "{res}");
}
