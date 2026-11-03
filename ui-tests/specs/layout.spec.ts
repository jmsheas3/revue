// Layout: the split/unified diff layout (s), what blocks split (a narrow diff pane, no-wrap lines) without touching the
// saved preference, the context setting, where the two show (text diffs only), the sidebar's width, the preference
// cookies (read again on focus, unknown values ignored, old localStorage values moved over), and the header at the
// window widths where it changes: the one-row bar with a drawer and a ⋯ menu at 760 px or less, and the tightened bar
// between 761 and 1150 px, with room for the stale badge.
import {
  test, expect, lines, png, focusMain, reload, treeRow, folderRow, section, addedLines, viewLabel, commitList, fileTree,
  overviewTitle, hold,
  type Repo, type Server, type StartOptions,
} from '../fixtures';
import type { Page, BrowserContext, Locator } from '@playwright/test';
import * as fs from 'node:fs';

// Wide enough that #main (the window less the 320 px sidebar) fits two SPLIT_MIN_CH (60) character sides. At 1050 px
// #main is 730 px, about 41 characters a side, so split is blocked; the toolbar still has room there for the reason on
// the Unified button, which style.css drops at 1000 px or less.
const WIDE = { width: 1600, height: 900 };
const MEDIUM = { width: 1050, height: 900 };
const PHONE = { width: 600, height: 800 };
// NARROW_SETTLE (200 ms in app.js), the time the width must hold before the diffs are redrawn, with a margin.
const PAST_SETTLE_MS = 600;

const diffTable = (page: Page) => page.locator('#main table.code');
const layoutButton = (page: Page, name: 'Split' | 'Unified') =>
  page.locator('#layout-seg').getByRole('button', { name: new RegExp(`^${name}`) });
// For a label's text as shown: innerText leaves out a part that style.css hides (the reason on the Unified button).
const SHOWN = { useInnerText: true };

// A revue.* preference cookie, as the browser holds it. Firefox hands a cookie the page set to the context a moment
// later, so poll for one that should change.
async function cookie(context: BrowserContext, name: string): Promise<string | undefined> {
  return (await context.cookies()).find(c => c.name === `revue.${name}`)?.value;
}
// Saves `layout` as the layout preference before the page loads.
async function saveLayout(context: BrowserContext, s: Server, layout: 'split' | 'unified') {
  await context.addCookies([{ name: 'revue.layout', value: layout, url: s.url }]);
}

// One modified file, so the overview shows one diff table.
function oneChange(repo: Repo) {
  repo.write('a.txt', lines(20)).commit('init');
  repo.write('a.txt', lines(20, 9));
}

