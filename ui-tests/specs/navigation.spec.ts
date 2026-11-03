// Views and how they're reached: the URL hash (written as you go, restored on reload, and a link to a file renders
// only that file), agent navigation (POST /api/navigate moves only the most recently focused tab, or the others once
// that one is closed or left, and can open a markdown or SVG file's preview), browser Back and Forward, a view still loading when a newer one is picked, and target
// lines (highlighted with .target and scrolled to just below the sticky file header).
import {
  test, expect, lines, modeTab, overviewTitle, treeRow, commitList, viewLabel, targetRows, focusMain, reload,
  recordRequests, sectionPaths, reEsc, hold, section, addedLines, recordEvents, type Repo, type Server,
} from '../fixtures';
import type { Page, Request } from '@playwright/test';

// `s` as a regular expression that matches only itself.

// Records whether an overview (its head or a file section) was ever put into #main, from before the page's own
// scripts run. resetOverviewSeen starts over, for a check that covers only what happens after it.
async function watchForOverview(page: Page) {
  await page.addInitScript(() => {
    const w = window as any;
    w.__overviewSeen = false;
    w.__watching = true;
    const isOverview = (n: Node) => n instanceof Element &&
      (n.matches('.overview-head, .section') || !!n.querySelector('.overview-head, .section'));
    new MutationObserver(records => {
      for (const r of records) {
        if ((r.target as Element).closest?.('#main') && [...r.addedNodes].some(isOverview)) w.__overviewSeen = true;
      }
    }).observe(document, { childList: true, subtree: true });
  });
}
const overviewSeen = (page: Page) => page.evaluate(() => (window as any).__overviewSeen as boolean);
const resetOverviewSeen = (page: Page) => page.evaluate(() => { (window as any).__overviewSeen = false; });

// Brings `page` to the front and tells it that it has focus, as switching to its browser tab does. Headless browsers
// treat every page as focused and visible, so bringToFront alone fires neither `focus` nor `visibilitychange`.
async function focusTab(page: Page) {
  await page.bringToFront();
  await page.evaluate(() => window.dispatchEvent(new FocusEvent('focus')));
}

test.describe('the URL hash', () => {
  test('keeps the mode, commit, file and view across a reload', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(20));
    const first = repo.commit('first');
    repo.write('a.txt', lines(20, 4));
    repo.write('b.txt', 'b\n');
    repo.commit('second');
    const s = await revue.start();
    await s.open(page);

    await modeTab(page, 'History').click();
    await commitList(page).getByRole('option', { name: /^first/ }).click();
    await expect(overviewTitle(page)).toHaveText('first');
    await treeRow(page, 'a.txt').click();
    await page.locator('#main .file-head').getByRole('button', { name: 'File', exact: true }).click();
    await expect(page.locator('#main table.fileview')).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${first}&file=a\\.txt&view=file$`));

    await reload(page);
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(commitList(page).getByRole('option', { name: /^first/ })).toHaveAttribute('aria-selected', 'true');
    await expect(treeRow(page, 'a.txt')).toHaveAttribute('aria-selected', 'true');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'File', exact: true })).toHaveClass(/\bon\b/);
    await expect(page.locator('#main table.fileview tr[data-n]')).toHaveCount(20);
    await expect(page.locator('#main table.fileview')).toContainText('line 5'); // the first commit's text
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${first}&file=a\\.txt&view=file$`));
  });

  test('a link to a file and lines renders only that file, and keeps it across a reload', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(40));
    repo.write('b.txt', 'b\n');
    repo.commit('base');
    repo.write('a.txt', lines(40, 29));
    repo.write('b.txt', 'b changed\n');
    const s = await revue.start();
    await watchForOverview(page);
    // The overview's diffs come in one /api/diffs request (fetched ahead of drawing it); a single file uses /api/diff.
    const diffs = recordRequests(page, /^\/api\/diffs$/);
    const hash = 'mode=working&file=a.txt&view=file&line=12&endLine=14';
    await s.open(page, hash);

    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(targetRows(page)).toHaveCount(3);
    await expect(targetRows(page).first()).toHaveAttribute('data-n', '12');
    await expect(treeRow(page, 'a.txt')).toHaveAttribute('aria-selected', 'true');
    expect(await page.evaluate(() => (window as any).__watching)).toBe(true); // the init script ran under the CSP
    expect(await overviewSeen(page)).toBe(false);
    expect(diffs).toEqual([]);

    await reload(page);
    await expect(targetRows(page)).toHaveCount(3);
    await expect(targetRows(page).last()).toHaveAttribute('data-n', '14');
    await expect(page).toHaveURL(new RegExp(`#${reEsc(hash)}$`));
    expect(await overviewSeen(page)).toBe(false);
    expect(diffs).toEqual([]);

    // Positive control: the overview, and its diffs' request, are noticed when it does appear.
    await focusMain(page);
    await page.keyboard.press('Escape');
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    expect(await overviewSeen(page)).toBe(true);
    await expect.poll(() => diffs.length).toBeGreaterThan(0);
  });

  test('keeps the branch to view (head) across a reload', async ({ page, repo, revue }) => {
    // main is checked out; feature, from "base", adds c.txt.
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    repo.switch('main');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('main vs main');
    await page.getByLabel('Branch to view').selectOption('feature');
    await treeRow(page, 'c.txt').click();
    await expect(viewLabel(page)).toHaveText('c.txt');
    const hash = 'mode=branch&base=main&head=feature&file=c.txt';
    await expect(page).toHaveURL(new RegExp(`#${reEsc(hash)}$`));

    await reload(page);
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    // feature, not the checked-out main: the header, the picker and the diff.
    await expect(page.locator('#branch-name')).toHaveText('feature');
    await expect(page.locator('#branch-note')).toHaveText('view only');
    await expect(page.getByLabel('Branch to view')).toHaveValue('feature');
    await expect(viewLabel(page)).toHaveText('c.txt');
    await expect(treeRow(page, 'c.txt')).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('#main table.code')).toContainText('feature line');
    await expect(page).toHaveURL(new RegExp(`#${reEsc(hash)}$`));
    expect(repo.currentBranch()).toBe('main');
  });

  test('keeps the old side of a file across a reload', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(20));
    repo.commit('base');
    repo.write('a.txt', lines(20, 4));
    const s = await revue.start();
    const hash = 'mode=working&file=a.txt&view=file&side=old';
    await s.open(page, hash);

    const head = page.locator('#main .file-head');
    const fileView = page.locator('#main table.fileview');
    for (const when of ['opened', 'reloaded']) {
      await test.step(when, async () => {
        if (when === 'reloaded') await reload(page);
        await expect(head.getByRole('button', { name: 'Before', exact: true })).toHaveClass(/\bon\b/);
        await expect(head.getByRole('button', { name: 'After', exact: true })).not.toHaveClass(/\bon\b/);
        // The committed text, numbered by the old side's lines.
        await expect(fileView.locator('tr[data-o="5"] td.src')).toHaveText('line 5');
        await expect(fileView.locator('tr[data-o]')).toHaveCount(20);
        await expect(fileView).not.toContainText('edited 5');
        await expect(page).toHaveURL(new RegExp(`#${reEsc(hash)}$`));
      });
    }
  });

  // The Before and After buttons used to redraw without writing the hash, so a reload or a copied link lost the side.
  test('picking Before writes side=old to the hash, so a reload keeps it, and After takes it out', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(20));
    repo.commit('base');
    repo.write('a.txt', lines(20, 4));
    const s = await revue.start();
    await s.open(page, 'mode=working&file=a.txt&view=file');
    const head = page.locator('#main .file-head');
    const before = head.getByRole('button', { name: 'Before', exact: true });
    const after = head.getByRole('button', { name: 'After', exact: true });
    await before.click();
    await expect(before).toHaveClass(/\bon\b/);
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&view=file&side=old$/);
    await reload(page);
    await expect(before).toHaveClass(/\bon\b/);
    await expect(page.locator('#main table.fileview tr[data-o="5"] td.src')).toHaveText('line 5');

    await after.click();
    await expect(after).toHaveClass(/\bon\b/);
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&view=file$/);
    await reload(page);
    await expect(after).toHaveClass(/\bon\b/);
    await expect(page.locator('#main table.fileview tr[data-n="5"] td.src')).toHaveText('edited 5');
  });

  test('a link to a commit\'s file renders only that file', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(10));
    repo.write('b.txt', 'b\n');
    const first = repo.commit('first');
    repo.write('a.txt', lines(10, 3));
    repo.commit('second');
    const s = await revue.start();
    await watchForOverview(page);
    await s.open(page, `mode=commit&ref=${first}&file=b.txt`);

    await expect(viewLabel(page)).toHaveText('b.txt');
    // The first commit added b.txt: its diff, whose one line is an addition (an add row in the unified layout, an add
    // cell beside an empty one in the split layout).
    await expect(page.locator('#main tr.add[data-n="1"] td.src, #main tr[data-n="1"] td.src.add')).toHaveText('b');
    await expect(commitList(page).getByRole('option', { name: /^first/ })).toHaveAttribute('aria-selected', 'true');
    expect(await overviewSeen(page)).toBe(false);
  });
});

