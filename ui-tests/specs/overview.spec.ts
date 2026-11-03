// The Changes overview: the file tree's status markers, folder dots and "Changed only" line counts, the overview's
// sections and their diffs, which load as they scroll into view, big diffs that start collapsed and load only when
// opened, and long overviews drawn a page of sections at a time.
import {
  test, expect, lines, treeRow, folderRow, section, sectionPaths, overviewTitle, viewLabel, recordRequests, recordEvents,
  addedLines, removedLines, hold, focusMain, Repo,
} from '../fixtures';
import type { Page } from '@playwright/test';
import * as fs from 'node:fs';

// "<name> 1" … "<name> n".
const numbered = (name: string, n: number) => Array.from({ length: n }, (_, i) => `${name} ${i + 1}`);

// One file of each kind Changes mode shows, next to unchanged files in a changed folder (src) and an unchanged folder (lib).
// Each file's content is its own, so git can't pair the deleted file with the renamed one.
function mixedChanges(repo: Repo) {
  const own = (name: string) => lines(10).replace(/line/g, name);
  repo.write('src/staged.txt', lines(10)).write('src/unstaged.txt', lines(10)).write('src/same.txt', own('same'))
    .write('lib/keep.txt', own('keep')).write('gone.txt', own('gone')).write('old-name.txt', own('renamed'));
  repo.commit('initial');
  repo.write('src/staged.txt', lines(10, 2)).add('src/staged.txt');
  repo.write('src/unstaged.txt', lines(10, 4));
  repo.write('added.txt', lines(3)).add('added.txt');
  repo.write('untracked.txt', lines(2));
  repo.rm('gone.txt');
  repo.rename('old-name.txt', 'docs/new-name.txt');
}

test('Changes mode marks each changed file with its status', async ({ page, repo, revue }) => {
  mixedChanges(repo);
  const s = await revue.start();
  await s.open(page);

  const expected: [string, string, string][] = [
    ['src/staged.txt', 'M', 'modified'],
    ['src/unstaged.txt', 'M', 'modified'],
    ['added.txt', 'A', 'added'],
    ['untracked.txt', 'U', 'untracked'],
    ['gone.txt', 'D', 'deleted'],
    ['docs/new-name.txt', 'R', 'renamed'],
  ];
  for (const [path, badge, label] of expected) {
    const row = treeRow(page, path);
    await expect(row).toBeVisible();
    await expect(row.locator('.badge')).toHaveText(badge);
    await expect(row).toHaveAttribute('title', `${path} (${label})`);
  }
  // The rename's old path is gone from the tree, and unchanged files have no marker.
  await expect(treeRow(page, 'old-name.txt')).toHaveCount(0);
  await expect(treeRow(page, 'src/same.txt').locator('.badge')).toHaveText('');
  await expect(treeRow(page, 'src/same.txt')).toHaveAttribute('title', 'src/same.txt');
  await expect(page.locator('#summary')).toContainText('6 files');

  // The overview's sections carry the same letters, and the rename shows where it came from.
  await expect(page.locator('#main .section')).toHaveCount(6);
  for (const [path, badge] of expected) await expect(section(page, path).locator('.file-head .letter')).toHaveText(badge);
  await expect(section(page, 'docs/new-name.txt').locator('.file-head')).toContainText('old-name.txt → docs/new-name.txt');
});