test.describe('split and unified', () => {
  test.use({ viewport: WIDE });

  test('s switches between split and unified and saves the choice', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    await expect(layoutButton(page, 'Split')).toHaveClass(/\bon\b/);

    await focusMain(page);
    await page.keyboard.press('s');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    await expect.poll(() => cookie(context, 'layout')).toBe('unified');

    await page.keyboard.press('s');
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    await expect.poll(() => cookie(context, 'layout')).toBe('split');
  });

  test('the layout buttons switch the layout too', async ({ page, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    await layoutButton(page, 'Unified').click();
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await layoutButton(page, 'Split').click();
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
  });

  // The saved preference is split throughout, so the cookie is checked against what was saved, not a default.
  test('a narrow diff pane forces unified and says why', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'split');
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);

    await page.setViewportSize(MEDIUM);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    // The diffs are redrawn once the width has settled (NARROW_SETTLE).
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    // Split only explains while it is blocked.
    await focusMain(page);
    await page.keyboard.press('s');
    await expect(page.getByRole('status')).toHaveText(/Too narrow for split/);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    // And Unified, already drawn, saves nothing: the reason (given only for a split preference) stays.
    await layoutButton(page, 'Unified').click();
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);

    // Widening draws the saved split again.
    await page.setViewportSize(WIDE);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'false');
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    expect(await cookie(context, 'layout')).toBe('split');
  });

  // The page draws nothing until /api/info answers (setMode waits for it), but the ResizeObserver on #main is wired
  // already: a width change then sets the layout controls without the branch lists, which need /api/info (it used to
  // throw reading state.info.branches), and doesn't draw the diffs early once the width settles. The first render draws
  // the layout of the moment, and the branch controls are filled.
  test('a resize before /api/info answers sets the layout, which the first render draws', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'split');
    let release!: () => void;
    const held = new Promise<void>(r => { release = r; });
    await page.route(u => u.pathname === '/api/info', async route => { await held; await route.continue(); });
    await page.goto(s.url);
    // wire() has set the sidebar's width, and wired wireNarrow right after it.
    await expect(page.locator('#side')).toHaveAttribute('style', /width: 320px/);
    await page.setViewportSize(MEDIUM);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    await page.waitForTimeout(PAST_SETTLE_MS);
    await expect(page.locator('#main')).toBeEmpty();

    release();
    await expect(page).toHaveTitle(/^revue · /);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page.getByLabel('Branch to view').locator('option')).toHaveText(['⎇ main (checked out)']);
  });

  test('w (no wrap) forces unified and says why', async ({ page, context, repo, revue }) => {
    // An added line of 400 characters: wrapped, it fits the pane; not wrapped, the diff scrolls sideways.
    repo.write('a.txt', lines(20)).commit('init');
    repo.write('a.txt', lines(20, 9) + `${'long '.repeat(80)}\n`);
    const s = await revue.start();
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    const scrollsSideways = () => page.locator('#main .diff-body').evaluate(el => el.scrollWidth > el.clientWidth);
    expect(await scrollsSideways()).toBe(false);

    await focusMain(page);
    await page.keyboard.press('w');
    await expect(page.getByRole('status')).toHaveText('Long lines scroll sideways');
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (no wrap)', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect.poll(scrollsSideways).toBe(true);
    await expect.poll(() => cookie(context, 'wrap')).toBe('0');

    await page.keyboard.press('s');
    await expect(page.getByRole('status')).toHaveText(/Split needs wrapped lines/);

    await page.keyboard.press('w');
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified', SHOWN);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    await expect.poll(scrollsSideways).toBe(false);
    await expect.poll(() => cookie(context, 'wrap')).toBe('1');
  });

  test('the saved split preference survives both blocks and comes back when they lift', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'split');
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);

    // Narrow, then no wrap on top of it: narrow is the reason given while both hold.
    await page.setViewportSize(MEDIUM);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await focusMain(page);
    await page.keyboard.press('w');
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    // A reload while both hold: the page reads the layout and wrap cookies again, and the reason shows, which it does
    // only when the preference it read is split.
    await reload(page);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);

    // Lifting one block leaves the other's reason.
    await page.setViewportSize(WIDE);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (no wrap)', SHOWN);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    await focusMain(page);
    await page.keyboard.press('w');
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveClass(/\bon\b/);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    expect(await cookie(context, 'layout')).toBe('split');
  });

  test('a saved unified preference gives no reason while blocked and stays unified after', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'unified');
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    await page.setViewportSize(MEDIUM);
    // Unified is what was asked for, so there is nothing to explain.
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified', SHOWN);

    await page.setViewportSize(WIDE);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'false');
    // The diffs are redrawn only once the width has held for NARROW_SETTLE: wait past that before checking that
    // they are still unified.
    await page.waitForTimeout(PAST_SETTLE_MS);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    expect(await cookie(context, 'layout')).toBe('unified');
    // The control: split can be drawn here, and s draws it.
    await focusMain(page);
    await page.keyboard.press('s');
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
  });

  // A click, like s, only explains while split is blocked. aria-disabled makes Playwright's click() wait for the button
  // to be enabled, which it never is here; a user's click still lands, so this one is forced.
  test('clicking Split while it is blocked says why and keeps unified', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'split');
    await page.setViewportSize(MEDIUM);
    await s.open(page);
    const split = layoutButton(page, 'Split'), toast = page.getByRole('status');
    await expect(split).toHaveAttribute('aria-disabled', 'true');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    await split.click({ force: true });
    await expect(toast).toHaveText('Too narrow for split: widen the window or the diff pane');
    await expect(toast).toHaveClass(/\bon\b/);
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    await expect(split).not.toHaveClass(/\bon\b/);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    // Blocked by no-wrap instead: the toast gives that reason.
    await page.setViewportSize(WIDE);
    await expect(split).toHaveAttribute('aria-disabled', 'false');
    await focusMain(page);
    await page.keyboard.press('w');
    await expect(toast).toHaveText('Long lines scroll sideways');
    await expect(split).toHaveAttribute('aria-disabled', 'true');
    await split.click({ force: true });
    await expect(toast).toHaveText('Split needs wrapped lines: press w to wrap them');
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    expect(await cookie(context, 'layout')).toBe('split');
  });

  // Between 761 and 1000 px the toolbar is short of room, so style.css drops the reason from the Unified button (the
  // Split button's title still gives it); above 1000 px it shows. #main is narrow at all three widths.
  test('the reason on Unified is left out at 1000 px or less and shown above', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await saveLayout(context, s, 'split');
    await page.setViewportSize({ width: 1000, height: 900 });
    await s.open(page);
    const unified = layoutButton(page, 'Unified');
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    await expect(layoutButton(page, 'Split')).toHaveAttribute('title', /^Too narrow for split/);
    // The reason is there, only not shown.
    await expect(unified.locator('.why')).toHaveText(' (narrow)');
    await expect(unified).toHaveText('Unified', SHOWN);

    await page.setViewportSize({ width: 1001, height: 900 });
    await expect(unified).toHaveText('Unified (narrow)', SHOWN);

    await page.setViewportSize({ width: 761, height: 900 });
    await expect(unified).toHaveText('Unified', SHOWN);
    await expect(unified.locator('.why')).toHaveText(' (narrow)');
  });
});

// The new-side line numbers the diff shows, first and last.
async function shownRange(page: Page): Promise<[number, number]> {
  return page.locator('#main table.code tr[data-n]').evaluateAll(rows => {
    const n = rows.map(r => Number((r as HTMLElement).dataset.n));
    return [Math.min(...n), Math.max(...n)] as [number, number];
  });
}

test.describe('context', () => {
  test.use({ viewport: WIDE });

  test('context 3, 10, 50 and the whole file show that many lines around a change', async ({ page, context, repo, revue }) => {
    repo.write('a.txt', lines(300)).commit('init');
    repo.write('a.txt', lines(300, 149)); // line 150
    const s = await revue.start();
    await s.open(page, 'mode=working&file=a.txt');
    await expect(diffTable(page)).toBeVisible();
    await expect.poll(() => shownRange(page)).toEqual([147, 153]);

    const select = page.locator('#context-select');
    for (const [value, range] of [['10', [140, 160]], ['50', [100, 200]], ['all', [1, 300]], ['3', [147, 153]]] as const) {
      await select.selectOption(value);
      await expect.poll(() => shownRange(page), `context ${value}`).toEqual(range);
      await expect.poll(() => cookie(context, 'context'), `context ${value}`).toBe(value);
    }
  });

  test('the context setting applies to the overview and is kept across a reload', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(300)).commit('init');
    repo.write('a.txt', lines(300, 149));
    const s = await revue.start();
    await s.open(page);
    await page.locator('#context-select').selectOption('10');
    await expect.poll(() => shownRange(page)).toEqual([140, 160]);
    await reload(page);
    await expect(page.locator('#context-select')).toHaveValue('10');
    await expect.poll(() => shownRange(page)).toEqual([140, 160]);
  });
});

// ---------------------------------------------------------------- where context and layout show

// Context and Split/Unified apply to text diffs only. The toolbar hides them where none is drawn: a binary change's
// diff (an image's Before and After, or "Binary file changed"), an untracked file too large to diff, and an overview
// whose changes are all binary. The ⋯ menu's Context, in narrow windows, follows the same rule (at 760 px or less).
async function expectDiffControls(page: Page, visible: boolean) {
  for (const control of [page.locator('#context-ctl'), page.locator('#layout-seg')]) await expect(control).toBeVisible({ visible });
}
const RED: [number, number, number, number] = [255, 0, 0, 255];
const BLUE: [number, number, number, number] = [0, 0, 255, 255];

