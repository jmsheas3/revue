// Keyboard use: the global shortcuts (j/k and n/p through the changed files, Esc back to the overview, / to filter,
// r to refresh, ? for the shortcut list; none of them with Ctrl, Alt or Meta held), and the file tree and commit list
// as keyboard lists (a roving tab stop that follows the selection, arrows, Right/Left on folders, Enter/Space to open,
// focus kept on the same row when a live update rebuilds them, "Load more" and the divider between the two).
import {
  test, expect, lines, fileTree, commitList, focusMain, section, treeRow, folderRow, viewLabel, overviewTitle,
  tabIntoTree, tabIntoCommits, recordRequests, reload, cleanEnv, type Repo, type Server,
  reEsc,
} from '../fixtures';
import type { Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';


// Opens the page with no tree load still to come once it returns. When the page's event stream opens after the first
// tree came back, the page loads the tree once more, as soon as the first render is done (an edit made in between
// sent no message). Left alone, whether that happens depends on which answer comes first, and nothing waits for it, so
// it can land after a key the test presses and pass for the load the key started. Holding the stream until the tree
// is drawn makes that load happen every time, and this waits for its answer. (revue.start() settles the index, so the
// server's own index refresh at startup doesn't write it and send a live update either.) `hash` is the view to open
// (without the '#'), and `held` runs while the stream is still held: on the page as its first render left it.
async function openQuiet(page: Page, s: Server, rows: number, hash = '', held?: () => Promise<void>) {
  let drawn!: () => void;
  const gate = new Promise<void>(r => { drawn = r; });
  await page.route(u => u.pathname === '/api/events', async route => { await gate; await route.continue(); });
  await page.goto(s.url + (hash ? `#${hash}` : ''));
  await expect(fileTree(page).getByRole('treeitem')).toHaveCount(rows);
  if (held) await held();
  const check = page.waitForResponse(r => new URL(r.url()).pathname === '/api/tree');
  drawn();
  await expect(page.locator('#live-label')).toHaveText('Live');
  await check;
}

// Three changed files at the top level and one unchanged.
function threeChanges(repo: Repo) {
  for (const f of ['a.txt', 'b.txt', 'c.txt', 'd.txt']) repo.write(f, lines(5));
  repo.commit('init');
  for (const f of ['a.txt', 'b.txt', 'c.txt']) repo.write(f, lines(5, 2));
}

test.describe('shortcuts', () => {
  test('j/k and n/p move through the changed files, and Esc returns to the overview', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
    await focusMain(page);

    const expectOpen = async (path: string) => {
      await expect(viewLabel(page)).toHaveText(path);
      await expect(treeRow(page, path)).toHaveAttribute('aria-selected', 'true');
      await expect(page).toHaveURL(new RegExp(`#mode=working&file=${reEsc(path)}$`));
    };
    await page.keyboard.press('j');
    await expectOpen('a.txt');
    await page.keyboard.press('j');
    await expectOpen('b.txt');
    await page.keyboard.press('n');
    await expectOpen('c.txt');
    // The last file stays put; d.txt is unchanged, so it's never visited.
    await page.keyboard.press('j');
    await expect(page.locator('#main table.code')).toBeVisible();
    await expectOpen('c.txt');
    await page.keyboard.press('k');
    await expectOpen('b.txt');
    await page.keyboard.press('p');
    await expectOpen('a.txt');

    await page.keyboard.press('Escape');
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
    await expect(fileTree(page).getByRole('treeitem', { selected: true })).toHaveCount(0);
    await expect(page).toHaveURL(/#mode=working$/);

    // From the overview, k starts at the last changed file.
    await page.keyboard.press('k');
    await expectOpen('c.txt');
  });

  test('/ focuses the filter, which narrows the tree; Enter opens the first match, Esc clears it', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(fileTree(page).getByRole('treeitem')).toHaveCount(4);
    await focusMain(page);
    await page.keyboard.press('/');
    const filter = page.getByRole('textbox', { name: 'Filter files' });
    await expect(filter).toBeFocused();
    // The slash itself isn't typed into it.
    await expect(filter).toHaveValue('');

    // Letters go to the filter, not to the shortcuts: j would open a.txt.
    await page.keyboard.press('j');
    await expect(filter).toHaveValue('j');
    await expect(fileTree(page)).toContainText('No matching files');
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');

    // The tree follows the typing once it pauses (100 ms). Enter doesn't wait for that: pressed at once, it opens the
    // first match of what was typed, while the tree still shows the matches for j (none).
    await page.keyboard.press('Backspace');
    await page.keyboard.type('b.t');
    await page.keyboard.press('Enter');
    await expect(viewLabel(page)).toHaveText('b.txt');
    await expect(fileTree(page).getByRole('treeitem')).toHaveCount(1);
    await expect(treeRow(page, 'b.txt')).toHaveAttribute('aria-selected', 'true');

    // Of several matches, it opens the first in the tree.
    await filter.fill('txt');
    await filter.press('Enter');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(fileTree(page).getByRole('treeitem')).toHaveCount(4);

    await filter.focus();
    await page.keyboard.press('Escape');
    await expect(filter).toHaveValue('');
    await expect(filter).not.toBeFocused();
    await expect(fileTree(page).getByRole('treeitem')).toHaveCount(4);
    // Esc in the filter only clears it; the open file stays.
    await expect(viewLabel(page)).toHaveText('a.txt');
  });

  // A forced redraw replaces every section, so a mark put on one is gone afterwards; a live update keeps the sections
  // whose diffs didn't change.
  const markSection = (page: Page, path: string) => section(page, path).evaluate(el => el.setAttribute('data-drawn', 'before'));
  const marked = (page: Page) => page.locator('#main [data-drawn]');

  test('r draws every section of the overview again', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await openQuiet(page, s, 4);
    await expect(page.locator('#main table.code')).toHaveCount(3);
    await markSection(page, 'a.txt');
    await focusMain(page);
    const tree = page.waitForRequest(r => new URL(r.url()).pathname === '/api/tree');
    await page.keyboard.press('r');
    await tree;
    await expect(marked(page)).toHaveCount(0);
    await expect(page.locator('#main .section')).toHaveCount(3);
    await expect(page.locator('#main table.code')).toHaveCount(3);
  });

  // r draws new sections, which the browser lays out at an estimated height until they are drawn: the page must stay
  // where it was anyway, to the pixel (the scroll position and what is on screen).
  test('r keeps the overview where it was scrolled to', async ({ page, repo, revue }) => {
    const files = Array.from({ length: 12 }, (_, i) => `f${String(i + 1).padStart(2, '0')}.txt`);
    for (const f of files) repo.write(f, lines(20));
    repo.commit('init');
    for (const f of files) repo.write(f, lines(20, 9));
    const s = await revue.start();
    await openQuiet(page, s, 12);
    const main = page.locator('#main');
    // Diffs load as their sections come near the window: go to the bottom so every one is in.
    await main.evaluate(el => { el.scrollTop = el.scrollHeight; });
    await expect(page.locator('#main .section table.code')).toHaveCount(12);
    await main.evaluate(el => { el.scrollTop = 400; });
    await expect.poll(() => main.evaluate(el => el.scrollTop)).toBe(400);

    // The section at the top of the window, and how far its top is above the window's.
    const atTop = () => main.evaluate(m => {
      const y = m.getBoundingClientRect().top;
      const s = [...m.querySelectorAll('.section')].find(s => s.getBoundingClientRect().bottom > y + 1)!;
      return { path: s.getAttribute('data-path')!, offset: s.getBoundingClientRect().top - y, scrollTop: m.scrollTop };
    });
    const before = await atTop();
    expect(before.offset).toBeLessThan(-1); // partly scrolled past, so a shift either way shows
    // Mark its diff: r draws it again, so the mark goes with the old table.
    const topTable = section(page, before.path).locator('table.code');
    await topTable.evaluate(el => { (el as any).__before = true; });
    await focusMain(page);
    await page.keyboard.press('r');
    await expect.poll(() => topTable.evaluate(el => (el as any).__before === true)).toBe(false);
    await expect(page.locator('#main .section')).toHaveCount(12);
    // Scroll anchoring adjusts the position as frames are laid out: let two go by.
    await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
    const after = await atTop();
    expect(after.scrollTop).toBe(400);
    expect(after.path).toBe(before.path);
    expect(after.offset).toBeCloseTo(before.offset, 1);
  });

  test('r picks up changes the file watcher missed', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(page.locator('#main table.code')).toHaveCount(3);
    await s.debug({ watchError: 'off for this test' });
    expect((await s.json('/api/server')).watcher).toBe('off');

    repo.write('d.txt', lines(5, 4));
    // Without a watcher that sees every change, the server reuses an answer for up to 3 s (by design, see `remember`
    // in src/server.rs). Wait until it answers with the edit, so the one r below gets it; no live update can bring it.
    await expect.poll(async () => Object.keys((await s.json('/api/tree', { mode: 'working' })).changes).sort())
      .toEqual(['a.txt', 'b.txt', 'c.txt', 'd.txt']);
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
    await focusMain(page);
    await page.keyboard.press('r');
    await expect(viewLabel(page)).toHaveText('All changes · 4 files');
    await expect(treeRow(page, 'd.txt')).toHaveAccessibleName('d.txt M');
    await expect(page.locator('#main table.code')).toHaveCount(4);
  });

  // A refresh whose tree load a newer one superseded returns before drawing; r's force must survive it.
  test('r still redraws everything when a live update lands during it', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await openQuiet(page, s, 4);
    await expect(page.locator('#main table.code')).toHaveCount(3);
    await markSection(page, 'a.txt');

    // Hold the tree request r makes until a live update's tree has come back.
    let release!: () => void;
    const held = new Promise<void>(r => { release = r; });
    let first = true;
    await page.route(u => u.pathname === '/api/tree', async route => {
      if (first) { first = false; await held; }
      await route.continue();
    });
    await focusMain(page);
    const rTree = page.waitForRequest(r => new URL(r.url()).pathname === '/api/tree');
    await page.keyboard.press('r');
    await rTree;
    repo.write('b.txt', lines(5, 0));
    await expect(section(page, 'b.txt').locator('tr.add, td.add').filter({ hasText: 'edited 1' })).not.toHaveCount(0);
    release();
    await expect(marked(page)).toHaveCount(0);
  });

  test('? opens and closes the shortcut list', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    const panel = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
    const button = page.getByRole('button', { name: 'Keyboard shortcuts' });
    await expect(panel).toBeHidden();
    await focusMain(page);

    await page.keyboard.press('?');
    await expect(panel).toBeVisible();
    await expect(panel).toBeFocused();
    await expect(button).toHaveAttribute('aria-expanded', 'true');
    await expect(panel).toContainText('next / previous changed file');

    await page.keyboard.press('?');
    await expect(panel).toBeHidden();
    await expect(button).toHaveAttribute('aria-expanded', 'false');
    await expect(button).toBeFocused();

    // Esc closes it too, without leaving the open file.
    await focusMain(page);
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await page.keyboard.press('?');
    await expect(panel).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(panel).toBeHidden();
    await expect(viewLabel(page)).toHaveText('a.txt');
  });

  test('the shortcut list has the file tree and commit list keys', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    await page.getByRole('button', { name: 'Keyboard shortcuts' }).click();
    const panel = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
    await expect(panel).toBeVisible();
    for (const text of ['parent folder', 'open / close a folder', 'move into the list', 'Home', 'End', 'Space']) {
      await expect(panel).toContainText(text);
    }
    await expect(panel.getByRole('heading', { name: 'File tree and commit list' })).toBeVisible();
  });

  // The keys panel is a popover: a press anywhere else closes it, and so does focus moving away from it. Focus going to
  // its own button keeps it open.
  test('the shortcut list closes on a press outside it', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    const panel = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
    const button = page.getByRole('button', { name: 'Keyboard shortcuts' });
    await focusMain(page);
    await page.keyboard.press('?');
    await expect(panel).toBeFocused();
    // Back to the ? button, so the press below takes focus from the button and not from the list.
    await page.keyboard.press('Shift+Tab');
    await expect(button).toBeFocused();
    await expect(panel).toBeVisible();

    // The label above the main pane: plain text, which does nothing when pressed.
    await viewLabel(page).click();
    await expect(panel).toBeHidden();
    await expect(button).toHaveAttribute('aria-expanded', 'false');
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
  });

  test('the shortcut list closes when focus leaves it', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    const panel = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
    const button = page.getByRole('button', { name: 'Keyboard shortcuts' });
    await focusMain(page);
    await page.keyboard.press('?');
    await expect(panel).toBeFocused();

    // Tab moves on to the theme button, after the list.
    await page.keyboard.press('Tab');
    await expect(page.locator('#theme-btn')).toBeFocused();
    await expect(panel).toBeHidden();
    await expect(button).toHaveAttribute('aria-expanded', 'false');
  });

  // Ctrl+J, Alt+J and Cmd+J belong to the browser or the system: none of them is j.
  test('j with Ctrl, Alt or Meta held is not a shortcut', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
    await focusMain(page);
    for (const mod of ['Control', 'Alt', 'Meta']) await page.keyboard.press(`${mod}+j`);
    // The control: j alone opens the first changed file. Each of the three would have moved a file on, so j would land
    // past it.
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await expect(page).toHaveURL(/#mode=working&file=a\.txt$/);
  });

  test('Ctrl+R is left to the browser rather than taken for r', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await openQuiet(page, s, 4);
    await expect(page.locator('#main table.code')).toHaveCount(3);
    await markSection(page, 'a.txt');
    await focusMain(page);
    // Every r keydown's Ctrl state and whether it was cancelled, read by a listener on window, which the event reaches
    // after the page's own (on document). It cancels the event itself, so the browser never reloads the page under the
    // test.
    await page.evaluate(() => {
      const seen: { ctrl: boolean; prevented: boolean }[] = (window as any).__rKeys = [];
      addEventListener('keydown', e => {
        if (e.key.toLowerCase() !== 'r') return;
        seen.push({ ctrl: e.ctrlKey, prevented: e.defaultPrevented });
        e.preventDefault();
      });
    });
    const trees = recordRequests(page, /^\/api\/tree$/);

    await page.keyboard.press('Control+r');
    // The control: r alone refreshes, cancelling the key, and draws every section again. A tree request from Ctrl+R
    // would have gone out before that one.
    await page.keyboard.press('r');
    await expect(marked(page)).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__rKeys)).toEqual([
      { ctrl: true, prevented: false },
      { ctrl: false, prevented: true },
    ]);
    expect(trees, 'only r loaded the tree').toHaveLength(1);
  });

  // A checkbox keeps focus after a click, and it takes no letters.
  test('the shortcuts still work while the "Changed only" checkbox has focus', async ({ page, repo, revue }) => {
    threeChanges(repo);
    const s = await revue.start();
    await s.open(page);
    const box = page.getByRole('checkbox', { name: 'Changed only' });
    await box.click();
    await expect(box).toBeChecked();
    // The click leaves focus on it, except on macOS, where a click doesn't focus a form control.
    await box.focus();
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('a.txt');
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('b.txt');
    await page.keyboard.press('Escape');
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
    await expect(box).toBeFocused();
    await expect(box).toBeChecked();
  });

  test('r with a file open draws it again and keeps it where it was scrolled to', async ({ page, repo, revue }) => {
    // One file with a change every fourth line: a diff long enough to scroll.
    const text = (edit: boolean) => Array.from({ length: 300 }, (_, i) => (edit && i % 4 === 0 ? 'edited' : 'line') + ` ${i + 1}\n`).join('');
    repo.write('big.txt', text(false));
    repo.commit('init');
    repo.write('big.txt', text(true));
    const s = await revue.start();
    await openQuiet(page, s, 1);
    await focusMain(page);
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('big.txt');
    const main = page.locator('#main');
    const table = main.locator('table.code');
    await expect(table).toBeVisible();
    await main.evaluate(el => { el.scrollTop = 600; });
    await expect.poll(() => main.evaluate(el => el.scrollTop)).toBe(600);
    // Tagged, so the table found afterwards shows whether it is a new one.
    await table.evaluate(el => { (el as any).__before = true; });

    await page.keyboard.press('r');
    await expect.poll(() => table.evaluate(el => (el as any).__before === true)).toBe(false);
    await expect(viewLabel(page)).toHaveText('big.txt');
    await expect(page).toHaveURL(/#mode=working&file=big\.txt$/);
    await expect(table).toContainText('edited 1');
    expect(await main.evaluate(el => el.scrollTop)).toBe(600);
  });
});

