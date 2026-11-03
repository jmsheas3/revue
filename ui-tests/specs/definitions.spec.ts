// Go to definition: Ctrl+click (Cmd+click on macOS) on an identifier in a diff or the file view jumps straight to a
// definition that clearly wins, opens a picker on a tie (a menu, used from the keyboard too), and browser Back returns
// to where the click was. A name defined only on the other side of the change is looked up there, one defined nowhere
// says so, holding the modifier over a name marks it as a link, and a lookup that answers after the page went elsewhere
// is dropped.
import {
  test, expect, modClickWord, recordRequests, targetRows, viewLabel, removedLines, MOD, type Repo, type Server,
  wordCenter, hold, focusMain,
} from '../fixtures';
import type { Page } from '@playwright/test';

// Two `render`s, of which src/use.js imports src/a.js's (a direct jump), and two `helper`s that nothing tells apart
// (a tie). The calls are uncommitted lines of src/use.js, so they are in its diff on the new side.
function setup(repo: Repo) {
  repo.write('src/a.js', 'export function render() {\n  return 1;\n}\n');
  repo.write('src/b.js', 'export function render() {\n  return 2;\n}\n');
  repo.write('src/x.js', 'function helper() {\n  return 1;\n}\n');
  repo.write('src/y.js', 'function helper() {\n  return 2;\n}\n');
  repo.write('src/use.js', "import { render } from './a';\n\nexport function main() {\n  return 0;\n}\n");
  repo.commit('init');
  repo.write('src/use.js', "import { render } from './a';\n\nexport function main() {\n  render();\n  helper();\n  return 0;\n}\n");
}

// The code cell of the new side of line `n` (the last code cell of the row, in either diff layout or the file view).
const newCell = (page: Page, n: number) => page.locator(`#main tr[data-n="${n}"] td.src`).last();

test('Ctrl+click in a diff jumps to the one clear definition, and Back returns to the click', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  await expect(newCell(page, 4)).toHaveText('  render();');

  await modClickWord(page, newCell(page, 4), 'render');
  await expect(page).toHaveURL(/#mode=working&file=src%2Fa\.js&view=file&line=1$/);
  await expect(viewLabel(page)).toHaveText('src/a.js');
  const target = targetRows(page);
  await expect(target).toHaveAttribute('data-n', '1');
  await expect(target).toContainText('export function render()');
  await expect(page.getByRole('menu', { name: /^Definitions of / })).toHaveCount(0);

  await page.goBack();
  await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);
  await expect(viewLabel(page)).toHaveText('src/use.js');
  await expect(newCell(page, 4)).toHaveText('  render();');
});

test('Ctrl+click on a name with tied definitions opens a picker', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');

  await modClickWord(page, newCell(page, 5), 'helper');
  const picker = page.getByRole('menu', { name: 'Definitions of helper' });
  await expect(picker).toContainText('2 definitions of helper');
  const items = picker.getByRole('menuitem');
  await expect(items).toHaveText([/src\/x\.js:1.*function helper\(\)/, /src\/y\.js:1.*function helper\(\)/]);
  await expect(items.first()).toBeFocused();
  // Nothing was opened yet.
  await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);

  await items.filter({ hasText: 'src/y.js:1' }).click();
  await expect(picker).toHaveCount(0);
  await expect(viewLabel(page)).toHaveText('src/y.js');
  await expect(targetRows(page)).toHaveAttribute('data-n', '1');
  // The focused button went with the picker; focus is on the pane that shows the definition, not dropped to the page.
  await expect(page.locator('#main')).toBeFocused();

  await page.goBack();
  await expect(viewLabel(page)).toHaveText('src/use.js');
});