test.describe('where context and layout show', () => {
  test.use({ viewport: WIDE });

  test("a binary change's diff hides context and layout, and a text change's shows them", async ({ page, repo, revue }) => {
    repo.write('a.png', png(10, 10, RED)).write('b.txt', lines(20)).commit('init');
    repo.write('a.png', png(10, 10, BLUE)).write('b.txt', lines(20, 9));
    // An untracked text file too large to count (over 2 MB) has no count of added lines either, as a binary file has
    // none. It is text, but the server doesn't diff it, so it has no text diff either: its diff says it is too large.
    const big = Buffer.alloc(2 * 1024 * 1024 + 1, 'y');
    repo.write('big.log', big);
    const s = await revue.start();
    await s.open(page, 'file=a.png');
    await expect(page.getByRole('img', { name: 'After: a.png' })).toBeVisible();
    await expectDiffControls(page, false);

    await treeRow(page, 'b.txt').click();
    await expect(addedLines(page.locator('#main'))).toHaveText(['edited 10']);
    await expectDiffControls(page, true);

    await treeRow(page, 'big.log').click();
    await expect(viewLabel(page)).toHaveText('big.log');
    await expect(page.locator('#main .file-body')).toHaveText(`File too large to display (${big.length} bytes)`);
    await expect(diffTable(page)).toHaveCount(0);
    await expectDiffControls(page, false);

    await treeRow(page, 'b.txt').click();
    await expect(addedLines(page.locator('#main'))).toHaveText(['edited 10']);
    await expectDiffControls(page, true);
  });

  test('an overview of only binary changes hides them, and a text change coming in shows them', async ({ page, repo, revue }) => {
    repo.write('a.png', png(10, 10, RED)).write('data.bin', Buffer.from([0, 1, 2, 3])).write('b.txt', lines(20)).commit('init');
    repo.write('a.png', png(10, 10, BLUE)).write('data.bin', Buffer.from([0, 1, 2, 4]));
    const s = await revue.start();
    await s.open(page);
    await expect(section(page, 'a.png').getByRole('img', { name: 'After: a.png' })).toBeVisible();
    await expect(section(page, 'data.bin').locator('.diff-body')).toHaveText('Binary file changed');
    await expectDiffControls(page, false);

    // A live update brings a text change, and takes it away again.
    repo.write('b.txt', lines(20, 9));
    await expect(addedLines(section(page, 'b.txt'))).toHaveText(['edited 10']);
    await expectDiffControls(page, true);
    repo.write('b.txt', lines(20));
    await expect(section(page, 'b.txt')).toHaveCount(0);
    await expectDiffControls(page, false);
  });
});

// ---------------------------------------------------------------- the sidebar's width

const sideWidth = (page: Page) => page.locator('#side').evaluate(el => el.getBoundingClientRect().width);
// Drags the divider between the sidebar and the diff pane (#resizer) to `x`: the sidebar, at the window's left edge,
// takes the pointer's x as its width.
async function dragResizer(page: Page, x: number) {
  const box = (await page.locator('#resizer').boundingBox())!;
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(x, y, { steps: 4 });
  await page.mouse.up();
}
// The tab gets focus, as when the user comes back to it: the page reads the preference cookies again.
const focusTab = (page: Page) => page.evaluate(() => { window.dispatchEvent(new Event('focus')); });

test.describe('the sidebar', () => {
  test.use({ viewport: WIDE });

  test('dragging the divider sets its width, which is saved and kept across a reload', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    expect(await sideWidth(page)).toBe(320);
    await dragResizer(page, 500);
    await expect.poll(() => sideWidth(page)).toBe(500);
    await expect.poll(() => cookie(context, 'sideWidth')).toBe('500');
    await reload(page);
    expect(await sideWidth(page)).toBe(500);
  });

  // Between 180 px and 300 px short of the window (setSide), dragged or saved: style.css's min-width alone would let a
  // narrower width be saved, so the saved value is checked as well as the width drawn.
  test('the width stays between 180 px and 300 px short of the window', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    await dragResizer(page, 40);
    await expect.poll(() => sideWidth(page)).toBe(180);
    await expect.poll(() => cookie(context, 'sideWidth')).toBe('180');
    await dragResizer(page, WIDE.width - 20);
    await expect.poll(() => sideWidth(page)).toBe(WIDE.width - 300);
    await expect.poll(() => cookie(context, 'sideWidth')).toBe(String(WIDE.width - 300));

    // A narrower window takes the width down with it, and a wider one gives back the saved width.
    await page.setViewportSize({ width: 1000, height: WIDE.height });
    await expect.poll(() => sideWidth(page)).toBe(700);
    await page.setViewportSize(WIDE);
    await expect.poll(() => sideWidth(page)).toBe(WIDE.width - 300);
  });

  // A width saved on a bigger screen is clamped on load too. The clamped width leaves #main too narrow for split, as
  // does a saved width that fits (900 px, after dragging the sidebar wide and reloading): the first render draws
  // unified, and the controls say why. This used to throw an uncaught TypeError on load (Chromium every time, Firefox
  // most times): wire() set the width after wireNarrow, so the ResizeObserver on #main saw #main turn narrow and called
  // syncControls, which read state.info.branches before /api/info had answered.
  test('a saved width too wide for split loads unified, clamped or not', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await context.addCookies([{ name: 'revue.sideWidth', value: '5000', url: s.url }]);
    await s.open(page);
    expect(await sideWidth(page)).toBe(WIDE.width - 300);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    // The controls that need /api/info are filled once it's in.
    await expect(page.locator('#branch-name')).toHaveText('main');

    await context.addCookies([{ name: 'revue.sideWidth', value: '900', url: s.url }]);
    await reload(page);
    expect(await sideWidth(page)).toBe(900);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(page.locator('#branch-name')).toHaveText('main');
  });

  // The ResizeObserver on #main sees the sidebar take its room, not only a window resize. The saved layout is never
  // touched: no layout cookie is written.
  test('dragging the sidebar wide forces unified, and dragging it back draws split again', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);

    await dragResizer(page, 900);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified (narrow)', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'true');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);

    await dragResizer(page, 320);
    await expect(layoutButton(page, 'Unified')).toHaveText('Unified', SHOWN);
    await expect(layoutButton(page, 'Split')).toHaveAttribute('aria-disabled', 'false');
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    expect(await cookie(context, 'layout')).toBeUndefined();
  });

  test('a width dragged in another tab applies here when this tab gets focus', async ({ page, context, repo, revue }) => {
    oneChange(repo);
    const s = await revue.start();
    await s.open(page);
    const other = await context.newPage();
    await s.open(other);
    await dragResizer(other, 450);
    await expect.poll(() => sideWidth(other)).toBe(450);
    // Nothing has told this tab yet.
    expect(await sideWidth(page)).toBe(320);
    // Firefox hands a cookie set in one page to the others a moment later: focus again until it has.
    await expect(async () => {
      await focusTab(page);
      expect(await sideWidth(page)).toBe(450);
    }).toPass();
  });
});

