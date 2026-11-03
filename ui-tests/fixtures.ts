// Shared by the UI specs: a throwaway git repo per test (`repo`), a throwaway folder outside any repo (`folder`, which
// revue opens as a plain folder) and revue servers on either (`revue`), isolated from
// the user's setup the same way tests/common/mod.rs isolates the Rust tests: git ignores the global and system
// config, each server gets its own XDG_RUNTIME_DIR (so it never shows up among the user's running servers), GIT_*
// and REVUE_* variables from the shell are dropped, REVUE_TEST=1 turns on GET /api/debug, and --idle-timeout 5 makes
// a server orphaned by a killed run exit on its own.
//
// Every test also fails (`guard`) on an uncaught page error, a Content-Security-Policy violation (reported on the
// console or as a securitypolicyviolation event), any other console error, an /api/ response of 500 or more, and
// when its server logged a panic or an internal error. A test that causes one of these on purpose opts out with
// test.use({ allowCspViolations: true }), allowConsoleErrors or allowServerErrors.
import { test as base, expect, type Locator, type Page, type Request } from '@playwright/test';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';

export { expect };

export const REPO_ROOT = path.resolve(__dirname, '..');
export const BINARY = path.join(process.env.CARGO_TARGET_DIR ?? path.join(REPO_ROOT, 'target'), 'debug', 'revue');

// The user's ~/.gitconfig (signing, hooks, default branch, diff settings) must not change what tests see.
export const GIT_ENV = { GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' };

// The parent environment without any GIT_* or REVUE_* variable, plus GIT_ENV. Git exports GIT_DIR, GIT_INDEX_FILE,
// GIT_WORK_TREE and more to the hooks it runs, so tests started from a hook would otherwise point the fixtures'
// `git add -A`/`commit` and every server at the user's own repo. REVUE_* settings (REVUE_NOTES and the like) would
// change how a server behaves.
export function cleanEnv(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [k, v] of Object.entries(process.env)) if (!/^(GIT|REVUE)_/.test(k)) env[k] = v;
  return { ...env, ...GIT_ENV };
}

function tempDir(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `revue-ui-${prefix}-`)));
}

// "line 1\n" … "line n\n", with line `edit` (0-based) changed to "edited <n>". Same as lines() in the Rust tests.
export function lines(n: number, edit?: number): string {
  let s = '';
  for (let i = 0; i < n; i++) s += i === edit ? `edited ${i + 1}\n` : `line ${i + 1}\n`;
  return s;
}

// EXIF for png() and jpeg(). Browsers draw an image turned as its orientation (1 to 8) says: 5 to 8 are a quarter turn,
// which swaps its width and height. One whose resolution in dots per inch isn't 72, with pixel dimensions of 72 / dpi
// of its size, rounded, they draw at that size, if it is a JPEG (Firefox a PNG too).
export type Exif = { orientation?: number, dpi?: number, pixels?: [number, number] };

// The TIFF structure an EXIF block holds, little endian, whose first directory has the orientation (tag 0x112, a
// SHORT), the resolution (XResolution and YResolution, 0x11A and 0x11B, RATIONALs written after the directory, and
// ResolutionUnit, 0x128, 2 for inches) and the pixel dimensions (PixelXDimension and PixelYDimension, 0xA002 and
// 0xA003, SHORTs), as many as are given, and no directory after it.
function exifTiff({ orientation, dpi, pixels }: Exif): Buffer {
  const entries: [number, number, number][] = []; // tag, type, value
  if (orientation) entries.push([0x112, 3, orientation]);
  if (dpi) entries.push([0x11A, 5, dpi], [0x11B, 5, dpi], [0x128, 3, 2]);
  if (pixels) entries.push([0xA002, 3, pixels[0]], [0xA003, 3, pixels[1]]);
  let ratio = 8 + 2 + 12 * entries.length + 4;
  const tiff = Buffer.alloc(ratio + 8 * entries.filter(([, type]) => type === 5).length);
  tiff.write('II', 0, 'latin1');
  tiff.writeUInt16LE(42, 2);
  tiff.writeUInt32LE(8, 4); // the directory's offset
  tiff.writeUInt16LE(entries.length, 8);
  entries.forEach(([tag, type, value], i) => {
    const at = 10 + 12 * i;
    tiff.writeUInt16LE(tag, at);
    tiff.writeUInt16LE(type, at + 2);
    tiff.writeUInt32LE(1, at + 4); // one value
    if (type === 3) tiff.writeUInt16LE(value, at + 8);
    else {
      tiff.writeUInt32LE(ratio, at + 8);
      tiff.writeUInt32LE(value, ratio);
      tiff.writeUInt32LE(1, ratio + 4);
      ratio += 8;
    }
  });
  return tiff;
}