test('Esc closes the picker and puts focus back on the main pane', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');

  await modClickWord(page, newCell(page, 5), 'helper');
  const picker = page.getByRole('menu', { name: 'Definitions of helper' });
  const items = picker.getByRole('menuitem');
  await expect(items.first()).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(items.nth(1)).toBeFocused();

  await page.keyboard.press('Escape');
  await expect(picker).toHaveCount(0);
  await expect(page.locator('#main')).toBeFocused();
  // The picker kept that Esc to itself: the file is still open. A second Esc, from the main pane, goes back to all
  // changes.
  await expect(viewLabel(page)).toHaveText('src/use.js');
  await page.keyboard.press('Escape');
  await expect(viewLabel(page)).toHaveText(/^All changes/);
});

// As the header's menus do: Tab closes the picker and focus moves on from where the picker is, the end of the page,
// rather than to the main pane, where Esc and a pick send it.
test('Tab past the last item or Shift+Tab past the first closes the picker', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  const picker = page.getByRole('menu', { name: 'Definitions of helper' });
  const items = picker.getByRole('menuitem');

  await modClickWord(page, newCell(page, 5), 'helper');
  await expect(items).toHaveCount(2);
  await page.keyboard.press('ArrowDown');
  await expect(items.last()).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(picker).toHaveCount(0);
  // Past the end of the page: the first tab stop.
  await expect(page.getByLabel('Branch to view')).toBeFocused();
  await expect(viewLabel(page)).toHaveText('src/use.js');

  await modClickWord(page, newCell(page, 5), 'helper');
  await expect(items.first()).toBeFocused();
  await page.keyboard.press('Shift+Tab');
  await expect(picker).toHaveCount(0);
  // Back from the end of the page: the last tab stop, the file's view switch.
  await expect(page.locator('#main .file-head').getByRole('button', { name: 'File', exact: true })).toBeFocused();
  await expect(viewLabel(page)).toHaveText('src/use.js');
});

test('Ctrl+click works in the file view too', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js&view=file');
  await expect(page.locator('#main table.fileview')).toBeVisible();

  // The import on line 1, an unchanged line (the diff shows it too, as context).
  await modClickWord(page, newCell(page, 1), 'render');
  await expect(viewLabel(page)).toHaveText('src/a.js');
  await expect(targetRows(page)).toHaveAttribute('data-n', '1');

  await page.goBack();
  await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js&view=file$/);
  await expect(page.locator('#main table.fileview tr[data-n="1"]')).toContainText("import { render } from './a';");
});

test('a plain click on an identifier does not jump', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  const lookups = recordRequests(page, /^\/api\/def$/);

  await modClickWord(page, newCell(page, 4), 'render', false);
  // The control: the same word with the modifier looks it up and jumps. A lookup starts on the click, so by the time
  // the jump shows, one from the plain click would have been recorded first.
  await modClickWord(page, newCell(page, 4), 'render');
  await expect(viewLabel(page)).toHaveText('src/a.js');
  expect(lookups, 'only the Ctrl+click looked the word up').toHaveLength(1);
});