// ---------------------------------------------------------------- preferences in cookies

// a.txt changed at line 150 of 300 (so the context setting shows), and an unchanged b.txt (so "Changed only" shows).
function contextAndUnchanged(repo: Repo) {
  repo.write('a.txt', lines(300)).write('b.txt', lines(3)).commit('init');
  repo.write('a.txt', lines(300, 149));
}

test.describe('preferences', () => {
  test.use({ viewport: WIDE });

  // Cookies have no change event, so syncPrefs reads them again when the tab gets focus. The other tab is on another
  // server: cookies are scoped by host, not port, so every revue server shares them.
  test('preferences changed in a tab on another port apply here when this tab gets focus', async ({ page, context, repo, revue }) => {
    contextAndUnchanged(repo);
    const s = await revue.start(), s2 = await revue.start();
    await page.emulateMedia({ colorScheme: 'light' });
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    await expect(treeRow(page, 'b.txt')).toBeVisible();

    const other = await context.newPage();
    await s2.open(other);
    await focusMain(other);
    await other.keyboard.press('s');
    await expect(diffTable(other)).toHaveClass(/\bunified\b/);
    await other.keyboard.press('w');
    await expect(other.locator('#main')).toHaveClass(/\bnowrap\b/);
    await other.locator('#context-select').selectOption('10');
    await other.getByRole('checkbox', { name: 'Changed only' }).check();
    // The theme last, so once this tab shows it, it has read the others too.
    await other.locator('#theme-btn').click();
    await other.getByRole('menuitemradio', { name: /^Dark/ }).click();
    await expect(other.locator('html')).toHaveAttribute('data-theme', 'dark');

    // Nothing has told this tab yet.
    expect(await page.locator('html').getAttribute('data-theme')).toBeNull();
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);

    // Firefox hands a cookie set in one page to the others a moment later: focus again until it has.
    await expect(async () => {
      await focusTab(page);
      await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark', { timeout: 1_000 });
    }).toPass();
    await expect(page.locator('#theme-btn')).toHaveAttribute('aria-label', 'Theme: Dark');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(layoutButton(page, 'Unified')).toHaveClass(/\bon\b/);
    await expect(page.locator('#main')).toHaveClass(/\bnowrap\b/);
    await expect(page.locator('#context-select')).toHaveValue('10');
    await expect.poll(() => shownRange(page)).toEqual([140, 160]);
    await expect(page.getByRole('checkbox', { name: 'Changed only' })).toBeChecked();
    await expect(treeRow(page, 'b.txt')).toBeHidden();
  });

  // Any app on 127.0.0.1 can write these cookies, so readPrefs takes only a known value (prefChoice) and falls back
  // to the default for anything else, markup included.
  test('preference cookies with unknown values fall back to the defaults', async ({ page, context, repo, revue }) => {
    contextAndUnchanged(repo);
    const s = await revue.start();
    const bogus = {
      layout: 'evil', theme: encodeURIComponent('<img src=x onerror=alert(1)>'), context: 'abc', wrap: 'maybe',
      changedOnly: 'yes', sideWidth: 'wide',
    };
    await context.addCookies(Object.entries(bogus).map(([k, value]) => ({ name: `revue.${k}`, value, url: s.url })));
    await page.emulateMedia({ colorScheme: 'light' });
    await s.open(page);

    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);
    await expect(layoutButton(page, 'Split')).toHaveClass(/\bon\b/);
    await expect(page.locator('#context-select')).toHaveValue('3');
    await expect.poll(() => shownRange(page)).toEqual([147, 153]);
    await expect(page.locator('#main')).not.toHaveClass(/\bnowrap\b/);
    await expect(page.getByRole('checkbox', { name: 'Changed only' })).not.toBeChecked();
    await expect(treeRow(page, 'b.txt')).toBeVisible();
    // Auto, with Auto's icon on the button.
    await expect(page.locator('html')).not.toHaveAttribute('data-theme', /.*/);
    await expect(page.locator('#theme-btn')).toHaveAttribute('aria-label', 'Theme: Auto');
    await expect(page.locator('#theme-menu [data-theme="auto"]')).toHaveAttribute('aria-checked', 'true');
    expect(await page.locator('#theme-btn').innerHTML()).toBe(await page.locator('#theme-menu [data-theme="auto"] .ico').innerHTML());
    expect(await sideWidth(page)).toBe(320);

    // And the page works as usual from there.
    await focusMain(page);
    await page.keyboard.press('s');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect.poll(() => cookie(context, 'layout')).toBe('unified');
  });

  // Before prefs moved to cookies they were kept in localStorage. Such a value still counts while no cookie exists, and
  // is moved to a cookie: theme.js applies the theme before first paint, and app.js reads and moves them all.
  test('a preference saved in localStorage before prefs moved to cookies is honored and moved to a cookie', async ({ page, context, repo, revue }) => {
    contextAndUnchanged(repo);
    const s = await revue.start();
    await page.emulateMedia({ colorScheme: 'light' });
    await s.open(page);
    await expect(diffTable(page)).toHaveClass(/\bsplit\b/);

    const old = { layout: 'unified', theme: 'dark', context: '10', sideWidth: '400' };
    await context.clearCookies();
    await page.evaluate(old => { for (const [k, v] of Object.entries(old)) localStorage.setItem(`revue.${k}`, v); }, old);
    await reload(page);
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await expect(page.locator('#theme-btn')).toHaveAttribute('aria-label', 'Theme: Dark');
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
    await expect(page.locator('#context-select')).toHaveValue('10');
    await expect.poll(() => shownRange(page)).toEqual([140, 160]);
    expect(await sideWidth(page)).toBe(400);

    // Moved: the cookies hold them now, and localStorage doesn't.
    for (const [k, v] of Object.entries(old)) await expect.poll(() => cookie(context, k), k).toBe(v);
    expect(await page.evaluate(keys => keys.map(k => localStorage.getItem(`revue.${k}`)), Object.keys(old)))
      .toEqual([null, null, null, null]);

    // From then on the cookie decides: an old value left in localStorage doesn't count.
    await page.evaluate(() => localStorage.setItem('revue.layout', 'split'));
    await reload(page);
    await expect(diffTable(page)).toHaveClass(/\bunified\b/);
  });
});