// A real PNG, width × height pixels of one color (RGBA), that a browser decodes. With EXIF it has an eXIf chunk before
// its image data.
export function png(width: number, height: number, rgba: [number, number, number, number] = [0, 120, 215, 255],
  exif?: Exif): Buffer {
  const chunk = (type: string, data: Buffer) => {
    const body = Buffer.concat([Buffer.from(type, 'latin1'), data]);
    const head = Buffer.alloc(4), crc = Buffer.alloc(4);
    head.writeUInt32BE(data.length);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([head, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bits per channel
  ihdr[9] = 6; // RGBA
  const row = Buffer.alloc(1 + 4 * width); // filter type 0, then the pixels
  for (let i = 0; i < 4 * width; i++) row[1 + i] = rgba[i % 4];
  const pixels = zlib.deflateSync(Buffer.concat(Array<Buffer>(height).fill(row)));
  const signature = Buffer.from([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
  return Buffer.concat([signature, chunk('IHDR', ihdr), ...(exif ? [chunk('eXIf', exifTiff(exif))] : []),
    chunk('IDAT', pixels), chunk('IEND', Buffer.alloc(0))]);
}

// A real JPEG, width × height pixels of mid gray, that a browser decodes, with an EXIF segment (APP1: "Exif\0\0" and
// the TIFF structure) when given. It is a baseline JPEG of one component in which every 8 × 8 block is the same, so
// each is coded in two bits: a DC difference of 0 and the end of the block, each the one 1-bit code of its table.
export function jpeg(width: number, height: number, exif?: Exif): Buffer {
  const segment = (marker: number, data: Buffer) => {
    const head = Buffer.alloc(4);
    head.writeUInt16BE(0xFF00 | marker);
    head.writeUInt16BE(2 + data.length, 2);
    return Buffer.concat([head, data]);
  };
  // Precision, height, width, then one component: its id, sampling (1 × 1) and quantization table.
  const frame = Buffer.from([8, 0, 0, 0, 0, 1, 1, 0x11, 0]);
  frame.writeUInt16BE(height, 1);
  frame.writeUInt16BE(width, 3);
  // A Huffman table of DC (class 0) or AC (class 1) codes: how many there are of each length, 1 to 16, then their
  // values. The one value is 0, which is a DC difference of 0, and for AC the end of the block.
  const huffman = (cls: number) => Buffer.from([cls << 4, 1, ...Array<number>(15).fill(0), 0]);
  const blocks = Math.ceil(width / 8) * Math.ceil(height / 8);
  const scan = Buffer.alloc(Math.ceil(blocks / 4)); // 0 bits, the last byte filled out with 1s
  if (blocks % 4) scan[scan.length - 1] = 0xFF >> (2 * (blocks % 4));
  return Buffer.concat([
    Buffer.from([0xFF, 0xD8]),
    ...(exif ? [segment(0xE1, Buffer.concat([Buffer.from('Exif\0\0', 'latin1'), exifTiff(exif)]))] : []),
    segment(0xDB, Buffer.concat([Buffer.from([0]), Buffer.alloc(64, 1)])), // quantization table 0: all 1s
    segment(0xC0, frame),
    segment(0xC4, huffman(0)),
    segment(0xC4, huffman(1)),
    // One component, with Huffman tables 0 and 0, and every coefficient (0 to 63) in this scan.
    segment(0xDA, Buffer.from([1, 1, 0x00, 0, 63, 0])),
    scan,
    Buffer.from([0xFF, 0xD9]),
  ]);
}

// An image's natural size and the color of its top-left pixel, once it has loaded. A canvas can read it: the image is
// this origin's.
export const decodedImage = (img: Locator) => img.evaluate(async (el: HTMLImageElement) => {
  await el.decode();
  const c = document.createElement('canvas');
  c.width = c.height = 1;
  const ctx = c.getContext('2d')!;
  ctx.drawImage(el, 0, 0);
  return { size: [el.naturalWidth, el.naturalHeight], pixel: [...ctx.getImageData(0, 0, 1, 1).data] };
});

// Picks a theme in the header's theme menu.
export async function pickTheme(page: Page, name: 'Auto' | 'Light' | 'Dark') {
  await page.locator('#theme-btn').click();
  await page.getByRole('menuitemradio', { name: new RegExp(`^${name}`) }).click();
  await expect(page.locator('#theme-btn')).toHaveAttribute('aria-label', `Theme: ${name}`);
}

// CRC-32 as PNG uses it (zlib.crc32 needs Node 22).
const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(data: Buffer): number {
  let c = 0xFFFFFFFF;
  for (const b of data) c = CRC_TABLE[(c ^ b) & 0xFF] ^ (c >>> 8);
  return (c ^ 0xFFFFFFFF) >>> 0;
}

// ---------------------------------------------------------------- folders and repos
// A folder of files: the `folder` fixture (a temp dir outside any git repo, which revue opens as a plain folder), and
// the base of Repo.
export class Folder {
  readonly dir: string;

  constructor(dir: string) {
    this.dir = dir;
  }

  abs(rel: string): string {
    return path.join(this.dir, rel);
  }

  write(rel: string, data: string | Buffer): this {
    fs.mkdirSync(path.dirname(this.abs(rel)), { recursive: true });
    fs.writeFileSync(this.abs(rel), data);
    return this;
  }

  read(rel: string): string {
    return fs.readFileSync(this.abs(rel), 'utf8');
  }

  exists(rel: string): boolean {
    return fs.existsSync(this.abs(rel));
  }

  // Deletes from the working tree only (in a repo, an unstaged deletion; `git('rm', …)` stages one).
  rm(rel: string): this {
    fs.rmSync(this.abs(rel), { recursive: true, force: true });
    return this;
  }
}

export class Repo extends Folder {
  // Commits get increasing fixed dates, so history order and the dates shown are the same on every run.
  private commits = 0;

  git(...args: string[]): string {
    const when = `${1_700_000_000 + this.commits * 60} +0000`;
    try {
      return execFileSync('git', args, {
        cwd: this.dir,
        env: {
          ...cleanEnv(),
          GIT_AUTHOR_NAME: 'Test', GIT_AUTHOR_EMAIL: 'test@example.com', GIT_AUTHOR_DATE: when,
          GIT_COMMITTER_NAME: 'Test', GIT_COMMITTER_EMAIL: 'test@example.com', GIT_COMMITTER_DATE: when,
        },
        stdio: ['ignore', 'pipe', 'pipe'],
        encoding: 'utf8',
      });
    } catch (e: any) {
      throw new Error(`git ${args.join(' ')}: ${e.stderr || e.message}`);
    }
  }

  init(): this {
    this.git('init', '-q', '-b', 'main');
    return this;
  }

  // A staged rename (git mv), which Changes mode shows as R.
  rename(from: string, to: string): this {
    fs.mkdirSync(path.dirname(this.abs(to)), { recursive: true });
    this.git('mv', from, to);
    return this;
  }

  add(...paths: string[]): this {
    this.git('add', '--', ...(paths.length ? paths : ['.']));
    return this;
  }

  // Stages everything (add -A) and commits it; returns the new commit's full sha.
  commit(msg: string): string {
    this.git('add', '-A');
    return this.commitStaged(msg);
  }

  // Commits only what is staged (--allow-empty, so an empty commit works too).
  commitStaged(msg: string): string {
    this.commits++;
    this.git('commit', '-q', '--allow-empty', '-m', msg);
    return this.rev('HEAD');
  }

  // Creates a branch at `from` (default HEAD) without switching to it.
  branch(name: string, from = 'HEAD'): this {
    this.git('branch', name, from);
    return this;
  }

  switch(name: string, create = false): this {
    this.git('switch', '-q', ...(create ? ['-c'] : []), name);
    return this;
  }

  rev(ref: string): string {
    return this.git('rev-parse', ref).trim();
  }

  // The checked-out branch's name, or '' when HEAD is detached.
  currentBranch(): string {
    return this.git('branch', '--show-current').trim();
  }

  // Leaves the index with nothing for `git update-index --refresh` to rewrite. Otherwise the server's own index
  // refresh at startup (refresh_index) rewrites it, and that write is a live update a moment after the page loads,
  // racing whatever the test does next. revue.start() does this first.
  // The work tree's mtimes go an hour back, and the index is refreshed only once the clock is past the second their
  // ctimes (which can't be set) were changed in: git treats an entry whose file changed in the second the index was
  // written as racily clean, and writes the index again on the next refresh. So this can take up to a second.
  settle(): this {
    const past = new Date(Date.now() - 3_600_000);
    for (const f of this.git('ls-files', '-co', '--exclude-standard', '-z').split('\0').filter(Boolean)) {
      if (fs.lstatSync(this.abs(f), { throwIfNoEntry: false })?.isFile()) fs.utimesSync(this.abs(f), past, past);
    }
    const nextSecond = (Math.floor(Date.now() / 1000) + 1) * 1000;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, Math.max(0, nextSecond - Date.now()) + 10);
    this.git('update-index', '-q', '--refresh');
    return this;
  }
}

// ---------------------------------------------------------------- servers
export type Json = any;
export type Res = { status: number; body: Json };

export class Server {
  readonly port: number;
  readonly url: string;
  private stderrText: { s: string };

  // closed: resolves once the process has exited and both of its pipes are drained.
  constructor(readonly proc: ChildProcess, port: number, stderr: { s: string }, readonly runtimeDir: string, private closed: Promise<void>) {
    this.port = port;
    this.url = `http://127.0.0.1:${port}/`;
    this.stderrText = stderr;
  }

  stderr(): string {
    return this.stderrText.s;
  }

  get exited(): boolean {
    return this.proc.exitCode !== null || this.proc.signalCode !== null;
  }

  // Opens the page (with an optional hash, without the '#') and waits until the first view is drawn and the event
  // stream is connected, so live updates made after this are seen.
  async open(page: Page, hash = ''): Promise<void> {
    await page.goto(this.url + (hash ? `#${hash.replace(/^#/, '')}` : ''));
    await untilLive(page);
  }

  private query(p: string, params?: Record<string, string | number | boolean>): string {
    const q = params ? new URLSearchParams(Object.entries(params).map(([k, v]) => [k, String(v)])).toString() : '';
    return this.url + p.replace(/^\//, '') + (q ? `?${q}` : '');
  }

  async get(p: string, params?: Record<string, string | number | boolean>): Promise<Res> {
    const r = await fetch(this.query(p, params));
    const text = await r.text();
    let body: Json = text;
    try { body = JSON.parse(text); } catch {}
    return { status: r.status, body };
  }

  // GET, expecting 200, returning the JSON body.
  async json(p: string, params?: Record<string, string | number | boolean>): Promise<Json> {
    const r = await this.get(p, params);
    expect(r.status, `GET ${p}: ${JSON.stringify(r.body)}`).toBe(200);
    return r.body;
  }

  // A write: POST with the x-revue header the write endpoints require.
  async post(p: string, body: Json, params?: Record<string, string | number | boolean>): Promise<Res> {
    const r = await fetch(this.query(p, params), {
      method: 'POST',
      headers: { 'x-revue': '1', 'content-type': 'application/json' },
      body: typeof body === 'string' ? body : JSON.stringify(body),
    });
    const text = await r.text();
    let parsed: Json = text;
    try { parsed = JSON.parse(text); } catch {}
    return { status: r.status, body: parsed };
  }

  // Adds an agent note ({ file, line, endLine?, text, side?, mode?, ref?, base?, head? }); returns the note.
  async annotate(note: Json): Promise<Json> {
    const r = await this.post('/api/annotations', { action: 'add', ...note });
    expect(r.status, JSON.stringify(r.body)).toBe(200);
    return r.body.note;
  }

  // Sends a view to the tabs that follow navigation; returns the server's answer ({ view, tabs }).
  async navigate(view: Json): Promise<Json> {
    const r = await this.post('/api/navigate', view);
    expect(r.status, JSON.stringify(r.body)).toBe(200);
    return r.body;
  }

  // GET /api/debug (debug builds with REVUE_TEST=1): watched directories, file reads, ?watchError=.
  async debug(params?: Record<string, string>): Promise<Json> {
    return this.json('/api/debug', params);
  }

  // SIGTERM (so it exits normally and removes its discovery file), then SIGKILL if it hangs. Resolves once the
  // process is gone and its output is all read ('close', not 'exit'), so a panic printed on the way out is seen.
  async stop(): Promise<void> {
    if (this.exited) return this.closed;
    this.proc.kill('SIGTERM');
    const timer = setTimeout(() => this.proc.kill('SIGKILL'), 5_000);
    await this.closed;
    clearTimeout(timer);
  }
}

export type StartOptions = { port?: number; args?: string[]; env?: Record<string, string> };

// Spawns `revue <dir> --no-open --port 0 --idle-timeout 5 …` and waits for the URL it prints.
export async function startServer(dir: string, opts: StartOptions = {}): Promise<Server> {
  const args = [dir, '--no-open', '--port', String(opts.port ?? 0), '--idle-timeout', '5', ...(opts.args ?? [])];
  const runtimeDir = tempDir('runtime');
  const proc = spawn(BINARY, args, {
    env: { ...cleanEnv(), REVUE_TEST: '1', XDG_RUNTIME_DIR: runtimeDir, ...opts.env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const closed = new Promise<void>(r => proc.once('close', () => r()));
  const stderr = { s: '' };
  // Keep draining both pipes for the server's whole life, so it never blocks on a full pipe.
  proc.stderr!.setEncoding('utf8').on('data', d => { stderr.s += d; });
  let out = '';
  const port = await new Promise<number>((resolve, reject) => {
    const timer = setTimeout(() => fail(`no URL within 15 s; stdout ${JSON.stringify(out)}`), 15_000);
    const fail = (why: string) => {
      clearTimeout(timer);
      proc.kill('SIGKILL');
      reject(new Error(`revue ${args.join(' ')}: ${why}\n${stderr.s}`));
    };
    proc.on('error', e => fail(e.message));
    proc.on('exit', code => fail(`exited with ${code}`));
    proc.stdout!.setEncoding('utf8').on('data', d => {
      out += d;
      const m = out.match(/http:\/\/127\.0\.0\.1:(\d+)/);
      if (m) {
        clearTimeout(timer);
        proc.removeAllListeners('exit');
        resolve(Number(m[1]));
      }
    });
  });
  return new Server(proc, port, stderr, runtimeDir, closed);
}

// The lines a server logged for requests that failed on its side (500s: `revue: <method> <target>: <error>`).
function internalErrors(stderr: string): string[] {
  return stderr.split('\n').filter(l => /^revue: (GET|POST|HEAD) /.test(l));
}

// ---------------------------------------------------------------- page helpers
// The modifier for Ctrl+click (go to definition): Cmd on macOS.
export const MOD = process.platform === 'darwin' ? 'Meta' : 'Control';

export const modeTab = (page: Page, name: 'Changes' | 'History' | 'Branch') =>
  page.getByRole('navigation', { name: 'Mode' }).getByRole('button', { name: new RegExp(`^${name}\\b`) });
export const fileTree = (page: Page) => page.getByRole('tree', { name: 'Files' });
export const commitList = (page: Page) => page.getByRole('listbox', { name: 'Commits' });
// A tree row's accessible name is its text plus its badge ("a.txt M"), which doesn't tell same-named files in
// different folders apart; these find a row by its full path instead.
export const treeRow = (page: Page, path: string) => page.locator(`#tree [role="treeitem"][data-path="${path}"]`);
export const folderRow = (page: Page, dir: string) => page.locator(`#tree [role="treeitem"][data-dir="${dir}"]`);
export const overviewTitle = (page: Page) => page.locator('#main .overview-head h2');
export const section = (page: Page, path: string) => page.locator(`#main .section[data-path="${path}"]`);
// The overview's section paths, in page order.
export const sectionPaths = (page: Page) =>
  page.locator('#main .section').evaluateAll(els => els.map(el => el.getAttribute('data-path')));
// The source cells of the lines a section's diff adds or removes, in either layout: the split layout marks the cell
// (td.src.add on the right, td.src.del on the left), the unified one marks the row (tr.add, tr.del).
export const addedLines = (sec: Locator) => sec.locator('td.src.add, tr.add > td.src');
export const removedLines = (sec: Locator) => sec.locator('td.src.del, tr.del > td.src');
// Escapes a string for use in a RegExp.
export const reEsc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
// The label above the main pane: the open file's path, or "All changes · N files" over the overview.
export const viewLabel = (page: Page) => page.locator('#view-label');
// The rows highlighted as the target lines (state.line/endLine), in the diff or the file view.
export const targetRows = (page: Page) => page.locator('#main tr.target');

// Waits until the first view is drawn (the title names the repo) and the event stream is connected, so live updates
// made after this are seen.
async function untilLive(page: Page) {
  await expect(page.locator('#live-label')).toHaveText('Live');
  await expect(page).toHaveTitle(/^revue · /);
}

// Reloads the page and waits for it the way Server.open does.
export async function reload(page: Page) {
  await page.reload();
  await untilLive(page);
}

// The URL of every request the page makes from now on whose path (no query) matches `pathRe`, e.g.
// `recordRequests(page, /^\/api\/diffs$/)`. The array fills in as the requests go out.
export function recordRequests(page: Page, pathRe: RegExp): string[] {
  const urls: string[] = [];
  page.on('request', r => { if (pathRe.test(new URL(r.url()).pathname)) urls.push(r.url()); });
  return urls;
}

// Holds the first request the page makes whose URL `match` picks until release() is called. `held` resolves once the
// page has sent it, and answered() once its answer is in (after the release).
// With `answer: true` the request goes on to the server at once and its answer is held instead: `held` resolves once
// the server has answered, and the page gets that answer, as it was then, on release(). That is how an answer that
// is out of date by the time it arrives is made (a request held before the server is answered with what is current).
export async function hold(page: Page, match: (u: URL) => boolean, { answer = false } = {}) {
  let release!: () => void;
  const gate = new Promise<void>(r => { release = r; });
  let reached!: (r: Request) => void;
  const request = new Promise<Request>(r => { reached = r; });
  let holding = true;
  await page.route(match, async route => {
    if (!holding) return route.continue();
    holding = false;
    const response = answer ? await route.fetch() : null;
    reached(route.request());
    await gate;
    await (response ? route.fulfill({ response }) : route.continue());
  });
  return { held: request.then(() => {}), release, answered: () => request.then(r => r.response()) };
}

// The view in the URL's hash (writeHash) as a plain object, e.g. { mode: 'branch', base: 'main', file: 'a.txt' }.
export function hashParams(page: Page): Record<string, string> {
  return Object.fromEntries(new URLSearchParams(new URL(page.url()).hash.slice(1)));
}

// Tab into the file tree from the "Changed only" checkbox, the control just before it; focus lands on its tab stop.
export async function tabIntoTree(page: Page) {
  await page.getByRole('checkbox', { name: 'Changed only' }).focus();
  await page.keyboard.press('Tab');
}

// Tab into the commit list (History) from the "all branches" checkbox, the control just before it.
export async function tabIntoCommits(page: Page) {
  await page.getByRole('checkbox', { name: 'all branches' }).focus();
  await page.keyboard.press('Tab');
}

// Shortcuts are ignored while an input has focus.
export async function focusMain(page: Page) {
  await page.locator('#main').focus();
}

// The middle of the first whole-word `word` in a code cell, in page coordinates. The page finds the identifier from the
// caret position under the pointer, so a click or hover has to land on the word's text.
export async function wordCenter(cell: Locator, word: string): Promise<{ x: number; y: number }> {
  await cell.scrollIntoViewIfNeeded();
  return cell.evaluate((td, word) => {
    const start = td.textContent!.search(new RegExp(`\\b${word}\\b`));
    if (start < 0) throw new Error(`${word} is not in "${td.textContent}"`);
    const range = document.createRange();
    const walker = document.createTreeWalker(td, NodeFilter.SHOW_TEXT);
    let pos = 0, n: Node | null, started = false;
    while ((n = walker.nextNode())) {
      const len = (n as Text).data.length;
      if (!started && start < pos + len) { range.setStart(n, start - pos); started = true; }
      if (started && start + word.length <= pos + len) { range.setEnd(n, start + word.length - pos); break; }
      pos += len;
    }
    const r = range.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }, word);
}

// Clicks the middle of the first whole-word `word` in a code cell, with Ctrl/Cmd held unless `modifier` is false.
export async function modClickWord(page: Page, cell: Locator, word: string, modifier = true) {
  const at = await wordCenter(cell, word);
  if (modifier) await page.keyboard.down(MOD);
  await page.mouse.click(at.x, at.y);
  if (modifier) await page.keyboard.up(MOD);
}

// Opens a second event stream in the page that records the data of every unnamed message the server sends, and
// returns a reader for them. The page's own stream gets the same messages, so once this one has a message, the page
// has had it too.
export async function recordEvents(page: Page): Promise<() => Promise<string[]>> {
  await page.evaluate(() => new Promise<void>(resolve => {
    const w = window as any;
    w.__messages = [];
    const es = new EventSource('/api/events');
    es.onmessage = e => w.__messages.push(e.data);
    es.onopen = () => resolve();
  }));
  return () => page.evaluate(() => (window as any).__messages as string[]);
}

// ---------------------------------------------------------------- fixtures
type Fixtures = {
  // An empty repo on branch `main`, in its own temp dir (removed afterwards).
  repo: Repo;
  // An empty folder outside any git repo, in its own temp dir (removed afterwards), which revue opens as a plain
  // folder: `revue.start({ folder })`.
  folder: Folder;
  // Starts servers on `repo`: `await revue.start()` once the repo has its content (it runs repo.settle() first), or
  // with `folder` on that folder instead. Every server it started is stopped after the test, and the test fails if one
  // logged a panic or, unless allowServerErrors is set, an internal error.
  revue: { start(opts?: StartOptions & { folder?: Folder }): Promise<Server> };
  // Set to true (test.use) in a test that causes CSP violations on purpose.
  allowCspViolations: boolean;
  // Set to true (test.use) in a test that makes the page log console errors on purpose.
  allowConsoleErrors: boolean;
  // Set to true (test.use) in a test that makes the server fail a request (a 5xx answer, which it also logs) on
  // purpose, or fakes such an answer with page.route.
  allowServerErrors: boolean;
  guard: void;
};

// Console errors that are no fault of the page, all from Chromium's network log (Firefox logs none of them):
// ERR_INCOMPLETE_CHUNKED_ENCODING for the event stream (a chunked response that never ends) when a page is reloaded
// or closed, ERR_CONNECTION_REFUSED for each request to a server a test has stopped, and "Failed to load resource:
// the server responded with a status of 4xx" for the server's answers to bad input (a base that doesn't exist, a
// refused switch), which the page handles.
const HARMLESS_CONSOLE_ERRORS = [
  /net::ERR_INCOMPLETE_CHUNKED_ENCODING/,
  /net::ERR_CONNECTION_REFUSED/,
  /^Failed to load resource: the server responded with a status of 4\d\d /,
];
// The same log for a failed request (500 and up): a server error, so allowServerErrors excuses it.
const SERVER_ERROR_MESSAGE = /^Failed to load resource: the server responded with a status of 5\d\d /;
const CSP_MESSAGE = /Content[- ]Security[- ]Policy/i;

export const test = base.extend<Fixtures>({
  repo: async ({}, use) => {
    const dir = tempDir('repo');
    await use(new Repo(dir).init());
    fs.rmSync(dir, { recursive: true, force: true });
  },

  folder: async ({}, use) => {
    const dir = tempDir('folder');
    await use(new Folder(dir));
    fs.rmSync(dir, { recursive: true, force: true });
  },

  revue: async ({ repo, allowServerErrors }, use) => {
    const servers: Server[] = [];
    await use({
      async start({ folder, ...opts } = {}) {
        if (!folder) repo.settle();
        const s = await startServer(folder ? folder.dir : repo.dir, opts);
        servers.push(s);
        return s;
      },
    });
    await Promise.all(servers.map(s => s.stop()));
    for (const s of servers) fs.rmSync(s.runtimeDir, { recursive: true, force: true });
    for (const s of servers) {
      expect(s.stderr(), 'the server panicked').not.toMatch(/panicked/);
      if (!allowServerErrors) expect(internalErrors(s.stderr()), 'the server failed requests').toEqual([]);
    }
  },

  allowCspViolations: [false, { option: true }],
  allowConsoleErrors: [false, { option: true }],
  allowServerErrors: [false, { option: true }],

  // Watches every page of the test's context: uncaught exceptions, console errors, CSP violations (both the console
  // message and the securitypolicyviolation event, which doesn't depend on a browser's wording), and /api/ answers of
  // 500 or more. Firefox reports neither for markup parsed into DOMPurify's inert document; Chromium reports both.
  guard: [async ({ context, allowCspViolations, allowConsoleErrors, allowServerErrors }, use) => {
    const problems: string[] = [];
    context.on('weberror', e => problems.push(`uncaught: ${e.error().message}`));
    context.on('console', m => {
      if (m.type() !== 'error') return;
      const text = m.text(), at = m.location().url;
      const what = at ? `${text} (at ${at})` : text;
      if (CSP_MESSAGE.test(text)) {
        if (!allowCspViolations) problems.push(`CSP: ${what}`);
      } else if (SERVER_ERROR_MESSAGE.test(text)) {
        if (!allowServerErrors) problems.push(`console.error: ${what}`);
      } else if (!allowConsoleErrors && !HARMLESS_CONSOLE_ERRORS.some(re => re.test(text))) {
        problems.push(`console.error: ${what}`);
      }
    });
    await context.exposeBinding('__revueGuardCsp', (_source, what: string) => {
      if (!allowCspViolations) problems.push(`CSP event: ${what}`);
    });
    await context.addInitScript(() => {
      // Capturing on window sees the event first, wherever in the page (shadow roots included) it was fired.
      addEventListener('securitypolicyviolation', e => {
        (window as any).__revueGuardCsp(`${e.violatedDirective} ${e.blockedURI} at ${e.sourceFile}:${e.lineNumber}`);
      }, true);
    });
    context.on('response', r => {
      if (!allowServerErrors && r.status() >= 500 && new URL(r.url()).pathname.startsWith('/api/')) {
        problems.push(`HTTP ${r.status()}: ${r.request().method()} ${r.url()}`);
      }
    });
    await use();
    expect(problems, 'page errors, console errors, CSP violations or server errors').toEqual([]);
  }, { auto: true }],
});