test.describe('agent navigation', () => {
  test('moves the most recently focused tab and not the other', async ({ page, context, repo, revue }) => {
    repo.write('a.txt', lines(10));
    repo.write('b.txt', lines(10));
    repo.commit('base');
    repo.write('a.txt', lines(10, 1));
    repo.write('b.txt', lines(10, 2));
    const s = await revue.start();
    const other = await context.newPage(); // the same context: the tabs share localStorage, as real tabs do
    await s.open(page);
    await s.open(other);
    await focusTab(page);

    expect((await s.navigate({ file: 'a.txt', line: 2 })).tabs).toBe(2);
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(targetRows(page).first()).toHaveAttribute('data-n', '2');
    await page.waitForTimeout(500);
    await expect(viewLabel(other)).toHaveText(/^All changes/);
    await expect(overviewTitle(other)).toHaveText('Uncommitted changes');

    // Positive control: once the other tab has focus, it is the one that moves.
    await focusTab(other);
    await s.navigate({ file: 'b.txt', line: 3 });
    await expect(viewLabel(other)).toHaveText('b.txt');
    await expect(targetRows(other).first()).toHaveAttribute('data-n', '3');
    await expect(viewLabel(page)).toHaveText('a.txt');
  });

  // A tab that goes away gives up its claim (pagehide), so no tab holds it and the others follow until one is focused.
  // Closing the tab and leaving the page both fire pagehide. Playwright closes a page the way the tab's close button
  // does (gBrowser.removeTab in Firefox), with beforeunload first if asked to, as a user's close goes. Firefox can stop
  // a closing tab's pagehide handler at its first localStorage access, so the claim it removes stays: it did on most
  // closes, both ways, until the tab told the others it was going. The last case loses the tab's own release every
  // time, so only the others' can drop the claim.
  const leave = {
    'closed': (p: Page) => p.close(),
    'closed after beforeunload': async (p: Page) => {
      const closed = p.waitForEvent('close');
      await p.close({ runBeforeUnload: true });
      await closed;
    },
    'navigated away': (p: Page) => p.goto('about:blank'),
    'closed without removing its claim itself': async (p: Page) => {
      await p.evaluate(() => { Storage.prototype.removeItem = () => {}; });
      await p.close();
    },
  };
  for (const [how, go] of Object.entries(leave)) {
    test(`moves the other tab once the most recently focused one is ${how}`, async ({ page, context, repo, revue }) => {
      repo.write('a.txt', lines(10));
      repo.commit('base');
      repo.write('a.txt', lines(10, 1));
      const s = await revue.start();
      const other = await context.newPage();
      await s.open(page);
      await s.open(other);
      await focusTab(other);
      await go(other);

      // (The server's count of tabs isn't checked: the gone tab's stream may not have been dropped yet.)
      await s.navigate({ file: 'a.txt', line: 2 });
      await expect(viewLabel(page)).toHaveText('a.txt');
      await expect(targetRows(page)).toHaveAttribute('data-n', '2');
    });
  }

  test('scrolls the file tree to the file, in the view already shown too', async ({ page, repo, revue }) => {
    for (let i = 0; i < 80; i++) repo.write(`f${String(i).padStart(2, '0')}.txt`, `${i}\n`);
    repo.commit('base');
    repo.write('f70.txt', 'edited\n');
    const s = await revue.start();
    await s.open(page);
    await expect(treeRow(page, 'f00.txt')).toBeInViewport();
    await expect(treeRow(page, 'f70.txt')).not.toBeInViewport();

    // A changed file and unchanged ones, all in the tree's open root folder: nothing to rebuild, only to scroll to.
    for (const file of ['f70.txt', 'f05.txt', 'f78.txt']) {
      await s.navigate({ file });
      await expect(viewLabel(page)).toHaveText(file);
      await expect(treeRow(page, file)).toHaveAttribute('aria-selected', 'true');
      await expect(treeRow(page, file)).toBeInViewport();
    }
  });

  // Only a mode change used to fill in the default base, so a tab that was in Branch mode already lost its base.
  test('to Branch mode without a base compares with the default base, also from Branch mode', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('feature vs main');

    // A file the branch changed opens in its diff.
    await s.navigate({ mode: 'branch', file: 'c.txt' });
    await expect(viewLabel(page)).toHaveText('c.txt');
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Diff', exact: true })).toHaveClass(/\bon\b/);
    await expect(page.locator('#main table.code')).not.toHaveClass(/fileview/);
    await expect(page.locator('#main table.code')).toContainText('feature line');
    await expect(page).toHaveURL(/#mode=branch&base=main&file=c\.txt$/);
    await expect(page.getByLabel('Compared with')).toHaveValue('main');

    await s.navigate({ mode: 'branch' });
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'c.txt');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(page.getByLabel('Compared with')).toHaveValue('main');
  });

  // An agent can show a file's preview: a markdown file rendered, an SVG drawn. Both files are changed, so without the
  // view they would open in their diffs.
  test('opens a markdown file in its preview and an SVG in its Image view', async ({ page, repo, revue }) => {
    const svg = (fill: string) => `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"><rect width="8" height="6" fill="${fill}"/></svg>\n`;
    repo.write('README.md', '# Old title\n');
    repo.write('logo.svg', svg('#ff0000'));
    repo.commit('base');
    repo.write('README.md', '# New title\n');
    repo.write('logo.svg', svg('#0000ff'));
    const s = await revue.start();
    await s.open(page);

    await s.navigate({ file: 'README.md', view: 'preview' });
    await expect(page.locator('#main .markdown').getByRole('heading', { name: 'New title' })).toBeVisible();
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Preview', exact: true })).toHaveClass(/\bon\b/);
    await expect(page).toHaveURL(/#mode=working&file=README\.md&view=preview$/);

    await s.navigate({ file: 'logo.svg', view: 'preview' });
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Image', exact: true })).toHaveClass(/\bon\b/);
    await expect(page.getByRole('img', { name: 'logo.svg' })).toBeVisible();
    await expect(page).toHaveURL(/#mode=working&file=logo\.svg&view=preview$/);

    // The control: without a view, a changed file opens in its diff.
    await s.navigate({ file: 'README.md' });
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Diff', exact: true })).toHaveClass(/\bon\b/);
  });
});

test.describe('browser Back', () => {
  test('returns to the previous view and its scroll position, without drawing the overview first', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(200));
    const first = repo.commit('first');
    repo.write('b.txt', 'b\n');
    repo.commit('second');
    repo.write('a.txt', lines(200, 49));
    const s = await revue.start();
    await watchForOverview(page);
    await s.open(page);
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');

    await s.navigate({ file: 'a.txt', view: 'file', line: 150 });
    await expect(targetRows(page)).toHaveAttribute('data-n', '150');
    const main = page.locator('#main');
    await expect.poll(() => main.evaluate(el => el.scrollTop)).toBeGreaterThan(1000);
    const atTarget = await main.evaluate(el => el.scrollTop);
    // Scroll 700 px up from where the target put the page, so the position Back restores is the page's own: going to
    // the target again would land elsewhere.
    const scrolled = await main.evaluate(el => { el.scrollTop -= 700; return el.scrollTop; });
    expect(scrolled).toBe(atTarget - 700);

    await s.navigate({ mode: 'commit', ref: first, file: 'a.txt' });
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(page.locator('#main table.code')).toBeVisible();
    await resetOverviewSeen(page);

    await page.goBack();
    await expect(modeTab(page, 'Changes')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(targetRows(page)).toHaveAttribute('data-n', '150');
    await expect.poll(() => main.evaluate(el => el.scrollTop)).toBe(scrolled);
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&view=file&line=150$/);
    // The mode change drew the file straight away; the new mode's overview never came first.
    expect(await overviewSeen(page)).toBe(false);

    await page.goBack();
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    expect(await overviewSeen(page)).toBe(true); // positive control: the overview is noticed when it is drawn
    // The view belongs to the open file: with none open, the hash names no view.
    await expect(page).toHaveURL(/#mode=working$/);
  });

  test('lands on the previous view even when a live update overtakes it', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(20));
    const first = repo.commit('first');
    repo.write('a.txt', lines(20, 4));
    const s = await revue.start();
    await s.open(page);
    await s.navigate({ file: 'a.txt', view: 'file', line: 5 });
    await expect(targetRows(page)).toHaveAttribute('data-n', '5');
    await s.navigate({ mode: 'commit', ref: first, file: 'a.txt' });
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('a.txt');

    // Hold the tree that Back's mode change loads until a live update's newer tree load has been answered, so the
    // live update's refresh is the one that draws.
    const workingTrees: Request[] = [];
    page.on('request', r => {
      const u = new URL(r.url());
      if (u.pathname === '/api/tree' && u.searchParams.get('mode') === 'working') workingTrees.push(r);
    });
    let release!: () => void;
    const held = new Promise<void>(r => { release = r; });
    let holding = true;
    await page.route(u => u.pathname === '/api/tree' && u.searchParams.get('mode') === 'working', async route => {
      if (holding) { holding = false; await held; }
      await route.continue();
    });
    await page.goBack();
    await expect.poll(() => workingTrees.length).toBe(1);
    repo.branch('other'); // a ref change: a `git` live update, which refreshes the tree
    // (The watcher may report the ref change in more than one message, so there can be more than one.)
    await expect.poll(() => workingTrees.length).toBeGreaterThanOrEqual(2);
    await Promise.all(workingTrees.slice(1).map(r => r.response()));
    release();

    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(targetRows(page)).toHaveAttribute('data-n', '5');
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&view=file&line=5$/);
  });
});