test('folders with changes get a dot and "Changed only" hides everything else', async ({ page, repo, revue }) => {
  mixedChanges(repo);
  const s = await revue.start();
  await s.open(page);

  const src = folderRow(page, 'src'), lib = folderRow(page, 'lib');
  await expect(src.locator('.dot')).toHaveAttribute('title', '2 changed');
  await expect(lib).toBeVisible();
  await expect(lib.locator('.dot')).toHaveCount(0);
  // Folders holding changes open by themselves; the others stay closed.
  await expect(src).toHaveAttribute('aria-expanded', 'true');
  await expect(lib).toHaveAttribute('aria-expanded', 'false');
  await expect(treeRow(page, 'src/same.txt')).toBeVisible();

  const changedOnly = page.getByLabel('Changed only');
  await changedOnly.check();
  await expect(lib).toHaveCount(0);
  await expect(treeRow(page, 'src/same.txt')).toHaveCount(0);
  await expect(treeRow(page, 'src/staged.txt')).toBeVisible();
  await expect(treeRow(page, 'src/unstaged.txt')).toBeVisible();
  await expect(treeRow(page, 'gone.txt')).toBeVisible();
  await expect(page.locator('#tree [role="treeitem"]')).toHaveCount(8); // 6 files, src and docs

  await changedOnly.uncheck();
  await expect(lib).toBeVisible();
  await expect(treeRow(page, 'src/same.txt')).toBeVisible();
});

test('overview sections show their diffs', async ({ page, repo, revue }) => {
  mixedChanges(repo);
  const s = await revue.start();
  await s.open(page);

  await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
  await expect(page.locator('#main .overview-head .meta')).toHaveText('6 files · +7 −12');
  await expect(viewLabel(page)).toHaveText('All changes · 6 files');

  // A modified file removes the old line and adds its replacement, and nothing else.
  for (const [path, n] of [['src/unstaged.txt', 5], ['src/staged.txt', 3]] as const) {
    const sec = section(page, path);
    await expect(addedLines(sec)).toHaveText([`edited ${n}`]);
    await expect(removedLines(sec)).toHaveText([`line ${n}`]);
  }
  // New files are all additions, a deleted one all removals.
  for (const [path, n] of [['added.txt', 3], ['untracked.txt', 2]] as const) {
    await expect(addedLines(section(page, path))).toHaveText(numbered('line', n));
    await expect(removedLines(section(page, path))).toHaveCount(0);
  }
  await expect(removedLines(section(page, 'gone.txt'))).toHaveText(numbered('gone', 10));
  await expect(addedLines(section(page, 'gone.txt'))).toHaveCount(0);
  // A pure rename has nothing to show line by line: git's rename lines stand in for the diff.
  const renamed = section(page, 'docs/new-name.txt');
  await expect(renamed.locator('.diff-body')).toHaveText('similarity index 100% · rename from old-name.txt · rename to docs/new-name.txt');
  await expect(renamed.getByRole('table')).toHaveCount(0);
});

test('a file with over 1500 changed lines starts collapsed and loads when opened', async ({ page, repo, revue }) => {
  repo.write('small.txt', lines(5)).commit('initial');
  repo.write('small.txt', lines(5, 0));
  repo.write('big.txt', lines(1600));
  const s = await revue.start();

  // Every diff request from the first page load on, and those of them that name big.txt.
  const diffRequests = recordRequests(page, /^\/api\/diffs?$/);
  const bigRequests = () => diffRequests.filter(u => new URL(u).searchParams.getAll('path').includes('big.txt'));
  await s.open(page);

  const big = section(page, 'big.txt');
  const toggle = big.getByRole('button', { name: '▸' });
  await expect(big.locator('.file-head')).toContainText('+1600');
  await expect(toggle).toBeVisible();
  await expect(big.locator('.diff-body')).toBeHidden();
  // The small file's diff comes from the first screen's prefetch and can be drawn before the IntersectionObserver that
  // loads sections as they come into view has called back; that observer would ask for big.txt too if it didn't skip
  // closed sections. Its callbacks are queued in the frame that first lays the sections out, so wait two more frames
  // for them to have run before checking that nothing asked for big.txt.
  await expect(section(page, 'small.txt').getByRole('table')).toContainText('edited 1');
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  expect(bigRequests()).toEqual([]);

  await toggle.click();
  await expect(big.getByRole('button', { name: '▾' })).toBeVisible();
  const table = big.getByRole('table');
  await expect(table).toBeVisible();
  await expect(table.locator('tr', { hasText: 'line 1600' })).toHaveCount(1);
  expect(bigRequests()).toHaveLength(1);

  // Closing it again hides the diff without dropping it. Tag the table with a property the app doesn't know about:
  // a table drawn again, even from the diff cache without a request, doesn't have it.
  await table.evaluate(t => { (t as any)._tagged = true; });
  await big.getByRole('button', { name: '▾' }).click();
  await expect(big.locator('.diff-body')).toBeHidden();
  await big.getByRole('button', { name: '▸' }).click();
  await expect(table).toBeVisible();
  expect(await table.evaluate(t => (t as any)._tagged)).toBe(true);
  expect(bigRequests()).toHaveLength(1);
});