// ---------------------------------------------------------------- the header at each width

// Fails when the page scrolls sideways, or a visible header control overlaps another, reaches past the window's edges,
// leaves the bar (its padding included: a row that only fits by running into it has no room left), or spills out of
// its slot (the child of #top it sits in): a slot squeezed below what its content needs lets that content run into the
// gap and over its neighbors. The controls are the leaves: buttons, selects, the repo name, the base select's label
// text and the stale badge. The branch picker's select is left out, since it lies invisibly over the picker's face
// (.picker in style.css), which is checked instead.
async function expectHeaderFits(page: Page) {
  const problems = await page.evaluate(() => {
    const out: string[] = [];
    const vw = window.innerWidth;
    if (document.documentElement.scrollWidth > vw) out.push(`page scrolls sideways: ${document.documentElement.scrollWidth} > ${vw}`);
    const bar = document.getElementById('top')!;
    const top = bar.getBoundingClientRect(), pad = getComputedStyle(bar);
    const shown = (el: Element) => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
    const name = (el: Element) => el.id ? `#${el.id}` : `${el.tagName.toLowerCase()} "${el.textContent!.trim()}"`;
    const fmt = (r: { left: number; right: number }) => `${r.left.toFixed(1)}..${r.right.toFixed(1)}`;
    const inside = (r: DOMRect, outer: { left: number; right: number; top: number; bottom: number }) =>
      r.left >= outer.left - 0.5 && r.right <= outer.right + 0.5 && r.top >= outer.top - 0.5 && r.bottom <= outer.bottom + 0.5;
    const win = { left: 0, right: vw, top: -Infinity, bottom: Infinity };
    const inBar = {
      left: top.left + parseFloat(pad.paddingLeft), right: top.right - parseFloat(pad.paddingRight), top: top.top, bottom: top.bottom,
    };

    const slots = [...bar.children].filter(shown);
    const leaves = [...bar.querySelectorAll('button, select, #repo-name, #branch-controls label > span, #stale, .picker-face')]
      .filter(el => !el.matches('.picker select') && shown(el));
    for (const el of [...slots, ...leaves]) {
      const r = el.getBoundingClientRect();
      if (!inside(r, win)) out.push(`${name(el)} outside the window: ${fmt(r)} of ${vw}`);
      if (!inside(r, inBar)) out.push(`${name(el)} outside the bar: ${fmt(r)} × ${r.top}..${r.bottom} of ${fmt(inBar)} × ${top.top}..${top.bottom}`);
    }
    for (const el of leaves) {
      const slot = slots.find(s => s.contains(el))!;
      const r = el.getBoundingClientRect(), sr = slot.getBoundingClientRect();
      if (slot !== el && !inside(r, sr)) out.push(`${name(el)} spills out of ${name(slot)}: ${fmt(r)} in ${fmt(sr)}`);
    }
    leaves.forEach((a, i) => leaves.slice(i + 1).forEach(b => {
      if (a.contains(b) || b.contains(a)) return;
      const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
      const w = Math.min(ra.right, rb.right) - Math.max(ra.left, rb.left), h = Math.min(ra.bottom, rb.bottom) - Math.max(ra.top, rb.top);
      if (w > 0.5 && h > 0.5) out.push(`${name(a)} overlaps ${name(b)}: ${fmt(ra)} and ${fmt(rb)}`);
    }));
    return out;
  });
  expect(problems).toEqual([]);
}

// The key hints on the mode tabs (1, 2, 3).
const modeKeys = (page: Page) => page.getByRole('navigation', { name: 'Mode' }).locator('.key');

// main with a commit, and a checked-out feature branch with one more, so Branch mode compares feature with main.
function featureBranch(repo: Repo) {
  repo.write('a.txt', lines(20)).commit('init');
  repo.switch('feature', true);
  repo.write('a.txt', lines(20, 4)).commit('change a');
}

const LONG_HEAD = 'feature/a-rather-long-branch-name-for-testing';
const LONG_BASE = 'release/2026-10-long-maintenance-branch-name';
// LONG_HEAD with a commit on top of LONG_BASE (and main), checked out or left for the picker to view from main.
function longBranches(repo: Repo, checkedOut: boolean) {
  repo.write('a.txt', lines(20)).commit('init');
  repo.branch(LONG_BASE);
  repo.switch(LONG_HEAD, true);
  repo.write('a.txt', lines(20, 4)).commit('change a');
  if (!checkedOut) repo.switch('main');
}

const STALE = 'Server out of date: restart revue';
// A server whose code changed on disk after it started, so the page shows the "Server out of date" badge:
// REVUE_CODE_FILE stands in for its binary (as in tests/server.rs), and is replaced once the server runs.
async function staleServer(revue: { start(opts?: StartOptions): Promise<Server> }): Promise<Server> {
  const code = test.info().outputPath('revue.bin');
  fs.writeFileSync(code, 'v1');
  const s = await revue.start({ env: { REVUE_CODE_FILE: code } });
  fs.rmSync(code);
  fs.writeFileSync(code, 'v2');
  expect((await s.json('/api/info')).stale).toBe(true);
  return s;
}