test.describe('browser Forward', () => {
  test('returns to the view Back left, without drawing the overview first', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(200));
    const first = repo.commit('first');
    repo.write('b.txt', 'b\n');
    repo.commit('second');
    repo.write('a.txt', lines(200, 49));
    const s = await revue.start();
    await watchForOverview(page);
    await s.open(page);

    await s.navigate({ file: 'a.txt', view: 'file', line: 150 });
    await expect(targetRows(page)).toHaveAttribute('data-n', '150');
    await s.navigate({ mode: 'commit', ref: first, file: 'a.txt', view: 'file', line: 120 });
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(targetRows(page)).toHaveAttribute('data-n', '120');
    const later = new RegExp(`#mode=commit&ref=${first}&file=a\\.txt&view=file&line=120$`);
    await expect(page).toHaveURL(later);

    await page.goBack();
    await expect(modeTab(page, 'Changes')).toHaveAttribute('aria-current', 'page');
    await expect(targetRows(page)).toHaveAttribute('data-n', '150');
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&view=file&line=150$/);
    await resetOverviewSeen(page);

    await page.goForward();
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(commitList(page).getByRole('option', { name: /^first/ })).toHaveAttribute('aria-selected', 'true');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(targetRows(page)).toHaveAttribute('data-n', '120');
    await expect(targetRows(page)).toBeInViewport();
    // The first commit's text, not the working tree's.
    await expect(page.locator('#main table.fileview tr[data-n="50"] td.src')).toHaveText('line 50');
    await expect(page).toHaveURL(later);
    // The mode change drew the file straight away; the commit's overview never came first.
    expect(await overviewSeen(page)).toBe(false);
  });
});

// hold() for the first /api/tree request `match` picks.
const holdTree = (page: Page, match: (u: URL) => boolean) => hold(page, u => u.pathname === '/api/tree' && match(u));

