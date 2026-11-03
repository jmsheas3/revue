'use strict';
// revue client: file tree, diff renderer, file viewer, live updates. Vanilla JS, no build step.

const $ = (s, el = document) => el.querySelector(s);
const h = (tag, attrs = {}, ...kids) => {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'style') el.style.cssText = v; // through the CSSOM: the CSP blocks style attributes
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) el.append(kid);
  return el;
};
const esc = (s) => s.replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const STATUS_LABEL = { M: 'modified', A: 'added', D: 'deleted', R: 'renamed', C: 'copied', T: 'type changed', '?': 'untracked' };
const badgeChar = (st) => (st === '?' ? 'U' : st);

// Per-origin (so per server port) browser state, like which tab follows agent navigation.
const store = {
  get(k, d) { try { return localStorage.getItem('revue.' + k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem('revue.' + k, v); } catch {} },
  del(k) { try { localStorage.removeItem('revue.' + k); } catch {} },
};

// User preferences live in cookies, which are scoped by host but not port, so every revue server on
// 127.0.0.1 shares them. A value saved in localStorage before prefs moved to cookies is moved over on first read.
// The first read of each pref on a page load rewrites its cookie, so the one-year expiry counts from the last visit.
const renewedPrefs = new Set();
const readCookie = (k) => {
  try {
    const m = document.cookie.match(new RegExp(`(?:^|; )revue\\.${k}=([^;]*)`));
    return m ? decodeURIComponent(m[1]) : null;
  } catch { return null; }
};
const prefs = {
  get(k, d) {
    const v = readCookie(k);
    if (v !== null) {
      if (!renewedPrefs.has(k)) { renewedPrefs.add(k); prefs.set(k, v); }
      return v;
    }
    const old = store.get(k, null);
    if (old === null) return d;
    prefs.set(k, old);
    if (readCookie(k) === old) store.del(k); // only once the cookie took, so blocked cookies don't lose the value
    return old;
  },
  set(k, v) { try { document.cookie = `revue.${k}=${encodeURIComponent(v)}; path=/; max-age=31536000; samesite=strict`; } catch {} },
};

// Any app on 127.0.0.1 can write these cookies, so a value outside the known choices falls back to the default.
function prefChoice(k, choices, d) {
  const v = prefs.get(k, d);
  return choices.includes(v) ? v : d;
}
function readPrefs() {
  return {
    layout: prefChoice('layout', ['split', 'unified'], 'split'),
    theme: prefChoice('theme', ['auto', 'light', 'dark'], 'auto'),
    context: prefChoice('context', ['3', '10', '50', 'all'], '3'),
    changedOnly: prefs.get('changedOnly', '0') === '1',
    wrap: prefChoice('wrap', ['0', '1'], '1') === '1',
  };
}

const state = {
  info: null,
  mode: 'working', ref: null, base: null, head: 'HEAD',
  tree: { files: [], changes: {} },
  file: null,              // selected path, or null for the "all changes" overview
  view: 'diff',            // 'diff' | 'file' | 'preview' (rendered markdown, or an SVG drawn: hasPreview)
  fileSide: 'new',
  line: null, endLine: null, // target line range, highlighted and scrolled to (go to definition, agents)
  notes: [],               // agent notes for the current range (/api/annotations)
  ...readPrefs(),          // layout, theme, context, changedOnly, wrap
  narrow: false,           // #main too narrow for split diffs (see diffLayout)
  filter: '',
  expanded: new Set(),
  collapsed: new Set(),     // folders closed by hand; "Changed only" and filters show the rest open
  seenChanges: new Set(), seenRange: null, // changed paths already auto-expanded, for this range
  commits: [], allBranches: false,
};

// ---------------------------------------------------------------- view intent
// The intent is the view last asked for. Each request for another view, by the user, an agent or the URL (setMode,
// applyView, selectCommit, a base pick, a branch picked in Branch mode), makes a new one before its first await, and an
// intent is replaced, never reused. What its first draw still has to do waits on it: the view to show once its tree is
// in (`select`), closing the open file (`resetFile`), drawing everything again (`force`, the r key), and a scroll
// position to restore (`restoreScroll`, Back). See refresh. Data refreshes of the view shown (live updates, r, notes
// events, Load more) and file-level changes (a file pick, j/k, Esc, f, layout, context) work within the current intent.
// `settled`: state holds the intent's whole range (see setMode and applyView); until it does, a live update only notes
// that it is `due` (run, reloadAll), and notes that changed that they are (`notesDue`, loadNotes); settle() loads them.
// `drawn`: a refresh has drawn it. `shown`: a render of it is on the page (renderMain), so the rows there are its own:
// until then notes aren't put on them, and Clear all, which clears the notes of the view asked for, is disabled.
// `drawer`: the narrow windows' drawer was open when the view was asked for (Back, a link, an agent), so showing it
// closes the drawer (refresh); one opened while the view loads, as the page's first view does, stays open.
let intent;
function newIntent(o = {}) {
  // Read from the page itself: the first intent is made before drawerOpen is defined.
  const drawer = document.body.classList.contains('drawer-open');
  intent = { settled: true, due: null, notesDue: false, drawn: false, shown: false, select: null, resetFile: false,
    force: false, restoreScroll: null, drawer, seq: {}, ...o };
  syncClearAll();
  return intent;
}
function markShown(it) {
  it.shown = true;
  if (it === intent) syncClearAll();
}
newIntent();
// Orders the answers of one kind of request (`name`) within the intent. fresh(): nothing newer has started (renders,
// the commit list). apply(): nothing newer has been applied (data: the tree, notes, info), and marks this one applied,
// so a newer request that fails never strands an older answer. Both are false once the intent is replaced, except for
// what belongs to the page rather than a view ({ page: true }: info, the commit list). { peek } takes no number of its
// own: fresh() then says whether anything started since.
const pageSeq = {};
function ticket(name, { page = false, peek = false } = {}) {
  const it = intent, r = (page ? pageSeq : it.seq)[name] ??= { started: 0, applied: 0 };
  const n = peek ? r.started : ++r.started, live = () => page || it === intent;
  return { fresh: () => live() && r.started === n, apply: () => live() && n > r.applied && (r.applied = n, true) };
}

// ---------------------------------------------------------------- api
function rangeParams() {
  const p = { mode: state.mode };
  if (state.mode === 'commit' && state.ref) p.ref = state.ref;
  if (state.mode === 'branch') { if (state.base) p.base = state.base; p.head = state.head; }
  return p;
}
// params: an object (its keys replace range params), or [key, value] pairs / URLSearchParams for repeated keys.
// signal: an AbortSignal that cancels the request. Nothing passes one yet: an answer for a view the page left is ignored
// rather than cancelled (see the intent), and promises shared by several callers must never get one.
// range: the view's range params to ask with, when not the one in state (an overview section's, kept from when it was
// drawn, for a request made after an await).
async function api(pathname, params = {}, signal = undefined, range = rangeParams()) {
  const q = new URLSearchParams(range);
  if (Array.isArray(params) || params instanceof URLSearchParams) for (const [k, v] of params) q.append(k, v);
  else for (const [k, v] of Object.entries(params)) q.set(k, v);
  const res = await fetch(pathname + '?' + q, { signal });
  let data;
  try { data = await res.json(); } catch { throw new Error(`HTTP ${res.status}${res.statusText ? ' ' + res.statusText : ''} from ${pathname}`); }
  if (data && data.error) throw new Error(data.error);
  if (!res.ok) throw new Error(`HTTP ${res.status} from ${pathname}`);
  return data;
}

// ---------------------------------------------------------------- url hash state
function writeHash() {
  // A plain folder has one mode, so its links name none (and a link from a repo's view still opens in it).
  const p = new URLSearchParams(state.mode === 'folder' ? {} : { mode: state.mode });
  if (state.mode === 'commit' && state.ref) p.set('ref', state.ref);
  if (state.mode === 'branch' && state.base) p.set('base', state.base);
  if (state.head !== 'HEAD') p.set('head', state.head);
  if (state.file) p.set('file', state.file);
  if (state.file && state.view !== 'diff') p.set('view', state.view);
  if (state.file && state.fileSide === 'old') p.set('side', 'old');
  if (state.file && state.line) p.set('line', state.line);
  if (state.file && state.line && state.endLine) p.set('endLine', state.endLine);
  history.replaceState(history.state, '', '#' + p);
}
// A view: what the hash holds, and what agents send through /api/navigate.
function parseView(p) {
  const line = parseInt(p.get('line')) || null;
  return {
    mode: ['working', 'commit', 'branch'].includes(p.get('mode')) ? p.get('mode') : 'working',
    ref: p.get('ref'), base: p.get('base'), head: p.get('head') || 'HEAD',
    file: p.get('file'), view: ['file', 'preview'].includes(p.get('view')) ? p.get('view') : 'diff',
    side: p.get('side') === 'old' ? 'old' : 'new',
    line, endLine: line && parseInt(p.get('endLine')) > line ? parseInt(p.get('endLine')) : null,
  };
}
const hashView = () => parseView(new URLSearchParams(location.hash.slice(1)));
// A plain folder (/api/info's `git` is false) has one view of its files on disk: whatever mode, commit or branch a view
// names (an old link, Back to an entry from before, an agent) opens the same file there, on its one side.
const isFolder = () => state.info?.git === false;
const folderView = (v) => ({ ...v, mode: 'folder', ref: null, base: null, head: 'HEAD', side: 'new' });

// Show a view: switch mode or branch if needed, and the file once the view's tree is in. `scroll` restores a position
// (Back). A branch comparison without a base compares with the default one, as setMode does on a mode change; while the
// page loads that waits for /api/info, which names it (the intent is unsettled until then). Nothing is drawn before
// info is in either way (refresh).
// The view is a new intent's `select` either way (see refresh), so it draws only what came last: a newer view, mode
// change, file pick or commit pick replaces it, and a live update whose load overtakes this one's draws it.
async function applyView(v, scroll = null) {
  closePicker();
  if (isFolder()) v = folderView(v);
  const prevMode = state.mode, prevHead = state.head;
  state.mode = v.mode; state.ref = v.ref ?? null; state.head = v.head || 'HEAD';
  state.base = v.base ?? (v.mode === 'branch' ? state.info?.defaultBase ?? null : null);
  // The file is shown as part of the mode's first render, so the overview isn't drawn (and fetched) first.
  if (state.mode !== prevMode || state.head !== prevHead) return setMode(state.mode, { keepRef: true, select: v, scroll });
  const it = newIntent({ select: v, restoreScroll: scroll, settled: !(state.mode === 'branch' && !state.base && !state.info) });
  if (!it.settled) {
    await infoLoaded;
    if (it !== intent) return;
    state.base = state.info.defaultBase;
    settle(it);
  }
  const tree = Promise.all([loadTree(), state.mode === 'commit' && resolveRefCommit()]).then(([loaded]) => {
    if (it !== intent) return false;
    if (state.mode === 'commit' && !markCommit()) renderCommits();
    return loaded;
  });
  await refresh({ tree });
}

// ---------------------------------------------------------------- highlighting
function langFor(path) {
  if (!window.hljs) return null;
  const base = path.split('/').pop();
  const special = { Dockerfile: 'dockerfile', Makefile: 'makefile', 'CMakeLists.txt': 'cmake', '.bashrc': 'bash', '.zshrc': 'bash' };
  if (special[base]) return special[base];
  const ext = base.includes('.') ? base.split('.').pop().toLowerCase() : '';
  const map = { mjs: 'javascript', cjs: 'javascript', jsx: 'javascript', tsx: 'typescript', vue: 'xml', svelte: 'xml', svg: 'xml', toml: 'ini', lock: null, txt: null };
  const lang = ext in map ? map[ext] : ext;
  return lang && hljs.getLanguage(lang) ? lang : null;
}
// Highlight a block of text and split it into per-line HTML, re-opening spans that cross newlines.
function highlightLines(text, lang) {
  if (!lang || text.length > 1_000_000) return text.split('\n').map(esc);
  let html;
  try { html = hljs.highlight(text, { language: lang, ignoreIllegals: true }).value; } catch { return text.split('\n').map(esc); }
  const out = []; const stack = []; let cur = '';
  const re = /(<span[^>]*>)|(<\/span>)|(\n)|([^<\n]+)/g; let m;
  while ((m = re.exec(html))) {
    if (m[1]) { stack.push(m[1]); cur += m[1]; }
    else if (m[2]) { stack.pop(); cur += m[2]; }
    else if (m[3]) { cur += '</span>'.repeat(stack.length); out.push(cur); cur = stack.join(''); }
    else cur += m[4];
  }
  out.push(cur);
  return out;
}
// Wrap the character ranges [start, end) (sorted, not overlapping) of a line's highlighted HTML in <mark>, across
// syntax spans: each run of text inside a range gets its own <mark>, inside whatever span holds it. Offsets count
// characters of the line's text, so an entity (&amp;) is one character and is never cut.
function markHtml(html, ranges) {
  if (!ranges.length) return html;
  let out = '', pos = 0, ri = 0, m;
  const re = /(<[^>]*>)|([^<]+)/g;
  while ((m = re.exec(html))) {
    if (m[1] || ri >= ranges.length) { out += m[0]; continue; }
    const text = m[2], units = text.includes('&') ? text.match(/&[^;]*;|[^&]/g) : null;
    const len = units ? units.length : text.length, end = pos + len;
    if (ranges[ri][0] >= end) { out += text; pos = end; continue; }
    const cut = (a, b) => (units ? units.slice(a, b).join('') : text.slice(a, b));
    let i = 0;
    while (i < len) {
      const r = ranges[ri];
      if (!r || r[0] >= end) { out += cut(i, len); break; }
      const s = Math.max(r[0], pos + i) - pos, e = Math.min(r[1], end) - pos;
      out += cut(i, s) + '<mark>' + cut(s, e) + '</mark>';
      i = e;
      if (r[1] <= end) ri++;
    }
    pos = end;
  }
  return out;
}
// Intraline marks: what changed inside a removed line and the added line it pairs with, word by word.
// Only the middle of a pair (past the tokens both lines start and end with) is compared token by token; a middle over
// MARK_TOKENS tokens is marked whole instead. Blocks with more than MARK_PAIRS candidate pairs pair by position.
const MARK_TOKENS = 200, MARK_PAIRS = 400;
const lineTokens = (s) => s.match(/[\p{L}\p{N}_]+|\s+|[^\p{L}\p{N}_\s]/gu) || [];
const solidLen = (s) => s.replace(/\s/g, '').length;
// A line's tokens and how often each non-whitespace one occurs, worked out once per line of a change block.
function lineInfo(text) {
  const tokens = lineTokens(text), counts = new Map();
  for (const t of tokens) if (t.trim()) counts.set(t, (counts.get(t) || 0) + 1);
  return { text, tokens, counts, solid: solidLen(text) };
}
// A lower bound on how much of x's non-whitespace text no comparison with y can match: only equal tokens match, so
// every copy of a token beyond what y has stays changed. Cheap, and it rules out most unrelated pairs.
function unmatchedAtLeast(x, y) {
  let n = 0;
  for (const [t, c] of x.counts) { const d = c - (y.counts.get(t) || 0); if (d > 0) n += d * t.length; }
  return n;
}
// [start, end) without the whitespace at its ends, as a list of zero or one ranges.
function trimmedRange(text, s, e) {
  while (s < e && /\s/.test(text[s])) s++;
  while (e > s && /\s/.test(text[e - 1])) e--;
  return s < e ? [[s, e]] : [];
}
// The changed character ranges of lines x and y (from lineInfo), from a longest common subsequence of their tokens.
// Ranges only whitespace apart are merged, so "foo bar" is one mark, not two, and whitespace alone (a re-indent) is
// never marked.
function changedRanges(x, y) {
  const a = x.text, b = y.text, X = x.tokens, Y = y.tokens;
  let p = 0, s = 0, start = 0, tail = 0;
  while (p < X.length && p < Y.length && X[p] === Y[p]) start += X[p++].length;
  while (s < X.length - p && s < Y.length - p && X[X.length - 1 - s] === Y[Y.length - 1 - s]) tail += X[X.length - 1 - s++].length;
  const n = X.length - p - s, m = Y.length - p - s;
  if (n > MARK_TOKENS || m > MARK_TOKENS) return { a: trimmedRange(a, start, a.length - tail), b: trimmedRange(b, start, b.length - tail) };
  const w = m + 1, L = new Uint16Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--)
    L[i * w + j] = X[p + i] === Y[p + j] ? L[(i + 1) * w + j + 1] + 1 : Math.max(L[(i + 1) * w + j], L[i * w + j + 1]);
  const ra = [], rb = [];
  const add = (r, text, s, e) => {
    if (!text.slice(s, e).trim()) return;
    const last = r[r.length - 1];
    if (last && !text.slice(last[1], s).trim()) last[1] = e; else r.push([s, e]);
  };
  let i = 0, j = 0, pa = start, pb = start;
  while (i < n || j < m) {
    if (i < n && j < m && X[p + i] === Y[p + j]) { pa += X[p + i++].length; pb += Y[p + j++].length; }
    else if (j < m && (i === n || L[i * w + j + 1] >= L[(i + 1) * w + j])) { const s = pb; pb += Y[p + j++].length; add(rb, b, s, pb); }
    else { const s = pa; pa += X[p + i++].length; add(ra, a, s, pa); }
  }
  return { a: ra, b: rb };
}

// ---------------------------------------------------------------- diff parsing
function parseDiff(text) {
  const lines = text.split('\n');
  const file = { binary: false, hunks: [], meta: [] };
  let hunk = null, oldNo = 0, newNo = 0;
  for (const line of lines) {
    const hm = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)/.exec(line);
    if (hm) {
      oldNo = +hm[1]; newNo = +hm[3];
      hunk = { header: line, ctx: hm[5].trim(), oldStart: oldNo, oldCount: hm[2] == null ? 1 : +hm[2], newStart: newNo, lines: [] };
      file.hunks.push(hunk);
      continue;
    }
    if (!hunk) {
      if (/^Binary files/.test(line)) file.binary = true;
      if (/^(new file|deleted file|old mode|new mode|similarity|rename|copy)/.test(line)) file.meta.push(line);
      continue;
    }
    const c = line[0];
    if (c === ' ') hunk.lines.push({ t: 'ctx', text: line.slice(1), o: oldNo++, n: newNo++ });
    else if (c === '-') hunk.lines.push({ t: 'del', text: line.slice(1), o: oldNo++ });
    else if (c === '+') hunk.lines.push({ t: 'add', text: line.slice(1), n: newNo++ });
    else if (c === '\\') hunk.lines.push({ t: 'noeol', text: line.slice(2) });
  }
  return file;
}

// Highlight each hunk's old and new side as a block so multi-line constructs color correctly.
function highlightHunk(hunk, lang) {
  const oldL = hunk.lines.filter(l => l.t === 'ctx' || l.t === 'del');
  const newL = hunk.lines.filter(l => l.t === 'ctx' || l.t === 'add');
  const oh = highlightLines(oldL.map(l => l.text).join('\n'), lang);
  const nh = highlightLines(newL.map(l => l.text).join('\n'), lang);
  oldL.forEach((l, i) => { l.oh = oh[i]; });
  newL.forEach((l, i) => { l.nh = nh[i]; });
  for (const l of hunk.lines) l.html = l.t === 'del' ? l.oh : l.t === 'noeol' ? esc(l.text) : l.nh;
}

// Group a hunk into rows: context lines, and change blocks of paired deletions/additions.
function hunkBlocks(hunk) {
  const blocks = []; let i = 0; const L = hunk.lines;
  while (i < L.length) {
    if (L[i].t === 'del' || L[i].t === 'add') {
      const dels = [], adds = [];
      while (i < L.length && L[i].t === 'del') dels.push(L[i++]);
      while (i < L.length && L[i].t === 'add') adds.push(L[i++]);
      blocks.push({ kind: 'change', dels, adds });
    } else blocks.push({ kind: 'line', line: L[i++] });
  }
  return blocks;
}

// Unchanged lines between hunk i and the one before it (or the top of the file), counted on the old side.
// An empty range ("-5,0") sits after its start line.
function hiddenBefore(hunks, i) {
  const first = (hk) => hk.oldCount ? hk.oldStart : hk.oldStart + 1;
  const prevEnd = i ? first(hunks[i - 1]) + hunks[i - 1].oldCount - 1 : 0;
  return first(hunks[i]) - prevEnd - 1;
}

// The changed part of a line is marked only while it is a small part of it: past about half the line's
// non-whitespace text the mark just repaints the row.
const MARK_MAX = 0.5;
const changedLen = (text, ranges) => ranges.reduce((n, [s, e]) => n + solidLen(text.slice(s, e)), 0);
function worthMarking(text, ranges) {
  const solid = solidLen(text);
  return solid > 0 && changedLen(text, ranges) <= solid * MARK_MAX;
}
// What changed in a removed and an added line (from lineInfo) and how alike they are (score, 0 to 1), or null when
// they are too different to mark. Most such pairs are ruled out before their tokens are compared.
function markPair(d, a) {
  if (unmatchedAtLeast(d, a) > d.solid * MARK_MAX || unmatchedAtLeast(a, d) > a.solid * MARK_MAX) return null;
  const r = changedRanges(d, a);
  if (!worthMarking(d.text, r.a) || !worthMarking(a.text, r.b)) return null;
  return { ...r, score: 1 - (changedLen(d.text, r.a) + changedLen(a.text, r.b)) / (d.solid + a.solid) };
}
// Pair a change block's removed lines with its added lines, keeping their order and taking the pairing with the
// most likeness overall, so a line inserted among edited ones doesn't shift every pair after it.
// Returns [delIndex, addIndex, ranges] for each pair worth marking.
function pairLines(dels, adds) {
  const n = dels.length, m = adds.length;
  if (n * m > MARK_PAIRS) {
    const out = [];
    for (let i = 0; i < Math.min(n, m); i++) { const p = markPair(lineInfo(dels[i].text), lineInfo(adds[i].text)); if (p) out.push([i, i, p]); }
    return out;
  }
  const A = adds.map(l => lineInfo(l.text));
  const pair = dels.map(l => { const d = lineInfo(l.text); return A.map(a => markPair(d, a)); });
  const best = Array.from({ length: n + 1 }, () => new Float64Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--)
    best[i][j] = Math.max(best[i + 1][j], best[i][j + 1], pair[i][j] ? pair[i][j].score + best[i + 1][j + 1] : 0);
  const out = []; let i = 0, j = 0;
  while (i < n && j < m) {
    if (pair[i][j] && best[i][j] === pair[i][j].score + best[i + 1][j + 1]) { out.push([i, j, pair[i][j]]); i++; j++; }
    else if (best[i][j] === best[i + 1][j]) i++;
    else j++;
  }
  return out;
}

// Work a parsed diff out once for either layout: each line's highlighted HTML with its intraline marks, and each
// hunk's context lines and change blocks with their pairs. Done in place, once; the diff cache keeps the result, so
// a layout switch only builds the table again.
function prepareDiff(parsed, path) {
  if (parsed.prepared) return parsed;
  const lang = langFor(path);
  for (const hunk of parsed.hunks) {
    highlightHunk(hunk, lang);
    hunk.blocks = hunkBlocks(hunk);
    for (const b of hunk.blocks) {
      if (b.kind !== 'change') continue;
      b.pairs = b.dels.length && b.adds.length ? pairLines(b.dels, b.adds) : [];
      for (const [i, j, r] of b.pairs) {
        b.dels[i].html = markHtml(b.dels[i].html, r.a);
        b.adds[j].html = markHtml(b.adds[j].html, r.b);
      }
    }
  }
  parsed.prepared = true;
  return parsed;
}