for (const width of [761, 1150]) {
  test.describe(`at ${width} px`, () => {
    test.use({ viewport: { width, height: 800 } });

    test('the header fits on one row, without key hints, and Branch mode says "vs"', async ({ page, repo, revue }) => {
      featureBranch(repo);
      const s = await revue.start();
      await s.open(page, 'mode=branch');
      const modes = page.getByRole('navigation', { name: 'Mode' });
      await expect(modes.getByRole('button', { name: /^Changes/ })).toBeVisible();
      await expect(modeKeys(page)).toHaveCount(3);
      await expect(modeKeys(page).filter({ visible: true })).toHaveCount(0);
      await expect(page.locator('#branch-controls').getByText('vs', { exact: true })).toBeVisible();
      await expect(page.locator('#branch-controls').getByText('compared with')).toBeHidden();
      await expect(page.getByRole('combobox', { name: 'Compared with' })).toHaveValue('main');
      // The narrow-window controls stay out of the way.
      await expect(page.getByRole('button', { name: 'Files and commits' })).toBeHidden();
      await expect(page.getByRole('button', { name: 'More', exact: true })).toBeHidden();
      await expectHeaderFits(page);
    });

    // The branch picker and the base select are the widest controls; long names must shrink them, not push them into
    // their neighbors. Viewing a branch that isn't checked out adds the Check out button next to the picker.
    for (const checkedOut of [true, false]) {
      test(`long branch and base names fit, with the branch ${checkedOut ? 'checked out' : 'only viewed'}`, async ({ page, repo, revue }) => {
        longBranches(repo, checkedOut);
        const s = await revue.start();
        await s.open(page, `mode=branch&base=${encodeURIComponent(LONG_BASE)}${checkedOut ? '' : `&head=${encodeURIComponent(LONG_HEAD)}`}`);
        await expect(page.getByRole('combobox', { name: 'Compared with' })).toHaveValue(LONG_BASE);
        await expect(page.locator('#branch-name')).toHaveText(LONG_HEAD);
        await expect(page.getByRole('button', { name: 'Check out' })).toBeVisible({ visible: !checkedOut });
        await expect(page.locator('#main .overview-head h2')).toHaveText(`${LONG_HEAD} vs ${LONG_BASE}`);
        await expectHeaderFits(page);
      });

      // Below 1150 px the badge says only "Restart" (its name and title keep the whole message), and either way it keeps
      // its width: the long names shrink instead, and at 761 px, where they can't shrink enough with a Check out button
      // there too, the bar's gaps and the live status's label make room.
      test(`a stale server's badge stays readable next to long names, with the branch ${checkedOut ? 'checked out' : 'only viewed'}`, async ({ page, repo, revue }) => {
        longBranches(repo, checkedOut);
        const s = await staleServer(revue);
        await s.open(page, `mode=branch&base=${encodeURIComponent(LONG_BASE)}${checkedOut ? '' : `&head=${encodeURIComponent(LONG_HEAD)}`}`);
        await expect(page.locator('#main .overview-head h2')).toHaveText(`${LONG_HEAD} vs ${LONG_BASE}`);
        await expect(page.getByRole('button', { name: 'Check out' })).toBeVisible({ visible: !checkedOut });
        const badge = page.locator('#stale');
        await expect(badge).toBeVisible();
        await expect(badge).toHaveText(width < 1150 ? 'Restart' : STALE, { useInnerText: true });
        await expect(badge).toHaveAccessibleName(STALE);
        await expect(badge).toHaveAttribute('title', new RegExp(`^${STALE}\\. `));
        // The whole label shows: no ellipsis.
        expect(await badge.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
        await expectHeaderFits(page);
      });
    }
  });
}

test.describe('at 1280 px', () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test('the header shows key hints and "compared with"', async ({ page, repo, revue }) => {
    featureBranch(repo);
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(modeKeys(page)).toHaveCount(3);
    await expect(modeKeys(page).filter({ visible: false })).toHaveCount(0);
    await expect(page.locator('#branch-controls').getByText('compared with')).toBeVisible();
    await expect(page.locator('#branch-controls').getByText('vs', { exact: true })).toBeHidden();
    await expectHeaderFits(page);
  });
});