const BINARY = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0, 1, 0, 2]);

// The tree shows a file's line counts only with "Changed only" on, where every row is a change.
test('"Changed only" shows each changed file\'s line counts in the tree', async ({ page, repo, revue }) => {
  mixedChanges(repo);
  repo.write('logo.bin', BINARY).add('logo.bin');
  const s = await revue.start();
  await s.open(page);
  const counts = page.locator('#tree .count');
  await expect(treeRow(page, 'logo.bin').locator('.badge')).toHaveText('A');
  await expect(counts).toHaveCount(0);

  const changedOnly = page.getByLabel('Changed only');
  await changedOnly.check();
  await expect(treeRow(page, 'src/same.txt')).toHaveCount(0);
  const expected: [string, string][] = [
    ['src/staged.txt', '+1 −1'],
    ['src/unstaged.txt', '+1 −1'],
    ['added.txt', '+3'],
    ['untracked.txt', '+2'],
    ['gone.txt', '−10'],
    ['logo.bin', 'bin'],
  ];
  for (const [path, text] of expected) await expect(treeRow(page, path).locator('.count')).toHaveText(text);

  await changedOnly.uncheck();
  await expect(treeRow(page, 'src/same.txt')).toBeVisible();
  await expect(counts).toHaveCount(0);
});

// git counts no lines in a binary file, which the tree and the section's header mark "bin". The server reports an
// untracked one with null counts, as git's numstat gives a staged one, so it is marked the same, and the totals,
// which add up the counts there are, leave both out.
test('an untracked binary file is marked "bin" like a staged one', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3)).commit('initial');
  repo.write('a.txt', lines(3, 1));
  repo.write('staged.bin', BINARY).add('staged.bin');
  repo.write('untracked.bin', BINARY);
  const s = await revue.start();
  await s.open(page);
  await page.getByLabel('Changed only').check();
  for (const path of ['staged.bin', 'untracked.bin']) {
    await expect(treeRow(page, path).locator('.count')).toHaveText('bin');
    await expect(section(page, path).locator('.file-head')).toContainText('bin');
  }
  await expect(treeRow(page, 'a.txt').locator('.count')).toHaveText('+1 −1');
  await expect(page.locator('#summary')).toHaveText('3 files +1 −1');
  await expect(page.locator('#main .overview-head .meta')).toHaveText('3 files · +1 −1');
});

// git shows a link as itself: its target path, one added line. A link to a directory showed an empty diff, since
// `git diff --no-index` follows it and fails; the server writes the link's diff itself, so it shows like any other.
test('an untracked link to a directory shows its target as one added line', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3)).write('d/f.txt', lines(2)).commit('initial');
  fs.symlinkSync('d', repo.abs('to-dir'));
  fs.symlinkSync('a.txt', repo.abs('to-file'));
  const s = await revue.start();
  await s.open(page);
  for (const [path, target] of [['to-dir', 'd'], ['to-file', 'a.txt']]) {
    await expect(addedLines(section(page, path))).toHaveText([target]);
    await expect(treeRow(page, path).locator('.badge')).toHaveText('U');
  }
});