// A refresh keeps what it was asked to do (open a file, close the open one) until it draws, in case a newer load
// overtakes it; a newer view, mode change, file pick or commit pick replaces that, so a view the user moved on from
// doesn't come back.
test.describe('a view still loading', () => {
  test('a file picked while a base change loads stays open', async ({ page, repo, revue }) => {
    // feature is checked out: c.txt and d.txt against main, only d.txt against older.
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    repo.branch('older');
    repo.write('d.txt', 'more feature\n');
    repo.commit('more feature work');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('feature vs main');

    // The base change closes the open file once its tree is in; the file picked meanwhile is newer.
    const tree = await holdTree(page, u => u.searchParams.get('base') === 'older');
    await page.getByLabel('Compared with').selectOption('older');
    await tree.held;
    await treeRow(page, 'd.txt').click();
    await expect(viewLabel(page)).toHaveText('d.txt');
    tree.release();

    // The held tree is drawn (c.txt is unchanged against older), and d.txt is still open.
    await expect(treeRow(page, 'c.txt').locator('.badge')).toHaveText('');
    await expect(viewLabel(page)).toHaveText('d.txt');
    await expect(page.locator('#main table.code')).toContainText('more feature');
    await expect(page).toHaveURL(/#mode=branch&base=older&file=d\.txt$/);
  });

  // A refresh builds the tree once the main pane's frame is painted. A newer view asked for within that frame (the base
  // picked here the moment the page's first overview is put in, from an init script) used to stop it: the tree stayed
  // empty while the newer view loaded (its tree is held), so no file could be picked from it. The page's own first tree
  // is held until the event stream is open: one that came in before would be loaded once more after the start
  // (connectEvents), for the base picked by then, and not held.
  test('the tree of the view shown is built even when another view is asked for before it is painted', async ({ page, repo, revue }) => {
    // feature is checked out: c.txt and d.txt against main, only d.txt against older.
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    repo.branch('older');
    repo.write('d.txt', 'more feature\n');
    repo.commit('more feature work');
    const s = await revue.start();
    await page.addInitScript(() => {
      new MutationObserver((_, observer) => {
        if (!document.querySelector('#main .overview-head')) return;
        observer.disconnect();
        const base = document.querySelector<HTMLSelectElement>('#base-select')!;
        base.value = 'older';
        base.dispatchEvent(new Event('change'));
      }).observe(document, { childList: true, subtree: true });
    });
    const first = await holdTree(page, u => u.searchParams.get('base') === 'main');
    const tree = await holdTree(page, u => u.searchParams.get('base') === 'older');
    await page.goto(`${s.url}#mode=branch&base=main`);
    await first.held;
    await expect(page.locator('#live-label')).toHaveText('Live');
    first.release();
    await tree.held;
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(treeRow(page, 'd.txt')).toBeVisible();

    // The control: a file picked from it stays open once the newer view is drawn.
    await treeRow(page, 'd.txt').click();
    await expect(viewLabel(page)).toHaveText('d.txt');
    tree.release();
    await expect(treeRow(page, 'c.txt').locator('.badge')).toHaveText('');
    await expect(viewLabel(page)).toHaveText('d.txt');
    await expect(page).toHaveURL(/#mode=branch&base=older&file=d\.txt$/);
  });

  // feature is checked out, with c.txt from its last commit; a.txt and c.txt are edited, and new.txt is untracked, so
  // only the working tree's file list has it.
  function editedRepo(repo: Repo) {
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    repo.write('a.txt', lines(5, 1));
    repo.write('c.txt', 'feature line, edited\n');
    repo.write('new.txt', 'new\n');
  }

  test('a file picked while a mode change loads another one stays open', async ({ page, repo, revue }) => {
    editedRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('feature work');

    // A link to a.txt in Changes, whose tree is held; c.txt is picked in History's tree, still shown, meanwhile.
    const tree = await holdTree(page, u => u.searchParams.get('mode') === 'working');
    await page.evaluate(() => { location.hash = 'mode=working&file=a.txt'; });
    await tree.held;
    await treeRow(page, 'c.txt').click();
    await expect(viewLabel(page)).toHaveText('c.txt');
    tree.release();

    await expect(treeRow(page, 'new.txt')).toBeVisible(); // the held tree is drawn
    await expect(viewLabel(page)).toHaveText('c.txt');
    await expect(page.locator('#main table.code')).toContainText('feature line, edited');
    await expect(page).toHaveURL(/#mode=working&file=c\.txt$/);
  });

  test('a mode change drops the file a mode change still loading was to open', async ({ page, repo, revue }) => {
    editedRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('feature work');

    const tree = await holdTree(page, u => u.searchParams.get('mode') === 'working');
    const answered = page.waitForResponse(r => new URL(r.url()).pathname === '/api/tree' && new URL(r.url()).searchParams.get('mode') === 'working');
    await page.evaluate(() => { location.hash = 'mode=working&file=a.txt'; });
    await tree.held;
    await modeTab(page, 'Branch').click();
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    tree.release();
    await answered;

    // Branch mode's overview, not a.txt: the link's file went with its mode.
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt']);
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
  });

  // A view in the mode already shown (another commit, here) has its tree loaded and drawn too, and whatever replaces
  // it meanwhile wins: a link to x.txt in A whose tree is held must not open x.txt in the view that came after it.
  // B touches y.txt, A adds x.txt and C touches z.txt, so x.txt isn't in B at all. w.txt is left untracked, for Changes.
  function threeCommits(repo: Repo) {
    repo.write('y.txt', 'y1\n');
    repo.write('z.txt', 'z1\n');
    repo.commit('base');
    repo.write('y.txt', 'y2\n');
    const B = repo.commit('touch y');
    repo.write('x.txt', 'x1\n');
    const A = repo.commit('add x');
    repo.write('z.txt', 'z2\n');
    const C = repo.commit('touch z');
    repo.write('w.txt', 'w\n');
    return { A, B, C };
  }
  const treeFor = (ref: string) => (u: URL) => u.searchParams.get('ref') === ref;
  // The next answer to a request for `ref`'s tree.
  const treeAnswer = (page: Page, ref: string) =>
    page.waitForResponse(r => new URL(r.url()).pathname === '/api/tree' && treeFor(ref)(new URL(r.url())));
  const setHash = (page: Page, hash: string) => page.evaluate(h => { location.hash = h; }, hash);

  // Opens `start` (History at C by default), then follows a link to x.txt in A while A's tree is held.
  async function linkHeld(page: Page, repo: Repo, revue: { start(): Promise<Server> }, start?: (c: ReturnType<typeof threeCommits>) => string) {
    const commits = threeCommits(repo);
    const s = await revue.start();
    await s.open(page, start?.(commits) ?? `mode=commit&ref=${commits.C}`);
    await expect(page.locator('#main .overview-head, #main .file-head').first()).toBeVisible();
    const tree = await holdTree(page, treeFor(commits.A));
    await setHash(page, `mode=commit&ref=${commits.A}&file=x.txt`);
    await tree.held;
    return { s, ...commits, tree };
  }
  // Lets the held tree through and waits until the page has its answer, and then a little longer, for the negative
  // checks after it: the link's x.txt, if it were still to be opened, is opened right after the answer.
  async function releaseHeld(page: Page, tree: { release(): void; answered(): Promise<unknown> }) {
    tree.release();
    await tree.answered();
    await page.waitForTimeout(500);
  }

  test('a link to another commit replaces a link still loading in History', async ({ page, repo, revue }) => {
    const { B, tree } = await linkHeld(page, repo, revue);
    await setHash(page, `mode=commit&ref=${B}`);
    await expect(overviewTitle(page)).toHaveText('touch y');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect(overviewTitle(page)).toHaveText('touch y');
    await expect.poll(() => sectionPaths(page)).toEqual(['y.txt']);
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });

  test('a link to another commit\'s file replaces a link still loading in History', async ({ page, repo, revue }) => {
    const { B, tree } = await linkHeld(page, repo, revue);
    await setHash(page, `mode=commit&ref=${B}&file=y.txt`);
    await expect(viewLabel(page)).toHaveText('y.txt');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('y.txt');
    await expect(page.locator('#main table.code')).toContainText('y2');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}&file=y\\.txt$`));
  });

  test('agent navigation replaces a link still loading in History', async ({ page, repo, revue }) => {
    const { s, B, tree } = await linkHeld(page, repo, revue);
    await s.navigate({ mode: 'commit', ref: B });
    await expect(overviewTitle(page)).toHaveText('touch y');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect(overviewTitle(page)).toHaveText('touch y');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });

  test('browser Back replaces a link still loading in History', async ({ page, repo, revue }) => {
    // Back returns to C's z.txt, which is what is still shown: its tree's answer tells that Back's view is in.
    const { C, tree } = await linkHeld(page, repo, revue, ({ C }) => `mode=commit&ref=${C}&file=z.txt`);
    const back = treeAnswer(page, C);
    await page.goBack();
    await back;
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}&file=z\\.txt$`));
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('z.txt');
    await expect(page.locator('#main table.code')).toContainText('z2');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}&file=z\\.txt$`));
  });

  test('browser Forward replaces a Back still loading in History', async ({ page, repo, revue }) => {
    const { A, C } = threeCommits(repo);
    const s = await revue.start();
    await s.open(page, `mode=commit&ref=${A}&file=x.txt`);
    await expect(viewLabel(page)).toHaveText('x.txt');
    await setHash(page, `mode=commit&ref=${C}&file=z.txt`);
    await expect(viewLabel(page)).toHaveText('z.txt');

    const tree = await holdTree(page, treeFor(A));
    await page.goBack();
    await tree.held;
    const forward = treeAnswer(page, C);
    await page.goForward();
    await forward;
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}&file=z\\.txt$`));
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('z.txt');
    await expect(page.locator('#main table.code')).toContainText('z2');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}&file=z\\.txt$`));
  });

  test('a mode key replaces a link still loading in History', async ({ page, repo, revue }) => {
    const { tree } = await linkHeld(page, repo, revue);
    await focusMain(page);
    await page.keyboard.press('1');
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    await releaseHeld(page, tree);

    // Changes' overview, not x.txt (which the working tree has too).
    await expect(modeTab(page, 'Changes')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect(page).toHaveURL(/#mode=working$/);
  });

  test('a file picked while a link loads in History stays open, and the link\'s tree is drawn', async ({ page, repo, revue }) => {
    const { A, tree } = await linkHeld(page, repo, revue);
    // C's tree is still shown; the file opens in A, which the link already set.
    await treeRow(page, 'y.txt').click();
    await expect(viewLabel(page)).toHaveText('y.txt');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('y.txt');
    await expect(page.locator('#main table.fileview')).toContainText('y2');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${A}&file=y\\.txt&view=file$`));
    // A's changes in the tree, not C's.
    await expect(treeRow(page, 'x.txt').locator('.badge')).toHaveText('A');
    await expect(treeRow(page, 'z.txt').locator('.badge')).toHaveText('');
    await expect(commitList(page).getByRole('option', { name: /^add x/ })).toHaveAttribute('aria-selected', 'true');
  });

  test('a commit picked while a link loads in History is shown without the link\'s file', async ({ page, repo, revue }) => {
    const { B, tree } = await linkHeld(page, repo, revue);
    await commitList(page).getByRole('option', { name: /^touch y/ }).click();
    await expect(overviewTitle(page)).toHaveText('touch y');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect(overviewTitle(page)).toHaveText('touch y');
    await expect(commitList(page).getByRole('option', { name: /^touch y/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });

  // The mode change to History waits for the commit list (held here) before it draws; a link in History meanwhile is
  // drawn first. The mode change's file mustn't be opened when the list comes in (with its tree already in), or be left
  // for the next refresh (r) to open (with its tree held too, and dropped when it comes in after the link's).
  for (const held of ['list', 'list and tree'] as const) {
    test(`a link in History replaces a link to History still loading (${held} held)`, async ({ page, repo, revue }) => {
      const { A, B } = threeCommits(repo);
      const s = await revue.start();
      await s.open(page);
      // The list's pages; a single commit (limit=1) is how a linked commit is looked up.
      const log = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
      const tree = held === 'list and tree' ? await holdTree(page, treeFor(A)) : null;
      const treeA = treeAnswer(page, A);
      await setHash(page, `mode=commit&ref=${A}&file=x.txt`);
      await log.held;
      await (tree ? tree.held : treeA);
      const treeB = treeAnswer(page, B);
      await setHash(page, `mode=commit&ref=${B}`);
      await treeB;
      await expect(overviewTitle(page)).toHaveText('touch y');
      await releaseHeld(page, log);
      if (tree) await releaseHeld(page, tree);
      await expect(viewLabel(page)).toHaveText('All changes · 1 file');
      await expect(commitList(page).getByRole('option', { name: /^touch y/ })).toHaveAttribute('aria-selected', 'true');

      // r draws the view again, and with it anything still left to draw.
      const shown = await page.locator('#main .overview-head').elementHandle();
      await focusMain(page);
      await page.keyboard.press('r');
      await expect.poll(() => shown!.evaluate(el => el.isConnected)).toBe(false);
      await expect(viewLabel(page)).toHaveText('All changes · 1 file');
      await expect(overviewTitle(page)).toHaveText('touch y');
      await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
    });
  }

  // The mode change to History picks the commit to show once its commit list (held here) is in. A commit linked
  // meanwhile, on another branch and so not in the list, used to be replaced by the list's first commit then.
  test('a commit on another branch linked while History waits for its commit list stays shown', async ({ page, repo, revue }) => {
    repo.write('a.txt', '1\n');
    repo.commit('base');
    repo.switch('side', true);
    repo.write('s.txt', 'X\n');
    const X = repo.commit('side X');
    repo.switch('main');
    repo.write('a.txt', '2\n');
    repo.commit('main work');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('main work');
    await focusMain(page);
    await page.keyboard.press('1');
    await expect(page.getByRole('heading', { name: 'No uncommitted changes on main' })).toBeVisible();

    const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
    await page.keyboard.press('2');
    await list.held;
    await setHash(page, `mode=commit&ref=${X}`);
    await expect(overviewTitle(page)).toHaveText('side X');
    await releaseHeld(page, list);

    const linked = commitList(page).locator('li[data-pinned]');
    await expect(overviewTitle(page)).toHaveText('side X');
    await expect(linked).toHaveAttribute('data-sha', X);
    await expect(linked).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${X}$`));
    // r draws the view again, and with it anything still left to draw.
    const shown = await page.locator('#main .overview-head').elementHandle();
    await focusMain(page);
    await page.keyboard.press('r');
    await expect.poll(() => shown!.evaluate(el => el.isConnected)).toBe(false);
    await expect(overviewTitle(page)).toHaveText('side X');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 's.txt');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${X}$`));
  });

  // Control: a live update's load overtakes the link's too, but then the link's view must still be shown. The live
  // update's own request for A's tree isn't held, so it draws the link's view.
  test('a link still loading in History is shown when a live update overtakes it', async ({ page, repo, revue }) => {
    const { A, tree } = await linkHeld(page, repo, revue);
    const update = treeAnswer(page, A); // the held request can't be answered first
    repo.branch('other'); // a ref change: a `git` live update, which loads A's tree again
    await update;
    await expect(viewLabel(page)).toHaveText('x.txt');
    await releaseHeld(page, tree);

    await expect(viewLabel(page)).toHaveText('x.txt');
    await expect(page.locator('#main table.code')).toContainText('x1');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${A}&file=x\\.txt$`));
    await expect(commitList(page).getByRole('option', { name: /^add x/ })).toHaveAttribute('aria-selected', 'true');
  });

  // A live update loads the tree of the view the page is on when it comes. One in Changes whose tree (held here) comes
  // in after 2 was pressed belongs to Changes, and History, still waiting for its commit list (held too), used to draw
  // it: Changes' files under History's "No commits" state, with no commit chosen yet.
  test('a live update\'s tree that comes in while History waits for its commit list is not drawn', async ({ page, repo, revue }) => {
    const { C } = threeCommits(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    await expect(treeRow(page, 'w.txt')).toBeVisible(); // the first refresh is done
    const update = await holdTree(page, u => u.searchParams.get('mode') === 'working');
    repo.write('v.txt', 'v\n'); // a live update, which loads Changes' tree again
    await update.held;
    const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
    await focusMain(page);
    await page.keyboard.press('2');
    await list.held;
    await releaseHeld(page, update);
    // Changes' overview is still on the page.
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    await expect(page.locator('#main .empty-state')).toHaveCount(0);

    // The control: History is drawn once its list is in.
    list.release();
    await expect(overviewTitle(page)).toHaveText('touch z');
    await expect.poll(() => sectionPaths(page)).toEqual(['z.txt']);
    await expect(commitList(page).getByRole('option', { name: /^touch z/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}$`));
  });

  // A live update that comes while History waits for its commit list (held here, answered as it was before the commit)
  // waits too: run then, it loaded and drew History's range half set, with no commit chosen yet ("Commit" and HEAD's
  // files). It runs once the list is in, and lists the commit made meanwhile above the one History opened at.
  test('a commit made while History waits for its commit list is listed once History is drawn', async ({ page, repo, revue }) => {
    const { C } = threeCommits(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    const messages = await recordEvents(page);
    const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1', { answer: true });
    const tip = treeAnswer(page, 'HEAD');
    await focusMain(page);
    await page.keyboard.press('2');
    await list.held;
    await tip;
    repo.commit('made while History loads'); // a `git` live update
    await expect.poll(messages).toContain('git');
    await page.waitForTimeout(500);
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');

    // The control: History opens at the commit its list named, and the update lists the new one above it.
    list.release();
    await expect(overviewTitle(page)).toHaveText('touch z');
    await expect(commitList(page).getByRole('option').first()).toContainText('made while History loads');
    await expect(commitList(page).getByRole('option', { name: /^touch z/ })).toHaveAttribute('aria-selected', 'true');
    await expect.poll(() => sectionPaths(page)).toEqual(['z.txt']);
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${C}$`));
  });
  // A commit outside the loaded history (a side branch's, here) is looked up on its own (resolveRefCommit) and listed
  // first as "linked". A lookup that comes back after a newer link's used to be stored as the newer commit's: the
  // linked row, and after r the title, showed the older commit for the newer one's URL and diff.
  test('a commit outside the history linked while another one is looked up is shown as itself', async ({ page, repo, revue }) => {
    repo.write('a.txt', '1\n');
    repo.commit('base');
    repo.switch('side', true);
    repo.write('s.txt', 'A\n');
    const A = repo.commit('side A');
    repo.write('s.txt', 'B\n');
    const B = repo.commit('side B');
    repo.switch('main');
    repo.write('a.txt', '2\n');
    repo.commit('main work');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('main work');
    const linked = commitList(page).locator('li[data-pinned]');

    const lookupA = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('branch') === A
      && u.searchParams.get('limit') === '1');
    await setHash(page, `mode=commit&ref=${A}`);
    await lookupA.held;
    await setHash(page, `mode=commit&ref=${B}`);
    await expect(overviewTitle(page)).toHaveText('side B');
    await expect(linked).toHaveAttribute('data-sha', B);
    lookupA.release();
    await lookupA.answered();

    // r draws the title again from the commit the page holds for the URL's.
    const shown = await page.locator('#main .overview-head').elementHandle();
    await focusMain(page);
    await page.keyboard.press('r');
    await expect.poll(() => shown!.evaluate(el => el.isConnected)).toBe(false);
    await expect(overviewTitle(page)).toHaveText('side B');
    await expect(page.locator('#main .overview-head .meta code')).toHaveText(B.slice(0, 7));
    await expect(linked).toHaveAttribute('data-sha', B);
    await expect(linked.locator('.subj')).toHaveText('side B');
    await expect(linked.locator('.meta code')).toHaveText(B.slice(0, 7));
    await expect(commitList(page).getByRole('option')).toHaveText([/^side B/, /^main work/, /^base/]);
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 's.txt');
    await expect(addedLines(section(page, 's.txt'))).toHaveText(['B']);
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });

  // feature is checked out: c.txt and d.txt against main, only d.txt against older.
  function featureRepo(repo: Repo) {
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.switch('feature', true);
    repo.write('c.txt', 'feature line\n');
    repo.commit('feature work');
    repo.branch('older');
    repo.write('d.txt', 'more feature\n');
    repo.commit('more feature work');
  }

  // Branch mode without a base compares with /api/info's default base, so while the page loads such a view waits for
  // it, as a mode change does. Back or a hash change within Branch mode used to draw "Choose a base branch" at once and
  // keep it. The page opens at a base of its own (older), so its own start doesn't wait for info's default base.
  async function openHoldingInfo(page: Page, repo: Repo, revue: { start(): Promise<Server> }) {
    featureRepo(repo);
    const s = await revue.start();
    const info = await hold(page, u => u.pathname === '/api/info');
    await page.goto(s.url + '#mode=branch&base=older');
    await info.held;
    return info;
  }
  async function expectDefaultBase(page: Page) {
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt', 'd.txt']);
    await expect(page.getByLabel('Compared with')).toHaveValue('main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
  }

  test('a hash change to Branch mode without a base while /api/info loads compares with the default base', async ({ page, repo, revue }) => {
    const info = await openHoldingInfo(page, repo, revue);
    await setHash(page, 'mode=branch');
    info.release();
    await expectDefaultBase(page);
  });

  test('Back to Branch mode without a base while /api/info loads compares with the default base', async ({ page, repo, revue }) => {
    const info = await openHoldingInfo(page, repo, revue);
    await setHash(page, 'mode=branch');
    await setHash(page, 'mode=branch&base=older');
    await page.goBack();
    await expect(page).toHaveURL(/#mode=branch$/);
    info.release();
    await expectDefaultBase(page);
  });

  // While the view without a base waits for info, a newer one (with a base, so it doesn't wait) replaces it: c.txt,
  // the waiting view's file, isn't opened once info is in, and the newer view's base stays.
  test('a view picked while one without a base waits for /api/info replaces it', async ({ page, repo, revue }) => {
    const info = await openHoldingInfo(page, repo, revue);
    await setHash(page, 'mode=branch&file=c.txt');
    await setHash(page, 'mode=branch&base=older');
    info.release();
    await expect(overviewTitle(page)).toHaveText('feature vs older');
    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect.poll(() => sectionPaths(page)).toEqual(['d.txt']);
    await expect(page.getByLabel('Compared with')).toHaveValue('older');
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
  });

  // The page draws nothing until /api/info is in. A view in the mode already shown used to be drawn as soon as its tree
  // was in, info or not: in Branch mode as "HEAD vs older", since info names the checked-out branch, and that title
  // stayed, as the page's own start, its tree load overtaken, drew nothing once info came in. Every view picked while
  // info loads now waits for it, as a mode change does.
  // The page opens at `hash` with /api/info and the tree its own start asks for held, once the event stream is open:
  // a tree that came in before the stream opened would be loaded and drawn once more after the start (connectEvents).
  async function openHoldingStart(page: Page, repo: Repo, revue: { start(): Promise<Server> }, hash: string) {
    featureRepo(repo);
    const s = await revue.start();
    const info = await hold(page, u => u.pathname === '/api/info');
    const tree = await holdTree(page, () => true);
    await page.goto(`${s.url}#${hash}`);
    await info.held;
    await tree.held;
    await expect(page.locator('#live-label')).toHaveText('Live');
    return { info, tree };
  }
  // Runs `step` and waits for the tree it asks for: a view drawn as soon as its tree is in, before info, is on the page
  // by the time expectNothingDrawn looks.
  async function withTree(page: Page, step: () => Promise<unknown>) {
    const tree = page.waitForResponse(r => new URL(r.url()).pathname === '/api/tree');
    await step();
    await tree;
  }
  async function expectNothingDrawn(page: Page) {
    await page.waitForTimeout(500);
    await expect(page.locator('#main')).toBeEmpty();
  }
  async function expectOlder(page: Page) {
    await expect(overviewTitle(page)).toHaveText('feature vs older');
    await expect.poll(() => sectionPaths(page)).toEqual(['d.txt']);
    await expect(page.getByLabel('Compared with')).toHaveValue('older');
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
  }

  test('a hash change within Branch mode while /api/info loads is drawn once info is in', async ({ page, repo, revue }) => {
    const { info, tree } = await openHoldingStart(page, repo, revue, 'mode=branch&base=main');
    await withTree(page, () => setHash(page, 'mode=branch&base=older'));
    tree.release();
    await expectNothingDrawn(page);
    info.release();
    await expectOlder(page);
  });

  test('Back within Branch mode while /api/info loads is drawn once info is in', async ({ page, repo, revue }) => {
    const { info, tree } = await openHoldingStart(page, repo, revue, 'mode=branch&base=main');
    await withTree(page, () => setHash(page, 'mode=branch&base=older'));
    await withTree(page, () => setHash(page, 'mode=branch&base=main&file=c.txt'));
    await withTree(page, () => page.goBack());
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
    tree.release();
    await expectNothingDrawn(page);
    info.release();
    await expectOlder(page);
  });

  // Changes' empty state names the checked-out branch, from info too.
  test('a view within Changes picked while /api/info loads is drawn once info is in', async ({ page, repo, revue }) => {
    const { info, tree } = await openHoldingStart(page, repo, revue, 'mode=working&file=a.txt');
    await withTree(page, () => setHash(page, 'mode=working'));
    tree.release();
    await expectNothingDrawn(page);
    info.release();
    await expect(page.getByRole('heading', { name: 'No uncommitted changes on feature' })).toBeVisible();
    await expect(page).toHaveURL(/#mode=working$/);
  });

  // The newer of two views in History is drawn, and only once info is in. (The page's start, with no commit named,
  // asks for the branch tip's tree, held here.)
  test('the last of two views within History picked while /api/info loads is drawn once info is in', async ({ page, repo, revue }) => {
    const { info, tree } = await openHoldingStart(page, repo, revue, 'mode=commit');
    const [base, featureWork] = [repo.rev('main'), repo.rev('older')];
    await withTree(page, () => setHash(page, `mode=commit&ref=${featureWork}&file=c.txt`));
    await withTree(page, () => setHash(page, `mode=commit&ref=${base}`));
    tree.release();
    await expectNothingDrawn(page);
    info.release();
    await expect(overviewTitle(page)).toHaveText('base');
    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect.poll(() => sectionPaths(page)).toEqual(['a.txt']);
    await expect(commitList(page).getByRole('option', { name: /^base/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${base}$`));
  });

  // History lists its commits before /api/info is in, and a commit picked there meanwhile used to be drawn as soon as
  // its tree was in, info or not.
  test('a commit picked while the page waits for /api/info is drawn once info is in', async ({ page, repo, revue }) => {
    const { info, tree } = await openHoldingStart(page, repo, revue, 'mode=commit');
    tree.release();
    await tree.answered();
    await withTree(page, () => commitList(page).getByRole('option', { name: /^feature work/ }).click());
    await expectNothingDrawn(page);
    info.release();
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt']);
    await expect(commitList(page).getByRole('option', { name: /^feature work/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${repo.rev('older')}$`));
  });

  // A refresh draws nothing while a newer tree load is under way. The page's start loads its tree early and draws it
  // once /api/info is in, and History lists its commits before then: a commit picked there meanwhile starts a newer
  // tree load. The start, whose own load was done before that one began, used to draw anyway once info came in: the
  // tree it had loaded, C's (z.txt), under the commit picked (B), until B's tree came in.
  // C's tree is held until the event stream is open, which would otherwise load the tree once more after the start.
  test('a commit picked while the page waits for /api/info is never drawn with the first commit\'s files', async ({ page, repo, revue }) => {
    const { B, C } = threeCommits(repo);
    const s = await revue.start();
    await s.annotate({ file: 'z.txt', line: 1, text: 'on C', mode: 'commit', ref: C });
    const info = await hold(page, u => u.pathname === '/api/info');
    const treeC = await holdTree(page, treeFor(C));
    await page.goto(`${s.url}#mode=commit&ref=${C}`);
    await info.held;
    await treeC.held;
    await expect(page.locator('#live-label')).toHaveText('Live');
    treeC.release();
    await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/); // C's tree is in, with its note
    const treeB = await holdTree(page, treeFor(B));
    await commitList(page).getByRole('option', { name: /^touch y/ }).click();
    await treeB.held;
    info.release();
    await info.answered();
    await page.waitForTimeout(500);
    await expect(section(page, 'z.txt')).toHaveCount(0);

    treeB.release();
    await expect(overviewTitle(page)).toHaveText('touch y');
    await expect.poll(() => sectionPaths(page)).toEqual(['y.txt']);
    await expect(commitList(page).getByRole('option', { name: /^touch y/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });

  // The same once the page is up: a link to a commit in History loads its tree before the commit list (held here), and
  // the list from the last visit is still there to pick from meanwhile.
  test('a commit picked while a link to History waits for the commit list is never drawn with the link\'s files', async ({ page, repo, revue }) => {
    const { A, B } = threeCommits(repo);
    const s = await revue.start();
    await s.annotate({ file: 'x.txt', line: 1, text: 'on A', mode: 'commit', ref: A });
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('touch z');
    await focusMain(page);
    await page.keyboard.press('1');
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');

    const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
    await setHash(page, `mode=commit&ref=${A}`);
    await list.held;
    await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/); // A's tree is in, with its note
    const treeB = await holdTree(page, treeFor(B));
    await commitList(page).getByRole('option', { name: /^touch y/ }).click();
    await treeB.held;
    list.release();
    await list.answered();
    await page.waitForTimeout(500);
    await expect(section(page, 'x.txt')).toHaveCount(0);

    treeB.release();
    await expect(overviewTitle(page)).toHaveText('touch y');
    await expect.poll(() => sectionPaths(page)).toEqual(['y.txt']);
    await expect(commitList(page).getByRole('option', { name: /^touch y/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${B}$`));
  });
});

test.describe('target lines', () => {
  // The target rows' and the sticky file header's boxes, with #main's.
  async function boxes(page: Page) {
    return page.evaluate(() => {
      const main = document.querySelector('#main')!.getBoundingClientRect();
      const head = document.querySelector('#main .file-head')!.getBoundingClientRect();
      const rows = [...document.querySelectorAll('#main tr.target')].map(r => r.getBoundingClientRect());
      return { main: { top: main.top, bottom: main.bottom }, headBottom: head.bottom,
        first: rows[0].top, last: rows[rows.length - 1].bottom };
    });
  }

  async function openAt(s: Server, page: Page, hash: string) {
    await s.open(page, hash);
    await expect(page.locator('#main .file-head')).toBeVisible();
  }

  // The tint style.css paints over a target line's code (a gradient background image, so it shows over the add and
  // del colors) is on line n, the first target line, and not on line n - 1 just above it. In a split diff a row has a
  // cell for each side; the last one is the new side's.
  async function expectTinted(page: Page, n: number) {
    await expect(page.locator(`#main tr[data-n="${n}"] td.src`).last()).toHaveCSS('background-image', /gradient/);
    await expect(page.locator(`#main tr[data-n="${n - 1}"] td.src`).last()).toHaveCSS('background-image', 'none');
  }

  test.beforeEach(async ({ repo }) => {
    repo.write('long.txt', lines(300));
    repo.commit('base');
    repo.write('long.txt', lines(300, 199));
  });

  test('a short range is highlighted and shown in full below the file header', async ({ page, revue }) => {
    const s = await revue.start();
    await openAt(s, page, 'file=long.txt&view=file&line=150&endLine=152');
    await expect(targetRows(page)).toHaveCount(3);
    await expect(targetRows(page).nth(0)).toHaveAttribute('data-n', '150');
    await expect(targetRows(page).nth(2)).toHaveAttribute('data-n', '152');
    await expect(async () => {
      const b = await boxes(page);
      expect(b.first).toBeGreaterThanOrEqual(b.headBottom);
      expect(b.last).toBeLessThanOrEqual(b.main.bottom);
      expect(b.headBottom).toBeGreaterThan(b.main.top); // the header is on screen, stuck at the top
      // Centered in the part of the pane the header leaves free.
      expect(Math.abs((b.first + b.last) / 2 - (b.headBottom + b.main.bottom) / 2)).toBeLessThan(8);
    }).toPass();
    await expectTinted(page, 150);
  });

  test('a range taller than the window starts just below the file header', async ({ page, revue }) => {
    const s = await revue.start();
    await openAt(s, page, 'file=long.txt&view=file&line=100&endLine=260');
    await expect(targetRows(page)).toHaveCount(161);
    await expect(async () => {
      const b = await boxes(page);
      expect(b.first).toBeGreaterThanOrEqual(b.headBottom);
      expect(b.first).toBeLessThan(b.headBottom + 20);
    }).toPass();
    await expectTinted(page, 100);
  });

  test('a target in the diff is highlighted there', async ({ page, repo, revue }) => {
    // Every 10th line edited: 30 hunks, a diff several windows tall, with line 200 far below the first screen.
    repo.write('long.txt', lines(300).replace(/^line (\d*0)$/gm, 'edited $1'));
    const s = await revue.start();
    await openAt(s, page, 'file=long.txt&line=200');
    await expect(targetRows(page)).toHaveCount(1);
    await expect(targetRows(page)).toHaveAttribute('data-n', '200');
    await expect(targetRows(page)).toContainText('edited 200');
    // Still the diff: it didn't fall back to the whole file (which would show the target too).
    await expect(page).toHaveURL(/#mode=working&file=long\.txt&line=200$/);
    await expect(page.locator('#main table.fileview')).toHaveCount(0);
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Diff', exact: true })).toHaveClass(/\bon\b/);
    expect(await page.locator('#main').evaluate(el => el.scrollHeight > 3 * el.clientHeight)).toBe(true);
    await expect(async () => {
      const b = await boxes(page);
      expect(b.first).toBeGreaterThanOrEqual(b.headBottom);
      expect(b.last).toBeLessThanOrEqual(b.main.bottom);
    }).toPass();
    await expectTinted(page, 200);
  });

  test('a target outside the diff opens the whole file at it', async ({ page, revue }) => {
    const s = await revue.start();
    // The diff is around line 200; line 100 is a few windows down the whole file.
    await openAt(s, page, 'file=long.txt&line=100');
    await expect(page.getByRole('status')).toHaveText('Line 100 isn\'t in the diff; showing the whole file');
    await expect(page.locator('#main table.fileview')).toBeVisible();
    await expect(targetRows(page)).toHaveAttribute('data-n', '100');
    await expect(page).toHaveURL(/#mode=working&file=long\.txt&view=file&line=100$/);
    expect(await page.locator('#main').evaluate(el => el.scrollHeight > 3 * el.clientHeight)).toBe(true);
    await expect(async () => {
      const b = await boxes(page);
      expect(b.first).toBeGreaterThanOrEqual(b.headBottom);
      expect(b.last).toBeLessThanOrEqual(b.main.bottom);
    }).toPass();
    await expectTinted(page, 100);
  });
});