test('Ctrl+click in the old half of a split diff finds a definition the change adds', async ({ page, repo, revue }) => {
  // helper() is called on an unchanged line but only defined in the working tree, so the old side has no definition.
  repo.write('src/run.js', 'export function run() {\n  return helper();\n}\n');
  repo.commit('init');
  repo.write('src/run.js', 'export function run() {\n  return helper();\n}\n\nfunction helper() {\n  return 1;\n}\n');
  const s = await revue.start();
  // Wide enough for split, which is the default layout.
  await page.setViewportSize({ width: 1600, height: 900 });
  await s.open(page, 'mode=working&file=src/run.js');
  const oldCell = page.locator('#main table.split tr[data-o="2"] td.src').first();
  await expect(oldCell).toHaveText('  return helper();');

  await modClickWord(page, oldCell, 'helper');
  await expect(page.getByRole('status')).toHaveText('helper is only defined after this change');
  const target = targetRows(page);
  await expect(target).toHaveAttribute('data-n', '5');
  await expect(target).toContainText('function helper()');
  // In the file view of the new side, where the definition is (no side=old).
  await expect(page).toHaveURL(/#mode=working&file=src%2Frun\.js&view=file&line=5$/);
});

// Three `helper`s that nothing tells apart, called from src/use.js, which has a changed file before it (docs/a.txt) and
// one after it (top.txt) in the tree: j and k from it would open one of those.
function threeHelpers(repo: Repo) {
  for (const f of ['x', 'y', 'z']) repo.write(`src/${f}.js`, `function helper() {\n  return '${f}';\n}\n`);
  repo.write('src/use.js', 'export function main() {\n  return 0;\n}\n');
  repo.write('docs/a.txt', 'a\n').write('top.txt', 'top\n');
  repo.commit('init');
  repo.write('src/use.js', 'export function main() {\n  helper();\n  return 0;\n}\n');
  repo.write('docs/a.txt', 'a, edited\n').write('top.txt', 'top, edited\n');
}

test.describe('the picker from the keyboard', () => {
  test('the arrows move through the definitions and wrap around, and Enter opens the focused one', async ({ page, repo, revue }) => {
    threeHelpers(repo);
    const s = await revue.start();
    await s.open(page, 'mode=working&file=src/use.js');

    await modClickWord(page, newCell(page, 2), 'helper');
    const picker = page.getByRole('menu', { name: 'Definitions of helper' });
    const items = picker.getByRole('menuitem');
    await expect(items).toHaveText([/^src\/x\.js:1/, /^src\/y\.js:1/, /^src\/z\.js:1/]);
    await expect(items.first()).toBeFocused();

    await page.keyboard.press('ArrowDown');
    await expect(items.nth(1)).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(items.nth(2)).toBeFocused();
    // Down from the last item goes round to the first, and Up from the first to the last.
    await page.keyboard.press('ArrowDown');
    await expect(items.first()).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(items.nth(2)).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(items.nth(1)).toBeFocused();
    // Nothing was opened by moving.
    await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);

    await page.keyboard.press('Enter');
    await expect(picker).toHaveCount(0);
    await expect(page).toHaveURL(/#mode=working&file=src%2Fy\.js&view=file&line=1$/);
    await expect(viewLabel(page)).toHaveText('src/y.js');
    await expect(targetRows(page)).toHaveAttribute('data-n', '1');
    await expect(targetRows(page)).toContainText('function helper()');
    await expect(page.locator('#main')).toBeFocused();
  });

  test('j, k and Esc in the picker stay there instead of reaching the page shortcuts', async ({ page, repo, revue }) => {
    threeHelpers(repo);
    const s = await revue.start();
    await s.open(page, 'mode=working&file=src/use.js');
    await modClickWord(page, newCell(page, 2), 'helper');
    const picker = page.getByRole('menu', { name: 'Definitions of helper' });
    const items = picker.getByRole('menuitem');
    await expect(items.first()).toBeFocused();

    // As shortcuts, j and k would open top.txt and docs/a.txt. Key events are handled in order, so once the ArrowDown
    // pressed after each has moved the focus, whatever it did is done.
    await page.keyboard.press('j');
    await page.keyboard.press('ArrowDown');
    await expect(items.nth(1)).toBeFocused();
    await expect(viewLabel(page)).toHaveText('src/use.js');
    await page.keyboard.press('k');
    await page.keyboard.press('ArrowDown');
    await expect(items.nth(2)).toBeFocused();
    await expect(viewLabel(page)).toHaveText('src/use.js');
    await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);
    await expect(picker).toBeVisible();

    // Esc closes the picker only: as a shortcut it would go back to all changes.
    await page.keyboard.press('Escape');
    await expect(picker).toHaveCount(0);
    await expect(page.locator('#main')).toBeFocused();
    await expect(viewLabel(page)).toHaveText('src/use.js');
    await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);

    // The control: from the main pane, the same keys are the shortcuts.
    await page.keyboard.press('j');
    await expect(viewLabel(page)).toHaveText('top.txt');
    await page.keyboard.press('Escape');
    await expect(viewLabel(page)).toHaveText('All changes · 3 files');
  });
});