// Built as one HTML string and parsed once, which is several times faster than a node per cell. Repo text goes in
// only escaped (line HTML comes from highlightLines, which escapes it); line numbers go in as they are.
function renderDiffTable(parsed, path, layout) {
  prepareDiff(parsed, path);
  const split = layout === 'split', cols = split ? 6 : 4;
  const table = h('table', { class: 'code ' + (split ? 'split' : 'unified'), 'data-path': path });
  const src = (html, cls) => `<td class="src ${cls || ''}">${html || ''}</td>`;
  const num = (n, cls) => `<td class="num ${cls || ''}">${n == null ? '' : n}</td>`;
  const sign = (s, cls) => `<td class="sign ${cls || ''}">${s}</td>`;
  // data-o/data-n carry the old/new line numbers so targets and notes can find their rows.
  const tr = (cls, o, n) => `<tr${cls ? ` class="${cls}"` : ''}${o == null ? '' : ` data-o="${o}"`}${n == null ? '' : ` data-n="${n}"`}>`;
  const out = [split
    ? '<colgroup><col class="num"><col class="sign"><col><col class="num"><col class="sign"><col></colgroup><tbody>'
    : '<colgroup><col class="num"><col class="num"><col class="sign"><col></colgroup><tbody>'];
  parsed.hunks.forEach((hunk, hi) => {
    if (hi > 0) out.push(`<tr class="gap"><td colspan="${cols}"></td></tr>`);
    const hidden = hiddenBefore(parsed.hunks, hi);
    // The header's text is in a div of its own, which stays in view when no-wrap lines scroll sideways.
    out.push(`<tr class="hunk"><td colspan="${cols}" title="${esc(hunk.header)}"><div class="hunk-line">`,
      hidden > 0 ? `<span class="hunk-gap">⇕ ${hidden} line${hidden > 1 ? 's' : ''}</span>` : '',
      `<span class="hunk-range">−${hunk.oldStart} +${hunk.newStart}</span>`,
      hunk.ctx ? `<span class="hunk-ctx">${esc(hunk.ctx)}</span>` : '', '</div></td></tr>');
    for (const b of hunk.blocks) {
      if (b.kind === 'line') {
        const l = b.line;
        if (l.t === 'noeol') { out.push(`<tr class="noeol"><td colspan="${cols}">↳ ${esc(l.text)}</td></tr>`); continue; }
        out.push(tr(null, l.o, l.n), split
          ? num(l.o) + sign(' ') + src(l.oh) + num(l.n) + sign(' ') + src(l.nh)
          : num(l.o) + num(l.n) + sign(' ') + src(l.html), '</tr>');
        continue;
      }
      if (split) {
        // Paired lines share a row, so a mark's partner sits beside it. The lines between two pairs fill rows side by
        // side, with an empty cell where one side has fewer.
        const row = (i, j) => {
          const d = b.dels[i], a = b.adds[j];
          out.push(tr(null, d?.o, a?.n),
            num(d?.o, d ? 'del' : 'empty'), sign(d ? '-' : '', d ? 'del' : 'empty'), d ? src(d.html, 'del') : src('', 'empty'),
            num(a?.n, a ? 'add' : 'empty'), sign(a ? '+' : '', a ? 'add' : 'empty'), a ? src(a.html, 'add') : src('', 'empty'), '</tr>');
        };
        let i = 0, j = 0;
        for (const [pi, pj] of [...b.pairs, [b.dels.length, b.adds.length]]) {
          while (i < pi || j < pj) row(i < pi ? i++ : -1, j < pj ? j++ : -1);
          if (pi < b.dels.length) row(i++, j++);
        }
      } else {
        for (const d of b.dels) out.push(tr('del', d.o), num(d.o), num(null), sign('-'), src(d.html), '</tr>');
        for (const a of b.adds) out.push(tr('add', null, a.n), num(null), num(a.n), sign('+'), src(a.html), '</tr>');
      }
    }
  });
  out.push('</tbody>');
  table.innerHTML = out.join('');
  return table;
}

// ---------------------------------------------------------------- file tree
// "Changed only" (a preference shared by every server) doesn't apply to a plain folder, which has no changes.
const changedOnly = () => state.changedOnly && state.mode !== 'folder';
function buildTree(files = state.tree.files, onlyChanged = changedOnly(), filter = state.filter) {
  const root = { name: '', path: '', dir: true, children: new Map(), changed: 0 };
  const { changes } = state.tree;
  const f = filter.toLowerCase();
  for (const p of files) {
    const ch = changes[p];
    if (onlyChanged && !ch) continue;
    if (f && !p.toLowerCase().includes(f)) continue;
    const parts = p.split('/');
    let node = root;
    for (let i = 0; i < parts.length - 1; i++) {
      let next = node.children.get(parts[i]);
      // The folder's path is its parent's plus its name, built only when the folder is first seen.
      if (!next) node.children.set(parts[i], next = { name: parts[i], path: node.path ? node.path + '/' + parts[i] : parts[i], dir: true, children: new Map(), changed: 0 });
      node = next;
      if (ch) node.changed++;
    }
    node.children.set(parts.at(-1), { name: parts.at(-1), path: p, dir: false, change: ch });
  }
  // Collapse single-child directory chains ("src/main/java") like most editors do.
  const compact = (node) => {
    for (const [k, c] of node.children) {
      if (!c.dir) continue;
      let cur = c;
      while (cur.children.size === 1) {
        const only = [...cur.children.values()][0];
        if (!only.dir) break;
        cur = { ...only, name: cur.name + '/' + only.name };
      }
      node.children.set(k, cur);
      compact(cur);
    }
  };
  compact(root);
  return root;
}

const collator = new Intl.Collator(undefined, { numeric: true }); // much faster than localeCompare with options

// Derived from state.tree once per tree, not on every lookup: with thousands of files, a scan per link
// or per keypress adds up.
const perTree = (name, make) => {
  const c = perTree.cache[name];
  if (c?.tree === state.tree) return c.value;
  return (perTree.cache[name] = { tree: state.tree, value: make() }).value;
};
perTree.cache = {};
const fileSet = () => perTree('files', () => new Set(state.tree.files));
// Paths that have notes, so the tree counts notes only on those rows.
const noteFiles = () => (noteFiles.cache?.notes === state.notes ? noteFiles.cache.value
  : (noteFiles.cache = { notes: state.notes, value: new Set(state.notes.map(n => n.file)) }).value);
function sortedKids(node) {
  return [...node.children.values()].sort((a, b) => (b.dir - a.dir) || collator.compare(a.name, b.name));
}

// Changed files in tree order. The tree is built from the changed paths only (every change is in
// files), so the order, folder compaction included, is the "Changed only" tree's. Sorted like
// files, so names the collator ties ("a01", "a1") keep the same order too.
function orderedChangedFiles() {
  return perTree('ordered', () => {
    const out = [];
    const walk = (node) => { for (const c of sortedKids(node)) c.dir ? walk(c) : c.change && out.push(c.path); };
    walk(buildTree(Object.keys(state.tree.changes).sort(), true, ''));
    return out;
  });
}

// Every file in tree order: what j/k step through in a plain folder, which has no changes.
function orderedFiles() {
  return perTree('orderedAll', () => {
    const out = [];
    const walk = (node) => { for (const c of sortedKids(node)) c.dir ? walk(c) : out.push(c.path); };
    walk(buildTree(state.tree.files, false, ''));
    return out;
  });
}

// A filter shows at most FILTER_ROWS rows: one letter can match nearly every file of a big repo, and building tens of
// thousands of rows freezes the page for no use. The rest is counted in a last row.
const FILTER_ROWS = 2000;
function renderTree() {
  state.treeDirty = false;
  const el = $('#tree');
  const focused = document.activeElement?.closest?.('#tree .node');
  const focusKey = focused ? nodeKey(focused) : null;
  el.textContent = '';
  const root = buildTree();
  const forceOpen = !!state.filter || changedOnly();
  // Unchanged files and folders are muted, except in a plain folder, where nothing changed and every file is the point.
  const dim = state.mode === 'folder' ? '' : ' dim';
  const cap = state.filter ? FILTER_ROWS : Infinity;
  let rows = 0, more = 0;
  // Clicks are handled once for the whole tree (wireTree), so rows carry no listeners.
  const walk = (node, depth, into) => {
    for (const c of sortedKids(node)) {
      // "Changed only" and filters show folders open unless closed by hand.
      const open = c.dir && (forceOpen ? !state.collapsed.has(c.path) : state.expanded.has(c.path));
      if (rows >= cap) {
        // Past the cap only the matching files are counted.
        if (!c.dir) more++;
        else if (open) walk(c, depth + 1, into);
        continue;
      }
      rows++;
      const pad = { style: `padding-left:${8 + depth * 14}px` };
      if (c.dir) {
        into.append(h('div', {
          class: 'node dir' + (c.changed ? '' : dim), ...pad, role: 'treeitem', tabindex: '-1',
          'aria-expanded': open ? 'true' : 'false', 'aria-level': depth + 1, 'data-dir': c.path,
        }, h('span', { class: 'twisty' }, open ? '▾' : '▸'), h('span', { class: 'name' }, c.name),
        c.changed ? h('span', { class: 'dot', title: `${c.changed} changed` }) : null));
        if (open) walk(c, depth + 1, into);
      } else {
        const st = c.change?.status;
        const notes = noteFiles().has(c.path) || c.change?.oldPath ? notesFor(c.path, c.change).length : 0;
        into.append(h('div', {
          class: 'node file' + (st ? ' st-' + st : dim) + (state.file === c.path ? ' sel' : ''), ...pad, role: 'treeitem', tabindex: '-1',
          'aria-selected': state.file === c.path ? 'true' : 'false', 'aria-level': depth + 1, 'data-path': c.path, title: c.path + (st ? ` (${STATUS_LABEL[st]})` : ''),
        }, h('span', { class: 'twisty' }), h('span', { class: 'name' }, c.name),
        notes ? h('span', { class: 'note-count', title: 'Notes left by agents on this file' }, `${notes} note${notes > 1 ? 's' : ''}`) : null,
        st && changedOnly() ? statText(c.change, 'count') : null,
        h('span', { class: 'badge' }, st ? badgeChar(st) : '')));
      }
    }
  };
  walk(root, 0, el);
  if (more) el.append(h('div', { class: 'placeholder tree-more' }, `${more} more matching file${more > 1 ? 's' : ''}: refine the filter`));
  // A plain folder's listing stops at the server's cap (folder::max_files): say so, rather than look complete.
  const count = state.tree.files.length.toLocaleString('en-US');
  if (state.tree.truncated) el.append(h('div', { class: 'placeholder tree-more', title: 'The folder has more files than revue lists' }, `Showing first ${count} files`));
  // One row is in the tab order: the one that had focus, else the selected file, else the first row.
  const keep = focusKey && [...el.querySelectorAll('.node')].find(n => nodeKey(n) === focusKey);
  const stop = keep || el.querySelector('.node.sel') || el.querySelector('.node');
  if (stop) { stop.tabIndex = 0; if (keep) keep.focus({ preventScroll: true }); }
  if (!el.children.length) el.append(h('div', { class: 'placeholder' }, state.filter ? 'No matching files' : needsBase() ? 'Choose a base branch' : changedOnly() ? 'No changes' : 'Empty'));
  // The scroll position isn't written back: the rows were replaced without a layout in between, so it never moved,
  // and writing it would force a layout of the whole tree right away.
  if (state.mode === 'folder') {
    const files = state.tree.files.length;
    $('#summary').textContent = `${count}${state.tree.truncated ? '+' : ''} file${files === 1 ? '' : 's'}`;
    return;
  }
  const n = Object.keys(state.tree.changes).length;
  let adds = 0, dels = 0;
  for (const c of Object.values(state.tree.changes)) { adds += c.add || 0; dels += c.del || 0; }
  $('#summary').innerHTML = n ? `${n} file${n > 1 ? 's' : ''} <span class="stat"><span class="a">+${adds}</span> <span class="d">−${dels}</span></span>` : 'no changes';
}

// A binary change, as git's numstat gives one: no line counts at all. An untracked text file too large to count has
// add null too, but del 0: it is text (uncountedChange).
const binaryChange = c => c.add == null && c.del == null;
// An untracked file the server didn't count (add null, del 0): a text file over 2 MB, which it doesn't diff either
// (/api/diff says tooLarge, and the page "File too large to display"), or one it couldn't read. A nested repo with no
// commits (`dir/`, the only path that ends in a slash) has no count either, but has its diff, a header.
const uncountedChange = (path, c) => c.status === '?' && c.add == null && c.del === 0 && !path.endsWith('/');
// Changes whose diff has no text to draw, so context and layout don't apply to it.
const noTextDiff = (path, c) => binaryChange(c) || uncountedChange(path, c);
// A diff too big to draw with the rest: over 1500 changed lines, or one too large to count. Its overview section starts
// collapsed, and its diff is only fetched once it is opened.
const bigChange = (path, c) => (c.add || 0) + (c.del || 0) > 1500 || uncountedChange(path, c);

function statText(c, cls = 'stat') {
  if (!c || binaryChange(c)) return c && c.status !== 'D' ? h('span', { class: cls }, 'bin') : null;
  return h('span', { class: cls + ' stat' }, c.add ? h('span', { class: 'a' }, '+' + c.add) : null, ' ', c.del ? h('span', { class: 'd' }, '−' + c.del) : null);
}

// Returns whether a folder opened, i.e. whether the tree needs rebuilding to show the path.
function expandTo(path) {
  const parts = path.split('/');
  let opened = false;
  for (let i = 1; i < parts.length; i++) {
    const dir = parts.slice(0, i).join('/');
    if (!state.expanded.has(dir) || state.collapsed.has(dir)) opened = true;
    state.expanded.add(dir); state.collapsed.delete(dir);
  }
  return opened;
}

const nodeKey = (n) => n.dataset.path != null ? 'f:' + n.dataset.path : 'd:' + n.dataset.dir;

// One listener for every row (a filter can show thousands): a folder opens or closes, a file opens. In a plain folder,
// opening a folder (a click, Enter or Space) also shows its README, the way a folder's page does on a forge; a folder
// without one only opens. Closing one, and the arrow keys (listKeys, which only open and close), never change the view.
function onTreeClick(e) {
  const n = e.target.closest('.node');
  if (!n) return;
  const dir = n.dataset.dir;
  if (dir == null) return selectFile(n.dataset.path);
  const opening = n.getAttribute('aria-expanded') !== 'true';
  toggleDir(n);
  const readme = opening && state.mode === 'folder' ? readmeIn(dir) : null;
  // The drawer stays open: a folder is opened to go further into it.
  if (readme && readme !== state.file) selectFile(readme, { keepDrawer: true });
}
function toggleDir(n) {
  const dir = n.dataset.dir;
  if (n.getAttribute('aria-expanded') === 'true') { state.expanded.delete(dir); state.collapsed.add(dir); } else expandTo(dir + '/');
  renderTree();
}

// A folder's README in a plain folder, found in the tree already loaded: README.md, then README, then any readme.*
// (whatever its case, the first in tree order), then index.md. `dir` is '' for the root, which the page opens on.
function readmeIn(dir) {
  const prefix = dir ? dir + '/' : '', names = new Set(), other = [];
  for (const p of state.tree.files) {
    if (!p.startsWith(prefix) || p.includes('/', prefix.length)) continue;
    const name = p.slice(prefix.length);
    names.add(name);
    if (/^readme\./i.test(name)) other.push(name);
  }
  const pick = names.has('README.md') ? 'README.md' : names.has('README') ? 'README'
    : other.sort(collator.compare)[0] ?? (names.has('index.md') ? 'index.md' : null);
  return pick == null ? null : prefix + pick;
}

// Move the selection highlight to state.file without rebuilding the tree. False when its row isn't
// shown (filtered out, or in a folder that has to open first).
function markSelected() {
  const tree = $('#tree');
  for (const n of tree.querySelectorAll('.node.sel')) { n.classList.remove('sel'); n.setAttribute('aria-selected', 'false'); }
  if (!state.file) return true;
  const node = tree.querySelector(`.node[data-path="${CSS.escape(state.file)}"]`);
  if (!node) return false;
  node.classList.add('sel'); node.setAttribute('aria-selected', 'true');
  // Unless the keyboard is in the tree, Tab should land on the selected file.
  if (!tree.contains(document.activeElement)) rovingStop(tree, '.node', node);
  return true;
}

// Make `node` the one item of `list` in the tab order (a roving tabindex).
function rovingStop(list, sel, node) {
  for (const n of list.querySelectorAll(sel + '[tabindex="0"]')) n.tabIndex = -1;
  node.tabIndex = 0;
}

// Arrow keys move between the items of a list (tree rows, commits), Home/End jump to its ends, and
// Enter or Space activate the focused one. In the tree (the WAI-ARIA tree pattern), Right opens a folder and then
// moves into it, and Left closes an open folder or else moves up to the row's folder.
function listKeys(list, sel, e) {
  const item = e.target.closest(sel);
  if (!item || !list.contains(item)) return;
  const items = [...list.querySelectorAll(sel)];
  const i = items.indexOf(item);
  const go = (n) => { if (n) { rovingStop(list, sel, n); n.focus(); n.scrollIntoView({ block: 'nearest' }); } };
  const isDir = item.classList.contains('dir'), open = item.getAttribute('aria-expanded') === 'true';
  // The rows are flat: a row's folder is the nearest row above it one level up. Commits have no level.
  const level = +item.getAttribute('aria-level');
  if (e.key === 'ArrowDown') go(items[i + 1]);
  else if (e.key === 'ArrowUp') go(items[i - 1]);
  else if (e.key === 'Home') go(items[0]);
  else if (e.key === 'End') go(items.at(-1));
  else if (e.key === 'Enter' || e.key === ' ') item.click();
  else if (e.key === 'ArrowRight' && isDir) { if (open) go(items[i + 1]); else toggleDir(item); }
  else if (e.key === 'ArrowLeft' && isDir && open) toggleDir(item);
  else if (e.key === 'ArrowLeft' && level > 1) go(items.slice(0, i).findLast(n => +n.getAttribute('aria-level') === level - 1));
  else return;
  e.preventDefault();
}
// Expand the folders of changes that are new since the last refresh (all of them, and the selected
// file's, in a new range) so edits are visible without clicking. Folders the user closed stay closed.
// At most AUTO_EXPAND new changes open folders per refresh, so thousands of new files (an unignored
// node_modules) don't unfold the whole tree.
const AUTO_EXPAND = 200;
function expandChanged() {
  const range = [state.mode, state.ref, state.base, state.head].join('|');
  if (range !== state.seenRange) {
    state.seenRange = range;
    state.seenChanges = new Set();
    if (state.file) expandTo(state.file);
  }
  const now = Object.keys(state.tree.changes);
  let budget = AUTO_EXPAND;
  for (const p of now) if (!state.seenChanges.has(p) && budget-- > 0) expandTo(p);
  state.seenChanges = new Set(now);
}

// ---------------------------------------------------------------- diff layout
// The sizes the code tables are built on (--line-h, --num-w, --sign-w in style.css), in px, read once each.
const cssSizes = new Map();
function cssPx(name) {
  if (!cssSizes.has(name)) cssSizes.set(name, parseFloat(getComputedStyle(document.documentElement).getPropertyValue(name)) || 0);
  return cssSizes.get(name);
}

// Split needs about SPLIT_MIN_CH characters a side. When #main is narrower, diffs are unified without changing the
// saved preference, and the layout control says why. So they are while long lines don't wrap (w): a split row would
// put the new side past the old side's longest line, thousands of pixels to the right.
const SPLIT_MIN_CH = 60;
let chWidth = 0;
function isNarrow() {
  if (!chWidth) {
    const probe = h('span', { class: 'ch-probe' }, '0'.repeat(100));
    document.body.append(probe);
    chWidth = probe.getBoundingClientRect().width / 100 || 7.2;
    probe.remove();
  }
  const gutters = 2 * (cssPx('--num-w') + cssPx('--sign-w')); // a number and a sign column a side
  return ($('#main').clientWidth - gutters) / 2 / chWidth < SPLIT_MIN_CH;
}
// Why split can't be drawn now, if it can't: 'narrow', 'nowrap' or null.
const splitBlocked = () => (state.narrow ? 'narrow' : !state.wrap ? 'nowrap' : null);
const diffLayout = () => (splitBlocked() ? 'unified' : state.layout);

// While split is blocked, unified is forced and the saved preference stays as it is: Split only explains, and Unified
// (already drawn) does nothing.
function setLayout(layout) {
  const blocked = splitBlocked();
  if (blocked) {
    if (layout === 'split') toast(blocked === 'narrow' ? 'Too narrow for split: widen the window or the diff pane' : 'Split needs wrapped lines: press w to wrap them');
    return;
  }
  state.layout = layout; prefs.set('layout', layout); syncControls(); relayout();
}

// Draw the main pane again in another layout at the same place. The key changes with the layout, which would otherwise
// scroll to the top, and the two layouts differ in height, so in the overview the section at the top of the pane is
// put back where it was rather than the pixel offset (which drifted by several files per switch).
function relayout() {
  const main = $('#main'), top = () => main.getBoundingClientRect().top;
  const at = state.file ? null : [...main.querySelectorAll(':scope > .section')].find(s => s.getBoundingClientRect().bottom > top());
  const off = at ? at.getBoundingClientRect().top - top() : 0;
  intent.restoreScroll = main.scrollTop;
  return renderMain().then(() => {
    const sec = at && main.querySelector(`:scope > .section[data-path="${CSS.escape(at.dataset.path)}"]`);
    if (sec) main.scrollTop += sec.getBoundingClientRect().top - top() - off;
  });
}

// The controls follow the width at once, but the diffs are redrawn only once it has settled for NARROW_SETTLE ms, so
// dragging the sidebar or the window back and forth across the threshold doesn't reload every diff each time.
const NARROW_SETTLE = 200;
function wireNarrow() {
  state.narrow = isNarrow();
  let timer = 0;
  new ResizeObserver(() => {
    const narrow = isNarrow();
    if (narrow === state.narrow) return;
    state.narrow = narrow;
    syncControls();
    clearTimeout(timer);
    timer = setTimeout(() => {
      // Nothing to do when the layout ended where it was last drawn (or another render already drew it), or before
      // the page's first render has started (no key yet): that render draws the layout of the moment.
      const drawn = $('#main').dataset.key;
      if (drawn === undefined || drawn === key()) return;
      relayout();
    }, NARROW_SETTLE);
  }).observe($('#main'));
}

// w: wrap long lines (the default), or keep each line on one row and scroll each diff sideways (unified only, see
// splitBlocked).
const applyWrap = () => $('#main').classList.toggle('nowrap', !state.wrap);
function setWrap(wrap) {
  const before = diffLayout();
  state.wrap = wrap; prefs.set('wrap', wrap ? '1' : '0'); applyWrap(); syncControls();
  toast(wrap ? 'Wrapping long lines' : 'Long lines scroll sideways');
  if (diffLayout() !== before) relayout();
}

// ---------------------------------------------------------------- main panel
// The overview builds OVERVIEW_PAGE sections, then a button for the next page; a new view starts over.
const OVERVIEW_PAGE = 200;
let overviewLimit = OVERVIEW_PAGE;

// The path with its folder muted and the file name in bold.
function pathParts(path) {
  const i = path.lastIndexOf('/');
  return [i >= 0 ? h('span', { class: 'dir' }, path.slice(0, i + 1)) : null, h('span', { class: 'base' }, path.slice(i + 1))];
}

// before: controls ahead of the path (the collapse toggle, the back button); after: controls at the end.
function fileHead(path, change, before = [], after = []) {
  const st = change?.status;
  return h('div', { class: 'file-head' },
    ...before,
    h('span', { class: 'path', title: path + (st ? ` (${STATUS_LABEL[st] || st})` : ''), onclick: () => selectFile(path) },
      change?.oldPath ? h('span', { class: 'old' }, change.oldPath + ' → ') : null, ...pathParts(path)),
    statText(change),
    st ? h('span', { class: 'letter st-' + st, title: STATUS_LABEL[st] || st }, badgeChar(st)) : null,
    ...after);
}

// Diffs already parsed and highlighted, by request (range, path, the change's version tag and context), so switching
// the layout, going back to a diff or a live update that didn't touch it draws its table again without fetching or
// highlighting it. The tag moves whenever the diff could, so an entry never goes stale. The least recently used
// entries go once the diffs held add up to DIFF_CACHE_CHARS.
const DIFF_CACHE_CHARS = 8_000_000;
const diffCache = new Map(); // key -> { parsed, size }
let diffCacheChars = 0;
const diffKey = (path, change) => JSON.stringify([rangeParams(), path, change.oldPath || '', change.status, change.v || '', state.context]);
function cachedDiff(key) {
  const e = diffCache.get(key);
  if (!e) return null;
  diffCache.delete(key);
  diffCache.set(key, e); // most recently used last
  return e.parsed;
}
// diff: the text, or why there is none (diffOf).
function cacheDiff(key, diff) {
  const text = typeof diff === 'string' ? diff : JSON.stringify(diff);
  const parsed = typeof diff === 'string' ? parseDiff(diff) : { binary: false, hunks: [], meta: [], tooLarge: diff.size };
  parsed.text = text; // what it was parsed from, to compare with a refetch
  if (text.length > DIFF_CACHE_CHARS / 4) return parsed;
  const old = diffCache.get(key);
  if (old) { diffCache.delete(key); diffCacheChars -= old.size; }
  diffCache.set(key, { parsed, size: text.length });
  diffCacheChars += text.length;
  for (const [k, e] of diffCache) {
    if (diffCacheChars <= DIFF_CACHE_CHARS) break;
    diffCache.delete(k); diffCacheChars -= e.size;
  }
  return parsed;
}
// A file's diff as /api/diffs gives it: its text, or, for an untracked text file over 2 MB, which the server doesn't
// diff, {tooLarge: true, size}. diffOf takes /api/diff's answer ({diff}, or {diff: null, tooLarge, size}) to the same.
const diffOf = (res) => res.tooLarge ? { tooLarge: true, size: res.size } : res.diff;
const isDiff = (d) => typeof d === 'string' || d?.tooLarge === true;
// One file's diff, parsed, from the cache or the server.
async function fetchDiff(path, change) {
  const key = diffKey(path, change);
  const parsed = cachedDiff(key);
  if (parsed) return parsed;
  return cacheDiff(key, diffOf(await api('/api/diff', { path, old: change.oldPath || '', status: change.status, context: state.context })));
}
// git lists a repo nested in the work tree as one untracked `dir/`, the only path that ends in a slash. The server
// shows it as the commit git would add for it, so one with no diff has no commit yet, which git can't add.
function emptyDiffText(path, change) {
  return change?.status === '?' && path.endsWith('/') ? 'Nested git repository with no commits yet' : 'No textual changes';
}