test.describe('at 760 px or less', () => {
  test.use({ viewport: PHONE });

  // A few files in a folder, one changed in the middle of 120 lines, so the drawer has a tree to land in and a larger
  // context shows more of the file.
  function smallTree(repo: Repo) {
    repo.write('src/a.txt', lines(120)).write('src/b.txt', lines(5)).write('top.txt', lines(5)).commit('init');
    repo.write('src/a.txt', lines(120, 59)); // line 60
  }

  const drawerButton = (page: Page) => page.getByRole('button', { name: 'Files and commits' });
  const side = (page: Page) => page.locator('#side');

  // At 760 px too, the last width before the bar has room for everything (the 761 px tests above).
  for (const width of [PHONE.width, 760]) {
    test.describe(`${width} px`, () => {
      test.use({ viewport: { width, height: PHONE.height } });

      test('the header is one row with a mode select, the drawer button and the ⋯ menu', async ({ page, repo, revue }) => {
        smallTree(repo);
        const s = await revue.start();
        await s.open(page);
        await expect(drawerButton(page)).toBeVisible();
        await expect(page.getByRole('button', { name: 'More', exact: true })).toBeVisible();
        await expect(page.getByRole('combobox', { name: 'Mode' })).toBeVisible();
        await expect(page.getByRole('navigation', { name: 'Mode' })).toBeHidden();
        await expect(page.getByRole('button', { name: 'Keyboard shortcuts' })).toBeHidden();
        // The diff settings live in the ⋯ menu here.
        await expect(page.locator('#layout-seg')).toBeHidden();
        await expect(page.locator('#context-select')).toBeHidden();
        await expect(side(page)).toBeHidden();
        await expectHeaderFits(page);

        // The mode select switches modes.
        await page.getByRole('combobox', { name: 'Mode' }).selectOption('commit');
        await expect(page).toHaveURL(/mode=commit/);
      });
    });
  }

  test('the drawer opens on the tree, and Esc closes it with focus back on its button', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page, 'mode=working&file=src/a.txt');
    await drawerButton(page).click();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'true');
    await expect(side(page)).toBeVisible();
    // The branch picker moved into the drawer.
    await expect(side(page).getByRole('combobox', { name: 'Branch to view' })).toBeAttached();
    // Focus lands on the tree's current row, the open file's (its tab stop), not the first row (the src folder).
    await expect(treeRow(page, 'src/a.txt')).toBeFocused();

    await page.keyboard.press('Escape');
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(side(page)).toBeHidden();
    await expect(drawerButton(page)).toBeFocused();
  });

  test('clicking the scrim closes the drawer', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();
    // The drawer is 320 px wide; the scrim covers the rest below the bar.
    await page.locator('#scrim').click({ position: { x: PHONE.width - 40, y: 300 } });
    await expect(side(page)).toBeHidden();
    await expect(page.locator('#scrim')).toBeHidden();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(drawerButton(page)).toBeFocused();
  });

  test('picking a file closes the drawer and moves focus to the main pane', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    await drawerButton(page).click();
    await page.getByRole('treeitem', { name: /^a\.txt/ }).click();
    await expect(side(page)).toBeHidden();
    await expect(page.locator('#view-label')).toHaveText('src/a.txt');
    await expect(page.locator('#main')).toBeFocused();
  });

  test('Tab and Shift+Tab stay inside the open drawer', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();

    const inDrawer = () => page.evaluate(() => {
      const a = document.activeElement;
      return !!a && a !== document.body && document.getElementById('side')!.contains(a);
    });
    const focusedId = () => page.evaluate(() => {
      const a = document.activeElement as HTMLElement;
      return a.id || a.getAttribute('data-path') || a.getAttribute('data-dir') || a.tagName;
    });
    // Walk forward far enough to wrap around at least once, then back.
    const seen = new Set<string>();
    for (let i = 0; i < 12; i++) {
      await page.keyboard.press('Tab');
      expect(await inDrawer(), `Tab ${i + 1} left the drawer (on ${await focusedId()})`).toBe(true);
      seen.add(await focusedId());
    }
    expect(seen.has('filter'), 'Tab reaches the filter').toBe(true);
    for (let i = 0; i < 12; i++) {
      await page.keyboard.press('Shift+Tab');
      expect(await inDrawer(), `Shift+Tab ${i + 1} left the drawer (on ${await focusedId()})`).toBe(true);
    }
  });

  // The page's first view, once drawn, closed a drawer opened while it loaded and sent focus to the main pane, which a
  // slow runner caught in the test above (Tab then went on into the overview). Only a view asked for while the drawer
  // is open closes it.
  test('a drawer opened while the page loads stays open once the first view is drawn', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    const tree = await hold(page, u => u.pathname === '/api/tree');
    await s.open(page);
    await tree.held;
    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();
    // No tree yet, so it opens on the filter.
    await expect(page.locator('#filter')).toBeFocused();
    tree.release();
    // Drawn: the overview behind the drawer, and the tree in it.
    await expect(section(page, 'src/a.txt')).toBeAttached();
    await expect(folderRow(page, 'src')).toBeVisible();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'true');
    await expect(side(page)).toBeVisible();
    await expect(page.locator('#filter')).toBeFocused();
  });

  // A view asked for while the drawer is open (here by a link's hash; Back and an agent's navigate go the same way)
  // closes it once drawn, so what it shows isn't left behind the drawer, and focus goes to the main pane, as a pick's.
  test('a view asked for while the drawer is open closes it once drawn', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(section(page, 'src/a.txt')).toBeVisible();
    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();
    await page.evaluate(() => { location.hash = 'mode=working&file=src/b.txt'; });
    await expect(viewLabel(page)).toHaveText('src/b.txt');
    await expect(side(page)).toBeHidden();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(page.locator('#main')).toBeFocused();
  });

  test('the ⋯ menu holds context, theme and the shortcut list, and Esc closes it', async ({ page, context, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    const more = page.getByRole('button', { name: 'More', exact: true });
    const menu = page.getByRole('group', { name: 'More options' });
    await expect.poll(() => shownRange(page)).toEqual([57, 63]);

    await more.click();
    await expect(more).toHaveAttribute('aria-expanded', 'true');
    await expect(menu).toBeVisible();
    // Focus goes to the checked theme (Auto, here also the first).
    await expect(menu.getByRole('radio', { name: /Auto/ })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();
    await expect(more).toBeFocused();

    // Context from the menu applies like the toolbar's.
    await more.click();
    await menu.getByRole('combobox', { name: 'Context lines' }).selectOption('50');
    await expect(menu).toBeHidden();
    await expect.poll(() => shownRange(page)).toEqual([10, 110]);
    await expect.poll(() => cookie(context, 'context')).toBe('50');
    await expect(more).toBeFocused();

    // Theme. Opened again, the menu focuses the theme now checked, Dark, not the first.
    await more.click();
    await menu.getByRole('radio', { name: 'Dark' }).click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await expect(menu).toBeHidden();
    await more.click();
    await expect(menu.getByRole('radio', { name: 'Dark' })).toBeFocused();
    await expect(menu.getByRole('radio', { name: 'Dark' })).toHaveAttribute('aria-checked', 'true');
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();

    // The shortcut list.
    await more.click();
    await menu.getByRole('button', { name: /Keyboard shortcuts/ }).click();
    await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeVisible();
  });

  // Up and Down move through everything in the menu, wrapping at the ends, the Context select included (where they
  // would otherwise change its value); Tab and a click outside close it. Esc is checked in the test above.
  test('in the ⋯ menu the arrows move through its controls, and Tab or a click outside closes it', async ({ page, repo, revue }) => {
    smallTree(repo);
    const s = await revue.start();
    await s.open(page);
    const more = page.getByRole('button', { name: 'More', exact: true });
    const menu = page.getByRole('group', { name: 'More options' });
    const radio = (name: string) => menu.getByRole('radio', { name });
    const contextLines = menu.getByRole('combobox', { name: 'Context lines' });
    const shortcuts = menu.getByRole('button', { name: /Keyboard shortcuts/ });

    await more.click();
    await expect(radio('Auto')).toBeFocused();
    for (const [key, next] of [
      ['ArrowDown', radio('Light')], ['ArrowDown', radio('Dark')], ['ArrowDown', shortcuts], ['ArrowDown', contextLines],
      ['ArrowUp', shortcuts], ['ArrowDown', contextLines], ['ArrowDown', radio('Auto')], ['ArrowUp', contextLines],
    ] as const) {
      await page.keyboard.press(key);
      await expect(next, key).toBeFocused();
    }
    // Moving picks nothing.
    await expect(contextLines).toHaveValue('3');
    await expect(page.locator('html')).not.toHaveAttribute('data-theme', /.*/);
    await expect(menu).toBeVisible();

    await page.keyboard.press('Tab');
    await expect(menu).toBeHidden();
    await expect(more).toHaveAttribute('aria-expanded', 'false');

    await more.click();
    await expect(menu).toBeVisible();
    await page.locator('#view-label').click();
    await expect(menu).toBeHidden();
    await expect(more).toHaveAttribute('aria-expanded', 'false');
  });

  // The menu's Context is the toolbar's, so it shows where the toolbar's would: where a text diff is drawn, not in a
  // file's other views or a binary change's diff. While it is hidden, the arrows skip it.
  test("the ⋯ menu's Context shows only where a text diff is drawn", async ({ page, repo, revue }) => {
    smallTree(repo);
    repo.write('data.bin', Buffer.from([0, 1, 2, 3]));
    const s = await revue.start();
    await s.open(page, 'file=src/a.txt');
    const more = page.getByRole('button', { name: 'More', exact: true });
    const menu = page.getByRole('group', { name: 'More options' });
    const radio = (name: string) => menu.getByRole('radio', { name });
    const contextLines = menu.getByRole('combobox', { name: 'Context lines' });
    const shortcuts = menu.getByRole('button', { name: /Keyboard shortcuts/ });
    await expect(addedLines(page.locator('#main'))).toHaveText(['edited 60']);
    await more.click();
    await expect(contextLines).toBeVisible();
    await page.keyboard.press('Escape');

    // The file view.
    await page.locator('#main .file-head').getByRole('button', { name: 'File', exact: true }).click();
    await expect(page.locator('#main table.fileview')).toBeVisible();
    await more.click();
    await expect(radio('Auto')).toBeFocused();
    await expect(contextLines).toBeHidden();
    // Up from the first theme wraps to the shortcut list, and Down from there comes back, past the hidden select.
    for (const [key, next] of [['ArrowUp', shortcuts], ['ArrowDown', radio('Auto')], ['ArrowUp', shortcuts]] as const) {
      await page.keyboard.press(key);
      await expect(next, key).toBeFocused();
    }
    await page.keyboard.press('Escape');

    // A binary change's diff (the file opens in the view picked last, File).
    await drawerButton(page).click();
    await treeRow(page, 'data.bin').click();
    await expect(page.locator('#main .file-body')).toHaveText('Binary file (4 bytes)');
    await page.locator('#main .file-head').getByRole('button', { name: 'Diff', exact: true }).click();
    await expect(page.locator('#main .file-body')).toHaveText('Binary file changed');
    await more.click();
    await expect(radio('Auto')).toBeFocused();
    await expect(contextLines).toBeHidden();
    await page.keyboard.press('Escape');

    // The overview, with a text diff in it, shows it again.
    await page.getByRole('button', { name: 'All changes', exact: true }).click();
    await expect(addedLines(section(page, 'src/a.txt'))).toHaveText(['edited 60']);
    await more.click();
    await expect(contextLines).toBeVisible();
  });

  // In History the drawer holds the commit list above the tree, and opens on it.
  test('in History the drawer opens on the commit list, and picking a commit closes it', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(5)).commit('first');
    repo.write('a.txt', lines(5, 1)).commit('second');
    repo.write('a.txt', lines(5, 2)).commit('third');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('third');
    const option = (subject: string) => commitList(page).getByRole('option', { name: new RegExp(`^${subject}\\b`) });

    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();
    await expect(option('third')).toHaveAttribute('aria-selected', 'true');
    await expect(option('third')).toBeFocused();
    const top = async (l: Locator) => (await l.boundingBox())!.y;
    expect(await top(commitList(page))).toBeLessThan(await top(fileTree(page)));

    // From the keyboard.
    await page.keyboard.press('ArrowDown');
    await expect(option('second')).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(side(page)).toBeHidden();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(overviewTitle(page)).toHaveText('second');
    await expect(page.locator('#main')).toBeFocused();

    // And with a click, opened again on the commit now shown.
    await drawerButton(page).click();
    await expect(option('second')).toBeFocused();
    await option('first').click();
    await expect(side(page)).toBeHidden();
    await expect(overviewTitle(page)).toHaveText('first');
    await expect(page.locator('#main')).toBeFocused();
  });

  // In Branch mode, so the base select (in the drawer at this width) has to come back to the bar too.
  test('widening past 760 px puts the sidebar and the full bar back, and closes the drawer', async ({ page, repo, revue }) => {
    smallTree(repo);
    repo.switch('feature', true).commit('change a');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    const top = page.locator('#top');
    await drawerButton(page).click();
    await expect(side(page)).toBeVisible();
    await expect(side(page).getByRole('combobox', { name: 'Compared with' })).toBeVisible();

    await page.setViewportSize({ width: 1280, height: 800 });
    await expect(page.getByRole('navigation', { name: 'Mode' })).toBeVisible();
    await expect(drawerButton(page)).toBeHidden();
    await expect(side(page)).toBeVisible();
    // The branch picker and the base select are back in the bar.
    await expect(top.getByRole('combobox', { name: 'Branch to view' })).toBeAttached();
    await expect(top.getByRole('combobox', { name: 'Compared with' })).toBeVisible();
    await expect(top.getByRole('combobox', { name: 'Compared with' })).toHaveValue('main');
    await expectHeaderFits(page);

    // The drawer closed on the way: narrowed again, the page shows it shut, not open over the content.
    await page.setViewportSize(PHONE);
    await expect(drawerButton(page)).toBeVisible();
    await expect(drawerButton(page)).toHaveAttribute('aria-expanded', 'false');
    await expect(side(page)).toBeHidden();
    await expect(page.locator('#scrim')).toBeHidden();
  });
});