// git lists a repo nested in the work tree as one untracked entry, `nest/`, and adds it as a gitlink: the commit
// checked out there, one line. Its diff was empty; it now shows that line, and follows a commit made in the nested repo.
test('an untracked nested repo shows the commit it has checked out, and follows it', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3)).commit('initial');
  fs.mkdirSync(repo.abs('nest'));
  const nest = new Repo(repo.abs('nest')).init();
  const first = nest.write('x.txt', lines(1)).commit('inner');
  const s = await revue.start();
  await s.open(page);
  await expect(treeRow(page, 'nest/').locator('.badge')).toHaveText('U');
  await expect(addedLines(section(page, 'nest/'))).toHaveText([`Subproject commit ${first}`]);
  const second = nest.write('x.txt', lines(2)).commit('inner 2');
  await expect(addedLines(section(page, 'nest/'))).toHaveText([`Subproject commit ${second}`]);
});

// A nested repo with no commit yet has nothing git could add, so its diff is only a header; the page says why
// rather than "No textual changes", in the overview and with the entry open.
test('an untracked nested repo with no commits says so', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3)).commit('initial');
  fs.mkdirSync(repo.abs('empty'));
  new Repo(repo.abs('empty')).init();
  const s = await revue.start();
  await s.open(page);
  const message = 'Nested git repository with no commits yet';
  await expect(section(page, 'empty/').locator('.placeholder')).toHaveText(message);
  await treeRow(page, 'empty/').click();
  await expect(viewLabel(page)).toContainText('empty/');
  await expect(page.locator('#main .placeholder')).toHaveText(message);
});

// A file over 2 MB isn't shown, but one whose start holds a NUL is binary however big it is, as git decides it, so
// the file view says so rather than "too large", for the working tree and a commit alike.
test('a binary file over 2 MB opens in the file view as binary, not too large', async ({ page, repo, revue }) => {
  const big = Buffer.alloc(3 * 1024 * 1024, 'x');
  big[100] = 0;
  const text = Buffer.alloc(3 * 1024 * 1024, 'y');
  repo.write('big.bin', big).write('big.txt', text).commit('big files');
  const s = await revue.start();
  for (const hash of ['mode=working', 'mode=commit&ref=HEAD']) {
    await s.open(page, `${hash}&file=big.bin&view=file`);
    await expect(page.locator('#main .placeholder')).toHaveText(`Binary file (${big.length} bytes)`);
    await s.open(page, `${hash}&file=big.txt&view=file`);
    await expect(page.locator('#main .placeholder')).toHaveText(`File too large to display (${text.length} bytes)`);
  }
});

// An untracked text file over 2 MB isn't counted (the tree has no line count for it), and the server doesn't diff it,
// since its diff would be all of it. Its section starts collapsed, as a big diff's does, and isn't asked for until it
// is opened; opened, in the overview or on its own, it says it is too large, as the file view does. No answer the page
// gets holds its text.
test('an untracked text file over 2 MB starts collapsed and says it is too large to display', async ({ page, repo, revue }) => {
  repo.write('small.txt', lines(5)).commit('initial');
  repo.write('small.txt', lines(5, 0));
  const big = 'y\n'.repeat(3 * 512 * 1024);
  repo.write('big.log', big);
  const s = await revue.start();

  const diffRequests = recordRequests(page, /^\/api\/diffs?$/);
  const bigRequests = () => diffRequests.filter(u => new URL(u).searchParams.getAll('path').includes('big.log'));
  const answers: Promise<string>[] = [];
  page.on('response', r => {
    if (/^\/api\/diffs?$/.test(new URL(r.url()).pathname)) answers.push(r.text().catch(() => ''));
  });
  await s.open(page);

  const sec = section(page, 'big.log');
  const message = `File too large to display (${big.length} bytes)`;
  await expect(sec.getByRole('button', { name: '▸' })).toBeVisible();
  await expect(sec.locator('.diff-body')).toBeHidden();
  // As in the 1500-line test above: the observer's callbacks have run two frames after the first screen is drawn.
  await expect(section(page, 'small.txt').getByRole('table')).toContainText('edited 1');
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  expect(bigRequests()).toEqual([]);

  await sec.getByRole('button', { name: '▸' }).click();
  await expect(sec.locator('.diff-body')).toHaveText(message);
  expect(bigRequests()).toHaveLength(1);

  // On its own, its diff says the same, with no context or layout to set.
  await treeRow(page, 'big.log').click();
  await expect(viewLabel(page)).toHaveText('big.log');
  await expect(page.locator('#main .file-body')).toHaveText(message);
  await expect(page.locator('#context-ctl')).toBeHidden();
  await expect(page.locator('#layout-seg')).toBeHidden();

  const texts = await Promise.all(answers);
  expect(texts.length).toBeGreaterThan(0);
  for (const t of texts) expect(t.length, t.slice(0, 200)).toBeLessThan(10_000);
});