// parsed: from parseDiff (cacheDiff), or the diff text. imgs: a binary change's images (changeImages), drawn in place
// of its placeholder; in an overview section they are capped in height.
function showDiff(container, path, change, parsed, imgs = null) {
  if (typeof parsed === 'string') parsed = parseDiff(parsed);
  container.textContent = '';
  if (parsed.binary && imgs) container.append(imageDiff(imgs, !!container.closest('.section')));
  else if (parsed.binary) container.append(h('div', { class: 'placeholder' }, 'Binary file changed'));
  else if (parsed.tooLarge != null) container.append(h('div', { class: 'placeholder' }, tooLargeText(parsed.tooLarge)));
  else if (!parsed.hunks.length) container.append(h('div', { class: 'placeholder' }, parsed.meta.join(' · ') || emptyDiffText(path, change)));
  else container.append(renderDiffTable(parsed, path, diffLayout()));
  addNotes(container, path, change, null, true);
  return parsed;
}

// ---------------------------------------------------------------- target lines and notes
// Each side's rows in a diff table or file view (both carry data-o/data-n) as [line, row] in line order, made once
// for looking up many line ranges: a scan of every row per note was slow on big files.
function rowIndex(scope) {
  const index = { old: [], new: [] };
  for (const r of scope.querySelectorAll('tr[data-o], tr[data-n]')) {
    const o = r.getAttribute('data-o'), n = r.getAttribute('data-n');
    if (o) index.old.push([+o, r]);
    if (n) index.new.push([+n, r]);
  }
  for (const list of [index.old, index.new]) {
    if (list.some((e, i) => i && e[0] < list[i - 1][0])) list.sort((a, b) => a[0] - b[0]); // stable: rows keep page order
  }
  return index;
}
// Rows showing lines [from, to] of one side.
function lineRows(scope, side, from, to = from, index = rowIndex(scope)) {
  const list = index[side === 'old' ? 'old' : 'new'];
  let lo = 0, hi = list.length;
  while (lo < hi) { const mid = (lo + hi) >> 1; if (list[mid][0] < from) lo = mid + 1; else hi = mid; }
  const rows = [];
  for (let i = lo; i < list.length && list[i][0] <= to; i++) rows.push(list[i][1]);
  return rows;
}

function markTarget(scope, side) {
  const rows = state.line ? lineRows(scope, side, state.line, state.endLine || state.line) : [];
  rows.forEach(r => r.classList.add('target'));
  return rows;
}

// How much of the top of #main the sticky file header over el covers, plus a little air. It is measured, not assumed:
// in narrow windows the header's controls wrap onto a second row.
function headClearance(el) {
  const head = el.closest('.section')?.querySelector('.file-head') || $('#main .file-head');
  return (head ? head.getBoundingClientRect().height : 0) + 8;
}
// Scroll #main so that el starts just below the sticky file header.
function scrollBelowHead(el) {
  const main = $('#main');
  main.scrollTop += el.getBoundingClientRect().top - main.getBoundingClientRect().top - headClearance(el);
}

// Center a range of rows in the part of #main below the sticky header, or put its top just below the header if it
// doesn't fit. It is dropped when the view changed or its rows were replaced by then; a render that kept them (a live
// update that changed nothing) doesn't stop it.
function scrollToRows(rows) {
  const view = $('#main').dataset.key;
  requestAnimationFrame(() => {
    if (!rows[0].isConnected || $('#main').dataset.key !== view) return;
    const main = $('#main'), mr = main.getBoundingClientRect(), clear = headClearance(rows[0]);
    const a = rows[0].getBoundingClientRect(), b = rows.at(-1).getBoundingClientRect();
    const room = main.clientHeight - clear;
    if (b.bottom - a.top < room * 0.8) main.scrollTop += (a.top + b.bottom) / 2 - mr.top - (clear + room / 2);
    else scrollBelowHead(rows[0]);
  });
}

// Notes on this file; old-side notes of a renamed file use the old path.
function notesFor(path, change) {
  return state.notes.filter(n => n.file === (n.side === 'old' ? (change?.oldPath || path) : path));
}

const noteLines = (n) => `line${n.endLine ? `s ${n.line}–${n.endLine}` : ' ' + n.line}${n.side === 'old' ? ' (before)' : ''}`;

// This view's notes in file and line order: the order Next and the toolbar's count step through. Sorted once per
// notes list, with each note's place in it by id.
const sortedOnce = () => {
  if (sortedOnce.cache?.notes !== state.notes) {
    const list = [...state.notes].sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
    sortedOnce.cache = { notes: state.notes, list, index: new Map(list.map((n, i) => [n.id, i])) };
  }
  return sortedOnce.cache;
};
const sortedNotes = () => sortedOnce().list;

// `tag` names the side a note is about where its position can't show it (the unified layout).
function noteBox(n, extra = null, tag = false) {
  const { list, index } = sortedOnce(), i = index.get(n.id) ?? -1;
  return h('div', { class: 'note-box' + (n.stale ? ' stale' : '') },
    h('div', { class: 'note-head' },
      h('strong', {}, 'Agent note'), h('span', {}, noteLines(n)),
      n.author ? h('span', { class: 'muted', title: 'Left by' }, n.author) : null,
      tag ? h('span', { class: 'note-side', title: `About the ${n.side === 'old' ? 'old' : 'new'} version of these lines` }, n.side === 'old' ? 'old' : 'new') : null,
      n.stale ? h('span', { class: 'note-stale', title: 'These lines changed after the note was added' }, 'outdated') : null,
      h('span', { class: 'end' },
        extra,
        list.length > 1 && i >= 0 ? [h('span', {}, `${i + 1} of ${list.length}`),
          h('button', { class: 'note-link', title: 'Go to the next note', onclick: () => { noteIdx = i; nextNote(); } }, 'Next ›')] : null,
        h('button', { class: 'note-link', title: 'Dismiss this note', onclick: () => clearNotes({ id: n.id }) }, 'Dismiss'))),
    h('div', { class: 'note-text' }, n.text));
}

// Put each note under the last line it covers, on the side it is about. In a split diff the note sits in that side's
// half, and notes on both sides of the same line share a row; in the unified layout it spans the row with an old/new
// tag. Only that side's line numbers are marked. Notes whose lines aren't shown (outside the diff's hunks, or past the
// end of the file) go in a strip at the top, with a link to the file view.
function addNotes(container, path, change, side, linkMissing) {
  const missing = [];
  let index = null;
  for (const n of notesFor(path, change)) {
    if (side && n.side !== side) continue;
    index ??= rowIndex(container); // a big file view has several tables
    const rows = lineRows(container, n.side, n.line, n.endLine || n.line, index);
    if (!rows.length) { missing.push(n); continue; }
    for (const r of rows) {
      const nums = r.querySelectorAll('td.num'); // old then new; the file view has just one
      nums[n.side === 'old' ? 0 : nums.length - 1]?.classList.add('noted');
    }
    const last = rows.at(-1), table = last.closest('table');
    if (table.classList.contains('split')) { placeSplitNote(last, n); continue; }
    let anchor = last;
    while (anchor.nextElementSibling?.classList.contains('note')) anchor = anchor.nextElementSibling;
    anchor.after(h('tr', { class: 'note' }, h('td', { colspan: table.querySelectorAll('col').length },
      noteBox(n, null, table.classList.contains('unified')))));
  }
  if (!missing.length) return;
  container.prepend(h('div', { class: 'notes-missing' }, missing.map(n => noteBox(n, linkMissing
    ? h('button', { class: 'note-link', onclick: () => jumpToLines(path, n, 'file') }, 'show in file')
    : null))));
}

// A split row is old number, sign, code, then the same for new. The first note row under `last` whose half for this
// side is still empty takes the note, so an old and a new note on the same line end up side by side.
function placeSplitNote(last, n) {
  const half = n.side === 'old' ? 0 : 1;
  let row = last;
  while (row.nextElementSibling?.classList.contains('note')) {
    row = row.nextElementSibling;
    const cell = row.children[half];
    if (!cell.firstChild) { cell.append(noteBox(n)); return; }
  }
  const cells = [h('td', { colspan: 3, class: 'note-half' }), h('td', { colspan: 3, class: 'note-half' })];
  cells[half].append(noteBox(n));
  row.after(h('tr', { class: 'note split-note' }, ...cells));
}

// This view's notes, plus a summary of the views that have notes of their own (`others`), and a ticket, taken as the
// request starts, for storeNotes. A view with no range (see noRange) has no notes of its own, and every view's are
// another's: the summary is the one seen from no view (all=1). Its notes used to be stored as none at all, which hid
// "N in other views" there.
async function fetchNotes(params = {}) {
  const t = ticket('notes');
  try {
    if (noRange(params)) return { notes: [], others: (await api('/api/annotations', { all: '1' })).others || [], t };
    const r = await api('/api/annotations', params);
    return { notes: r.notes, others: r.others || [], t };
  } catch { return { notes: [], others: [], t }; }
}
// The notes come with every tree (loadTree) and on their own after they changed (loadNotes), and the answers can come
// in in any order: an agent that edits a file and annotates it at once gets the notes loaded for the new note before
// the live update's slower tree, whose notes are from before it. So an answer is stored only when no request for the
// same view that started after it has had its answer stored: the newest request's answer wins. An answer for a view
// left meanwhile is never stored. Returns whether it stored them.
function storeNotes({ notes, others, t }) {
  if (!t.apply()) return false;
  state.notes = notes; state.otherNotes = others;
  // The tree shows each file's note count, so new notes rebuild it too.
  const sig = JSON.stringify(notes.map(n => [n.file, n.id]));
  if (sig !== state.notesSig) { state.notesSig = sig; state.treeDirty = true; }
  renderNotesBar();
  return true;
}
// This view's notes again, after they changed (an `annotations` event, a clear). While the view asked for last doesn't
// hold its whole range yet (History waiting for its commit list), they are only noted as due, and loaded once it does
// (settle): loaded before, they were the half-set range's. Stored, they go on the rows (the caller's reapplyNotes, and
// the tree's counts) only once the view is shown: until then the rows on the page are the last view's, and the view's
// first draw puts its notes on its own. Returns whether they are to go on the rows.
async function loadNotes() {
  const it = intent;
  if (!it.settled) { it.notesDue = true; return false; }
  if (!storeNotes(await fetchNotes()) || !it.shown) return false;
  renderTree(); // the note counts on its rows
  return true;
}

// After the notes changed (an agent added one, or one was dismissed), put them on the rows already on
// the page instead of rendering it again: that would refetch and re-highlight every diff or the whole file.
function reapplyNotes() {
  const main = $('#main');
  if (main._shown) main._shown.notes = notesSig();
  for (const el of main.querySelectorAll('tr.note, .notes-missing')) el.remove();
  for (const c of main.querySelectorAll('td.noted')) c.classList.remove('noted');
  if (!state.file) {
    // A section showing a diff (its own, or the one carried over from the last render) gets the notes
    // now; one still waiting for its first diff gets them when it loads.
    for (const sec of main.querySelectorAll('.section')) {
      if (sec._body?.querySelector('table') || (!sec._load && !sec._body?.querySelector('.skeleton'))) {
        addNotes(sec._body, sec.dataset.path, sec._change, null, true);
      }
    }
    return;
  }
  const body = main.querySelector('.file-body');
  if (state.view === 'preview' || !body) return renderMain({ force: true }); // the preview lists notes above the document
  const change = state.tree.changes[state.file];
  const fileTable = body.querySelector('table.fileview');
  addNotes(body, state.file, change, fileTable ? fileTable.dataset.side : null, !fileTable);
}

async function clearNotes(body) {
  try {
    const res = await fetch('/api/annotations', {
      method: 'POST', headers: { 'x-revue': '1', 'content-type': 'application/json' },
      body: JSON.stringify({ action: 'clear', ...rangeParams(), ...body }),
    });
    const data = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
    if (data.error) return toast(data.error);
  } catch (e) { return toast(e.message); }
  if (await loadNotes()) reapplyNotes();
}

// Notes belong to the view they were left in (a commit, a branch comparison, the working tree), so other views'
// notes are only counted here, with a button that opens them.
function renderNotesBar() {
  const n = state.notes.length, others = state.otherNotes || [];
  const elsewhere = others.reduce((s, o) => s + o.count, 0);
  $('#notes-bar').hidden = !n && !elsewhere;
  $('#notes-next').hidden = $('#notes-more').hidden = !n;
  $('#notes-next').replaceChildren(`${n} note${n === 1 ? '' : 's'}`, h('span', { class: 'more', 'aria-hidden': 'true' }, '›'));
  const btn = $('#notes-other');
  btn.hidden = !elsewhere;
  btn.textContent = `${elsewhere} in other view${others.length === 1 ? '' : 's'}`;
  btn.title = 'Notes left in other views; click to open them:\n' + others.map(o => `${o.count} in ${viewLabel(o.view)}`).join('\n');
}

function viewLabel(v) {
  if (v.mode === 'commit') return `commit ${v.ref.slice(0, 7)}`;
  if (v.mode === 'branch') return `${v.head} vs ${v.base}`;
  if (v.mode === 'folder') return 'the folder';
  return 'uncommitted changes';
}

// Open the next view that has notes, at its first note.
let otherIdx = -1;
function openOtherNotes() {
  const others = state.otherNotes || [];
  if (!others.length) return;
  const o = others[otherIdx = (otherIdx + 1) % others.length];
  history.replaceState({ ...history.state, scroll: $('#main').scrollTop }, '', location.hash);
  history.pushState(null, '', location.hash);
  applyView({ ...o.view, file: o.first.file, line: o.first.line, endLine: o.first.endLine, side: o.first.side, view: 'diff' })
    .catch(e => toast(e.message));
}

// Cycle through notes in file and line order.
let noteIdx = -1;
function nextNote() {
  const list = sortedNotes();
  if (!list.length) return;
  const n = list[noteIdx = (noteIdx + 1) % list.length];
  // An old-side note on a renamed file is listed under the file's new name.
  const path = fileSet().has(n.file) ? n.file
    : Object.keys(state.tree.changes).find(p => state.tree.changes[p].oldPath === n.file) || n.file;
  jumpToLines(path, n, state.tree.changes[path] ? 'diff' : 'file');
}

// Open lines of a file, pushing a history entry so browser Back returns here.
function jumpToLines(path, { line, endLine = null, side = 'new' }, view = null) {
  history.replaceState({ ...history.state, scroll: $('#main').scrollTop }, '', location.hash);
  history.pushState(null, '', location.hash);
  selectFile(path, { line, endLine, side, view });
}

// What #main shows (`main._shown`, set when a render is put on the page): the view's key(), the open file's tag and
// the data it was drawn from, the notes it carries, and whether it is an overview with sections. A render of the same
// view (a live update, j/k onto the same file) changes only what changed: the overview is updated in place
// (updateOverview), and an open file whose tag is unchanged, or whose refetched text is what is shown, is left alone.
// force (the r key) draws everything again.
const notesSig = () => JSON.stringify(state.notes);
// The selected file's version: its change's tag from /api/tree. In History and Branch mode a file without a change
// can only change with the compared refs, which come with a `git` live update. In Changes mode it has no tag (null):
// an ignored file, or one an agent opened, can change with no entry in the tree, so it is refetched every time and
// kept only when the data is the same. So is every file of a plain folder, which has no changes.
const fileTag = () => {
  const c = state.tree.changes[state.file];
  if (c) return `${c.status}|${c.oldPath || ''}|${c.v}`;
  return state.mode === 'working' || state.mode === 'folder' ? null : `-|${gitEvents}`;
};
const SAME = Symbol('same');
async function renderMain({ force = false } = {}) {
  // As in refresh, nothing is drawn before /api/info is in (Esc or a file pick while the page loads draws here).
  if (!state.info) { try { await infoLoaded; } catch { return; } }
  // A render is dropped once a newer one starts, or once another view is asked for (`t`, the intent's render ticket).
  const t = ticket('render'), it = intent;
  const main = $('#main');
  const keepScroll = main.dataset.key === key();
  const restore = intent.restoreScroll; intent.restoreScroll = null;
  main.dataset.key = key();
  renderToolbar();
  // A new view starts at the first page of sections; one that restores a scroll position (a layout switch, Back) keeps
  // the pages that were open, so the position is still there.
  if (!keepScroll && restore == null) overviewLimit = OVERVIEW_PAGE;
  const shown = !force && keepScroll && restore == null && main._shown?.key === key() ? main._shown : null;
  // Wherever the render lands (a page updated in place, kept, or replaced), the intent it was for is shown.
  if (shown && !state.file && shown.sections && updateOverview(main, shown)) return markShown(it);
  let same = null; // the data the open file was drawn from, when only a refetch can tell whether it changed
  if (shown && state.file && !(state.view === 'preview' && shown.notes !== notesSig())) {
    // A preview that shows images is fetched again and compared even when its own tag holds: an image can change
    // without its document changing.
    if (shown.tag != null && shown.tag === fileTag() && !shown.images) { markShown(it); return keepNotes(shown); }
    same = shown.data;
  }
  // On a full render of the same overview (r), carry each section over from the old one.
  const prev = !state.file && keepScroll ? new Map([...main.querySelectorAll('.section')].map(s => [s.dataset.path, s])) : null;
  // Their diffs move out of the page, which leaves it short, and Chromium then clamps its scroll position on the next
  // read: read the position first. Nothing is awaited before the sections are carried over.
  const before = prev?.size ? main.scrollTop : null;
  const frag = document.createDocumentFragment();
  let result;
  try {
    if (!state.file) await renderOverview(frag, t, prev);
    // Jump to the target (or the first change) unless this view is already on the page: a render that superseded the
    // first one before it was drawn (the catch-up after load) must still do it.
    else result = await renderFile(frag, t, main._shown?.key !== key() && restore == null, same);
  } catch (e) {
    frag.append(h('div', { class: 'placeholder' }, 'Error: ' + e.message));
  }
  if (!t.fresh()) return;
  markShown(it);
  if (result === SAME) { shown.tag = fileTag(); return keepNotes(shown); }
  // Read here, not before the awaits: a slow render must not jump back to where it started.
  const scroll = main.scrollTop;
  main.textContent = '';
  main.append(frag);
  // Swapping the content doesn't move the scroll position (no layout ran in between), so it is written only when it
  // has to change: a write forces the new content to be laid out at once.
  const want = keepScroll ? before ?? scroll : (restore ?? 0);
  if (want !== scroll) main.scrollTop = want;
  main._shown = { key: key(), tag: state.file ? fileTag() : null, data: frag._data, notes: notesSig(), images: !!frag._images,
    sections: !state.file && !!main.querySelector(':scope > .section') };
  observeSections();
}
// The page stays; only notes that changed since it was drawn are put on it again.
function keepNotes(shown) {
  if (shown.notes !== notesSig()) reapplyNotes();
}
// The bar above the main pane: what it shows on the left; notes, context and layout on the right. Context and layout
// apply to diffs only.
function renderToolbar() {
  const label = $('#view-label');
  if (state.file) {
    label.replaceChildren(h('span', { class: 'file' }, state.file));
    label.title = state.file;
  } else if (state.mode === 'folder') {
    label.textContent = 'No file open';
    label.title = '';
  } else {
    const n = Object.keys(state.tree.changes).length;
    label.textContent = `All changes · ${n} file${n === 1 ? '' : 's'}`;
    label.title = '';
  }
  // Context and layout apply to text diffs: hidden for the file and preview views, a binary change's diff (its images,
  // or "Binary file changed"), an untracked file too large to diff, and an overview without a text change (and so
  // always in a plain folder, which has no diffs). Decided from the tree's entries, since no diff is in yet, on every
  // render, a live update's included. The ⋯ menu's Context row follows the toolbar's class (style.css).
  const changes = state.tree.changes, open = state.file ? changes[state.file] : null;
  $('#toolbar').classList.toggle('no-diff', state.file
    ? state.view !== 'diff' || (!!open && noTextDiff(state.file, open))
    : Object.entries(changes).every(([p, c]) => noTextDiff(p, c)));
}

// The view: what is shown (mode, range, file, lines, view and side). key() adds how diffs are drawn, which decides
// whether a render of the same view keeps its scroll position and what is on the page.
const viewKey = () => [state.mode, state.ref, state.base, state.head, state.file, state.line, state.endLine, state.view, state.fileSide].join('|');
const key = () => [viewKey(), diffLayout(), state.context].join('|');

const FILE_CHUNK = 500;
// same: the data the file on the page was drawn from; when the refetched data equals it, this returns SAME and draws
// nothing. Otherwise the data is left on frag._data for the next render to compare.
async function renderFile(frag, t, autoScroll, same = null) {
  const path = state.file;
  const change = state.tree.changes[path];
  // A plain folder has no diffs, so its files have no Diff tab, and only markdown and SVG have a choice (File, or
  // Preview and Image: their second view, hasPreview).
  const folder = state.mode === 'folder';
  const tabs = folder && !hasPreview(path) ? null : h('div', { class: 'seg' },
    folder ? null : h('button', { class: state.view === 'diff' ? 'on' : '', onclick: () => setView('diff'), disabled: !change, title: 'Diff (f)' }, 'Diff'),
    h('button', { class: state.view === 'file' ? 'on' : '', onclick: () => setView('file'), title: 'Whole file (f)' }, 'File'),
    hasPreview(path) ? h('button', { class: state.view === 'preview' ? 'on' : '', onclick: () => setView('preview'),
      title: isSvg(path) ? 'The drawing (f)' : 'Rendered markdown (f)' }, isSvg(path) ? 'Image' : 'Preview') : null);
  // Esc's place: the overview, or in a plain folder the page it opens on (its README).
  const home = folder ? "The folder's start page" : 'All changes';
  const back = h('button', { class: 'back-btn', title: `${home} (Esc)`, 'aria-label': home, onclick: () => selectFile(null) }, '←');
  const sides = state.view !== 'diff' && change && change.status !== '?' && change.status !== 'A' && change.status !== 'D'
    ? h('div', { class: 'seg' },
      h('button', { class: state.fileSide === 'old' ? 'on' : '', onclick: () => setSide('old') }, 'Before'),
      h('button', { class: state.fileSide === 'new' ? 'on' : '', onclick: () => setSide('new') }, 'After'))
    : null;
  frag.append(fileHead(path, change, [back], [sides, tabs]));
  const body = h('div', { class: 'file-body' });
  frag.append(body);

  if (state.view === 'diff' && change) {
    // A cached diff (a layout switch, or a view of it seen before) is drawn without a request; it is only cached under
    // the change's version tag, so it can't be stale.
    const key = diffKey(path, change);
    let parsed = cachedDiff(key);
    if (!parsed) {
      const res = await api('/api/diff', { path, old: change.oldPath || '', status: change.status, context: state.context });
      parsed = cacheDiff(key, diffOf(res)); // under the new tag even when nothing changed, so a layout switch still needn't fetch
    }
    // A binary change's diff text says only that it changed: its images, with their versions, are what is drawn.
    const imgs = parsed.binary ? await changeImages(path, change) : null;
    if (!t.fresh()) return;
    frag._data = JSON.stringify(['diff', change.status, change.oldPath, parsed.text, imgs]);
    if (frag._data === same) return SAME;
    showDiff(body, path, change, parsed, imgs);
    if (!t.fresh() || !state.line) return;
    const rows = markTarget(body, state.fileSide);
    if (rows.length) { if (autoScroll) scrollToRows(rows); return; }
    // The target isn't in any hunk: show it in the whole file instead.
    toast(`Line ${state.line} isn't in the diff; showing the whole file`);
    state.view = 'file';
    writeHash();
    renderMain(); // supersedes this render
    return;
  }
  // Whole-file view. Deleted files show the old side; the gutter marks lines this change touched.
  const side = change?.status === 'D' ? 'old' : (change ? state.fileSide : 'new');
  const at = { path: side === 'old' && change?.oldPath ? change.oldPath : path, side };
  const fileReq = api('/api/file', at);
  if (state.view === 'preview' && isSvg(path)) {
    // An SVG's Image view: the drawing as an image (never as a document of its own), or why it isn't drawn.
    const file = await fileReq;
    if (!t.fresh()) return;
    frag._data = JSON.stringify(['image', change?.status, file]);
    if (frag._data === same) return SAME;
    body.append(...previewNotes(path));
    if (file.image) body.append(imageView(sideImage(rangeParams(), at.path, at.side, file)));
    else if (!fileProblem(body, file, at)) {
      body.append(h('div', { class: 'placeholder' }, "Not drawn: an SVG's text has to start with an <svg> element, with no doctype that defines entities"));
    }
    return;
  }
  if (state.view === 'preview') {
    const file = await fileReq;
    if (!t.fresh()) return;
    // The document's images are looked up before it is drawn, so their places are kept from the start (a scroll
    // position restored on Back lands where it was), and what they are is part of what is compared, so a live update
    // that changed only an image draws it again.
    const doc = file.content != null ? renderMarkdown(file.content, path, { side: at.side, range: rangeParams() }) : null;
    const images = await doc?._images;
    if (!t.fresh()) return;
    frag._data = JSON.stringify(['preview', change?.status, file, images]);
    frag._images = images?.length > 0;
    if (frag._data === same) return SAME;
    if (!fileProblem(body, file, at)) body.append(...previewNotes(path), doc);
    return;
  }
  const [file, diffRes] = await Promise.all([
    fileReq,
    change && change.status !== '?' ? api('/api/diff', { path, old: change.oldPath || '', status: change.status, context: '0' }) : null,
  ]);
  if (!t.fresh()) return;
  frag._data = JSON.stringify(['file', change?.status, side, file, diffRes?.diff]);
  if (frag._data === same) return SAME;
  if (fileProblem(body, file, at)) return;
  const changed = new Set(), delAfter = new Set(), delBefore = new Set();
  if (change?.status === '?' || change?.status === 'A') file.content.split('\n').forEach((_, i) => changed.add(i + 1));
  if (diffRes) {
    for (const hk of parseDiff(diffRes.diff).hunks) {
      for (const l of hk.lines) {
        if (side === 'new') { if (l.t === 'add') changed.add(l.n); }
        else if (l.t === 'del') changed.add(l.o);
      }
      // A pure deletion after new line n ("+n,0"); n is 0 when the deleted lines were at the very top.
      if (side === 'new' && !hk.lines.some(l => l.t === 'add')) {
        const m = /\+(\d+)/.exec(hk.header);
        if (m) +m[1] ? delAfter.add(+m[1]) : delBefore.add(1);
      }
    }
  }
  let content = file.content;
  if (content.endsWith('\n')) content = content.slice(0, -1);
  const lines = highlightLines(content, langFor(path));
  // A big file is split into tables of FILE_CHUNK lines that the browser skips laying out and
  // painting while offscreen (content-visibility, which doesn't apply to a tbody), and built with
  // pauses so the page stays responsive and a newer render can cancel it. Every row is in the DOM.
  const big = lines.length > 2000;
  for (let start = 0; start < lines.length; start += FILE_CHUNK) {
    if (big && start && start % (FILE_CHUNK * 4) === 0) {
      await new Promise(r => setTimeout(r));
      if (!t.fresh()) return;
    }
    const end = Math.min(lines.length, start + FILE_CHUNK);
    // One HTML string per table, parsed once (lines[] is escaped, highlighted HTML).
    const tbody = h('tbody'), attr = side === 'old' ? 'data-o' : 'data-n', rows = [];
    for (let i = start; i < end; i++) {
      const n = i + 1;
      const cls = (changed.has(n) ? 'changed ' : '') + (delAfter.has(n) ? 'delafter ' : '') + (delBefore.has(n) ? 'delbefore' : '');
      rows.push(`<tr class="${cls}" id="L${n}" ${attr}="${n}"><td class="num">${n}</td><td class="src">${lines[i]}</td></tr>`);
    }
    tbody.innerHTML = rows.join('');
    body.append(h('table', { class: 'code fileview' + (big ? ' chunk' : ''), 'data-path': path, 'data-side': side,
      style: big ? `contain-intrinsic-size: auto ${(end - start) * cssPx('--line-h')}px` : null }, // one unwrapped line each
    h('colgroup', {}, h('col', { class: 'num' }), h('col')), tbody));
  }
  addNotes(body, path, change, side, false);
  const rows = markTarget(body, side);
  if (!autoScroll) return;
  // Jump to the requested lines (go to definition, agents), or to the first change when opening a file.
  if (rows.length) return scrollToRows(rows);
  if (state.line) toast(`${path} has no line ${state.line}`);
  let first = Infinity; // a loop, not Math.min(...set): spreading a huge file's lines overflows the stack
  for (const n of changed) if (n < first) first = n;
  for (const n of delAfter) if (n < first) first = n;
  for (const n of delBefore) if (n < first) first = n;
  const view = $('#main').dataset.key;
  if (Number.isFinite(first) && first > 1) requestAnimationFrame(() => {
    if (!body.isConnected || $('#main').dataset.key !== view) return;
    const row = document.getElementById('L' + Math.max(1, first - 5));
    if (row && $('#main').scrollTop === 0) scrollBelowHead(row);
  });
}