test.describe('file tree', () => {
  // docs/ has no changes, so it starts closed; src/ has two and starts open.
  function folders(repo: Repo) {
    repo.write('docs/guide.txt', lines(3)).write('src/a.txt', lines(5)).write('src/b.txt', lines(5)).write('top.txt', lines(3));
    repo.commit('init');
    repo.write('src/a.txt', lines(5, 0)).write('src/b.txt', lines(5, 1));
  }

  test('Tab enters the tree on one row, arrows move, Right/Left open and close folders', async ({ page, repo, revue }) => {
    folders(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(folderRow(page, 'src')).toHaveAttribute('aria-expanded', 'true');
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'false');
    // Exactly one row is in the tab order.
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);

    await tabIntoTree(page);
    await expect(folderRow(page, 'docs')).toBeFocused();
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-level', '1');

    // Right opens a closed folder, then moves into it.
    await page.keyboard.press('ArrowRight');
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'true');
    await expect(folderRow(page, 'docs')).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(treeRow(page, 'docs/guide.txt')).toBeFocused();
    await expect(treeRow(page, 'docs/guide.txt')).toHaveAttribute('aria-level', '2');

    // Left on a file moves to its folder (the WAI-ARIA tree pattern), and Left on the open folder closes it.
    await page.keyboard.press('ArrowLeft');
    await expect(folderRow(page, 'docs')).toBeFocused();
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'true');
    await page.keyboard.press('ArrowLeft');
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'false');
    await expect(treeRow(page, 'docs/guide.txt')).toHaveCount(0);
    await expect(folderRow(page, 'docs')).toBeFocused();
    // A closed folder at the top level has no parent to go to.
    await page.keyboard.press('ArrowLeft');
    await expect(folderRow(page, 'docs')).toBeFocused();
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'false');

    // Down walks the rows in order and Up goes back; End and Home jump to the ends.
    await page.keyboard.press('ArrowDown');
    await expect(folderRow(page, 'src')).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(treeRow(page, 'src/a.txt')).toBeFocused();
    await page.keyboard.press('End');
    await expect(treeRow(page, 'top.txt')).toBeFocused();
    // Left on a file at the top level stays put too; Up then still moves.
    await page.keyboard.press('ArrowLeft');
    await expect(treeRow(page, 'top.txt')).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(treeRow(page, 'src/b.txt')).toBeFocused();
    await page.keyboard.press('Home');
    await expect(folderRow(page, 'docs')).toBeFocused();

    // The focused row holds the tab stop: Shift+Tab out and Tab back lands on it.
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Shift+Tab');
    await expect(page.getByRole('checkbox', { name: 'Changed only' })).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(folderRow(page, 'src')).toBeFocused();
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);
  });

  test('Left goes up to the parent folder from a file or a closed folder inside it', async ({ page, repo, revue }) => {
    // src/ holds an open folder (lib/, which has a change), a closed one (util/, which has none) and a file.
    repo.write('src/lib/x.txt', lines(3)).write('src/lib/y.txt', lines(3)).write('src/util/z.txt', lines(3))
      .write('src/main.txt', lines(3)).write('top.txt', lines(3));
    repo.commit('init');
    repo.write('src/lib/x.txt', lines(3, 0)).write('src/main.txt', lines(3, 1));
    const s = await revue.start();
    await s.open(page);
    const src = folderRow(page, 'src'), lib = folderRow(page, 'src/lib'), util = folderRow(page, 'src/util');
    await expect(lib).toHaveAttribute('aria-expanded', 'true');
    await expect(util).toHaveAttribute('aria-expanded', 'false');

    await tabIntoTree(page);
    await expect(src).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await expect(treeRow(page, 'src/lib/x.txt')).toBeFocused();
    await expect(treeRow(page, 'src/lib/x.txt')).toHaveAttribute('aria-level', '3');

    // From a file to its folder, which stays open; Left there closes it; Left on it closed goes up again.
    await page.keyboard.press('ArrowLeft');
    await expect(lib).toBeFocused();
    await expect(lib).toHaveAttribute('aria-expanded', 'true');
    await page.keyboard.press('ArrowLeft');
    await expect(lib).toHaveAttribute('aria-expanded', 'false');
    await expect(lib).toBeFocused();
    await page.keyboard.press('ArrowLeft');
    await expect(src).toBeFocused();
    await expect(src).toHaveAttribute('aria-expanded', 'true');

    // A folder that was closed all along, and a file next to it.
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await expect(util).toBeFocused();
    await page.keyboard.press('ArrowLeft');
    await expect(src).toBeFocused();
    await page.keyboard.press('End');
    await page.keyboard.press('ArrowUp');
    await expect(treeRow(page, 'src/main.txt')).toBeFocused();
    await page.keyboard.press('ArrowLeft');
    await expect(src).toBeFocused();
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);
    await expect(src).toHaveAttribute('tabindex', '0');
  });

  // Firefox puts a box that scrolls in the tab order unless it says otherwise, which would make Tab stop on the tree
  // itself before reaching a row.
  test('a tree long enough to scroll still takes one Tab to enter and one to leave', async ({ page, repo, revue }) => {
    for (let i = 0; i < 60; i++) repo.write(`f${String(i).padStart(2, '0')}.txt`, `${i}\n`);
    repo.commit('init');
    const s = await revue.start();
    await s.open(page);
    await expect(fileTree(page).getByRole('treeitem')).toHaveCount(60);
    expect(await page.locator('#tree').evaluate(el => el.scrollHeight > el.clientHeight), 'the tree scrolls').toBe(true);

    await tabIntoTree(page);
    await expect(treeRow(page, 'f00.txt')).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(page.getByRole('checkbox', { name: 'Changed only' })).toBeFocused();
  });

  test('Enter and Space open the focused file instead of scrolling the tree', async ({ page, repo, revue }) => {
    // Sixty files after the folders: a tree long enough to scroll, so a key that scrolled it as well would show.
    for (let i = 0; i < 60; i++) repo.write(`f${String(i).padStart(2, '0')}.txt`, `${i}\n`);
    folders(repo);
    const s = await revue.start();
    await s.open(page);
    const tree = page.locator('#tree');
    const scrollTop = () => tree.evaluate(el => el.scrollTop);
    expect(await tree.evaluate(el => el.scrollHeight > el.clientHeight), 'the tree scrolls').toBe(true);
    await tabIntoTree(page);
    await page.keyboard.press('ArrowDown'); // src
    await page.keyboard.press('ArrowDown'); // src/a.txt
    await page.keyboard.press('Enter');
    await expect(viewLabel(page)).toHaveText('src/a.txt');
    await expect(treeRow(page, 'src/a.txt')).toHaveAttribute('aria-selected', 'true');
    await expect(treeRow(page, 'src/a.txt')).toBeFocused();

    await page.keyboard.press('ArrowDown');
    await page.keyboard.press(' ');
    await expect(viewLabel(page)).toHaveText('src/b.txt');
    await expect(treeRow(page, 'src/b.txt')).toHaveAttribute('aria-selected', 'true');
    await expect(treeRow(page, 'src/a.txt')).toHaveAttribute('aria-selected', 'false');
    await expect(treeRow(page, 'src/b.txt')).toBeFocused();
    // The arrows, Enter and Space did nothing else: by default the arrows and Space scroll the tree. That scroll is
    // smooth and starts a frame or two after the key, so give it time to show.
    await page.waitForTimeout(300);
    expect(await scrollTop(), 'the tree did not scroll').toBe(0);
    // The control: PageDown, which the list leaves to the browser, does scroll it.
    await page.keyboard.press('PageDown');
    await expect.poll(scrollTop).toBeGreaterThan(0);

    // Enter on a folder opens and closes it.
    await page.keyboard.press('Home');
    await page.keyboard.press('Enter');
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'true');
    await page.keyboard.press(' ');
    await expect(folderRow(page, 'docs')).toHaveAttribute('aria-expanded', 'false');
  });

  test('the focused row keeps focus when a live update rebuilds the tree', async ({ page, repo, revue }) => {
    folders(repo);
    const s = await revue.start();
    await s.open(page);
    await tabIntoTree(page);
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    const b = treeRow(page, 'src/b.txt');
    await expect(b).toBeFocused();
    // Tagged, so the row found afterwards shows whether it is a new one.
    await b.evaluate(el => { (el as any).__before = true; });

    // A new file changes the tree, so it is built again.
    repo.write('src/c.txt', 'new\n');
    await expect(treeRow(page, 'src/c.txt')).toBeVisible();
    expect(await b.evaluate(el => (el as any).__before === true), 'the tree was rebuilt').toBe(false);
    await expect(b).toBeFocused();
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);
    await expect(b).toHaveAttribute('tabindex', '0');

    // And the keyboard goes on from there.
    await page.keyboard.press('ArrowDown');
    await expect(treeRow(page, 'src/c.txt')).toBeFocused();
  });

  // Tab into the tree lands on the selected file, however it was selected, unless a row was focused since.
  test('a file opened with j and k holds the tree\'s tab stop', async ({ page, repo, revue }) => {
    for (const f of ['a.txt', 'b.txt', 'c.txt', 'd.txt']) repo.write(f, lines(5));
    repo.commit('init');
    for (const f of ['a.txt', 'b.txt', 'c.txt']) repo.write(f, lines(5, 2));
    const s = await revue.start();
    await openQuiet(page, s, 4);
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveAttribute('data-path', 'a.txt');
    await focusMain(page);
    await page.keyboard.press('j');
    await page.keyboard.press('j');
    await expect(treeRow(page, 'b.txt')).toHaveAttribute('aria-selected', 'true');

    await tabIntoTree(page);
    await expect(treeRow(page, 'b.txt')).toBeFocused();
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);

    // From the main pane again, and back the other way.
    await focusMain(page);
    await page.keyboard.press('j');
    await page.keyboard.press('k');
    await page.keyboard.press('k');
    await expect(treeRow(page, 'a.txt')).toHaveAttribute('aria-selected', 'true');
    await tabIntoTree(page);
    await expect(treeRow(page, 'a.txt')).toBeFocused();
    await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);
  });

  test('a file selected by the URL holds the tree\'s tab stop', async ({ page, repo, revue }) => {
    for (const f of ['a.txt', 'b.txt', 'c.txt', 'd.txt']) repo.write(f, lines(5));
    repo.commit('init');
    for (const f of ['a.txt', 'b.txt', 'c.txt']) repo.write(f, lines(5, 2));
    const s = await revue.start();
    // Checked as the first render drew the tree: the load that follows the event stream's opening would move the tab
    // stop to the selected file in its own way (markSelected).
    await openQuiet(page, s, 4, 'mode=working&file=c.txt', async () => {
      await expect(treeRow(page, 'c.txt')).toHaveAttribute('aria-selected', 'true');
      await tabIntoTree(page);
      await expect(treeRow(page, 'c.txt')).toBeFocused();
      await expect(fileTree(page).locator('[role=treeitem][tabindex="0"]')).toHaveCount(1);
    });
    await expect(viewLabel(page)).toHaveText('c.txt');
  });
});