// Only the first screen's diffs are fetched up front; the rest load as their sections come within the observer's
// margin of the viewport.
test('overview diffs load only as their sections come into view', async ({ page, repo, revue }) => {
  const files = Array.from({ length: 40 }, (_, i) => `f${String(i + 1).padStart(2, '0')}.txt`);
  for (const f of files) repo.write(f, lines(10));
  repo.commit('initial');
  for (const f of files) repo.write(f, lines(10, 4));
  const s = await revue.start();
  const diffRequests = recordRequests(page, /^\/api\/diffs?$/);
  const requested = () => diffRequests.flatMap(u => new URL(u).searchParams.getAll('path'));
  await s.open(page);

  await expect(viewLabel(page)).toHaveText('All changes · 40 files');
  await expect(addedLines(section(page, 'f01.txt'))).toHaveText(['edited 5']);
  // The observer's first callbacks are queued in the frame that first lays the sections out (see the 1500-line test
  // above): let two more frames run so that anything it was going to ask for has been asked for.
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  // Each section is under 200 px tall, so the last ten are thousands of pixels below the first screen.
  const far = files.slice(30);
  expect(requested()).toContain('f01.txt');
  expect(requested().filter(p => far.includes(p))).toEqual([]);
  expect(await section(page, 'f40.txt').getByRole('table').count()).toBe(0);

  // Jumping to the end loads the last section, and not the ones the jump went past.
  const last = section(page, 'f40.txt');
  await last.scrollIntoViewIfNeeded();
  await expect(addedLines(last)).toHaveText(['edited 5']);
  expect(requested()).toContain('f40.txt');
  expect(requested()).not.toContain('f20.txt');
  await expect(section(page, 'f20.txt').getByRole('table')).toHaveCount(0);
});

// The page holds the next view's range as soon as it is asked for, and api() adds the range to every request. The
// overview still shown while History waits for its commit list (held here) used to load the sections scrolled to with
// History's range: the wrong diffs, for sections that are about to go.
test('an overview being replaced loads no diffs with the next view\'s range', async ({ page, repo, revue }) => {
  const files = Array.from({ length: 60 }, (_, i) => `f${String(i + 1).padStart(2, '0')}.txt`);
  for (const f of files) repo.write(f, lines(10));
  repo.commit('initial');
  for (const f of files) repo.write(f, lines(10, 4));
  const s = await revue.start();
  await s.open(page);
  await expect(addedLines(section(page, 'f01.txt'))).toHaveText(['edited 5']);
  const asked = recordRequests(page, /^\/api\/diffs?$/);
  const forHistory = () => asked.filter(u => new URL(u).searchParams.get('mode') === 'commit');

  const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
  await focusMain(page);
  await page.keyboard.press('2');
  await list.held;
  await page.locator('#main').evaluate(el => { el.scrollTop = el.scrollHeight; });
  // Two frames for the observer's callbacks (see the test above), then time for any request they make.
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.waitForTimeout(500);
  expect(forHistory()).toEqual([]);

  // The control: History, once drawn, loads its own diffs.
  list.release();
  await expect(overviewTitle(page)).toHaveText('initial');
  await expect.poll(() => forHistory().length).toBeGreaterThan(0);
});