// A file over 2 MB, in the file view and, for an untracked text file, which the server doesn't diff, in its diff.
const tooLargeText = (size) => `File too large to display (${size} bytes)`;
// Append what is shown in place of the text, and return true, when /api/file sent none: an image, or a placeholder.
// `at`: the path and side the file was asked for, with the range in state (the caller has checked its ticket, so it
// is still the view's).
function fileProblem(body, file, at) {
  if (file.image && file.content == null) { // an SVG is text too, which this view shows
    body.append(imageView(sideImage(rangeParams(), at.path, at.side, file)));
    return true;
  }
  const msg = file.binary ? `Binary file (${file.size} bytes)`
    : file.tooLarge ? tooLargeText(file.size)
    : file.outside ? 'Symlink to a location outside the repository (not shown)' : null;
  if (msg) body.append(h('div', { class: 'placeholder' }, msg));
  return !!msg;
}

// ---------------------------------------------------------------- images
// The server tells an image by its bytes (image.rs: PNG, JPEG, GIF, WebP or ICO), and /api/file describes it: its type,
// its size in pixels, and either the version of its bytes (`v`) or why it isn't shown (`refused`). /api/image serves
// it. Its address carries the version, so a changed image is fetched again and an unchanged one comes from the
// browser's cache, and the range it was asked with, so it is always the image of the view that asked. A binary change
// shows its two sides' images (changeImages, imageDiff); the file view shows one.
const IMAGE_PATH = /\.(png|jpe?g|gif|webp|ico)$/i; // for the overview's height estimates only: the bytes decide
const OVERVIEW_IMAGE_PX = 360; // the most an image is drawn tall in the overview
const fmtBytes = (n) => n < 1024 ? `${n} bytes` : n < 1 << 20 ? `${(n / 1024).toFixed(1)} KB` : `${(n / (1 << 20)).toFixed(1)} MB`;

// One side's image from its /api/file answer: { path, info, size, src }, src null when it is refused.
function sideImage(range, path, side, file) {
  const img = { path, info: file.image, size: file.size, src: null };
  if (!file.image.refused) img.src = '/api/image?' + new URLSearchParams({ ...range, path, side, v: file.image.v });
  return img;
}

function imageRefusal({ info, size }) {
  if (info.refused === 'bytes') {
    // image::MAX_SVG_BYTES for an SVG, the text limit, and image::MAX_BYTES for any other image.
    const most = info.type === 'image/svg+xml' ? '2 MB' : '20 MB';
    return `Image too large to show (${fmtBytes(size)}; the most is ${most})`;
  }
  if (info.refused === 'pixels') return `Image too large to show (${info.width} × ${info.height} pixels; the most is 50 million)`;
  // An SVG whose <use>s, each drawing a copy of a group that may hold more of them, would draw too much.
  if (info.refused === 'complex') return 'Image too complex to draw (over 50,000 elements, counting the copies its <use>s draw)';
  // An SVG that holds images of its own as data: URLs, which an <img> loads, past what the server checks of a file.
  if (info.refused === 'embedded') return "Image not shown: it embeds an image revue can't check, or images of over 50 million pixels in all";
  // An SVG that isn't well-formed XML, of which browsers draw nothing: the first error, and its line.
  if (info.refused === 'malformed') return `Not a well-formed SVG: ${info.reason} (line ${info.line})`;
  return `Image not shown: its size can't be read (${fmtBytes(size)})`;
}

// The server's size for an image is its best guess of the size the browser draws it at (image.rs: turned as its EXIF
// orientation says, and for a JPEG scaled by its EXIF resolution, as Chromium does), which keeps its place while it
// loads. Browsers don't all agree (Firefox scales a PNG by its EXIF resolution too), so once it has loaded, the size
// the browser gives it is the one it is drawn at: a load handler for an <img> of `info` that calls fix(img, width,
// height) when that size isn't the server's, or null when there is nothing to compare. An SVG is left as the server
// sizes it: one with only a viewBox has no size of its own, yet browsers report a default one (300 × 150, or a square
// of 150 or 300) as its natural size, and one with a width or height, in whatever unit, is sized as browsers size it
// (image.rs, svg_size), or not at all where the server can't tell, and then drawn at the browser's.
const naturalSize = (info, fix) => info.width && info.type !== 'image/svg+xml' ? (e) => {
  const img = e.currentTarget, w = img.naturalWidth, ht = img.naturalHeight;
  if (w && ht && (w !== info.width || ht !== info.height)) fix(img, w, ht);
} : null;

// One image under a caption: which side it is (label), its type and its size in pixels and bytes. Its width and height
// come from /api/file, so its place is kept while it loads, and once it has loaded they and the caption are the size
// the browser gives it, if that is another (naturalSize). CSS scales it down to fit the width, never up, and in the
// overview (capped) it is also at most OVERVIEW_IMAGE_PX tall. A checkerboard behind it shows where it is transparent.
function imageView(img, label = null, capped = false) {
  const { info } = img;
  const type = { 'image/x-icon': 'ICO', 'image/svg+xml': 'SVG' }[info.type] ?? info.type.slice('image/'.length).toUpperCase();
  const describe = (w, ht) => [label, type, w && `${w} × ${ht}`, fmtBytes(img.size)].filter(Boolean).join(' · ');
  // The width that keeps it at most OVERVIEW_IMAGE_PX tall, or '' (its width attribute's) when it is no taller.
  const cappedWidth = (w, ht) => {
    const scale = capped ? Math.min(1, OVERVIEW_IMAGE_PX / ht) : 1;
    return scale < 1 ? `${Math.round(w * scale)}px` : '';
  };
  const caption = h('figcaption', {}, describe(info.width, info.height));
  let shown;
  if (!img.src) shown = h('div', { class: 'placeholder' }, imageRefusal(img));
  else {
    const width = cappedWidth(info.width, info.height);
    shown = h('img', { src: img.src, alt: label ? `${label}: ${img.path}` : img.path, width: info.width, height: info.height,
      decoding: 'async', style: width ? `width: ${width}` : null,
      onerror: () => shown.replaceWith(h('div', { class: 'placeholder' }, "The image couldn't be loaded")),
      onload: naturalSize(info, (el, w, ht) => {
        el.width = w;
        el.height = ht;
        el.style.width = cappedWidth(w, ht);
        caption.textContent = describe(w, ht);
      }) });
  }
  return h('figure', { class: 'image' }, caption, shown);
}

// The images of a binary change: { sides, old, new }, where sides lists the sides the change has ('old' unless it adds
// the file, 'new' unless it deletes it) and old and new are sideImages (null for a side that isn't an image), or null
// when neither side is one. They are kept by the change, whose version tag moves whenever either side could, so drawing
// it again (a layout switch, the overview after the file) asks for nothing. `range`: the view's, see api.
const imageCache = new Map();
const imagesKey = (path, change, range) => JSON.stringify([range, path, change.oldPath || '', change.status, change.v || '']);
async function changeImages(path, change, range = rangeParams()) {
  const key = imagesKey(path, change, range);
  if (imageCache.has(key)) return imageCache.get(key);
  const sides = ['old', 'new'].filter(s => s === 'old' ? change.status !== 'A' && change.status !== '?' : change.status !== 'D');
  const found = await Promise.all(sides.map(async (side) => {
    const p = side === 'old' && change.oldPath ? change.oldPath : path;
    const file = await api('/api/file', { path: p, side }, undefined, range);
    return file.image ? sideImage(range, p, side, file) : null;
  }));
  const imgs = found.some(Boolean) ? { sides, old: found[sides.indexOf('old')] ?? null, new: found[sides.indexOf('new')] ?? null } : null;
  imageCache.set(key, imgs);
  if (imageCache.size > 500) imageCache.delete(imageCache.keys().next().value);
  return imgs;
}

// A binary change's images: Before and After side by side (one above the other where both don't fit), or the one side
// an added or deleted file has. A side that isn't an image says so.
function imageDiff(imgs, capped) {
  const pair = imgs.sides.length === 2;
  return h('div', { class: 'image-diff' }, imgs.sides.map(side => {
    const label = pair ? (side === 'old' ? 'Before' : 'After') : null;
    return imgs[side] ? imageView(imgs[side], label, capped)
      : h('figure', { class: 'image' }, h('figcaption', {}, label), h('div', { class: 'placeholder' }, 'Not an image'));
  }));
}

// ---------------------------------------------------------------- markdown preview
// The source is untrusted (any repo's README) and this page can call the write endpoints, so marked's
// output always goes through DOMPurify before it reaches the page. Nothing the document names loads by itself:
// marked turns images into a text placeholder, DOMPurify drops the tags that fetch (<img>, media) and the
// attributes that would (background on tables, src on <input type=image>, srcset, poster), and <input>
// survives only as a task-list checkbox. The only things a preview loads are the repo's own images, which this code
// puts in place of their placeholders after all that, with addresses it builds itself (see the preview's images,
// below). Nor does a preview style anything: style attributes and <style>, <link>,
// <meta> and <base> are cut before DOMPurify parses, forbidden to DOMPurify, and swept out after it, any one of which
// is enough. Behind those, the CSP blocks inline styles (and other origins, though not this server's own URLs), and
// the preview is drawn in its own shadow root and contained frame (mdFrame).
const isMarkdown = (path) => /\.(md|markdown)$/i.test(path || '');
const isSvg = (path) => /\.svg$/i.test(path || '');
// The files with a second view besides File (and Diff): markdown's Preview, and an SVG's Image. Both are view=preview.
const hasPreview = (path) => isMarkdown(path) || isSvg(path);
const MD_FORBID_TAGS = ['style', 'link', 'meta', 'base', 'img', 'picture', 'source', 'video', 'audio', 'track', 'iframe', 'object', 'embed', 'form', 'button', 'select', 'textarea', 'dialog'];
const MD_FORBID_ATTR = ['style', 'id', 'name', 'target', 'background', 'src', 'srcset', 'poster', 'ping', 'formaction',
  'popover', 'popovertarget', 'popovertargetaction']; // a popover is drawn in the top layer, outside the preview's frame
let mdRenderer = null;

// Chromium reports a CSP violation for some markup it parses even into DOMPurify's inert document (every on* or style
// attribute, <style> and <base href>), so marked's raw HTML (block and inline) loses these before DOMPurify sees it:
// on* and style attributes come off every tag, and <link>, <meta> and <base> tags go. A <style> becomes a <noembed>:
// the parser ends both at the same place (the content of each is raw text, so it can't hide markup), but a noembed
// applies nothing, and DOMPurify drops it with its content as it would a style. Cutting from <style> to </style>
// instead would get that wrong where the two aren't one tag apart, as when marked hands over an inline <style> on its
// own. Tags are found the way the browser finds them: quoted values are skipped over, so a '>' or ' on' inside one
// changes nothing, and an attribute right after one (title="x"onclick=…, which the browser reads as two attributes) is
// cut too. This keeps the console clean, and DOMPurify and the sweep after it remove anything it misses.
const RAW_ATTRS = String.raw`(?:"[^"]*"|'[^']*'|[^"'>])*`; // the rest of a tag, quoted values whole
const RAW_HTML = new RegExp(String.raw`<(\/?)style(?=[\s/>])${RAW_ATTRS}>|(<\/?(?:link|meta|base)(?=[\s/>])${RAW_ATTRS}>)|<[a-z]${RAW_ATTRS}>`, 'gi');
const RAW_ATTR = /("[^"]*"|'[^']*')|(?:[\s/]|(?<=["']))(?:on[\w-]+|style(?![\w-]))(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'>]+))?/gi;
const stripRawHtml = (html) => html.replace(RAW_HTML, (tag, close, drop) =>
  close != null ? `<${close}noembed>` : drop ? '' : tag.replace(RAW_ATTR, (m, quoted) => quoted ?? ' '));

// DOMPurify hooks are global; this is the only sanitize call on the page. Inputs other than checkboxes
// go before their attributes are even looked at. A raw <img> or <picture>, which DOMPurify then drops, leaves a
// placeholder in its place and what it names in mdImageSink (see the preview's images); a <picture>'s own children
// go with it, so DOMPurify doesn't keep them as loose <source>s and <img>s.
function mdHooks() {
  DOMPurify.addHook('uponSanitizeElement', (node, data) => {
    if (data.tagName === 'input' && (node.getAttribute('type') || '').toLowerCase() !== 'checkbox') node.parentNode?.removeChild(node);
    if (!mdImageSink || !node.parentNode || (data.tagName !== 'img' && data.tagName !== 'picture')) return;
    const img = data.tagName === 'img' ? node : node.querySelector('img');
    const sources = data.tagName === 'img' ? [] : [...node.querySelectorAll('source')]
      .map(s => ({ href: (s.getAttribute('srcset') || '').trim().split(/\s+/)[0], media: s.getAttribute('media') }));
    if (img) sources.push({ href: img.getAttribute('src'), media: null });
    const alt = img?.getAttribute('alt') || '';
    const span = node.ownerDocument.createElement('span');
    span.className = 'md-img';
    span.textContent = `[image${alt ? ': ' + alt : ''}]`;
    span.dataset.mdImg = mdImageToken({ alt, title: img?.getAttribute('title'), width: mdWidth(img?.getAttribute('width')), sources });
    node.before(span);
    if (data.tagName === 'picture') node.replaceChildren();
  });
}

// marked and DOMPurify are loaded the first time a preview needs them, not with the page (scripts from this server,
// which the CSP allows), and so are the preview's styles (mdSheets). A failed load is retried by the next preview.
let mdLibs = null;
const mdReady = () => !!(window.marked && window.DOMPurify && mdSheets);
function loadMarkdownLibs() {
  if (mdReady()) return Promise.resolve();
  return mdLibs ||= Promise.all([...['vendor/marked.min.js', 'vendor/purify.min.js'].map(src => new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = src;
    s.onload = resolve;
    s.onerror = () => { s.remove(); reject(new Error(src)); };
    document.head.append(s);
  })), loadMarkdownSheets()]).catch(e => { mdLibs = null; throw e; });
}

// The preview's styles, which its shadow root (mdFrame) doesn't get from the page: the two highlight themes and
// markdown.css, in the page's order, built once as constructed sheets that every preview adopts. They apply the moment
// a preview is drawn. A <link> in each shadow root applies only once loaded, which in Chromium is a frame later even
// from its cache, so a scroll position set at once (Back to a preview near its end) was cut short by the unstyled
// document's height. The themes' media follow the page's: syncPreviewThemes.
let mdSheets = null;
async function loadMarkdownSheets() {
  if (mdSheets) return;
  const sheets = await Promise.all(['vendor/hl-light.css', 'vendor/hl-dark.css', 'markdown.css'].map(async (url) => {
    const res = await fetch(url);
    if (!res.ok) throw new Error(url);
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(await res.text());
    return sheet;
  }));
  mdSheets = { all: sheets, theme: { 'hl-light': sheets[0], 'hl-dark': sheets[1] } };
  syncPreviewThemes();
}
// applyTheme (theme.js) sets the media of the page's highlight sheets; the previews' copies take the same.
function syncPreviewThemes() {
  for (const [id, sheet] of Object.entries(mdSheets?.theme ?? {})) sheet.media.mediaText = document.getElementById(id).media;
  syncPreviewImages();
}

// The rendered document, or (the first time) a placeholder that it replaces once the libraries are in. Nothing is
// shown from the source until it has been sanitized.
const MD_UNAVAILABLE = 'Markdown preview is unavailable (its scripts or styles failed to load)';
// `at`: the side the document was read from and the range it was asked with, which its images are asked with too.
// The rendered document carries the loading of its images (`_images`, see previewImages); a placeholder for one still
// to come (the libraries load first) loads them once it is replaced.
function renderMarkdown(text, path, at) {
  if (mdReady()) return withImages(renderMarkdownNow(text, path), at);
  const box = h('div', { class: 'placeholder' }, 'Loading preview…');
  loadMarkdownLibs().then(
    () => { if (box.parentNode) { const doc = renderMarkdownNow(text, path); box.replaceWith(doc); withImages(doc, at); } },
    () => { box.textContent = MD_UNAVAILABLE; });
  return box;
}
function withImages(doc, at) {
  doc._images = previewImages(doc, at);
  return doc;
}

function renderMarkdownNow(text, path) {
  if (!mdReady()) return h('div', { class: 'placeholder' }, MD_UNAVAILABLE);
  if (!mdRenderer) {
    mdRenderer = new marked.Renderer();
    // alt and title arrive escaped
    mdRenderer.image = (href, title, alt) => `<span class="md-img" data-md-img="${mdImageToken({ sources: [{ href, media: null }] })}"${
      title ? ` title="${title}"` : ''}>[image${alt ? ': ' + alt : ''}]</span>`;
    mdRenderer.html = (html) => stripRawHtml(html);
    mdHooks();
  }
  // The images the document names, collected as marked and DOMPurify meet them (mdImageToken).
  mdImageSink = { nonce: Math.random().toString(36).slice(2), images: [] };
  const sink = mdImageSink;
  const root = h('div', { class: 'markdown' });
  try {
    const html = marked.parse(text, { gfm: true, async: false, renderer: mdRenderer });
    root.append(DOMPurify.sanitize(html, {
      USE_PROFILES: { html: true }, FORBID_TAGS: MD_FORBID_TAGS, FORBID_ATTR: MD_FORBID_ATTR, RETURN_DOM_FRAGMENT: true,
    }));
  } finally {
    mdImageSink = null;
  }
  // The last line: whatever got this far still can't style or retarget the page.
  for (const el of root.querySelectorAll('style, link, meta, base')) el.remove();
  for (const el of root.querySelectorAll('[style]')) el.removeAttribute('style');
  // Keep only the classes this code sets, so the document can't borrow the preview's own styles.
  for (const el of root.querySelectorAll('[class]')) {
    const keep = el.tagName === 'CODE' ? /^language-[\w+#.-]+$/.exec(el.className)?.[0] : el.className === 'md-img' ? 'md-img' : null;
    if (keep) el.className = keep; else el.removeAttribute('class');
  }
  // GFM task lists are the only inputs allowed (the hook already dropped the rest), and they're read-only.
  for (const el of root.querySelectorAll('input')) {
    if (el.type === 'checkbox') el.disabled = true; else el.remove();
  }
  for (const code of root.querySelectorAll('pre > code')) {
    const lang = code.className.slice('language-'.length);
    if (lang && window.hljs && hljs.getLanguage(lang)) {
      try { code.innerHTML = hljs.highlight(code.textContent, { language: lang, ignoreIllegals: true }).value; } catch {}
    }
  }
  // Headings get GitHub-style slugs as ids, prefixed with md-; they live in the shadow root, apart from revue's ids.
  const slugs = new Map();
  for (const el of root.querySelectorAll('h1, h2, h3, h4, h5, h6')) {
    const base = mdSlug(el.textContent);
    const n = slugs.get(base) ?? 0;
    slugs.set(base, n + 1);
    el.id = 'md-' + (n ? `${base}-${n}` : base);
  }
  for (const a of root.querySelectorAll('a')) mdLink(a, path, root);
  const frame = mdFrame(root);
  frame._imageRefs = mdImageRefs(root, sink, path);
  return frame;
}

// The preview is drawn in a shadow root, so that styling that got past everything above would stay inside it: rules in
// a shadow root match only its own elements, and the document's ids don't share revue's. It adopts mdSheets; colors,
// fonts and color-scheme inherit from the page. The host sits in a frame with layout and paint containment
// (style.css), so even a host made position:fixed stays inside the frame's box. The frame can't be the host, since a
// :host rule can override any property of the host, containment included.
function mdFrame(root) {
  const host = h('div', { class: 'md-host' });
  const shadow = host.attachShadow({ mode: 'open' });
  shadow.adoptedStyleSheets = mdSheets.all;
  shadow.append(root);
  return h('div', { class: 'md-frame' }, host);
}

const mdSlug = (s) => s.trim().toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, '').replace(/\s/g, '-');