test('a mouse press outside the picker closes it', async ({ page, repo, revue }) => {
  threeHelpers(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  await modClickWord(page, newCell(page, 2), 'helper');
  const picker = page.getByRole('menu', { name: 'Definitions of helper' });
  await expect(picker.getByRole('menuitem').first()).toBeFocused();

  // The label above the main pane: plain text, so the press neither moves focus nor does anything else.
  await viewLabel(page).click();
  await expect(picker).toHaveCount(0);
  await expect(viewLabel(page)).toHaveText('src/use.js');
  await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);
});

test('Ctrl+click on the old side of a renamed file opens the definition under the new name', async ({ page, repo, revue }) => {
  // compute() is defined in src/old.js, which the change renames to src/new.js and edits the call in.
  const body = (call: string) => `function compute() {\n  return 1;\n}\n\nexport function run() {\n  return ${call};\n}\n\n`
    + 'export function other() {\n  return 2;\n}\n';
  repo.write('src/old.js', body('compute()'));
  repo.commit('init');
  repo.rename('src/old.js', 'src/new.js');
  repo.write('src/new.js', body('compute() * 2'));
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/new.js');
  await expect(page.locator('#main .file-head')).toContainText('src/old.js');
  // The removed line, on the old side: src/old.js as it was at HEAD.
  const oldCell = removedLines(page.locator('#main'));
  await expect(oldCell).toHaveText(['  return compute();']);

  await modClickWord(page, oldCell, 'compute');
  // src/old.js isn't in the working tree: the definition opens in src/new.js, on its old side.
  await expect(page).toHaveURL(/#mode=working&file=src%2Fnew\.js&view=file&side=old&line=1$/);
  await expect(viewLabel(page)).toHaveText('src/new.js');
  const target = targetRows(page);
  await expect(target).toHaveAttribute('data-o', '1');
  await expect(target).toContainText('function compute()');
});

test('Ctrl+click on a name with no definition says so and stays put', async ({ page, repo, revue }) => {
  repo.write('src/use.js', 'export function main() {\n  return 0;\n}\n');
  repo.commit('init');
  repo.write('src/use.js', 'export function main() {\n  missing();\n  return 0;\n}\n');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  const lookups = recordRequests(page, /^\/api\/def$/);

  await modClickWord(page, newCell(page, 2), 'missing');
  await expect(page.getByRole('status')).toHaveText('No definition found for missing');
  // Both sides were asked.
  expect(lookups.map(u => new URL(u).searchParams.get('side'))).toEqual(['new', 'old']);
  await expect(page).toHaveURL(/#mode=working&file=src%2Fuse\.js$/);
  await expect(viewLabel(page)).toHaveText('src/use.js');
});

test('Ctrl+click on the new side finds a definition the change removes', async ({ page, repo, revue }) => {
  // helper() is still called on an unchanged line, but its definition is gone from the working tree.
  repo.write('src/run.js', 'export function run() {\n  return helper();\n}\n\nfunction helper() {\n  return 1;\n}\n');
  repo.commit('init');
  repo.write('src/run.js', 'export function run() {\n  return helper();\n}\n');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/run.js');
  await expect(newCell(page, 2)).toHaveText('  return helper();');

  await modClickWord(page, newCell(page, 2), 'helper');
  await expect(page.getByRole('status')).toHaveText('helper is only defined before this change');
  // In the file view of the old side, where the definition is.
  await expect(page).toHaveURL(/#mode=working&file=src%2Frun\.js&view=file&side=old&line=5$/);
  const target = targetRows(page);
  await expect(target).toHaveAttribute('data-o', '5');
  await expect(target).toContainText('function helper()');
});


test('holding Ctrl over an identifier marks it as a link, and letting go unmarks it', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  const main = page.locator('#main');
  const cell = newCell(page, 4);
  await expect(cell).toHaveText('  render();');
  // The marked word: the text of the ranges in the def-link highlight (style.css underlines it), and the pointer cursor
  // over code (#main.def-hover).
  const linked = () => page.evaluate(() => [...((CSS as any).highlights.get('def-link') ?? [])].map((r: Range) => r.toString()));
  const cursor = () => cell.evaluate(el => getComputedStyle(el).cursor);

  const at = await wordCenter(cell, 'render');
  await page.mouse.move(at.x, at.y);
  await expect(main).not.toHaveClass(/\bdef-hover\b/);
  expect(await linked()).toEqual([]);

  // Pressing the modifier with the pointer already over the word.
  await page.keyboard.down(MOD);
  await expect(main).toHaveClass(/\bdef-hover\b/);
  expect(await linked()).toEqual(['render']);
  expect(await cursor()).toBe('pointer');
  await page.keyboard.up(MOD);
  await expect(main).not.toHaveClass(/\bdef-hover\b/);
  expect(await linked()).toEqual([]);
  expect(await cursor()).not.toBe('pointer');

  // With the modifier held, moving from the blank end of the line onto the word marks it, and moving off unmarks it.
  const box = (await cell.boundingBox())!;
  const blank = { x: box.x + box.width - 4, y: at.y };
  await page.mouse.move(blank.x, blank.y);
  await page.keyboard.down(MOD);
  await expect(main).not.toHaveClass(/\bdef-hover\b/);
  await page.mouse.move(at.x, at.y);
  await expect(main).toHaveClass(/\bdef-hover\b/);
  expect(await linked()).toEqual(['render']);
  await page.mouse.move(blank.x, blank.y);
  await expect(main).not.toHaveClass(/\bdef-hover\b/);
  expect(await linked()).toEqual([]);
  await page.keyboard.up(MOD);
});

// A lookup asks about the range the page holds, which is the next view's as soon as one is asked for, while the rows
// clicked are still the last view's. So a Ctrl+click is ignored until the next view (its tree held here) is drawn.
test('a Ctrl+click on a view that another one is replacing is ignored', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=src/use.js');
  await expect(newCell(page, 4)).toHaveText('  render();');
  const lookups = recordRequests(page, /^\/api\/def$/);

  const tree = await hold(page, u => u.pathname === '/api/tree');
  await page.evaluate(() => { location.hash = 'mode=working'; });
  await tree.held;
  await modClickWord(page, newCell(page, 4), 'render');
  await page.waitForTimeout(500);
  expect(lookups).toEqual([]);
  await expect(page.getByRole('menu', { name: /^Definitions of / })).toHaveCount(0);
  await expect(viewLabel(page)).toHaveText('src/use.js');

  // The control: once the overview is drawn, the same Ctrl+click jumps.
  tree.release();
  await expect(viewLabel(page)).toHaveText('All changes · 1 file');
  await modClickWord(page, newCell(page, 4), 'render');
  await expect(viewLabel(page)).toHaveText('src/a.js');
  await expect(targetRows(page)).toHaveAttribute('data-n', '1');
});

// A lookup that answers after the page went elsewhere (Esc to the overview, j to the next file) is dropped: no jump, no
// picker and no "No definition found", which would pull the user back from where they went or speak of a view they
// left. Each held its /api/def request until then. The controls let it through without going anywhere, and the answer
// is acted on.
test.describe('a lookup that answers late', () => {
  // setup's names, plus `missing`, defined nowhere, and top.txt, the changed file after src/use.js (j opens it).
  function lateSetup(repo: Repo) {
    setup(repo);
    repo.write('src/use.js', "import { render } from './a';\n\nexport function main() {\n  render();\n  helper();\n  missing();\n  return 0;\n}\n");
    repo.write('top.txt', 'top\n');
  }
  const picker = (page: Page) => page.getByRole('menu', { name: /^Definitions of / });
  const noDefinition = (page: Page) => page.getByText(/^No definition found for /);
  // What a lookup of each name does: render jumps to src/a.js, helper's two definitions open the picker, and missing has
  // none, which a toast says.
  const outcomes = {
    render: { line: 4, what: 'jumps', seen: async (page: Page) => {
      await expect(viewLabel(page)).toHaveText('src/a.js');
      await expect(targetRows(page)).toHaveAttribute('data-n', '1');
    } },
    helper: { line: 5, what: 'opens the picker', seen: async (page: Page) => {
      await expect(page.getByRole('menu', { name: 'Definitions of helper' }).getByRole('menuitem')).toHaveCount(2);
    } },
    missing: { line: 6, what: 'says there is none', seen: async (page: Page) => {
      await expect(page.getByRole('status')).toHaveText('No definition found for missing');
    } },
  };
  type Name = keyof typeof outcomes;
  // Opens src/use.js and Ctrl+clicks `name` with its lookup held; returns the held request.
  async function clickHeld(page: Page, repo: Repo, revue: { start(): Promise<Server> }, name: Name) {
    lateSetup(repo);
    const s = await revue.start();
    await s.open(page, 'mode=working&file=src/use.js');
    await expect(newCell(page, outcomes[name].line)).toContainText(name);
    const lookup = await hold(page, u => u.pathname === '/api/def');
    await modClickWord(page, newCell(page, outcomes[name].line), name);
    await lookup.held;
    return lookup;
  }

  const leaves: { name: Name; how: string; leave: (page: Page) => Promise<void>; url: RegExp; back: (page: Page) => Promise<void> }[] = [
    { name: 'render', how: 'Esc to the overview', url: /#mode=working$/, back: async () => {},
      leave: async page => { await page.keyboard.press('Escape'); await expect(viewLabel(page)).toHaveText('All changes · 2 files'); } },
    { name: 'helper', how: 'j to the next file', url: /#mode=working&file=top\.txt$/,
      leave: async page => { await page.keyboard.press('j'); await expect(viewLabel(page)).toHaveText('top.txt'); },
      back: async page => { await page.keyboard.press('k'); await expect(viewLabel(page)).toHaveText('src/use.js'); } },
    { name: 'missing', how: 'Esc to the overview', url: /#mode=working$/, back: async () => {},
      leave: async page => { await page.keyboard.press('Escape'); await expect(viewLabel(page)).toHaveText('All changes · 2 files'); } },
  ];
  for (const { name, how, leave, url, back } of leaves) {
    test(`a lookup that ${outcomes[name].what}, answering after ${how}, is dropped`, async ({ page, repo, revue }) => {
      const lookup = await clickHeld(page, repo, revue, name);
      await focusMain(page);
      await leave(page);
      const label = await viewLabel(page).textContent();
      lookup.release();
      await lookup.answered();
      await page.waitForTimeout(500);
      await expect(picker(page)).toHaveCount(0);
      await expect(noDefinition(page)).toHaveCount(0);
      await expect(viewLabel(page)).toHaveText(label!);
      await expect(page).toHaveURL(url);

      // The control: the same Ctrl+click where the page is now (the overview has src/use.js's diff too) is acted on.
      await back(page);
      await modClickWord(page, newCell(page, outcomes[name].line), name);
      await outcomes[name].seen(page);
    });
  }

  for (const name of Object.keys(outcomes) as Name[]) {
    test(`a lookup that ${outcomes[name].what}, answering late on the same view, is acted on`, async ({ page, repo, revue }) => {
      const lookup = await clickHeld(page, repo, revue, name);
      lookup.release();
      await outcomes[name].seen(page);
    });
  }
});