// The sections skipped meanwhile keep what they need to load: when the same overview is asked for again before the other
// view is drawn (1 pressed while History still waits for its list), its render takes them over, and they load.
test('sections skipped while another view loaded load once their overview is asked for again', async ({ page, repo, revue }) => {
  const files = Array.from({ length: 60 }, (_, i) => `f${String(i + 1).padStart(2, '0')}.txt`);
  for (const f of files) repo.write(f, lines(10));
  repo.commit('initial');
  for (const f of files) repo.write(f, lines(10, 4));
  const s = await revue.start();
  await s.open(page);
  await expect(addedLines(section(page, 'f01.txt'))).toHaveText(['edited 5']);

  const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
  await focusMain(page);
  await page.keyboard.press('2');
  await list.held;
  await page.locator('#main').evaluate(el => { el.scrollTop = el.scrollHeight; });
  await page.keyboard.press('1');
  await expect(addedLines(section(page, 'f60.txt'))).toHaveText(['edited 5']);
  await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
  list.release();
  await list.answered();
});

// Going back to an overview seen before draws its diffs from diffCache. Its first screen used to be asked for all the
// same (prefetchDiffs didn't look at the cache): a request for nothing, every time. A file edited meanwhile has a new
// version tag, so its diff isn't cached: only it is asked for.
test('an overview seen before asks only for the diffs that changed since', async ({ page, repo, revue }) => {
  const files = ['a.txt', 'b.txt', 'c.txt'];
  for (const f of files) repo.write(f, lines(5));
  repo.commit('initial');
  for (const f of files) repo.write(f, lines(5, 2));
  const s = await revue.start();
  await s.open(page);
  for (const f of files) await expect(addedLines(section(page, f))).toHaveText(['edited 3']);
  const messages = await recordEvents(page);
  await page.keyboard.press('2');
  await expect(overviewTitle(page)).toHaveText('initial');
  // History ignores working-tree edits: b.txt's is seen only by the tree loaded on the way back.
  repo.write('b.txt', lines(5, 3));
  await expect.poll(messages).toContain('worktree');

  const asked = recordRequests(page, /^\/api\/diffs?$/);
  await page.keyboard.press('1');
  await expect(addedLines(section(page, 'b.txt'))).toHaveText(['edited 4']);
  for (const f of ['a.txt', 'c.txt']) await expect(addedLines(section(page, f))).toHaveText(['edited 3']);
  expect(asked.flatMap(u => new URL(u).searchParams.getAll('path'))).toEqual(['b.txt']);
});

// Big diffs start collapsed when they change more than 1500 lines, removed and added lines together.
test('a file with exactly 1500 changed lines starts open and one with 1501 starts collapsed', async ({ page, repo, revue }) => {
  const old = lines(750);
  repo.write('exact.txt', old).write('over.txt', old).commit('initial');
  const rewritten = old.replace(/line/g, 'new');
  repo.write('exact.txt', rewritten); // −750 +750
  repo.write('over.txt', rewritten + 'new 751\n'); // −750 +751
  const s = await revue.start();
  await s.open(page);

  const exact = section(page, 'exact.txt'), over = section(page, 'over.txt');
  await expect(exact.locator('.file-head')).toContainText('+750 −750');
  await expect(over.locator('.file-head')).toContainText('+751 −750');
  await expect(exact.getByRole('button', { name: '▾' })).toBeVisible();
  await expect(addedLines(exact)).toHaveCount(750);
  await expect(removedLines(exact)).toHaveCount(750);
  await expect(over.getByRole('button', { name: '▸' })).toBeVisible();
  await expect(over.locator('.diff-body')).toBeHidden();
});