// Links never navigate this page: #anchors scroll the preview, relative links open the file in revue,
// http(s) links open a new tab, and anything else (mailto:, other schemes) stays as plain text.
function mdLink(a, path, root) {
  const raw = a.getAttribute('href');
  a.removeAttribute('href');
  if (!raw) return;
  a.title = raw;
  if (/^https?:\/\//i.test(raw)) {
    a.href = raw; a.target = '_blank'; a.rel = 'noopener noreferrer';
    return;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith('//')) return;
  const [, rel, frag] = /^([^?#]*)(?:\?[^#]*)?(?:#(.*))?$/.exec(raw);
  const anchor = () => {
    if (frag == null) return;
    let id = frag; try { id = decodeURIComponent(frag); } catch {}
    const target = root.querySelector(`[id="md-${CSS.escape(mdSlug(id))}"]`);
    if (target) scrollBelowHead(target);
  };
  if (!rel) {
    a.href = location.hash;
    a.addEventListener('click', (e) => { e.preventDefault(); anchor(); });
    return;
  }
  const target = mdPath(rel, path);
  if (!fileSet().has(target)) { a.title = `${raw} (not a file in this view)`; return; }
  const p = new URLSearchParams(location.hash.slice(1));
  p.set('file', target);
  for (const k of ['line', 'endLine', 'side', 'view']) p.delete(k);
  if (hasPreview(target)) p.set('view', 'preview');
  a.href = '#' + p;
  a.addEventListener('click', (e) => {
    if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return; // let the browser open a new tab
    e.preventDefault();
    if (target === path) return anchor();
    jumpToLines(target, { line: null }, hasPreview(target) ? 'preview' : null);
  });
}

// The repo path a relative reference in the document at `path` names (its query and fragment already cut off): from
// the repo's root when it starts with /, else from the document's folder.
function mdPath(rel, path) {
  let target; try { target = decodeURIComponent(rel); } catch { target = rel; }
  const parts = target.startsWith('/') ? [] : path.split('/').slice(0, -1);
  for (const seg of target.split('/')) {
    if (seg === '..') parts.pop();
    else if (seg && seg !== '.') parts.push(seg);
  }
  return parts.join('/');
}

// ---------------------------------------------------------------- the preview's images
// marked's images and DOMPurify's raw <img>s and <picture>s become placeholders (`[image: alt]`) carrying a token, and
// what each names (its sources: a <picture>'s <source>s, then its <img>, each with its media) is kept here, out of the
// document, under that token (mdImageSink, for the one synchronous render that is collecting). The token holds a
// nonce of that render, so the document can't make a placeholder of its own name an image. Once the document is
// sanitized, each placeholder is matched to what it names (mdImageRefs), and previewImages asks /api/file about every
// file of the repo they name and puts each image in place of its placeholder (placeImage), with an address built here
// (sideImage). What the document says is only ever read as a path to look up in the view: a remote image (http, https)
// is never loaded, and its placeholder links to it instead; a file that isn't in the view, or isn't an image, keeps its
// placeholder, with a title that says why.
let mdImageSink = null;
const MD_MAX_IMAGES = 100; // files looked up per preview; the rest keep their placeholders
function mdImageToken(image) {
  mdImageSink.images.push(image);
  return `${mdImageSink.nonce}-${mdImageSink.images.length - 1}`;
}
// A raw <img>'s width, kept only when it is a plain size in pixels or a percentage.
function mdWidth(w) {
  const m = /^\s*(\d{1,5})\s*(px|%)?\s*$/.exec(w || '');
  return m ? m[1] + (m[2] || 'px') : null;
}

// What an image's address names: a file of the repo ({ path }), a remote image ({ remote }), or nothing (another
// scheme, data:, a protocol-relative address). GitHub's #gh-dark-mode-only and #gh-light-mode-only show it in only
// that theme (`only`).
function mdImageTarget(raw, path) {
  const only = /#gh-(dark|light)-mode-only$/.exec(raw || '')?.[1] ?? null;
  if (!raw) return { only };
  if (/^https?:\/\//i.test(raw)) return { remote: raw, only };
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith('//')) return { only };
  const rel = /^([^?#]*)/.exec(raw)[1];
  return rel ? { path: mdPath(rel, path), only } : { only };
}

// Each placeholder the render made, with what it names; one the document made itself (no token of this render) is
// left as the text it is.
function mdImageRefs(root, sink, path) {
  const refs = [];
  for (const span of root.querySelectorAll('[data-md-img]')) {
    const [nonce, i] = span.dataset.mdImg.split('-');
    delete span.dataset.mdImg;
    const image = nonce === sink.nonce ? sink.images[+i] : null;
    if (!image || span.className !== 'md-img') continue;
    const sources = image.sources.map(s => ({ ...s, target: mdImageTarget(s.href, path) }));
    refs.push({ span, alt: image.alt ?? span.textContent.replace(/^\[image(?:: )?|\]$/g, ''), title: image.title ?? span.title,
      width: image.width ?? null, sources, inLink: !!span.closest('a') });
  }
  return refs;
}

// The color scheme the page is drawn in: the theme picked in the header, or the OS's. The OS's query is kept, so that it
// isn't collected with the listener syncPreviewImages puts on it.
const osDark = matchMedia('(prefers-color-scheme: dark)');
const pageDark = () => {
  const t = document.documentElement.dataset.theme;
  return t ? t === 'dark' : osDark.matches;
};
// Whether a <source>'s media fits the page: a prefers-color-scheme query is answered by the page's theme rather than
// the OS's, any other by the browser.
function mdMedia(media) {
  if (!media) return true;
  const scheme = /^\s*\(\s*prefers-color-scheme\s*:\s*(dark|light)\s*\)\s*$/i.exec(media);
  if (scheme) return (scheme[1].toLowerCase() === 'dark') === pageDark();
  try { return matchMedia(media).matches; } catch { return false; }
}

// Looks up the images the document `doc` names (doc._imageRefs) on the side it was read from, with the range it was
// asked with (`at`), and puts them in place. Resolves to what was found, for renderFile to compare: [path, version or
// why it is refused or null] for each file looked up, why in full, since a malformed SVG's error moves as it is edited.
async function previewImages(doc, at) {
  const refs = doc._imageRefs || [];
  const files = new Map();
  for (const ref of refs) {
    for (const { target } of ref.sources) {
      const p = target.path;
      if (p == null || files.has(p) || files.size >= MD_MAX_IMAGES) continue;
      // The tree lists the new side's files; another side's are asked about, and a missing one is no image.
      files.set(p, at.side === 'new' && !fileSet().has(p) ? null
        : api('/api/file', { path: p, side: at.side }, undefined, at.range)
          .then(f => (f.image ? sideImage(at.range, p, at.side, f) : null), () => null));
    }
  }
  const found = new Map(await Promise.all([...files].map(async ([p, img]) => [p, await img])));
  for (const ref of refs) placeImage(ref, found);
  return [...found].map(([p, img]) => [p, img ? img.info.v ?? imageRefusal(img) : null]);
}

// One image of the preview, in place of its placeholder: the source that fits the page's theme (the first whose media
// fits, else the <img>'s), as an image when it is one of the repo's, a link when it is remote (unless it is in a link
// already), or the placeholder, saying why. An image the browser can't load or decode (a request that failed, bytes
// it can't read, an SVG it won't draw) is the placeholder too, once it has failed. One that depends on the theme is
// drawn again when the theme changes (`data-md-themed`, syncPreviewImages), also once it has failed; one for the other
// theme only (#gh-*-mode-only) is hidden.
function placeImage(ref, found) {
  const themed = ref.sources.length > 1 || ref.sources.some(s => s.target.only);
  let el = ref.span;
  // Puts `next` in the image's place: hidden when it is for the other theme only, and with what draws it again.
  const put = (next, only) => {
    if (only && only !== (pageDark() ? 'dark' : 'light')) next.hidden = true;
    if (themed) { next.setAttribute('data-md-themed', ''); next._redraw = draw; }
    el.replaceWith(next);
    el = next;
  };
  const draw = () => {
    const src = ref.sources.find(s => mdMedia(s.media)) ?? ref.sources[ref.sources.length - 1];
    const { path, remote, only } = src.target;
    const text = `[image${ref.alt ? ': ' + ref.alt : ''}]`;
    const img = path != null ? found.get(path) : null;
    let next;
    if (img?.src) {
      // Its width and height are /api/file's, and once it has loaded the browser's if they are others (naturalSize):
      // a width the document gave stays, and they give its proportions. Failed, it gives way to the placeholder,
      // unless a theme change has drawn another source in its place meanwhile.
      const shown = next = h('img', { src: img.src, alt: ref.alt, title: ref.title || null, width: img.info.width,
        height: img.info.height, decoding: 'async', style: ref.width ? `width: ${ref.width}` : null,
        onerror: () => {
          if (el === shown) put(h('span', { class: 'md-img', title: `${src.href} (couldn't be loaded)` }, text), only);
        },
        onload: naturalSize(img.info, (el, w, ht) => { el.width = w; el.height = ht; }) });
    } else if (remote && !ref.inLink) {
      next = h('a', { class: 'md-img', href: remote, target: '_blank', rel: 'noopener noreferrer', title: remote }, text);
    } else {
      const why = img ? imageRefusal(img)
        : path == null ? (remote || src.href || '') : !found.has(path) ? `${src.href} (not looked up: over ${MD_MAX_IMAGES} images)`
        : `${src.href} (not an image in this view)`;
      next = h('span', { class: 'md-img', title: why || null }, text);
    }
    put(next, only);
  };
  draw();
}
// The theme changed (the header's menu, or the OS's while the page follows it): previews pick their images again.
function syncPreviewImages() {
  for (const host of document.querySelectorAll('.md-host')) {
    for (const el of host.shadowRoot?.querySelectorAll('[data-md-themed]') ?? []) el._redraw?.();
  }
}
osDark.addEventListener('change', syncPreviewImages);

// Notes point at source lines, which the preview doesn't have: list them with a link to the file view.
function previewNotes(path) {
  const notes = notesFor(path, state.tree.changes[path]);
  if (!notes.length) return [];
  return [h('div', { class: 'notes-missing' }, notes.map(n =>
    noteBox(n, h('button', { class: 'note-link', onclick: () => jumpToLines(path, n, 'file') }, 'show in file'))))];
}

// `prev` maps paths to the sections of the overview being replaced (a forced render). A section that is still there
// keeps its open/closed state and shows its old diff until the new one loads. An open one also starts at the old one's
// height rather than an estimate: the browser lays out a new section at its --est until it is drawn (see .section in
// style.css), and scroll anchoring would carry any difference above the window into the scroll position.
async function renderOverview(frag, t, prev = null) {
  if (state.mode === 'folder') return frag.append(await folderEmptyState());
  const paths = orderedChangedFiles();
  if (state.mode === 'working' && !paths.length) return frag.append(await emptyState(t));
  if (noCommit()) return frag.append(await noCommitsState());
  frag.append(overviewHead(paths));
  if (!paths.length || needsBase()) return;
  const heights = new Map(prev && [...prev].filter(([, s]) => !s.classList.contains('closed'))
    .map(([p, s]) => [p, s.getBoundingClientRect().height]));
  for (const p of paths.slice(0, overviewLimit)) {
    const old = prev?.get(p);
    const sec = makeSection(p, state.tree.changes[p], old ? old.classList.contains('closed') : undefined);
    if (old) sec._body.replaceChildren(...old._body.childNodes);
    if (heights.has(p)) sec.style.setProperty('--est', heights.get(p) + 'px');
    frag.append(sec);
  }
  frag.append(...moreFiles(paths));
}

// The button that adds the next page of sections, when there are more files.
function moreFiles(paths) {
  const rest = paths.length - overviewLimit;
  return rest > 0 ? [h('button', { class: 'more-files', onclick: () => { overviewLimit += OVERVIEW_PAGE; renderMain(); } },
    `Show ${Math.min(rest, OVERVIEW_PAGE)} more of ${rest} remaining file${rest > 1 ? 's' : ''}`)] : [];
}

// One file's section of the overview. Big diffs (bigChange) start collapsed (and unloaded) so the page stays fast.
function makeSection(p, change, closed = bigChange(p, change)) {
  const body = h('div', { class: 'diff-body' }, skeleton(p, change));
  const sec = h('div', { class: 'section', 'data-path': p });
  const toggle = h('button', { class: 'collapse', onclick: () => {
    const closed = sec.classList.toggle('closed');
    toggle.textContent = closed ? '▸' : '▾';
    // Collapsed sections aren't loaded when they scroll into view, so load on opening (see setSectionChange).
    if (!closed && sec._load && sec._it === intent) {
      const f = sec._load; sec._load = null; observer?.unobserve(sec); f();
    }
  } }, closed ? '▸' : '▾');
  sec.classList.toggle('closed', closed);
  sec.append(fileHead(p, change, [toggle]), body);
  sec._body = body;
  setSectionChange(sec, change);
  return sec;
}

// What a section's diff is fetched for. A section whose change gets a new tag (or status, old path or counts) loads
// again; `_gen` drops a response for an older one. `_it` is the view (intent) it was drawn for: its diff is fetched
// with the range in state, which is another view's once another one is asked for, so it isn't loaded then (the
// observer, opening it and loadPrefetched check), and keeps its `_load` for a render of the same view to take it over
// (updateOverview).
// `_range` is that view's range, which a binary change's images are asked with after its diff is in (drawSectionDiff).
const changeTag = (c) => `${c.status}|${c.oldPath || ''}|${c.v}|${c.add}|${c.del}`;
function setSectionChange(sec, change) {
  const gen = sec._gen = (sec._gen || 0) + 1;
  sec._it = intent; sec._range = rangeParams();
  sec._change = change; sec._tag = changeTag(change);
  sec.style.setProperty('--est', sectionHeight(sec.dataset.path, change) + 'px');
  sec._load = () => fetchDiff(sec.dataset.path, change)
    .then(parsed => { if (sec._gen === gen) drawSectionDiff(sec, parsed); })
    .catch(e => { if (sec._gen === gen) sec._body.textContent = 'Error: ' + e.message; });
}
// A section's diff. A binary change shows its placeholder until /api/file has said which sides are images, unless
// they are known already.
function drawSectionDiff(sec, parsed) {
  const gen = sec._gen, path = sec.dataset.path, change = sec._change;
  const known = parsed.binary ? imageCache.get(imagesKey(path, change, sec._range)) : null;
  showDiff(sec._body, path, change, parsed, known);
  if (!parsed.binary || known !== undefined) return;
  changeImages(path, change, sec._range).then(
    imgs => { if (imgs && sec._gen === gen) showDiff(sec._body, path, change, parsed, imgs); },
    e => { if (sec._gen === gen) sec._body.textContent = 'Error: ' + e.message; });
}

// A live update of the overview on the page. Sections are matched by path: one that is still there stays, with its
// diff, open/closed state and place; one whose change differs keeps showing its old diff until the new one loads
// (when it is near the viewport, or opened); new ones are inserted in order and gone ones removed. Nothing else is
// rebuilt, so the page isn't laid out again. Returns false when a full render is needed (nothing to list).
function updateOverview(main, shown) {
  const paths = orderedChangedFiles();
  const oldHead = main.querySelector(':scope > .overview-head');
  if (!paths.length || needsBase() || !oldHead) return false;
  const head = overviewHead(paths);
  if (head.textContent !== oldHead.textContent) oldHead.replaceWith(head);
  const wanted = paths.slice(0, overviewLimit), keep = new Set(wanted);
  const secs = new Map();
  for (const s of main.querySelectorAll(':scope > .section')) {
    if (keep.has(s.dataset.path)) secs.set(s.dataset.path, s); else s.remove();
  }
  let at = main.querySelector(':scope > .overview-head');
  for (const p of wanted) {
    const change = state.tree.changes[p];
    let sec = secs.get(p);
    if (!sec) {
      sec = makeSection(p, change);
      observer?.observe(sec);
    } else if (sec._tag !== changeTag(change)) {
      sec.querySelector('.file-head').replaceWith(fileHead(p, change, [sec.querySelector('.collapse')]));
      setSectionChange(sec, change);
      observer?.unobserve(sec); observer?.observe(sec); // observe again: a section already loaded was unobserved
    } else {
      sec._change = change;
      // Drawn for a view replaced since by this one (the same view asked for again): it loads for this one now.
      if (sec._it !== intent) {
        sec._it = intent;
        if (sec._load) { observer?.unobserve(sec); observer?.observe(sec); }
      }
    }
    if (at.nextElementSibling !== sec) at.after(sec);
    at = sec;
  }
  main.querySelector(':scope > .more-files')?.remove();
  main.append(...moreFiles(paths));
  keepNotes(shown);
  return true;
}

// The checked-out branch, as a sentence or title names it. /api/info names a detached HEAD by its short sha, which
// would read as a branch's name there ("No uncommitted changes on 1a2b3c4"), so it is called what it is.
const checkedOutName = () => state.info?.detached ? `detached HEAD ${state.info.branch}` : state.info?.branch;

// The top of the overview: the commit (subject, meta, body), the branch comparison, or the uncommitted changes.
function overviewHead(paths) {
  const head = h('div', { class: 'overview-head' });
  const files = `${paths.length} file${paths.length > 1 ? 's' : ''}`;
  const counts = () => {
    let adds = 0, dels = 0;
    for (const p of paths) { adds += state.tree.changes[p].add || 0; dels += state.tree.changes[p].del || 0; }
    return h('div', { class: 'meta' }, paths.length
      ? h('span', {}, files + ' · ', h('span', { class: 'stat' }, h('span', { class: 'a' }, '+' + adds), ' ', h('span', { class: 'd' }, '−' + dels)))
      : 'Nothing changed here.');
  };
  if (state.mode === 'working') head.append(h('h2', {}, 'Uncommitted changes'), counts());
  else if (needsBase()) {
    head.append(h('h2', {}, 'Choose a base branch'),
      h('div', { class: 'meta' }, 'No default base branch (main, master, develop…) was found. Choose a base in "compared with" above.'));
  } else if (state.mode === 'branch') {
    head.append(h('h2', {}, `${state.head === 'HEAD' ? checkedOutName() || 'HEAD' : state.head} vs ${state.base}`), counts());
  } else {
    const c = currentCommit();
    head.append(h('h2', {}, c?.subject || 'Commit'));
    if (!c) return head.append(counts()), head;
    const parents = c.parents || [];
    const parentLink = (p) => h('button', { class: 'link', title: 'Show the parent commit',
      onclick: () => applyView({ mode: 'commit', ref: p, head: state.head }).catch(e => toast(e.message)) }, p.slice(0, 7));
    head.append(h('div', { class: 'meta' },
      h('code', {}, c.short), h('span', {}, c.author),
      h('span', { title: new Date(c.time * 1000).toLocaleString() }, `${commitDate(c.time)} (${longAgo(c.time)})`),
      parents.length ? h('span', {}, parents.length > 1 ? 'parents ' : 'parent ', parents.flatMap((p, i) => [i ? ', ' : null, parentLink(p)])) : null,
      paths.length ? h('span', {}, files) : null));
    if (c.body) {
      // Clickable only when the clamp actually hides something, which is known once the body is laid out.
      const body = h('div', { class: 'commit-body clamp' }, c.body);
      requestAnimationFrame(() => {
        if (!body.isConnected || body.scrollHeight <= body.clientHeight) return;
        body.classList.add('more'); body.title = 'Show the whole message';
        body.addEventListener('click', () => { body.classList.remove('clamp', 'more'); body.removeAttribute('title'); }, { once: true });
      });
      head.append(body);
    }
  }
  return head;
}

// "Oct 18, 21:23", with the year when it isn't this one.
function commitDate(t) {
  const d = new Date(t * 1000);
  const opts = { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
  return d.toLocaleString(undefined, opts);
}

// About how tall a change's diff will be, in px, before it has loaded: the skeleton's height, and with its header an
// overview section's size while the browser skips laying it out (content-visibility). The closer it is, the less the
// page jumps as diffs arrive. A split row holds a removed and an added line, so a file takes about as many rows as its
// larger side; unified takes both. The hunks aren't known yet: about √n / 2 of them for n changed lines (fitted to
// this repo's history), each with a header and its context lines. Notes add their boxes; wrapped lines aren't counted.
// A binary file that looks like an image by its name is about as tall as the overview draws one, with its caption.
const FILE_HEAD_PX = 34, HUNK_PX = 26, NOTE_PX = 56, PLACEHOLDER_PX = 51, IMAGE_PX = OVERVIEW_IMAGE_PX + 50;
function diffHeight(path, change) {
  const line = cssPx('--line-h');
  if (binaryChange(change)) return IMAGE_PATH.test(path) ? IMAGE_PX : PLACEHOLDER_PX;
  const add = change.add || 0, del = change.del || 0, n = add + del;
  if (!n) return PLACEHOLDER_PX; // a rename or mode change: "No textual changes"
  const rows = diffLayout() === 'split' ? Math.max(add, del) : n;
  // A new, deleted or untracked file is one hunk of nothing but changes; with all context, any file is one hunk.
  const whole = change.status === 'A' || change.status === 'D' || change.status === '?';
  const hunks = whole || state.context === 'all' ? 1 : Math.max(1, Math.sqrt(n) / 2);
  const context = whole ? 0 : 2 * (state.context === 'all' ? 10 : Math.min(+state.context, 10));
  return Math.round((rows + hunks * context) * line + hunks * HUNK_PX + notesFor(path, change).length * NOTE_PX);
}
const sectionHeight = (path, change) => FILE_HEAD_PX + diffHeight(path, change);
// A section's diff while it loads: about the height it will have, with a few bars for code.
function skeleton(path, change) {
  return h('div', { class: 'skeleton', style: `height:${diffHeight(path, change)}px`, 'aria-label': 'Loading' }, h('span'), h('span'), h('span'));
}

// Changes mode with nothing to show: which branch, whether revue is watching, and the latest commit. Live updates
// re-render it on every edit, so it reuses what it can: /api/server (only its watcher state is used here) for a few
// seconds, shared with the status card, and the latest commit until the next `git` live update. The commit is reused
// only while that server answer is fresh and says the watcher sees every change, and the event stream is up, so at
// worst it is fetched again every few seconds.
let emptyLatest = null; // { key, value }
async function emptyState(t) {
  const branch = checkedOutName();
  const cached = serverCache && Date.now() - serverCache.at < SERVER_CACHE_MS ? serverCache.value : null;
  const key = `${gitEvents} ${branch}`;
  const reuse = emptyLatest?.key === key && liveState === 'on' && cached?.watcher === 'ok';
  const [latest, server] = await Promise.all([
    state.info?.hasHead === false ? []
      : reuse ? emptyLatest.value
      : api('/api/log', { limit: 1, branch: 'HEAD', all: '0' }).then(v => (emptyLatest = { key, value: v }).value, () => []),
    cached || fetchServer().catch(() => null),
  ]);
  const watching = liveState !== 'off' && server?.watcher !== 'off';
  const box = h('div', { class: 'empty-state' },
    h('h2', {}, branch ? `No uncommitted changes on ${branch}` : 'No uncommitted changes'),
    h('div', { class: 'watching' + (watching ? ' on' : '') }, watching
      ? 'Watching the working tree. Edits show up here as they happen.'
      : 'Live updates are off. Press r to refresh.'));
  const c = latest[0];
  if (c && t.fresh()) {
    box.append(h('div', { class: 'latest' },
      h('div', { class: 'what' },
        h('div', { class: 'when', title: new Date(c.time * 1000).toLocaleString() }, `Latest commit · ${shortAgo(c.time)}`),
        h('div', { class: 'subj', title: c.subject }, h('code', {}, c.short), c.subject)),
      h('button', { title: 'Show this commit in History', onclick: () => { state.ref = c.sha; setMode('commit', { keepRef: true }).catch(e => toast(e.message)); } },
        'Open in History', h('kbd', {}, '2'))));
  }
  return box;
}

// History or Branch mode with no commit to show (noCommit): there are none yet. The first one made shows up by itself
// while revue is watching (refreshCommits picks it in History, and Branch mode compares it), or on r (reloadAll).
async function noCommitsState() {
  const branch = state.mode === 'commit' && state.allBranches ? null : state.head === 'HEAD' ? checkedOutName() : state.head;
  const cached = serverCache && Date.now() - serverCache.at < SERVER_CACHE_MS ? serverCache.value : null;
  const server = cached || await fetchServer().catch(() => null);
  const watching = liveState !== 'off' && server?.watcher !== 'off';
  return h('div', { class: 'empty-state' },
    h('h2', {}, branch ? `No commits on ${branch} yet` : 'No commits yet'),
    h('div', { class: 'watching' + (watching ? ' on' : '') }, watching
      ? 'Watching the repository. Commits show up here as they are made.'
      : 'Live updates are off. Press r to refresh.'));
}

// A plain folder with no file open: one with no README at its root (see setFile), or none picked yet.
async function folderEmptyState() {
  const cached = serverCache && Date.now() - serverCache.at < SERVER_CACHE_MS ? serverCache.value : null;
  const server = cached || await fetchServer().catch(() => null);
  const watching = liveState !== 'off' && server?.watcher !== 'off';
  return h('div', { class: 'empty-state' },
    h('h2', {}, 'Pick a file from the tree'),
    h('div', { class: 'watching' + (watching ? ' on' : '') }, watching
      ? 'Watching the folder. Files show up in the tree as they are added.'
      : 'Live updates are off. Press r to refresh.'));
}

// Lazily fetch diffs in the overview as sections scroll into view. Sections that show up together
// are fetched in one /api/diffs request (opening a collapsed one uses its own _load).
let observer, diffQueue = [];
function observeSections() {
  observer?.disconnect();
  observer = new IntersectionObserver((entries) => {
    for (const { target: s, isIntersecting } of entries) {
      if (!isIntersecting || !s._load || s._it !== intent || s.classList.contains('closed')) continue;
      s._load = null; observer.unobserve(s);
      if (!diffQueue.length) queueMicrotask(flushDiffs);
      diffQueue.push(s);
    }
  }, { root: $('#main'), rootMargin: '800px' });
  document.querySelectorAll('#main .section').forEach(s => observer.observe(s));
}
function flushDiffs() {
  const secs = diffQueue; diffQueue = [];
  for (let i = 0; i < secs.length; i += 50) loadDiffs(secs.slice(i, i + 50)); // 50: the server's cap
}
// Sections whose diff is cached are drawn without a request. Drawing yields to the browser between sections once
// it has run for DRAW_SLICE ms, so scrolling and input aren't held up while dozens of diffs are drawn.
const DRAW_SLICE = 30;
const yieldTask = () => (globalThis.scheduler?.yield ? scheduler.yield() : new Promise(r => setTimeout(r)));
async function loadDiffs(secs) {
  const gens = new Map(secs.map(s => [s, s._gen])), keys = new Map(secs.map(s => [s, diffKey(s.dataset.path, s._change)]));
  const ready = new Map(), want = [];
  for (const s of secs) { const p = cachedDiff(keys.get(s)); p ? ready.set(s, p) : want.push(s); }
  let diffs = {}, err;
  if (want.length) {
    const q = [['context', state.context]];
    for (const s of want) q.push(['path', s.dataset.path], ['old', s._change.oldPath || ''], ['status', s._change.status]);
    try { diffs = (await fetchDiffs(q)).diffs || {}; } catch (e) { err = e; }
  }
  let slice = performance.now();
  for (const s of secs) {
    if (performance.now() - slice > DRAW_SLICE) { await yieldTask(); slice = performance.now(); }
    if (!s.isConnected || s._gen !== gens.get(s)) continue; // a newer render replaced this section, or its change
    try {
      let parsed = ready.get(s);
      if (!parsed) {
        if (err) throw err;
        const d = Object.hasOwn(diffs, s.dataset.path) ? diffs[s.dataset.path] : null;
        if (!isDiff(d)) throw new Error('no diff returned');
        parsed = cacheDiff(keys.get(s), d);
      }
      drawSectionDiff(s, parsed);
    } catch (e) { s._body.textContent = 'Error: ' + e.message; }
  }
}

// ---------------------------------------------------------------- overview prefetch
// The overview asks for its diffs once its sections are laid out and seen by the observer. The first screen's are
// requested as soon as the tree arrives instead (prefetchDiffs), and the observer's request takes them from here
// (fetchDiffs). Entries are promises, so a request still under way is shared rather than repeated. They are dropped
// by every new tree (loadTree), and after PREFETCH_MS, so a diff fetched before the file changed is never shown.
const PREFETCH_MS = 10000, PREFETCH_MAX = 50; // 50: the server's cap per request
const PREFETCH_WAIT = 200; // how long the first tree waits for the first diffs (see refresh)
let prefetched = new Map(); // key -> { at, diff: Promise<string> }
let earlyCached = new Set(); // the diffKeys of the first screen's diffs that are cached, which aren't asked for
const prefetchKey = (path, old, status, context) => JSON.stringify([rangeParams(), path, old, status, context]);
function dropPrefetched() { prefetched = new Map(); earlyCached = new Set(); }

// The changed files the observer would load first: those whose sections (at their placeholder heights, see
// skeleton) fall within the first screen and the observer's margin. Big diffs start collapsed and aren't loaded, and
// cached ones (an overview seen before) are drawn without a request. Returns whether any of these sections can be
// drawn early (loadPrefetched): asked for here, or cached.
function prefetchDiffs() {
  const paths = orderedChangedFiles();
  if (!paths.length || needsBase()) return false;
  const want = [];
  earlyCached = new Set();
  let y = 120; // the overview's heading
  for (const p of paths) {
    if (y > innerHeight + 800 || want.length === PREFETCH_MAX) break;
    const c = state.tree.changes[p], k = diffKey(p, c);
    if (bigChange(p, c)) { y += FILE_HEAD_PX; continue; } // starts collapsed
    if (diffCache.has(k)) earlyCached.add(k); else want.push(p);
    y += sectionHeight(p, c); // the skeleton's height, and the header's
  }
  if (!want.length) return earlyCached.size > 0;
  const context = state.context, q = [['context', context]];
  for (const p of want) q.push(['path', p], ['old', state.tree.changes[p].oldPath || ''], ['status', state.tree.changes[p].status]);
  const res = api('/api/diffs', q);
  const at = Date.now();
  for (const p of want) {
    const c = state.tree.changes[p];
    // A failed prefetch is retried by the section's own request (fetchDiffs), which still sees the rejection. It is
    // handled here as well: fetchDiffs takes an entry only once its section loads, after the first paint, and never
    // takes one a newer tree dropped or one for a context changed meanwhile.
    const diff = res.then(r => r.diffs?.[p]);
    diff.catch(() => {});
    prefetched.set(prefetchKey(p, c.oldPath || '', c.status, context), { at, diff });
  }
  return true;
}

// /api/diffs for the [key, value] pairs `q` (context, then path/old/status per file), served from prefetched diffs
// where there are some.
async function fetchDiffs(q) {
  const context = q.find(([k]) => k === 'context')?.[1] ?? state.context;
  const files = [];
  for (let i = 0; i < q.length; i++) if (q[i][0] === 'path') files.push([q[i][1], q[i + 1][1], q[i + 2][1]]);
  const diffs = {}, waits = [], rest = [['context', context]];
  for (const [path, old, status] of files) {
    const k = prefetchKey(path, old, status, context), hit = prefetched.get(k);
    prefetched.delete(k);
    const ask = () => { rest.push(['path', path], ['old', old], ['status', status]); };
    if (hit && Date.now() - hit.at < PREFETCH_MS) waits.push(hit.diff.then(d => { if (isDiff(d)) diffs[path] = d; else ask(); }, ask));
    else ask();
  }
  await Promise.all(waits);
  if (rest.length > 1) Object.assign(diffs, (await api('/api/diffs', rest)).diffs);
  return { diffs };
}

// Load the first screen's sections (those with prefetched diffs, and those whose diffs are cached) now, without
// waiting for the observer, whose callbacks Chrome runs only after other pending work (like building the tree).
// Returns their loading, or null when there are none.
function loadPrefetched() {
  const secs = [...document.querySelectorAll('#main .section')].filter(s => s._load && s._it === intent
    && !s.classList.contains('closed')
    && (prefetched.has(prefetchKey(s.dataset.path, s._change.oldPath || '', s._change.status, state.context))
      || earlyCached.has(diffKey(s.dataset.path, s._change))));
  for (const s of secs) { s._load = null; observer?.unobserve(s); }
  return secs.length ? loadDiffs(secs) : null;
}

// ---------------------------------------------------------------- commits
// A page of the history the commit list shows: the branch in the header's, or every branch's. A branch that doesn't
// exist falls back to the checked-out one (dropHead), and the page is fetched again for it.
async function fetchLog({ limit, skip }) {
  const head = state.head;
  try {
    return await api('/api/log', { limit, skip, all: state.allBranches ? '1' : '0', branch: head });
  } catch (e) {
    if (head === 'HEAD' || !missingRef(e, head)) throw e;
    await dropHead(head);
    return fetchLog({ limit, skip });
  }
}

// more: append the next page. keep: reload at least this many (live updates keep "Load more" pages).
// render: false when the caller draws the list itself (setMode, once it knows which commit is selected).
// The list belongs to the page (the branch in the header, all branches or not) rather than to a view, so a commit picked
// while it loads doesn't drop it: only a newer load of the list does.
async function loadCommits(more = false, keep = 0, render = true) {
  const t = ticket('commits', { page: true });
  const skip = more ? state.commits.length : 0;
  const want = more ? 200 : Math.max(200, keep);
  // Ask for one extra commit to know whether there are more.
  let list = [], done = false;
  while (!done && list.length <= want) {
    const limit = Math.min(2000, want + 1 - list.length); // 2000: the server's cap per request
    const page = await fetchLog({ limit, skip: skip + list.length });
    list = list.concat(page);
    done = page.length < limit;
  }
  if (!t.fresh()) return;
  state._moreCommits = list.length > want;
  list = list.slice(0, want);
  state.commits = more ? state.commits.concat(list) : list;
  if (more) appendCommits(list);
  else if (render) renderCommits();
}
// A live update in History: fetch the first page only, and reload every loaded page ("Load more"
// included) only when it differs from what's shown.
async function refreshCommits() {
  const it = intent, t = ticket('commits', { page: true, peek: true });
  const page = await fetchLog({ limit: 200, skip: 0 });
  if (!t.fresh()) return; // a newer load (mode or branch change) is under way
  const same = page.every((c, i) => c.sha === state.commits[i]?.sha && c.refs === state.commits[i].refs)
    && (page.length === 200 || page.length === state.commits.length);
  if (!same) await loadCommits(false, state.commits.length);
  // History had no commit to show (none yet): the first one made is shown, as History opens at the latest. Not when
  // another view was asked for meanwhile: the commit is that view's to name.
  if (it === intent && state.mode === 'commit' && !state.ref && state.commits.length) {
    state.ref = state.commits[0].sha;
    if (!markCommit()) renderCommits();
  }
}

// The commit state.ref names: from the loaded history, or fetched on its own, since a link or an
// agent can name a commit older than the loaded pages (or on another branch).
function currentCommit() {
  return state.commits.find(c => c.sha === state.ref) || refCommits.get(state.ref) || null;
}
// Commits looked up on their own, by the ref that named them and by their sha. An answer is about its ref, so it is kept
// whatever view the page is on by the time it comes (it used to be dropped then, and an answer kept for the commit named
// last could be another one's). Only the view that asked for it has its short sha replaced by the full one, which the
// list matches on. A sha always names the same commit, so a commit named by one (or by a short one) is looked up once.
// A branch or tag name moves, so the commit it names is looked up again whenever a view naming it loads or reloads (a
// `git` live update, r), and the one found for it before is only shown until then. Returns whether the commit the view
// names is another one than before.
const refCommits = new Map();
async function resolveRefCommit() {
  const it = intent, ref = state.ref;
  if (state.mode !== 'commit' || !ref || state.commits.some(c => c.sha === ref)) return false;
  const before = refCommits.get(ref);
  if (before?.sha.startsWith(ref)) return false;
  let commit;
  try { [commit] = await api('/api/log', { branch: ref, limit: 1, all: '0' }); } catch { return false; }
  if (!commit) return false;
  refCommits.set(ref, commit).set(commit.sha, commit);
  if (it === intent && state.ref === ref && commit.sha.startsWith(ref)) state.ref = commit.sha;
  return commit.sha !== before?.sha;
}

// One row of the commit list. Clicks are handled once for the whole list (onCommitClick).
function commitRow(c, cur, pinned = false) {
  const refs = c.refs ? c.refs.split(', ').map(r => r.replace('HEAD -> ', '')) : [];
  if (pinned) refs.unshift('linked');
  const merge = (c.parents?.length || 0) > 1;
  return h('li', { class: c.sha === cur?.sha ? 'sel' : '', title: c.subject,
    role: 'option', tabindex: '-1', 'aria-selected': c.sha === cur?.sha ? 'true' : 'false', 'data-sha': c.sha, 'data-pinned': pinned },
    h('div', { class: 'line1' },
      h('span', { class: 'merge', title: merge ? 'Merge commit' : null }, merge ? '⑂' : ''),
      h('span', { class: 'subj' }, merge ? mergeTitle(c) : c.subject),
      refs.length ? h('span', { class: 'refs', title: refs[0] === 'linked' ? 'Not in the loaded history' : refs[0] }, refs[0]) : null,
      refs.length > 1 ? h('span', { class: 'refs-more', title: refs.slice(1).join('\n') }, '+' + (refs.length - 1)) : null),
    h('div', { class: 'meta' }, h('code', {}, c.short), h('span', { class: 'author' }, c.author),
      h('span', { class: 'age', title: new Date(c.time * 1000).toLocaleString() }, shortAgo(c.time))));
}
const moreRow = () => h('li', { class: 'more', role: 'option', tabindex: '-1' }, 'Load more…');

function renderCommits() {
  const ul = $('#commits');
  const focused = document.activeElement?.closest?.('#commits li');
  const focusSha = focused?.dataset.sha ?? (focused?.classList.contains('more') ? 'more' : null);
  ul.textContent = '';
  const cur = currentCommit();
  // Not in the loaded history: list it first so it's still shown and selected.
  const pinned = cur && !state.commits.some(c => c.sha === cur.sha) ? cur : null;
  if (pinned) ul.append(commitRow(pinned, cur, true));
  for (const c of state.commits) ul.append(commitRow(c, cur));
  if (state._moreCommits) ul.append(moreRow());
  if (!state.commits.length) ul.append(h('li', { class: 'muted' }, 'No commits yet'));
  // As in the tree: the focused commit (or "Load more…") keeps focus, else the selected one is the tab stop.
  const keep = focusSha && (focusSha === 'more' ? ul.querySelector('li.more') : ul.querySelector(`li[data-sha="${CSS.escape(focusSha)}"]`));
  const stop = keep || ul.querySelector('li.sel') || ul.querySelector('li[role=option]');
  if (stop) { stop.tabIndex = 0; if (keep) keep.focus({ preventScroll: true }); }
  // As in the tree, the scroll position never moved, so it isn't written back.
}

// "Load more…": add the new page's rows after the ones shown. Focus on "Load more…" moves to the new one, or to the
// first new commit once there are no more.
function appendCommits(list) {
  const ul = $('#commits');
  const old = ul.querySelector('li.more'), hadFocus = !!old && old === document.activeElement;
  const cur = currentCommit();
  // A shown "linked" commit that this page brings into the history would be listed twice: draw it all again.
  if (ul.querySelector('li[data-pinned]') && list.some(c => c.sha === cur?.sha)) return renderCommits();
  const frag = document.createDocumentFragment();
  for (const c of list) frag.append(commitRow(c, cur));
  const more = state._moreCommits ? moreRow() : null;
  if (more) frag.append(more);
  const first = frag.firstChild;
  old?.remove();
  ul.append(frag);
  if (!hadFocus) return;
  const next = more || first;
  if (next) { rovingStop(ul, 'li[role=option]', next); next.focus({ preventScroll: true }); }
}

// Move the selection to state.ref without rebuilding the list, as markSelected does in the tree. False when the list
// has to be drawn again: the commit isn't in it, or a "linked" commit shown first has to go.
function markCommit() {
  const ul = $('#commits');
  const row = state.ref && ul.querySelector(`li[data-sha="${CSS.escape(state.ref)}"]`);
  if (!row || ul.querySelector('li[data-pinned]')) return false;
  for (const n of ul.querySelectorAll('li.sel')) { n.classList.remove('sel'); n.setAttribute('aria-selected', 'false'); }
  row.classList.add('sel'); row.setAttribute('aria-selected', 'true');
  // A click focuses the row it hit, which then holds the tab stop; otherwise it is the selected commit.
  const focused = document.activeElement?.closest?.('#commits li[role=option]');
  rovingStop(ul, 'li[role=option]', focused && ul.contains(focused) ? focused : row);
  return true;
}

function onCommitClick(e) {
  const li = e.target.closest('li[role=option]');
  if (!li) return;
  toasted(li.classList.contains('more') ? loadCommits(true) : selectCommit(li.dataset.sha));
}
// A merge's subject without the wrapper the forge put around the pull request's title. GitHub's subject has no
// title ("Merge pull request #12 from user/branch"); it's the first line of the body.
function mergeTitle(c) {
  const gitea = /^Merge pull request '(.+)' \(#\d+\) from \S+ into \S+$/.exec(c.subject);
  if (gitea) return gitea[1];
  const github = /^Merge pull request (#\d+) from \S+$/.exec(c.subject);
  if (github) return c.body?.split('\n')[0].trim() || 'Pull request ' + github[1];
  return c.subject;
}

// Ages: compact for lists ("20d", "3h", "5m"; days up to two months), and in words for the commit header.
function shortAgo(t) {
  const s = Date.now() / 1000 - t;
  if (s >= 365 * 86400) return Math.floor(s / (365 * 86400)) + 'y';
  if (s >= 60 * 86400) return Math.floor(s / (30 * 86400)) + 'mo';
  if (s >= 86400) return Math.floor(s / 86400) + 'd';
  if (s >= 3600) return Math.floor(s / 3600) + 'h';
  if (s >= 60) return Math.floor(s / 60) + 'm';
  return 'now';
}
function longAgo(t) {
  const s = Date.now() / 1000 - t;
  for (const [n, word] of [[365 * 86400, 'year'], [30 * 86400, 'month'], [86400, 'day'], [3600, 'hour'], [60, 'minute']]) {
    if (s >= n) { const k = Math.floor(s / n); return `${k} ${word}${k > 1 ? 's' : ''} ago`; }
  }
  return 'just now';
}
async function selectCommit(sha) {
  closeDrawer();
  newIntent();
  state.ref = sha;
  if (!markCommit()) renderCommits();
  await refresh({ resetFile: false });
}

// ---------------------------------------------------------------- actions
// The tree and notes of the current range; `params` replace range params (History's branch tip, before its sha is known).
// The tree's ticket is taken here, as the request starts, and comes with the answer for loadTree.
function fetchTree(params = {}) {
  const t = ticket('tree');
  if (noRange(params)) return Promise.all([{ files: [], changes: {} }, fetchNotes(params), t]);
  return Promise.all([api('/api/tree', params), fetchNotes(params), t]);
}
// fetched: a fetchTree() already under way. Returns whether the tree was stored: not when the view it was asked for was
// replaced meanwhile (another view, mode or commit), nor when a load for the same view that started after it (a live
// update's) has stored its tree already.
async function loadTree(fetched = null) {
  const it = intent;
  const { mode, base, head, ref } = state;
  let tree, notes, t;
  try {
    [tree, notes, t] = await (fetched || fetchTree());
  } catch (e) {
    // The error of a view left meanwhile is no one's, and neither is what it would fall back to.
    if (it !== intent) return false;
    // A base, branch or commit that doesn't exist: show what it falls back to instead (see dropBase), and load that.
    if (mode === 'branch' && base && missingRef(e, base)) await dropBase(base);
    else if (mode === 'branch' && head !== 'HEAD' && missingRef(e, head)) await dropHead(head);
    else if (mode === 'commit' && ref && missingRef(e, ref)) await dropRef(ref);
    else throw e;
    return it === intent && loadTree();
  }
  if (!t.apply()) return false;
  dropPrefetched(); // diffs fetched before this tree may be out of date
  // A live update usually brings the same tree back (the event was for a file whose counts didn't
  // change, or for the index); then the tree and its folders are left as they are.
  // Each change's version tag ("v") is left out: it moves with every edit, which the tree doesn't show. When only tags
  // moved, they are copied into the tree in place, which keeps what is derived from it (perTree).
  const sig = JSON.stringify([rangeParams(), tree], (k, v) => (k === 'v' && typeof v === 'string' ? undefined : v));
  if (sig !== state.treeSig) { state.treeSig = sig; state.tree = tree; state.treeDirty = true; }
  else for (const [p, c] of Object.entries(tree.changes)) state.tree.changes[p].v = c.v;
  storeNotes(notes); // unless a request that started after this tree's has brought newer ones (loadNotes)
  return true;
}

const needsBase = () => state.mode === 'branch' && !state.base;
// No range to list or compare: Branch mode with no base (the repo has no main, master, develop...), which waits for the
// user to pick one rather than let the server compare with HEAD, and a view with no commit (noCommit). `params` replace
// range params (History's branch tip names its commit).
const noRange = (params = {}) => (noCommit() && !params.ref) || needsBase();
// The checked-out branch has no commits yet (a repo with no commits, or a branch born with `git switch --orphan`).
const unborn = () => state.head === 'HEAD' && state.info?.hasHead === false;
// No commit to show, where the server would look up HEAD, which doesn't exist: History when no commit is named and the
// list is empty, and Branch mode on an unborn branch, which has added nothing to compare with any base (so this comes
// before asking for a base, which a repo with no commits has none of to offer).
const noCommit = () => (state.mode === 'commit' && !state.ref && !state.commits.length) || (state.mode === 'branch' && unborn());

// Whether a request failed because `name`, one of the refs the view names, doesn't exist (or isn't a name check_ref
// accepts). These are the server's messages that name the ref they are about, so an error about another ref, or any
// other error, is never taken for it: git's "bad revision" (a base, or /api/log's branch), the tree listing's "Not a
// valid object name" (a branch or commit), the notes' "unknown revision" (a commit) and check_ref's "bad ref".
const missingRef = (e, name) => [`fatal: bad revision '${name}'`, `fatal: Not a valid object name ${name}`,
  `unknown revision: ${name}`, `bad ref: ${name}`].includes(e.message);

// A base, branch or commit that doesn't exist (an old link, a typo, a deleted branch) can come from the URL, Back, or a
// live update. Each is replaced by what the view falls back to, and a toast says so; a view that moved on meanwhile is
// left alone.
// The base: the default base, or none when there is no default, which asks for one ("Choose a base branch"). The
// default comes from /api/info, which may still be on its way on startup.
async function dropBase(base) {
  const it = intent;
  await infoLoaded;
  if (it !== intent || state.mode !== 'branch' || state.base !== base) return; // the view moved on, or replaced already
  const fallback = state.info.defaultBase !== base ? state.info.defaultBase : null;
  state.base = fallback;
  toast(`Base '${base}' doesn't exist${fallback ? `; comparing with ${fallback}` : ''}`);
  syncControls();
}
// The branch in the header (Branch mode's head, History's branch): the checked-out branch, named from /api/info.
async function dropHead(head) {
  const it = intent;
  await infoLoaded;
  if (it !== intent || state.head !== head) return;
  state.head = 'HEAD';
  toast(`Branch '${head}' doesn't exist; showing ${checkedOutName() || 'HEAD'}`);
  syncControls();
}
// History's commit: the latest commit, which History opens at, fetched here since the list may still be on its way.
// In a repo with no commits there is none, and History shows what it shows without one.
async function dropRef(ref) {
  const it = intent;
  const [latest] = await fetchLog({ limit: 1, skip: 0 });
  if (it !== intent || state.mode !== 'commit' || state.ref !== ref) return;
  state.ref = latest?.sha ?? null;
  toast(`Commit '${ref}' doesn't exist${latest ? '; showing the latest commit' : ''}`);
  if (!markCommit()) renderCommits();
}

// Resolves once the frame being prepared has been painted (or soon, in a tab that isn't painting).
const afterPaint = () => new Promise(r => {
  if (document.hidden) return r();
  requestAnimationFrame(() => setTimeout(r));
  setTimeout(r, 1000);
});

// tree: a loadTree() already started (setMode starts it early). The intent's `select` is a view to show once the tree
// is in ({ file, line, endLine, side, view }, as from parseView), so a link to a file renders that file and never the
// overview first. force: draw the main pane again even where nothing seems to have changed (the r key).
// The page draws nothing before /api/info is in, whichever way the view was asked for: info names the checked-out branch
// and the default base that Branch mode's title and Changes' empty state show. The wait comes before the tree is
// awaited, so a tree that fails is reported once info is in, in the order the page would have drawn it.
// A refresh draws only for the intent it was asked for (the one current when it was called): a newer view, mode change
// or commit pick drops it, even when its tree is in, since the state is the newer view's. Within the intent, one whose
// tree a newer load (a live update) superseded returns before drawing anything, so resetFile, select and force wait on
// the intent for the refresh that does draw, and a file pick drops select and resetFile.
async function refresh({ resetFile = false, tree = null, force = false } = {}) {
  const it = intent;
  if (resetFile) it.resetFile = true;
  if (force) it.force = true;
  const loaded = tree || loadTree();
  loaded.catch(() => {}); // awaited below, but it may fail while info is awaited
  if (!state.info) await infoLoaded;
  let ok;
  try {
    ok = await loaded;
  } catch (e) {
    if (it === intent && !it.drawn) showLoadError(it, e);
    throw e;
  }
  if (!ok || it !== intent) return;
  // Its first draw shows its mode, branch and base in the controls too (the branch lists come from info).
  if (!it.drawn) { it.drawn = true; syncControls(); }
  let select;
  ({ resetFile, select, force } = it);
  it.resetFile = false; it.select = null; it.force = false;
  if (resetFile) state.file = null;
  if (select) { if (it.drawer) closeDrawer(); if (setFile(select.file ?? null, select)) state.treeDirty = true; }
  // A diff whose change is gone (a live update that reverted the edit, another commit that didn't touch the file) has
  // nothing left to show: the file goes to the view an unchanged one opens in (setFile), and the hash says so below.
  // It stays there if the change comes back, as a view the page picked for a file does.
  if (state.file && state.view === 'diff' && !state.tree.changes[state.file]) state.view = unchangedView(state.file, state.line);
  const dirty = state.treeDirty;
  if (dirty) expandChanged(); else markSelected();
  writeHash();
  const prefetch = !state.file && $('#main').dataset.key !== key() && prefetchDiffs();
  // The main pane goes first and the tree is built after it is painted: a big tree takes a while to build and lay
  // out, and the changes are what the page is for. The first diffs go before the tree too.
  const shown = renderMain({ force });
  if (dirty || prefetch) {
    await afterPaint();
    // The tree waits for them, up to PREFETCH_WAIT ms, and for their frame to be painted. Not once a newer view was asked
    // for: the prefetched diffs are keyed by the range in state, which is that view's.
    const diffs = prefetch && it === intent && loadPrefetched();
    if (diffs) { await Promise.race([diffs, new Promise(r => setTimeout(r, PREFETCH_WAIT))]); await afterPaint(); }
    // The tree is built even then: until the newer view is drawn, this one is on the page, and its tree is the one to
    // pick files from.
    if (state.treeDirty) renderTree();
  }
  if (select?.file) document.querySelector(`#tree .node[data-path="${CSS.escape(select.file)}"]`)?.scrollIntoView({ block: 'nearest' });
  await shown;
}

// A view whose tree failed to load, with nothing to fall back to (loadTree), is its error: drawn in place of the last
// view's rows, which belong to another range. The view is on the page then, so it counts as shown (notes go on it,
// Ctrl+click and Clear all work), and its notes are loaded, since the notes bar would otherwise still count the last
// view's while Clear all clears this one's. A view drawn already keeps its rows when a live update fails.
function showLoadError(it, e) {
  const main = $('#main');
  main.replaceChildren(h('div', { class: 'placeholder' }, 'Error: ' + e.message));
  main._shown = null; main.dataset.key = key();
  syncControls(); // its mode, branch and base, which its first draw would show
  markShown(it);
  fetchNotes().then(storeNotes);
}

// The view an unchanged file opens in, which has no diff: markdown rendered and an SVG drawn (hasPreview), anything
// else in the file view, which a target line always needs, since only it numbers the lines.
const unchangedView = (path, line) => !line && hasPreview(path) ? 'preview' : 'file';

// Set the selected file and how to show it. Returns whether a folder had to open to show it in the tree.
// A plain folder has no overview: no file (startup without one in the link, Esc) opens the root's README in preview,
// and only a folder without one shows the empty state.
function setFile(path, { line = null, endLine = null, side = 'new', view = null } = {}) {
  if (noCommit()) path = null; // no commit, no files (a link may name one): the empty state says so instead
  if (!path && state.mode === 'folder') { path = readmeIn(''); line = null; view = null; }
  state.file = path;
  state.fileSide = side;
  state.line = line;
  state.endLine = line && endLine > line ? endLine : null;
  if (path) {
    const change = state.tree.changes[path];
    if (!change) state.view = !line && view && view !== 'diff' ? view : unchangedView(path, line);
    else if (view) state.view = view;
    else if (line) state.view = 'file';
    // The view the user last picked with the tabs or f, not one forced for the previous file (an
    // unchanged file opens in the file view, a target line needs it).
    else state.view = state._viewPref || 'diff';
    if (state.view === 'preview' && (line || !hasPreview(path))) state.view = line || !change ? 'file' : 'diff';
  }
  return path ? expandTo(path) : false;
}

// keepDrawer: leave the narrow windows' drawer open (a folder opened in it shows its README behind it).
function selectFile(path, opts = {}) {
  if (!opts.keepDrawer) closeDrawer();
  intent.select = null; intent.resetFile = false;
  // Rebuild the tree only when it changed or a folder has to open; moving between visible files
  // (j/k) just moves the highlight.
  const opened = setFile(path, opts);
  if (opened || state.treeDirty || !markSelected()) renderTree();
  writeHash();
  renderMain();
  const node = document.querySelector(`#tree .node[data-path="${CSS.escape(state.file || '')}"]`);
  node?.scrollIntoView({ block: 'nearest' });
}

function setView(v) {
  if (v === 'diff' && !state.tree.changes[state.file]) return;
  if (v === 'preview' && !hasPreview(state.file)) return;
  state.view = v; state._viewPref = v;
  writeHash(); renderMain();
}
// Before/After in the file view: the side is part of the view, so the hash keeps it (a reload, Back, a copied link).
function setSide(side) {
  state.fileSide = side;
  writeHash(); renderMain();
}
// f: the next of the views this file has (diff only when it changed, preview only for markdown and SVG).
function cycleView() {
  const views = ['diff', 'file', 'preview'].filter(v => (v !== 'diff' || state.tree.changes[state.file]) && (v !== 'preview' || hasPreview(state.file)));
  setView(views[(views.indexOf(state.view) + 1) % views.length]);
}

// keepRef: show state.ref even if it isn't in the branch's recent history (a link or an agent asked for it).
// select: the view to show once loaded, and scroll a position to restore (Back). They go on the new intent at once (see
// refresh), so a view, file or commit picked while the mode loads replaces them. Everything up to the first await runs
// at once, so the requests go out before the caller goes on (main() wires the page meanwhile).
// The intent is settled once state holds its whole range: at once, except in Branch mode without a base while the page
// loads (it compares with /api/info's default base, so its tree waits for info) and in History with no commit named
// (it opens at the first commit of its list). After each await the mode gives up if another view was asked for
// meanwhile: the state, and the draw, are that view's then.
// A plain folder has the one mode, 'folder' (enterFolder), and no other.
async function setMode(mode, { keepRef = false, select = null, scroll = null } = {}) {
  if (isFolder() && mode !== 'folder') return;
  const it = newIntent({ select, restoreScroll: scroll, settled: false });
  state.mode = mode;
  if (mode === 'working' || mode === 'folder') state.head = 'HEAD'; // uncommitted work only exists on the checked-out branch
  if (mode === 'folder') state.ref = state.base = null;
  state.file = null;
  document.querySelectorAll('#modes button').forEach(b => b.classList.toggle('on', b.dataset.mode === mode));
  $('#mode-select').value = mode;
  $('#commits-pane').hidden = $('#pane-split').hidden = mode !== 'commit';
  $('#branch-controls').hidden = mode !== 'branch';
  if (mode === 'branch' && !state.base) {
    if (!state.info) {
      await infoLoaded;
      if (it !== intent) return;
    }
    state.base ||= state.info.defaultBase;
  }
  // When the range is already known, fetch the tree alongside the history.
  const known = mode !== 'commit' || (keepRef && !!state.ref);
  if (known) settle(it);
  let tree = known ? loadTree() : null;
  // Handled where refresh() awaits it, but it may fail before then (/api/info is slower), or never be awaited (another
  // view was asked for meanwhile).
  tree?.catch(() => {});
  if (mode === 'commit') {
    // No commit chosen yet: History opens at the branch's latest commit, which is the first one /api/log returns, so
    // its tree is fetched by the branch name alongside the history. (With all branches listed, the first commit may
    // be another branch's, so then the tree waits for the list. A HEAD with no commits yet has no tree to fetch.)
    const tip = !known && !state.allBranches && !unborn() ? fetchTree({ ref: state.head }) : null;
    tip?.catch(() => {});
    // A kept ref is looked up alongside the history (a no-op when the history has it).
    await Promise.all([loadCommits(false, 0, false), known && resolveRefCommit()]);
    if (it !== intent) {
      // A view asked for meanwhile in History (a linked commit, Back) keeps its commit, and shows the list as it came.
      if (state.mode === 'commit') renderCommits();
      return;
    }
    if (!state.ref || (!keepRef && !state.commits.some(c => c.sha === state.ref))) state.ref = state.commits[0]?.sha || null;
    // The tip's tree is the one to show when the first commit is (a commit picked earlier may still be in the list),
    // unless the branch moved between the tip's answer and the list's: then it is an older commit's, and was shown under
    // the first commit's title. The tree names the commit it listed (`commit`); when that is another one, or the tip
    // failed, the tree is fetched by the first commit's sha. (Not once another view was asked for meanwhile: that view
    // loads its own.)
    if (tip && state.ref && state.ref === state.commits[0]?.sha) {
      const sha = state.ref;
      const atTip = tip.then(r => { if (r[0].commit !== sha) throw new Error('the branch moved'); return r; });
      tree = loadTree(atTip.catch(e => { if (it !== intent) throw e; return fetchTree(); }));
      tree.catch(() => {}); // as above
    }
    renderCommits();
    settle(it);
  }
  await refresh({ tree });
}

// The intent's whole range is in state now (see setMode). A live update that came while it wasn't runs now, for the view
// as it is (see run).
function settle(it) {
  it.settled = true;
  if (it.due) { const full = it.due === 'full'; it.due = null; liveUpdate(full); }
  if (it.notesDue) {
    it.notesDue = false;
    loadNotes().then(loaded => { if (loaded) reapplyNotes(); }).catch(e => toast(e.message));
  }
}

function syncControls() {
  document.querySelectorAll('#modes button').forEach(b => {
    b.classList.toggle('on', b.dataset.mode === state.mode);
    b.setAttribute('aria-current', b.dataset.mode === state.mode ? 'page' : 'false');
  });
  $('#mode-select').value = state.mode;
  const blocked = splitBlocked();
  document.querySelectorAll('#layout-seg button').forEach(b => {
    b.classList.toggle('on', b.dataset.layout === diffLayout());
    // The reason is left out of the button where the toolbar is short of room (style.css `.why`)
    if (b.dataset.layout === 'unified') b.replaceChildren('Unified', blocked && state.layout === 'split'
      ? h('span', { class: 'why' }, ` (${blocked === 'narrow' ? 'narrow' : 'no wrap'})`) : '');
    else {
      b.setAttribute('aria-disabled', !!blocked);
      b.title = blocked === 'narrow' ? `Too narrow for split: each side would show fewer than ${SPLIT_MIN_CH} characters`
        : blocked ? 'Split needs wrapped lines: long lines scroll sideways (w)' : '';
    }
  });
  $('#theme-btn').innerHTML = THEME_ICON[state.theme];
  const themeLabel = `Theme: ${state.theme[0].toUpperCase() + state.theme.slice(1)}`;
  $('#theme-btn').title = themeLabel;
  $('#theme-btn').setAttribute('aria-label', themeLabel);
  document.querySelectorAll('button[data-theme]').forEach(b => b.setAttribute('aria-checked', b.dataset.theme === state.theme));
  $('#context-select').value = $('#more-context').value = state.context;
  $('#changed-only').checked = state.changedOnly;
  // The branch lists come from /api/info, which may still be on its way while the page loads: the ResizeObserver on
  // #main (wireNarrow), the theme menu and the s and w keys get here then too. setMode calls this again once it's in.
  // A plain folder has none (its info has no branches), and the controls are hidden.
  if (!state.info || isFolder()) return;
  const opts = (sel, val, extra = []) => {
    sel.textContent = '';
    for (const b of [...extra, ...state.info.branches]) sel.append(h('option', { value: b, selected: b === val }, b));
  };
  // A base that isn't a branch (a tag or a commit, from a link or an agent) is listed too, so the select shows it.
  opts($('#base-select'), state.base, state.base && !state.info.branches.includes(state.base) ? [state.base] : []);
  if (!state.base) $('#base-select').prepend(h('option', { value: '', selected: true, disabled: true }, 'choose a base…'));
  syncBranchPicker();
}

// The header picker sets state.head: 'HEAD' is the checked-out branch, anything else is view-only.
// A detached HEAD comes as its short sha with `detached` set: named like a branch, but not called one.
function syncBranchPicker() {
  const { branch: cur, branches, detached } = state.info;
  if (state.head === cur) state.head = 'HEAD';
  const names = [...new Set([cur, ...branches, state.head].filter(b => b && b !== 'HEAD'))];
  const sel = $('#view-branch');
  sel.textContent = '';
  const current = detached ? `${cur} (detached)` : `⎇ ${cur} (checked out)`;
  for (const b of names) {
    const value = b === cur ? 'HEAD' : b;
    sel.append(h('option', { value, selected: value === state.head }, b === cur ? current : b));
  }
  // The face the select sits under: the branch, and whether it's the one checked out.
  $('#branch-name').textContent = state.head === 'HEAD' ? cur || 'HEAD' : state.head;
  $('#branch-note').textContent = state.head === 'HEAD' ? (cur && !detached ? 'checked out' : 'detached') : 'view only';
  $('#checkout').hidden = state.head === 'HEAD';
}

async function setViewBranch(b) {
  state.head = b;
  if (state.mode === 'working' && b !== 'HEAD') return setMode('branch');
  if (state.mode === 'commit') return setMode('commit'); // reload that branch's history
  newIntent();
  syncControls();
  await refresh({ resetFile: true });
}

async function checkout() {
  const b = state.head;
  if (!confirm(`Check out "${b}"?\n\nThis runs git switch in ${state.info.root} and changes the files on disk.`)) return;
  const btn = $('#checkout'), it = intent;
  btn.disabled = true;
  try {
    const res = await fetch('/api/switch?' + new URLSearchParams({ branch: b }), { method: 'POST', headers: { 'x-revue': '1' } });
    const data = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
    if (data.error) return alert(`Could not check out ${b}: ${data.error}`);
    await loadInfo();
    // The view asked for while git switch ran (if any) is drawn with the new info; otherwise the mode is loaded again.
    if (it !== intent) return;
    state.head = 'HEAD';
    await setMode(state.mode);
  } catch (e) {
    toast(`Check out failed: ${e.message}`);
  } finally {
    btn.disabled = false;
  }
}

// j/k: the next or previous changed file, or in a plain folder the next or previous file.
function moveChanged(dir) {
  const list = state.mode === 'folder' ? orderedFiles() : orderedChangedFiles();
  if (!list.length) return;
  const i = list.indexOf(state.file);
  const next = i < 0 ? (dir > 0 ? 0 : list.length - 1) : Math.min(list.length - 1, Math.max(0, i + dir));
  selectFile(list[next]);
}

// ---------------------------------------------------------------- go to definition
// Ctrl/Cmd+click an identifier in a diff or file view to jump to where it's defined (see /api/def).
const SKIP_TOKEN = /^hljs-(keyword|comment|string|number|literal|regexp|doctag)$/;

// A Range over the character offsets [start, end) of an element's text, across syntax spans.
function textRange(el, start, end) {
  const range = document.createRange();
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let pos = 0, n, started = false;
  while ((n = walker.nextNode())) {
    const len = n.data.length;
    if (!started && start < pos + len) { range.setStart(n, start - pos); started = true; }
    if (started && end <= pos + len) { range.setEnd(n, end - pos); break; }
    pos += len;
  }
  return range;
}

// The identifier under the pointer in a code cell, or null.
function identAt(x, y) {
  let node, off;
  if (document.caretPositionFromPoint) { const p = document.caretPositionFromPoint(x, y); node = p?.offsetNode; off = p?.offset; }
  else if (document.caretRangeFromPoint) { const r = document.caretRangeFromPoint(x, y); node = r?.startContainer; off = r?.startOffset; }
  if (!node || node.nodeType !== Node.TEXT_NODE) return null;
  const td = node.parentElement.closest('#main td.src');
  if (!td || [...node.parentElement.classList].some(c => SKIP_TOKEN.test(c))) return null;
  const before = document.createRange();
  before.setStart(td, 0); before.setEnd(node, off);
  const text = td.textContent, pos = before.toString().length;
  let s = pos, e = pos;
  while (s > 0 && /[\w$]/.test(text[s - 1])) s--;
  while (e < text.length && /[\w$]/.test(text[e])) e++;
  const name = text.slice(s, e);
  if (name.length > 100 || !/^[A-Za-z_$][\w$]*$/.test(name)) return null;
  const range = textRange(td, s, e);
  // The caret snaps to the nearest character, so make sure the pointer is really over the word.
  if (![...range.getClientRects()].some(r => x >= r.left && x <= r.right && y >= r.top && y <= r.bottom)) return null;
  return { td, name, range, col: s };
}

// Which file, side of the range and line number a code cell shows, and the same for the other side (`other`, whose
// line is 0 when the row has none there: an added or removed line, or the file view).
function cellContext(td) {
  const table = td.closest('table'), row = td.parentElement;
  let side = 'new', num;
  if (table.classList.contains('fileview')) { side = table.dataset.side; num = row.cells[0]; }
  else if (table.classList.contains('split')) { side = td.cellIndex < 3 ? 'old' : 'new'; num = row.cells[td.cellIndex - 2]; }
  else { if (row.classList.contains('del')) side = 'old'; num = row.cells[side === 'old' ? 0 : 1]; }
  const path = table.dataset.path;
  // The old side of a renamed file lives under its old name.
  const pathOn = (s) => s === 'old' ? state.tree.changes[path]?.oldPath || path : path;
  const otherSide = side === 'old' ? 'new' : 'old';
  const other = { path: pathOn(otherSide), side: otherSide, line: parseInt(otherSide === 'old' ? row.dataset.o : row.dataset.n) || 0 };
  return { path: pathOn(side), side, line: parseInt(num?.textContent) || 0, other };
}

function setDefHover(hit) {
  $('#main').classList.toggle('def-hover', !!hit);
  if (!window.Highlight || !CSS.highlights) return;
  if (hit) CSS.highlights.set('def-link', new Highlight(hit.range));
  else CSS.highlights.delete('def-link');
}

// A lookup can take a while. One that answers after the view changed (Esc to the overview, j to another file, another
// commit or mode, Back) is dropped, with its picker and toasts: they would pull the user back from where they went, or
// speak of a view they left.
async function goToDefinition(hit, x, y) {
  // The lookup is in the range of the view asked for last, and the rows clicked are the last view's until that one is
  // on the page: a click then is ignored.
  if (!intent.shown) return;
  const at = cellContext(hit.td);
  const clicked = viewKey(), moved = () => viewKey() !== clicked;
  const find = ({ path, side, line }) => api('/api/def', { name: hit.name, side, from: path, line, col: hit.col });
  let res, side = at.side;
  try {
    res = await find(at);
    if (moved()) return;
    // Nothing on the clicked side: a call in the old half of a split diff to a function this change adds, or one on
    // the new side to a function it removes. Look on the other side, from the same line if the row has one there.
    // A plain folder's files have one side.
    if (!res.jump && !res.candidates.length && state.mode !== 'folder') {
      const other = await find(at.other);
      if (moved()) return;
      if (other.jump || other.candidates.length) {
        res = other; side = at.other.side;
        toast(`${hit.name} is only defined ${side === 'new' ? 'after' : 'before'} this change`);
      }
    }
  } catch (e) {
    if (!moved()) toast(e.message);
    return;
  }
  // The server picks the definition when one candidate clearly wins, and otherwise returns the tie.
  if (res.jump) return jumpTo(res.jump, side);
  const defs = res.candidates;
  if (!defs.length) return toast(`No definition found for ${hit.name}`);
  if (defs.length === 1) return jumpTo(defs[0], side);
  showPicker(hit.name, defs, side, x, y);
}

// Open a definition in the file view. Pushes a history entry so browser Back returns here.
function jumpTo(d, side) {
  let path = d.path;
  // Found on the old side of a renamed file: open it under its new name, showing the old side.
  if (side === 'old' && !fileSet().has(path)) {
    path = Object.keys(state.tree.changes).find(p => state.tree.changes[p].oldPath === path) || path;
  }
  jumpToLines(path, { line: d.line, side });
}

function onPopState(e) {
  return applyView(hashView(), e.state?.scroll ?? null);
}

// The tied definitions, as a menu. Closing it removes the focused item, which would drop focus to the page, so focus
// goes back to the main pane: where the click was, and where a picked definition opens. Tab closes it too, as it does
// the header's menus, and focus moves on from the picker's place in the page (its end) without being sent anywhere.
let picker = null;
function closePicker(refocus = true) {
  if (!picker) return;
  const had = picker.contains(document.activeElement);
  picker.remove(); picker = null;
  if (had && refocus) $('#main').focus({ preventScroll: true });
}
function showPicker(name, defs, side, x, y) {
  closePicker();
  const items = defs.map(d => h('button', { class: 'def-item', role: 'menuitem', onclick: () => { closePicker(); jumpTo(d, side); } },
    h('span', { class: 'def-loc' }, `${d.path}:${d.line}`), h('span', { class: 'def-kind' }, d.kind), h('code', {}, d.text)));
  // The title repeats the menu's name, so assistive tech skips it: a menu should hold only its items.
  picker = h('div', { class: 'def-picker', role: 'menu', 'aria-label': `Definitions of ${name}` },
    h('div', { class: 'def-title muted', 'aria-hidden': 'true' }, `${defs.length} definitions of ${name}`), items);
  picker.addEventListener('keydown', (e) => {
    e.stopPropagation(); // keep j/k/Esc away from the global shortcuts
    const i = items.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus();
    else if (e.key === 'Escape') closePicker();
    else if (e.key === 'Tab') return closePicker(false);
    else return; // Enter clicks the focused button
    e.preventDefault();
  });
  document.body.append(picker);
  const w = picker.offsetWidth, ph = picker.offsetHeight;
  picker.style.left = Math.max(8, Math.min(x, innerWidth - w - 8)) + 'px';
  picker.style.top = (y + 16 + ph > innerHeight ? Math.max(8, y - ph - 4) : y + 16) + 'px';
  items[0].focus();
}

// A load started from the page's own controls (a mode key, tab or select, a commit, base or branch picked, r, Load more,
// all branches): its error is toasted, as Back's and an agent's are, instead of left uncaught. A view's own error is
// drawn in the main pane too (showLoadError).
const toasted = (p) => p.catch(e => toast(e.message));
let toastTimer;
function toast(msg) {
  let el = $('#toast');
  if (!el) document.body.append(el = h('div', { id: 'toast', role: 'status' }));
  el.textContent = msg;
  el.classList.add('on');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('on'), 2500);
}

function wireDefinitions() {
  const main = $('#main');
  const mod = (e) => e.ctrlKey || e.metaKey;
  let mouse = { x: -1, y: -1 };
  main.addEventListener('mousemove', (e) => { mouse = { x: e.clientX, y: e.clientY }; setDefHover(mod(e) ? identAt(e.clientX, e.clientY) : null); });
  main.addEventListener('mouseleave', () => { mouse = { x: -1, y: -1 }; setDefHover(null); });
  main.addEventListener('scroll', () => { setDefHover(null); closePicker(); });
  addEventListener('keydown', (e) => { if ((e.key === 'Control' || e.key === 'Meta') && mouse.x >= 0) setDefHover(identAt(mouse.x, mouse.y)); });
  addEventListener('keyup', (e) => { if (e.key === 'Control' || e.key === 'Meta') setDefHover(null); });
  addEventListener('blur', () => setDefHover(null));
  // Stop the modifier+mousedown from starting a text or table-cell selection.
  main.addEventListener('mousedown', (e) => { if (mod(e) && e.button === 0 && identAt(e.clientX, e.clientY)) e.preventDefault(); });
  main.addEventListener('click', (e) => {
    if (!mod(e) || e.button !== 0) return;
    const hit = identAt(e.clientX, e.clientY);
    if (!hit) return;
    e.preventDefault();
    setDefHover(null);
    goToDefinition(hit, e.clientX, e.clientY);
  });
  document.addEventListener('mousedown', (e) => { if (picker && !picker.contains(e.target)) closePicker(); });
  addEventListener('popstate', (e) => onPopState(e).catch(err => toast(err.message)));
}

// ---------------------------------------------------------------- live updates
// Agents (the MCP server) send views through /api/navigate. Only the most recently focused revue
// tab follows, so several open tabs don't all jump; if that tab closed, every tab follows.
const TAB_ID = Math.random().toString(36).slice(2);
function claimTab() { store.set('activeTab', TAB_ID); }
// A tab that goes away (closed, or left for another page) gives up its claim. Firefox can stop a closing tab's pagehide
// handler at its first localStorage access, so the tab first tells the others that it's going, on a BroadcastChannel
// (same origin, like localStorage), and they drop its claim themselves.
function releaseTab(id) { if (store.get('activeTab') === id) store.del('activeTab'); }
function wireActiveTab() {
  claimTab();
  addEventListener('focus', claimTab);
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') claimTab(); });
  const tabs = new BroadcastChannel('revue.tabs');
  tabs.onmessage = (e) => { if (e.data?.gone) releaseTab(e.data.gone); };
  addEventListener('pagehide', () => { tabs.postMessage({ gone: TAB_ID }); releaseTab(TAB_ID); });
}

// Cookies fire no change event, so a tab picks up prefs changed in another tab (on any port) when it's focused.
function syncPrefs() {
  const next = readPrefs();
  const changed = (k) => next[k] !== state[k];
  const theme = changed('theme'), tree = changed('changedOnly'), wrap = changed('wrap');
  let main = changed('layout') || changed('context');
  if (!theme && !main && !tree && !wrap) return;
  const layout = diffLayout();
  Object.assign(state, next);
  main ||= diffLayout() !== layout; // wrapping decides whether split can be drawn
  if (theme) { applyTheme(state.theme); syncPreviewThemes(); }
  if (wrap) applyWrap();
  syncControls();
  if (main) renderMain();
  if (tree) renderTree();
}
function wirePrefSync() {
  addEventListener('focus', syncPrefs);
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible') syncPrefs(); });
}

async function onNavigate(v) {
  const active = store.get('activeTab', null);
  if (active && active !== TAB_ID) return;
  claimTab();
  history.replaceState({ ...history.state, scroll: $('#main').scrollTop }, '', location.hash);
  history.pushState(null, '', location.hash);
  await applyView(v);
  window.focus();
  if (v.file) toast(`Showing ${v.file}${v.line ? ':' + v.line + (v.endLine ? '–' + v.endLine : '') : ''}`);
}

// ---------------------------------------------------------------- server status
// The card under the live-updates dot, shown while the dot is hovered or focused. It fetches /api/server each time
// it opens, so the numbers are current.
let liveState = 'connecting'; // 'connecting', 'on' or 'off': the event stream, as connectEvents sees it
let gitEvents = 0;            // how many `git` live updates (and reconnects) this tab has seen
// The last /api/server answer and when it came, shared by the status card and the empty state.
let serverCache = null;
const SERVER_CACHE_MS = 5000;
const fetchServer = () => api('/api/server').then(s => (serverCache = { at: Date.now(), value: s }).value);
let renderStatus = () => {};
const LIFETIME_LABEL = { external: 'started by hand', session: 'stops with the agent session', detached: 'in the background, stops when idle' };
const WATCHER_LABEL = { partial: 'partial: some changes show up only on the next refresh', off: 'off: the file watcher stopped, press r to refresh' };

function ago(ms) {
  const m = Math.floor(ms / 60000), hr = Math.floor(m / 60);
  if (m < 1) return 'just now';
  if (m < 60) return `${m} min ago`;
  if (hr < 24) return `${hr} h ${m % 60} min ago`;
  return `${Math.floor(hr / 24)} d ${hr % 24} h ago`;
}

function wireStatusCard() {
  const wrap = $('#status'), dot = $('#live'), card = $('#status-card');
  let server = null, error = null, seq = 0;
  const warn = (text) => h('span', { class: 'warn' }, text);
  const row = (k, ...v) => [h('dt', {}, k), h('dd', {}, ...v)];
  renderStatus = () => {
    if (card.hidden) return;
    const live = liveState === 'off' ? warn('disconnected, retrying')
      : liveState === 'connecting' ? 'connecting…'
      : WATCHER_LABEL[server?.watcher] ? warn(WATCHER_LABEL[server.watcher]) : 'on';
    const rows = [row('Live updates', live)];
    if (error) rows.push(row('Server', warn(`not reachable (${error})`)));
    else if (!server) rows.push(row('Server', 'loading…'));
    else {
      const build = server.commit ? ` · ${server.commit}${server.modified ? ' (modified)' : ''}` : '';
      rows.push(
        row('Version', server.version + build, server.stale ? warn(' · out of date, restart revue') : null),
        row('Server', `port ${server.port} · pid ${server.pid}`),
        row('Runs', LIFETIME_LABEL[server.lifetime] || server.lifetime),
        server.started ? row('Started', ago(Date.now() - server.started)) : [],
        row('Tabs', `${server.tabs} open, ${server.navTabs} following agent navigation`),
        row('Notes', `${server.annotations} kept ${server.notes !== 'file' ? 'in memory'
          : server.git === false ? "in revue's data folder" : 'in .git/revue/notes.json'}`),
      );
    }
    card.replaceChildren(h('h3', {}, 'revue server'), h('dl', {}, ...rows.flat()));
  };
  const open = () => {
    if (!card.hidden) return;
    card.hidden = false; dot.setAttribute('aria-expanded', 'true');
    const n = ++seq;
    renderStatus();
    fetchServer().then(s => { server = s; error = null; }, e => { server = null; error = e.message; })
      .then(() => { if (n === seq) renderStatus(); });
  };
  const close = () => { card.hidden = true; dot.setAttribute('aria-expanded', 'false'); };
  // Open while the pointer is over the dot or its card, or while the dot has focus (a click focuses it too).
  wrap.addEventListener('pointerenter', open);
  wrap.addEventListener('pointerleave', () => { if (document.activeElement !== dot) close(); });
  dot.addEventListener('focus', open);
  dot.addEventListener('click', open);
  dot.addEventListener('blur', () => { if (!wrap.matches(':hover')) close(); });
  // Handled here so Esc closes the card instead of reaching the global "all changes" shortcut.
  dot.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || card.hidden) return;
    e.preventDefault(); e.stopPropagation(); close();
  });
}

// Everything a commit, branch switch or other git change can move, loaded again: History's commit list
// (refreshCommits, which keeps the commit picked), /api/info (HEAD, the branches) and the tree. A live update runs it,
// and so does r (force: the main pane is drawn again too), which is how to catch up when live updates are off.
// While the view asked for last doesn't hold its whole range yet (History waiting for its commit list), it is only noted
// as due, and runs once the range is set (settle): before then it would load and draw a range that is only half set.
// When another view is asked for meanwhile, the commit list and info it loaded still apply to the page, but the tree is
// that view's to load and draw.
async function reloadAll({ force = false } = {}) {
  const it = intent;
  if (!it.settled) { it.due = 'full'; return; }
  if (state.mode === 'commit') {
    // A commit named by a branch (a link to `ref=feature`) is that branch's latest, which may have moved.
    const [, moved] = await Promise.all([refreshCommits(), resolveRefCommit()]);
    if (moved && it === intent && !markCommit()) renderCommits();
  }
  await loadInfo();
  if (it !== intent) return;
  if (!$('#view-branch').matches(':focus')) syncControls();
  await refresh({ force });
}
let liveUpdate = () => {}; // connectEvents' run, for settle

// It connects before the first render, so an edit made while the page loads isn't missed. Until `started` resolves
// (the first render is done), updates only note that one is due, and the one due runs then: a refresh racing the first
// render would just do the same work twice. Navigate and notes events wait the same way.
function connectEvents(started) {
  const live = $('#live');
  const es = new EventSource('/api/events?nav=1');
  es.addEventListener('navigate', (e) => started.then(() => onNavigate(JSON.parse(e.data))).catch(err => toast(err.message)));
  es.addEventListener('annotations', () => started.then(() => loadNotes())
    .then(loaded => { if (loaded) reapplyNotes(); }).catch(e => toast(e.message)));
  let pending = null, dropped = false, ready = false;
  let due = null; // before the first render: 'full' (a refresh) or 'check' (see onopen)
  // full: refresh everything. Otherwise only fetch the tree again and refresh from it: an edit that keeps a file's line
  // counts moves only its version tag, which the tree's signature leaves out, and a refresh redraws only what moved.
  // An update works within the view asked for last (its intent): it waits while that view's range is only half set (as
  // reloadAll does), and draws nothing once another view is asked for.
  const run = liveUpdate = async (full = true) => {
    if (!ready) { due = full || due === 'full' ? 'full' : 'check'; return; }
    const it = intent;
    if (!it.settled) { it.due = full || it.due === 'full' ? 'full' : 'check'; return; }
    try {
      if (full) return await reloadAll();
      if (state.mode === 'commit') await refreshCommits();
      if (it === intent && await loadTree() && it === intent) await refresh({ tree: Promise.resolve(true) });
    } catch (e) { toast(`Live update failed: ${e.message}`); }
  };
  started.then(() => { ready = true; if (due) run(due === 'full'); due = null; });
  // kind: 'worktree' or 'git', as the server sends it. History and Branch compare commits only, so edits
  // to working files can't change them. A plain folder's updates are all `worktree`.
  const update = (kind) => {
    if (kind !== 'worktree') gitEvents++;
    if (kind === 'worktree' && state.mode !== 'working' && state.mode !== 'folder') return;
    clearTimeout(pending);
    pending = setTimeout(run, 150);
  };
  // Changes made while the stream was down (sleep, a server restart) sent no message, so a reconnect
  // refetches everything; after a restart the in-memory notes are gone too.
  // The browser opens the stream a little after the first requests, so they can be answered before it is registered,
  // and an edit in between would send nothing. When the tree came back first, it is checked once more.
  const label = $('#live-label');
  let opened = false;
  es.onopen = () => {
    liveState = 'on'; live.classList.add('on'); label.textContent = 'Live'; renderStatus();
    if (dropped) { dropped = false; update('git'); }
    else if (!opened && state.treeSig) run(false);
    opened = true;
  };
  es.onerror = () => { dropped = true; liveState = 'off'; live.classList.remove('on'); label.textContent = 'Offline'; renderStatus(); };
  es.onmessage = (e) => update(e.data);
}

// ---------------------------------------------------------------- theme menu
// Inline SVG rather than ◐/☀/☾, whose size and style vary by font and which can render as color emoji.
const svgIcon = (body) => `<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
const THEME_ICON = {
  auto: svgIcon('<circle cx="8" cy="8" r="6"/><path d="M8 2a6 6 0 0 0 0 12z" fill="currentColor"/>'),
  light: svgIcon('<circle cx="8" cy="8" r="3"/><path d="M8 1v1.5M8 13.5V15M1 8h1.5M13.5 8H15M3.05 3.05l1.06 1.06M11.89 11.89l1.06 1.06M3.05 12.95l1.06-1.06M11.89 4.11l1.06-1.06"/>'),
  dark: svgIcon('<path d="M13.5 9.5A5.5 5.5 0 0 1 6.5 2.5a5.5 5.5 0 1 0 7 7z"/>'),
};

// applyTheme lives in theme.js, which index.html loads without defer so the saved theme applies before first paint.
function setTheme(t) { state.theme = t; prefs.set('theme', t); applyTheme(t); syncPreviewThemes(); syncControls(); }
function setContext(c) { state.context = c; prefs.set('context', c); syncControls(); renderMain(); }
function wireThemeMenu() {
  const btn = $('#theme-btn'), menu = $('#theme-menu');
  const items = [...menu.querySelectorAll('button')];
  items.forEach(b => { b.querySelector('.ico').innerHTML = THEME_ICON[b.dataset.theme]; });
  const close = (refocus) => {
    menu.hidden = true; btn.setAttribute('aria-expanded', 'false');
    if (refocus) btn.focus();
  };
  btn.addEventListener('click', () => {
    if (!menu.hidden) return close(false);
    menu.hidden = false; btn.setAttribute('aria-expanded', 'true');
    menu.querySelector('[aria-checked="true"]').focus();
  });
  items.forEach(b => b.addEventListener('click', () => { setTheme(b.dataset.theme); close(true); }));
  // Handled here so Esc closes the menu instead of reaching the global "all changes" shortcut.
  menu.addEventListener('keydown', (e) => {
    const i = items.indexOf(document.activeElement);
    if (e.key === 'Escape') close(true);
    else if (e.key === 'ArrowDown') items[(i + 1) % items.length].focus();
    else if (e.key === 'ArrowUp') items[(i - 1 + items.length) % items.length].focus();
    else if (e.key === 'Tab') return close(false);
    else return;
    e.preventDefault(); e.stopPropagation();
  });
  document.addEventListener('pointerdown', (e) => { if (!menu.hidden && !e.target.closest('#theme')) close(false); });
}

// Narrow windows: the overflow menu holds context, the theme and the shortcut list. Context shows only where the
// toolbar's would (style.css), and the arrows skip it while it is hidden.
function wireMoreMenu() {
  const btn = $('#more-btn'), menu = $('#more-menu');
  const items = () => [...menu.querySelectorAll('button, select')].filter(el => el.getClientRects().length);
  const close = (refocus) => {
    menu.hidden = true; btn.setAttribute('aria-expanded', 'false');
    if (refocus) btn.focus();
  };
  menu.querySelectorAll('.ico').forEach(el => { el.innerHTML = THEME_ICON[el.parentElement.dataset.theme]; });
  btn.addEventListener('click', () => {
    if (!menu.hidden) return close(false);
    menu.hidden = false; btn.setAttribute('aria-expanded', 'true');
    menu.querySelector('[aria-checked="true"]')?.focus();
  });
  menu.querySelectorAll('button[data-theme]').forEach(b => b.addEventListener('click', () => { setTheme(b.dataset.theme); close(true); }));
  $('#more-keys').addEventListener('click', () => { close(false); toggleKeys(); });
  $('#more-context').addEventListener('change', (e) => { setContext(e.target.value); close(true); });
  menu.addEventListener('keydown', (e) => {
    const list = items(), i = list.indexOf(document.activeElement);
    if (e.key === 'Escape') close(true);
    // On the Context select, Up and Down move through the popover like everywhere else in it (instead of changing
    // the value); Space or Alt+Down opens its list.
    else if (e.target.matches('select') && e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Tab') return;
    else if (e.key === 'ArrowDown') list[(i + 1) % list.length].focus();
    else if (e.key === 'ArrowUp') list[(i - 1 + list.length) % list.length].focus();
    else if (e.key === 'Tab') return close(false);
    else return;
    e.preventDefault(); e.stopPropagation();
  });
  document.addEventListener('pointerdown', (e) => { if (!menu.hidden && !e.target.closest('#more')) close(false); });
}

// Clear all clears the notes of the view asked for last, so it is disabled until that view is on the page: before then
// the notes shown are the last view's (see newIntent).
function syncClearAll() {
  $('#notes-clear').setAttribute('aria-disabled', String(!intent.shown));
}
// The notes menu in the toolbar: Clear.
function wireNotesMenu() {
  const btn = $('#notes-menu-btn'), menu = $('#notes-menu');
  const close = (refocus) => {
    menu.hidden = true; btn.setAttribute('aria-expanded', 'false');
    if (refocus) btn.focus();
  };
  btn.addEventListener('click', () => {
    if (!menu.hidden) return close(false);
    menu.hidden = false; btn.setAttribute('aria-expanded', 'true');
    syncClearAll();
    menu.querySelector('button').focus();
  });
  $('#notes-clear').addEventListener('click', () => {
    if (!intent.shown) return;
    close(true);
    const n = state.notes.length;
    if (confirm(n === 1 ? 'Clear the 1 note in this view?' : `Clear all ${n} notes in this view?`)) clearNotes({});
  });
  menu.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' && e.key !== 'Tab') return;
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); }
    close(e.key === 'Escape');
  });
  document.addEventListener('pointerdown', (e) => { if (!menu.hidden && !e.target.closest('#notes-more')) close(false); });
}

// Narrow windows (the media query in style.css): the sidebar is a drawer, and the branch picker and the base
// select move to its top, out of the one-row header.
const narrowMQ = matchMedia('(max-width: 760px)');
const drawerOpen = () => document.body.classList.contains('drawer-open');
// What Tab can reach in the drawer now (hidden parts, like the commit list's divider outside History, left out).
const drawerStops = () => [...$('#side').querySelectorAll('button, select, input, [tabindex="0"]')]
  .filter(el => !el.disabled && el.getClientRects().length);
// Closing hides #side, which would drop focus to the body: when focus was in it (or already lost), it moves to
// `returnTo`.
function setDrawer(open, returnTo = null) {
  const had = $('#side').contains(document.activeElement) || document.activeElement === document.body;
  document.body.classList.toggle('drawer-open', open);
  $('#drawer-btn').setAttribute('aria-expanded', open);
  $('#scrim').hidden = !open;
  // Opening lands on the list's current row (the commit list first, in History), else on the filter.
  if (open) (drawerStops().find(el => el.matches('.node, #commits li')) || $('#filter')).focus({ preventScroll: true });
  else if (returnTo && had) returnTo.focus({ preventScroll: true });
}
// Picking a file or commit: what it opens is in #main.
function closeDrawer() { if (drawerOpen()) setDrawer(false, $('#main')); }
function wireDrawer() {
  $('#drawer-btn').addEventListener('click', () => setDrawer(!drawerOpen(), $('#drawer-btn')));
  $('#scrim').addEventListener('click', () => setDrawer(false, $('#drawer-btn')));
  // While it is open, Tab wraps around inside it.
  $('#side').addEventListener('keydown', (e) => {
    if (e.key !== 'Tab' || !drawerOpen()) return;
    const all = drawerStops(), first = all[0], last = all[all.length - 1];
    if (e.shiftKey ? document.activeElement === first : document.activeElement === last) {
      e.preventDefault(); (e.shiftKey ? last : first).focus();
    }
  });
  const place = () => {
    if (narrowMQ.matches) $('#drawer-head').append($('#repo'), $('#branch-controls'));
    else { $('#modes').before($('#repo')); $('#mode-select').after($('#branch-controls')); setDrawer(false); }
  };
  narrowMQ.addEventListener('change', place);
  place();
}

// The divider between the commit list and the file tree. Its position is per server (localStorage), in pixels.
function wirePaneSplit() {
  const split = $('#pane-split'), pane = $('#commits-pane'), side = $('#side');
  const MIN = 60, max = () => side.clientHeight - 140; // leave room for the filter and a few files
  // For assistive tech, the position as a percentage of the range it can move in.
  const announce = () => {
    const range = max() - MIN;
    split.setAttribute('aria-valuenow', range > 0 ? Math.round(Math.max(0, Math.min(1, (pane.offsetHeight - MIN) / range)) * 100) : 0);
  };
  const set = (px) => {
    if (px == null || !max()) return;
    pane.style.height = Math.max(MIN, Math.min(max(), px)) + 'px';
    announce();
  };
  split.addEventListener('focus', announce);
  const saved = () => parseInt(store.get('commitsHeight', ''), 10) || null;
  set(saved());
  addEventListener('resize', () => set(saved()));
  split.addEventListener('pointerdown', (e) => {
    e.preventDefault(); split.setPointerCapture(e.pointerId); split.classList.add('drag');
    const top = pane.getBoundingClientRect().top;
    const move = (ev) => set(ev.clientY - top);
    const up = () => {
      split.classList.remove('drag'); store.set('commitsHeight', parseInt(pane.style.height));
      split.removeEventListener('pointermove', move); split.removeEventListener('pointerup', up);
    };
    split.addEventListener('pointermove', move); split.addEventListener('pointerup', up);
  });
  split.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
    e.preventDefault(); e.stopPropagation();
    set(pane.offsetHeight + (e.key === 'ArrowDown' ? 24 : -24));
    store.set('commitsHeight', parseInt(pane.style.height));
  });
}

// The shortcut list: a popover under the "?" button, also toggled by the ? key (see wire()).
let toggleKeys = () => {};
function wireKeysPanel() {
  const btn = $('#keys-btn'), panel = $('#keys-panel');
  const close = (refocus) => {
    panel.hidden = true; btn.setAttribute('aria-expanded', 'false');
    if (refocus) btn.focus();
  };
  toggleKeys = () => {
    if (!panel.hidden) return close(false);
    panel.hidden = false; btn.setAttribute('aria-expanded', 'true');
    panel.focus();
  };
  btn.addEventListener('click', toggleKeys);
  // Handled here so Esc closes the list instead of reaching the global "all changes" shortcut.
  panel.addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' && e.key !== '?') return;
    e.preventDefault(); e.stopPropagation(); close(true);
  });
  panel.addEventListener('focusout', (e) => { if (!e.relatedTarget?.closest('#keys')) close(false); });
  document.addEventListener('pointerdown', (e) => { if (!panel.hidden && !e.target.closest('#keys')) close(false); });
}

// ---------------------------------------------------------------- wiring
// Draggable sidebar width, kept between 180px and 300px short of the window width. The saved width
// may come from a bigger screen, so clamp it on load and whenever the window resizes.
function wireSideWidth() {
  const side = $('#side'), rz = $('#resizer');
  const setSide = (w) => { side.style.width = Math.max(180, Math.min(window.innerWidth - 300, w)) + 'px'; };
  const savedSide = () => parseInt(prefs.get('sideWidth', '320'), 10) || 320;
  setSide(savedSide());
  addEventListener('resize', () => setSide(savedSide()));
  addEventListener('focus', () => setSide(savedSide())); // pick up a width dragged in another tab
  rz.addEventListener('mousedown', (e) => {
    e.preventDefault(); rz.classList.add('drag');
    const move = (ev) => setSide(ev.clientX);
    const up = () => { rz.classList.remove('drag'); prefs.set('sideWidth', parseInt(side.style.width)); removeEventListener('mousemove', move); removeEventListener('mouseup', up); };
    addEventListener('mousemove', move); addEventListener('mouseup', up);
  });
}

function wire() {
  document.querySelectorAll('#modes button').forEach(b => b.addEventListener('click', () => toasted(setMode(b.dataset.mode))));
  $('#mode-select').addEventListener('change', (e) => { e.target.blur(); toasted(setMode(e.target.value)); });
  document.querySelectorAll('#layout-seg button').forEach(b => b.addEventListener('click', () => setLayout(b.dataset.layout)));
  // The saved sidebar width first: wireNarrow measures what it leaves #main, which the first render then draws.
  wireSideWidth();
  wireNarrow();
  applyWrap();
  wireThemeMenu();
  wireKeysPanel();
  wireStatusCard();
  wireMoreMenu();
  wireNotesMenu();
  wireDrawer();
  wirePaneSplit();
  // Selects blur after a change (like #view-branch) so the keyboard shortcuts work again right away.
  $('#context-select').addEventListener('change', (e) => { e.target.blur(); setContext(e.target.value); });
  $('#changed-only').addEventListener('change', (e) => { state.changedOnly = e.target.checked; prefs.set('changedOnly', e.target.checked ? '1' : '0'); renderTree(); });
  // A new filter shows all its matches, even in folders closed earlier.
  // Typing rebuilds the tree once it pauses (100 ms); Enter and Esc act on the current text right away.
  let filterTimer = null;
  const applyFilter = () => { clearTimeout(filterTimer); state.filter = $('#filter').value; state.collapsed.clear(); renderTree(); };
  $('#filter').addEventListener('input', () => { clearTimeout(filterTimer); filterTimer = setTimeout(applyFilter, 100); });
  $('#filter').addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { clearTimeout(filterTimer); e.target.value = ''; state.filter = ''; renderTree(); e.target.blur(); }
    if (e.key === 'Enter') {
      if (state.filter !== e.target.value) applyFilter();
      const first = $('#tree .node.file'); if (first) selectFile(first.dataset.path);
    }
  });
  $('#all-branches').addEventListener('change', (e) => { state.allBranches = e.target.checked; toasted(loadCommits()); });
  $('#base-select').addEventListener('change', (e) => {
    e.target.blur(); newIntent(); state.base = e.target.value; toasted(refresh({ resetFile: true }));
  });
  $('#view-branch').addEventListener('change', (e) => { e.target.blur(); toasted(setViewBranch(e.target.value)); });
  $('#checkout').addEventListener('click', checkout);
  wireDefinitions();
  wireActiveTab();
  wirePrefSync();
  $('#notes-next').addEventListener('click', nextNote);
  $('#notes-other').addEventListener('click', openOtherNotes);
  $('#tree').addEventListener('keydown', (e) => listKeys($('#tree'), '.node', e));
  $('#tree').addEventListener('click', onTreeClick);
  $('#commits').addEventListener('keydown', (e) => listKeys($('#commits'), 'li[role=option]', e));
  $('#commits').addEventListener('click', onCommitClick);

  document.addEventListener('keydown', (e) => {
    // A checkbox keeps focus after a click; it takes no letters, so the shortcuts still apply there.
    if (e.target.matches('input:not([type=checkbox]), select, textarea') || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key;
    // A plain folder has no diffs to lay out and no other modes.
    if (state.mode === 'folder' && ['s', '1', '2', '3'].includes(k)) return;
    if (k === 'j' || k === 'n') moveChanged(1);
    else if (k === 'k' || k === 'p') moveChanged(-1);
    else if (k === 'Escape') drawerOpen() ? setDrawer(false, $('#drawer-btn')) : selectFile(null);
    else if (k === 'f' && state.file) cycleView();
    else if (k === 's') setLayout(diffLayout() === 'split' ? 'unified' : 'split');
    else if (k === 'w') setWrap(!state.wrap);
    else if (k === '/') { e.preventDefault(); $('#filter').focus(); }
    else if (k === '1') toasted(setMode('working'));
    else if (k === '2') toasted(setMode('commit'));
    else if (k === '3') toasted(setMode('branch'));
    else if (k === 'r') toasted(reloadAll({ force: true }));
    else if (k === '?') toggleKeys();
    else return;
    e.preventDefault();
  });
}

// The server reports `stale` when its code on disk changed after it started (an upgrade or rebuild of
// revue). This page came from disk, so it may call endpoints that server doesn't have.
function setInfo(info) {
  state.info = info;
  $('#stale').hidden = !info.stale;
  // A plain folder (not a git repo, or --no-git): style.css hides the modes, branches, commit list and diff settings.
  document.body.classList.toggle('plain-folder', info.git === false);
  $('#folder-tag').hidden = info.git !== false;
}
// /api/info belongs to the page, not a view: whoever asked for it (startup, a live update, r, Check out), the answer
// stored last is the newest one asked for.
function loadInfo() {
  const t = ticket('info', { page: true });
  return api('/api/info').then(i => { if (t.apply()) setInfo(i); return i; });
}
let infoLoaded = null; // the first /api/info (main), for what needs state.info before the page is first drawn

// The requests go out first, then the page is wired while they're under way (wire() measures the layout, which takes
// a while on a big page), and the view the URL names (a file and lines included) is rendered once.
// A plain folder is only known once /api/info is in, and the page starts the URL's mode before then (a link with no mode
// is Changes). Info's answer then enters folder mode (enterFolder) as a new intent, before anything is drawn, which
// drops the first mode's draw as any newer view would; its requests were answered with the folder's files, since the
// server ignores the view there, except History's /api/log, which fails and is ignored.
let folderShown = null;
function enterFolder() {
  folderShown = setMode('folder', { select: folderView(hashView()) });
  folderShown.catch(() => {}); // awaited in main
}

async function main() {
  const v = hashView();
  let started;
  connectEvents(new Promise(r => { started = r; }));
  // /api/info loads alongside the first tree and history; only branch mode without a base needs it
  // first, for the default base (setMode waits for it then). Nothing is drawn before it is in (refresh).
  infoLoaded = loadInfo().then(i => {
    document.title = `revue · ${i.name}`;
    $('#repo-name').textContent = i.name;
    $('#repo-name').title = i.root;
    if (i.git === false) enterFolder();
  });
  infoLoaded.catch(() => {}); // handled where refresh awaits it
  Object.assign(state, { ref: v.ref, base: v.base, head: v.head });
  const shown = setMode(v.mode, { keepRef: true, select: v });
  wire();
  try {
    try {
      await shown;
    } catch (e) {
      await infoLoaded.catch(() => {});
      if (!folderShown) throw e; // in a folder, the first mode's failure is no one's: folder mode replaced it
    }
    await infoLoaded.catch(() => {});
    if (folderShown) await folderShown;
  } finally { started(); }
}
main().catch(e => { $('#main').textContent = 'Failed to start: ' + e.message; });