test.describe('commit list', () => {
  // Returns the second commit's sha.
  function history(repo: Repo): string {
    repo.write('a.txt', lines(3)).commit('first commit');
    const second = repo.write('a.txt', lines(3, 0)).commit('second commit');
    repo.write('a.txt', lines(3, 1)).commit('third commit');
    return second;
  }
  const commitRow = (page: Page, subject: string) => commitList(page).getByRole('option', { name: new RegExp(`^${reEsc(subject)}`) });

  test('Tab enters the list on the selected commit, arrows move, Enter and Space open', async ({ page, repo, revue }) => {
    const second = history(repo);
    const s = await revue.start();
    // The middle commit, so the row Tab lands on is the selected one and not just the first.
    await s.open(page, `mode=commit&ref=${second}`);
    await expect(commitList(page).getByRole('option')).toHaveCount(3);
    await expect(commitRow(page, 'second commit')).toHaveAttribute('aria-selected', 'true');
    await expect(overviewTitle(page)).toHaveText('second commit');

    await tabIntoCommits(page);
    await expect(commitRow(page, 'second commit')).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(commitRow(page, 'first commit')).toBeFocused();
    // Moving doesn't open.
    await expect(commitRow(page, 'second commit')).toHaveAttribute('aria-selected', 'true');

    await page.keyboard.press('Enter');
    await expect(commitRow(page, 'first commit')).toHaveAttribute('aria-selected', 'true');
    await expect(overviewTitle(page)).toHaveText('first commit');
    await expect(commitRow(page, 'first commit')).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(commitRow(page, 'second commit')).toBeFocused();

    // Home and End jump to the ends, and Up at the top stays put.
    await page.keyboard.press('Home');
    await expect(commitRow(page, 'third commit')).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(commitRow(page, 'third commit')).toBeFocused();
    await page.keyboard.press(' ');
    await expect(overviewTitle(page)).toHaveText('third commit');
    await expect(commitRow(page, 'third commit')).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('End');
    await expect(commitRow(page, 'first commit')).toBeFocused();
  });

  // As with the tree: Firefox would otherwise stop on the scrolling list itself.
  test('a list long enough to scroll still takes one Tab to enter and one to leave', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(3)).commit('commit 1');
    for (let i = 2; i <= 30; i++) repo.commitStaged(`commit ${i}`);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(commitList(page).getByRole('option')).toHaveCount(30);
    expect(await commitList(page).evaluate(el => el.scrollHeight > el.clientHeight), 'the list scrolls').toBe(true);

    await tabIntoCommits(page);
    await expect(commitRow(page, 'commit 30')).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(page.getByRole('checkbox', { name: 'all branches' })).toBeFocused();
  });

  test('the focused commit keeps focus when a new commit rebuilds the list', async ({ page, repo, revue }) => {
    history(repo);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(commitList(page).getByRole('option')).toHaveCount(3);
    await tabIntoCommits(page);
    await page.keyboard.press('ArrowDown');
    const second = commitRow(page, 'second commit');
    await expect(second).toBeFocused();

    repo.write('a.txt', lines(3, 2)).commit('fourth commit');
    await expect(commitList(page).getByRole('option')).toHaveCount(4);
    await expect(commitRow(page, 'fourth commit')).toBeVisible();
    await expect(second).toBeFocused();
    // It holds the one tab stop too, rather than the selected commit.
    await expect(commitList(page).locator('[role=option][tabindex="0"]')).toHaveCount(1);
    await expect(second).toHaveAttribute('tabindex', '0');

    await page.keyboard.press('ArrowUp');
    await expect(commitRow(page, 'third commit')).toBeFocused();
  });

  // The log loads 200 commits at a time; "Load more…" is the list's last row. One git fast-import makes the commits
  // (as in modes.spec.ts), with the same increasing fixed dates Repo.commit gives.
  function manyCommits(repo: Repo, n: number) {
    let stream = '';
    for (let i = 1; i <= n; i++) {
      const who = `Test <test@example.com> ${1_700_000_000 + i * 60} +0000`;
      const msg = `commit ${i}\n`;
      stream += `commit refs/heads/main\nauthor ${who}\ncommitter ${who}\ndata ${msg.length}\n${msg}`;
      if (i === 1) stream += 'M 100644 inline a.txt\ndata 2\na\n';
      stream += '\n';
    }
    execFileSync('git', ['fast-import', '--quiet'], { cwd: repo.dir, input: stream, env: cleanEnv(), stdio: 'pipe' });
    repo.git('reset', '-q', '--hard');
  }

  test('"Load more" from the keyboard moves focus on to the rows it adds', async ({ page, repo, revue }) => {
    manyCommits(repo, 405);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    const commits = commitList(page).locator('li[data-sha]');
    const bySubject = (subject: string) => commits.filter({ has: page.locator('.subj', { hasText: new RegExp(`^${reEsc(subject)}$`) }) });
    const more = commitList(page).getByRole('option', { name: 'Load more…' });
    await expect(commits).toHaveCount(200);

    await tabIntoCommits(page);
    await expect(bySubject('commit 405')).toBeFocused();
    await page.keyboard.press('End');
    await expect(more).toBeFocused();
    await more.evaluate(el => { (el as any).__before = true; });

    // More to come: focus goes to the new "Load more…" row at the end.
    await page.keyboard.press('Enter');
    await expect(commits).toHaveCount(400);
    await expect(more).toHaveCount(1);
    expect(await more.evaluate(el => (el as any).__before === true), 'a new row').toBe(false);
    await expect(more).toBeFocused();
    await expect(more).toHaveAttribute('tabindex', '0');

    // The last page: focus goes to its first commit, and the keyboard goes on from there.
    await page.keyboard.press('Enter');
    await expect(commits).toHaveCount(405);
    await expect(more).toHaveCount(0);
    await expect(bySubject('commit 5')).toBeFocused();
    await expect(commitList(page).locator('[role=option][tabindex="0"]')).toHaveCount(1);
    await expect(bySubject('commit 5')).toHaveAttribute('tabindex', '0');
    await page.keyboard.press('ArrowDown');
    await expect(bySubject('commit 4')).toBeFocused();
  });

  // The divider between the commit list and the file tree is a window splitter: Up and Down resize the list, which is
  // remembered for this server.
  test('the divider under the commit list resizes it from the keyboard and keeps the size', async ({ page, repo, revue }) => {
    history(repo);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(commitList(page).getByRole('option')).toHaveCount(3);
    const divider = page.getByRole('separator', { name: 'Resize the commit list' });
    const pane = page.locator('#commits-pane');
    const height = () => pane.evaluate(el => (el as HTMLElement).offsetHeight);
    const valueNow = async () => Number(await divider.getAttribute('aria-valuenow'));
    const saved = () => page.evaluate(() => localStorage.getItem('revue.commitsHeight'));

    await divider.focus();
    const h0 = await height(), v0 = await valueNow();
    expect(await saved()).toBeNull();
    await page.keyboard.press('ArrowDown');
    await expect.poll(height).toBe(h0 + 24);
    await expect.poll(valueNow).toBeGreaterThan(v0);
    expect(await saved()).toBe(String(h0 + 24));

    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('ArrowUp');
    await expect.poll(height).toBe(h0 - 24);
    await expect.poll(valueNow).toBeLessThan(v0);
    expect(await saved()).toBe(String(h0 - 24));

    await reload(page);
    await expect.poll(height).toBe(h0 - 24);
  });
});