// The overview draws 200 sections (OVERVIEW_PAGE), then a button for the next 200, so thousands of changed files
// don't build thousands of sections at once.
test('a long overview shows 200 files at a time, with a button for the next ones', async ({ page, repo, revue }) => {
  repo.write('README', 'readme\n').commit('initial');
  const files = Array.from({ length: 401 }, (_, i) => `f${String(i + 1).padStart(3, '0')}.txt`);
  for (const f of files) repo.write(f, `${f}\n`);
  const s = await revue.start();
  await s.open(page);

  await expect(viewLabel(page)).toHaveText('All changes · 401 files');
  const more = page.getByRole('button', { name: /^Show \d+ more of \d+ remaining files?$/ });
  await expect(more).toHaveText('Show 200 more of 201 remaining files');
  await expect.poll(() => sectionPaths(page)).toEqual(files.slice(0, 200));

  await more.click();
  await expect(more).toHaveText('Show 1 more of 1 remaining file');
  await expect.poll(() => sectionPaths(page)).toEqual(files.slice(0, 400));

  await more.click();
  await expect(more).toHaveCount(0);
  await expect.poll(() => sectionPaths(page)).toEqual(files);
  // The sections added last load their diffs like the others once they are scrolled to.
  const last = section(page, 'f401.txt');
  await last.scrollIntoViewIfNeeded();
  await expect(addedLines(last)).toHaveText(['f401.txt']);
});

// The first screen's diffs are asked for as soon as the tree arrives (prefetchDiffs), and each section takes its diff
// from that request once it loads (fetchDiffs). When the request fails, the failure must reach the sections that take
// it, and be handled for the ones that never do. A rejection nobody handled fails the test (the guard reports it as
// "uncaught: diffs failed").
test.describe('a failed prefetch of the first screen', () => {
  // /api/diffs answers 500 on purpose (page.route), which the guard otherwise fails the test for.
  test.use({ allowServerErrors: true });

  const files = ['a.txt', 'b.txt', 'c.txt'];
  function threeEdits(repo: Repo) {
    for (const f of files) repo.write(f, lines(5));
    repo.commit('initial');
    for (const f of files) repo.write(f, lines(5, 2));
  }
  const failDiffs = (page: Page) => page.route(u => u.pathname === '/api/diffs', route => route.fulfill({
    status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'diffs failed' }),
  }));

  // The sections take their diffs after the first paint, so a request that fails quickly has failed before they do.
  // Each section then asks again, and shows the error. Whether the failure comes first depends on timing: before the
  // fix it was reported as an uncaught error in more than half the runs (repeat this test to see it).
  test('each section shows the error, and nothing is left uncaught', async ({ page, repo, revue }) => {
    threeEdits(repo);
    const s = await revue.start();
    await failDiffs(page);
    await s.open(page);
    for (const f of files) await expect(section(page, f).locator('.diff-body')).toHaveText('Error: diffs failed');
  });

  // A prefetched diff no section takes: the context changes just as the first screen's diffs are asked for, before the
  // next frame, when the sections could first take them (an init script changes it the moment the page sends that
  // request). The sections load with the new context and ask the server for it, and the prefetch for the old context is
  // never taken. (Coming back to an overview seen before made such entries too, for every section that drew a cached
  // diff, until prefetchDiffs left out the diffs that are cached.)
  test('a failed prefetch that no section takes is not left uncaught either', async ({ page, repo, revue }) => {
    threeEdits(repo);
    const s = await revue.start();
    await page.addInitScript(() => {
      const send = window.fetch;
      let first = true;
      window.fetch = function (input: RequestInfo | URL, init?: RequestInit) {
        const res = send.call(this, input, init);
        if (first && new URL(String(input), location.href).pathname === '/api/diffs') {
          first = false;
          queueMicrotask(() => {
            const context = document.querySelector<HTMLSelectElement>('#context-select')!;
            context.value = '10';
            context.dispatchEvent(new Event('change'));
          });
        }
        return res;
      };
    });
    await failDiffs(page);
    const asked = recordRequests(page, /^\/api\/diffs$/);
    await s.open(page);
    for (const f of files) await expect(section(page, f).locator('.diff-body')).toHaveText('Error: diffs failed');
    await expect(page.locator('#context-select')).toHaveValue('10');
    // The prefetch was for 3 lines of context, and the sections asked for 10 themselves.
    const contexts = asked.map(u => new URL(u).searchParams.get('context'));
    expect(contexts[0]).toBe('3');
    expect(contexts.slice(1)).toContain('10');
    expect(contexts.slice(1)).not.toContain('3');
  });
});
