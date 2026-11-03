// Plain folders: a folder outside any git repo, or any folder with --no-git, opens as a document viewer. The git
// controls are hidden and a "folder" tag stands where the branch picker is; the tree lists every file (hidden ones and
// .git's included, with a last row when the listing was cut short); the page opens on the root's README in preview, or
// an empty state without one; opening a folder shows its README while the arrow keys only open and close; j/k step
// through every file, Esc goes back to the start, f switches file and preview, s and 1/2/3 do nothing, and Ctrl+click
// goes to a definition among the folder's files; live updates,
// notes and the narrow windows' drawer work as in a repo; an old link to a commit opens the file it names; and the page
// never asks for a diff or the log, which a plain folder doesn't have.
import {
  test, expect, treeRow, folderRow, viewLabel, recordRequests, hashParams, focusMain, fileTree, modClickWord, lines, targetRows,
  type Folder,
} from '../fixtures';
import type { Page } from '@playwright/test';

const preview = (page: Page) => page.locator('#main .markdown');
const fileView = (page: Page) => page.locator('#main table.fileview');
const emptyState = (page: Page) => page.locator('#main .empty-state');
const toastShown = (page: Page) => page.locator('#toast.on');

// README.md at the root, a folder with a README and one without, and a few plain files.
function docs(folder: Folder) {
  folder.write('README.md', '# Home\n\nWelcome home.\n')
    .write('guide/README.md', '# Guide\n\nRead me first.\n')
    .write('guide/setup.txt', 'step 1\nstep 2\n')
    .write('notes/todo.txt', 'todo 1\n')
    .write('notes/done.txt', 'done 1\n')
    .write('a.txt', lines(5))
    .write('.hidden/secret.txt', 'hidden\n');
}

test('hides the git controls and shows every file under a folder tag', async ({ page, context, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  // "Changed only" is a preference every server shares; a plain folder has no changes, so it lists every file anyway.
  await context.addCookies([{ name: 'revue.changedOnly', value: '1', url: s.url }]);
  await s.open(page);

  await expect(page.locator('#folder-tag')).toHaveText('folder');
  await expect(page.locator('#folder-tag')).toBeVisible();
  await expect(page.locator('#repo-name')).toHaveText(folder.dir.split('/').pop()!);
  await expect(page.getByRole('navigation', { name: 'Mode' })).toBeHidden();
  await expect(page.locator('#mode-select')).toBeHidden();
  await expect(page.getByRole('combobox', { name: 'Branch to view' })).toBeHidden();
  await expect(page.getByRole('combobox', { name: 'Compared with' })).toBeHidden();
  await expect(page.getByRole('listbox', { name: 'Commits' })).toBeHidden();
  await expect(page.locator('#pane-split')).toBeHidden();
  await expect(page.getByRole('checkbox', { name: 'Changed only' })).toBeHidden();
  await expect(page.locator('#context-ctl')).toBeHidden();
  await expect(page.locator('#layout-seg')).toBeHidden();

  // Every file, hidden ones included, and none of them muted as unchanged.
  await expect(treeRow(page, 'README.md')).toBeVisible();
  await expect(treeRow(page, 'a.txt')).toBeVisible();
  await expect(folderRow(page, '.hidden')).toBeVisible();
  await expect(fileTree(page).locator('.node.dim')).toHaveCount(0);
  await expect(page.locator('#summary')).toHaveText('7 files');

  // A file has no Diff tab and no Before/After; markdown has File and Preview.
  await expect(preview(page)).toContainText('Welcome home.');
  const head = page.locator('#main .file-head');
  await expect(head.getByRole('button', { name: 'Diff' })).toHaveCount(0);
  await expect(head.getByRole('button', { name: 'Before' })).toHaveCount(0);
  await expect(head.getByRole('button', { name: 'File' })).toBeVisible();
  await expect(head.getByRole('button', { name: 'Preview' })).toBeVisible();
  await treeRow(page, 'a.txt').click();
  await expect(fileView(page)).toContainText('line 5');
  await expect(head.locator('.seg')).toHaveCount(0);

  // The shortcut list leaves out what a plain folder doesn't have.
  await focusMain(page);
  await page.keyboard.press('?');
  const panel = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
  await expect(panel.getByText('next / previous file', { exact: true })).toBeVisible();
  await expect(panel.getByText('open the row; on a folder, also show its README')).toBeVisible();
  await expect(panel.getByText('next / previous changed file')).toBeHidden();
  await expect(panel.getByText('split / unified')).toBeHidden();
  await expect(panel.getByText('Changes / History / Branch')).toBeHidden();
});

test('opens on the root README in preview', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(preview(page)).toContainText('Welcome home.');
  await expect(viewLabel(page)).toHaveText('README.md');
  await expect(treeRow(page, 'README.md')).toHaveAttribute('aria-selected', 'true');
  // A file pick: the link names the file and the view, and no mode.
  expect(hashParams(page)).toEqual({ file: 'README.md', view: 'preview' });
});

test('takes README.md, then README, then any readme.*, then index.md', async ({ page, folder, revue }) => {
  folder.write('index.md', '# Index page\n')
    .write('a/Readme.rst', 'rst readme\n').write('a/index.md', '# not this\n')
    .write('b/README', 'plain readme\n').write('b/readme.md', '# not this either\n')
    .write('c/index.md', '# c index\n').write('c/x.txt', 'x\n');
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(preview(page)).toContainText('Index page');
  await expect(viewLabel(page)).toHaveText('index.md');

  await folderRow(page, 'a').click();
  await expect(viewLabel(page)).toHaveText('a/Readme.rst');
  await expect(fileView(page)).toContainText('rst readme');
  await folderRow(page, 'b').click();
  await expect(viewLabel(page)).toHaveText('b/README');
  await expect(fileView(page)).toContainText('plain readme');
  await folderRow(page, 'c').click();
  await expect(viewLabel(page)).toHaveText('c/index.md');
  await expect(preview(page)).toContainText('c index');
});

test('with no README at its root, opens on an empty state', async ({ page, folder, revue }) => {
  folder.write('a.txt', 'a\n').write('docs/README.md', '# Docs\n');
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(emptyState(page).getByRole('heading')).toHaveText('Pick a file from the tree');
  await expect(emptyState(page).locator('.watching')).toHaveText('Watching the folder. Files show up in the tree as they are added.');
  await expect(viewLabel(page)).toHaveText('No file open');
  expect(hashParams(page)).toEqual({});

  // Esc from a file comes back to it.
  await treeRow(page, 'a.txt').click();
  await expect(fileView(page)).toContainText('a');
  await focusMain(page);
  await page.keyboard.press('Escape');
  await expect(emptyState(page)).toBeVisible();
  expect(hashParams(page)).toEqual({});
});

test("opening a folder shows its README; closing one, a folder without one and the arrow keys leave the view", async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(viewLabel(page)).toHaveText('README.md');

  const guide = folderRow(page, 'guide'), notes = folderRow(page, 'notes');
  await guide.click();
  await expect(guide).toHaveAttribute('aria-expanded', 'true');
  await expect(viewLabel(page)).toHaveText('guide/README.md');
  await expect(preview(page)).toContainText('Read me first.');
  expect(hashParams(page)).toEqual({ file: 'guide/README.md', view: 'preview' });

  // No README: it only opens.
  await notes.click();
  await expect(notes).toHaveAttribute('aria-expanded', 'true');
  await expect(treeRow(page, 'notes/todo.txt')).toBeVisible();
  await expect(viewLabel(page)).toHaveText('guide/README.md');

  // Closing never changes the view, even back to a folder with a README.
  await guide.click();
  await expect(guide).toHaveAttribute('aria-expanded', 'false');
  await expect(viewLabel(page)).toHaveText('guide/README.md');

  // Back at the start, the arrow keys open and close the folder and leave the view alone.
  await focusMain(page);
  await page.keyboard.press('Escape');
  await expect(viewLabel(page)).toHaveText('README.md');
  await guide.focus();
  await page.keyboard.press('ArrowRight');
  await expect(guide).toHaveAttribute('aria-expanded', 'true');
  await expect(guide).toBeFocused();
  await page.keyboard.press('ArrowLeft');
  await expect(guide).toHaveAttribute('aria-expanded', 'false');
  await expect(viewLabel(page)).toHaveText('README.md');
  await expect(preview(page)).toContainText('Welcome home.');

  // Enter opens it as a click does: its README too.
  await page.keyboard.press('Enter');
  await expect(guide).toHaveAttribute('aria-expanded', 'true');
  await expect(viewLabel(page)).toHaveText('guide/README.md');
});

test('with --no-git, a repo opens as a plain folder whose .git files are listed and open', async ({ page, repo, revue }) => {
  repo.write('a.txt', 'a\n').commit('init');
  const s = await revue.start({ args: ['--no-git'] });
  await s.open(page);
  await expect(page.locator('#folder-tag')).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Mode' })).toBeHidden();
  const git = folderRow(page, '.git');
  await git.click();
  await expect(git).toHaveAttribute('aria-expanded', 'true');
  await treeRow(page, '.git/HEAD').click();
  await expect(viewLabel(page)).toHaveText('.git/HEAD');
  await expect(fileView(page)).toContainText('ref: refs/heads/main');
});

test('a listing cut short ends with a row that says so', async ({ page, folder, revue }) => {
  for (let i = 0; i < 30; i++) folder.write(`f${String(i).padStart(2, '0')}.txt`, `${i}\n`);
  const s = await revue.start({ folder, env: { REVUE_TEST_MAX_FILES: '10' } });
  await s.open(page);
  await expect(fileTree(page).locator('.node.file')).toHaveCount(10);
  await expect(fileTree(page).locator('.tree-more')).toHaveText('Showing first 10 files');
  await expect(fileTree(page).locator('.tree-more')).toBeVisible();
  await expect(page.locator('#summary')).toHaveText('10+ files');
});

test('live updates add new files and redraw the open one', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(preview(page)).toContainText('Welcome home.');

  folder.write('README.md', '# Home\n\nWelcome back.\n');
  await expect(preview(page)).toContainText('Welcome back.');

  folder.write('new.txt', 'fresh\n');
  await expect(treeRow(page, 'new.txt')).toBeVisible();
  await expect(page.locator('#summary')).toHaveText('8 files');

  await treeRow(page, 'a.txt').click();
  await expect(fileView(page)).toContainText('line 3');
  folder.write('a.txt', lines(5, 2));
  await expect(fileView(page)).toContainText('edited 3');
  await expect(viewLabel(page)).toHaveText('a.txt');

  folder.rm('new.txt');
  await expect(treeRow(page, 'new.txt')).toHaveCount(0);
});

test('notes show on their file, in the toolbar and as they arrive', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.annotate({ file: 'a.txt', line: 2, text: 'look at line 2' });
  await s.open(page);
  await expect(page.locator('#notes-next')).toHaveText(/^1 note/);
  await expect(treeRow(page, 'a.txt').locator('.note-count')).toHaveText('1 note');

  await treeRow(page, 'a.txt').click();
  const row2 = fileView(page).locator('tr[data-n="2"]');
  await expect(row2.locator('td.num')).toHaveClass(/\bnoted\b/);
  await expect(page.locator('#main tr.note .note-box')).toContainText('look at line 2');

  // One that arrives while the file is open goes on it at once.
  await s.annotate({ file: 'a.txt', line: 4, endLine: 5, text: 'and these' });
  await expect(page.locator('#main tr.note .note-box', { hasText: 'and these' })).toBeVisible();
  await expect(page.locator('#notes-next')).toHaveText(/^2 notes/);

  // A note on a markdown file is listed above its preview.
  await s.annotate({ file: 'README.md', line: 1, text: 'about the title' });
  await focusMain(page);
  await page.keyboard.press('Escape');
  await expect(page.locator('#main .notes-missing')).toContainText('about the title');
  await expect(preview(page)).toContainText('Welcome home.');
});

test('j/k step through every file, Esc goes back to the start, f switches file and preview, and s and 1/2/3 do nothing', async ({ page, context, folder, revue }) => {
  folder.write('README.md', '# Home\n').write('a.txt', 'a\n').write('b.txt', 'b\n').write('docs/c.txt', 'c\n');
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(viewLabel(page)).toHaveText('README.md');
  await focusMain(page);

  // Tree order: folders first, then names: docs/c.txt, a.txt, b.txt, README.md.
  await page.keyboard.press('k');
  await expect(viewLabel(page)).toHaveText('b.txt');
  await page.keyboard.press('k');
  await expect(viewLabel(page)).toHaveText('a.txt');
  await page.keyboard.press('p');
  await expect(viewLabel(page)).toHaveText('docs/c.txt');
  await expect(treeRow(page, 'docs/c.txt')).toBeVisible();
  await page.keyboard.press('j');
  await expect(viewLabel(page)).toHaveText('a.txt');
  await page.keyboard.press('n');
  await expect(viewLabel(page)).toHaveText('b.txt');

  await page.keyboard.press('Escape');
  await expect(viewLabel(page)).toHaveText('README.md');
  await expect(preview(page)).toContainText('Home');
  await page.keyboard.press('f');
  await expect(fileView(page)).toContainText('# Home');
  expect(hashParams(page)).toEqual({ file: 'README.md', view: 'file' });
  await page.keyboard.press('f');
  await expect(preview(page)).toContainText('Home');
  expect(hashParams(page)).toEqual({ file: 'README.md', view: 'preview' });

  // s and 1/2/3 do nothing: no toast, no mode, the layout preference unchanged.
  const layoutCookie = async () => (await context.cookies(s.url)).find(c => c.name === 'revue.layout')?.value;
  const before = await layoutCookie();
  for (const k of ['s', '1', '2', '3']) await page.keyboard.press(k);
  await page.waitForTimeout(300);
  await expect(toastShown(page)).toHaveCount(0);
  expect(hashParams(page)).toEqual({ file: 'README.md', view: 'preview' });
  expect(await layoutCookie()).toBe(before);
  await expect(page.getByRole('listbox', { name: 'Commits' })).toBeHidden();
  // Positive control: the keys still work.
  await page.keyboard.press('k');
  await expect(viewLabel(page)).toHaveText('b.txt');
});

test("Ctrl+click goes to a definition among the folder's files", async ({ page, folder, revue }) => {
  folder.write('lib/util.js', 'export function hello() {}\n');
  folder.write('other.js', 'export function hello() {}\n');
  folder.write('a.js', "import { hello } from './lib/util.js';\nhello();\nnothingHere();\n");
  const s = await revue.start({ folder });
  await s.open(page, 'file=a.js');
  const cell = fileView(page).locator('tr[data-n="2"] td.src');
  await expect(cell).toContainText('hello');
  // The import picks lib/util.js over the other hello.
  await modClickWord(page, cell, 'hello');
  await expect(page).toHaveURL(/#file=lib%2Futil\.js&view=file&line=1$/);
  await expect(viewLabel(page)).toHaveText('lib/util.js');
  await expect(targetRows(page)).toContainText('export function hello()');
  await page.goBack();
  await expect(viewLabel(page)).toHaveText('a.js');
  // A name defined nowhere says so, as in a repo.
  const none = fileView(page).locator('tr[data-n="3"] td.src');
  await expect(none).toContainText('nothingHere');
  await modClickWord(page, none, 'nothingHere');
  await expect(toastShown(page)).toContainText('No definition found for nothingHere');
  await expect(viewLabel(page)).toHaveText('a.js');
});

test('in narrow windows the drawer holds the folder tag and the tree', async ({ page, folder, revue }) => {
  docs(folder);
  await page.setViewportSize({ width: 600, height: 800 });
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(viewLabel(page)).toHaveText('README.md');
  await expect(page.locator('#mode-select')).toBeHidden();

  const drawerBtn = page.getByRole('button', { name: 'Files and commits' });
  await drawerBtn.click();
  await expect(page.locator('#drawer-head #folder-tag')).toBeVisible();
  await expect(page.getByRole('combobox', { name: 'Branch to view' })).toBeHidden();
  await expect(page.getByRole('listbox', { name: 'Commits' })).toBeHidden();

  // A folder opened in the drawer shows its README behind it and leaves the drawer open, to go further in.
  await folderRow(page, 'guide').click();
  await expect(viewLabel(page)).toHaveText('guide/README.md');
  await expect(drawerBtn).toHaveAttribute('aria-expanded', 'true');
  // A file picked closes it.
  await treeRow(page, 'guide/setup.txt').click();
  await expect(drawerBtn).toHaveAttribute('aria-expanded', 'false');
  await expect(fileView(page)).toContainText('step 2');
});

test('an old link to a commit or a branch comparison opens the file it names', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.open(page, 'mode=commit&ref=abc1234&file=a.txt&side=old&line=3');
  await expect(viewLabel(page)).toHaveText('a.txt');
  await expect(fileView(page).locator('tr.target')).toHaveText(/line 3/);
  expect(hashParams(page)).toEqual({ file: 'a.txt', view: 'file', line: '3' });
  await expect(page.getByRole('navigation', { name: 'Mode' })).toBeHidden();

  await s.open(page, 'mode=branch&base=main&head=feature&file=guide/README.md');
  await expect(viewLabel(page)).toHaveText('guide/README.md');
  await expect(preview(page)).toContainText('Read me first.');
  expect(hashParams(page)).toEqual({ file: 'guide/README.md', view: 'preview' });
});

test('an agent can show a file, and Back returns', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  await s.open(page);
  await expect(viewLabel(page)).toHaveText('README.md');
  await s.navigate({ file: 'a.txt', line: 2 });
  await expect(viewLabel(page)).toHaveText('a.txt');
  await expect(fileView(page).locator('tr.target')).toHaveText(/line 2/);
  await page.goBack();
  await expect(viewLabel(page)).toHaveText('README.md');
  await expect(preview(page)).toContainText('Welcome home.');
});

test('never asks for a diff or the log', async ({ page, folder, revue }) => {
  docs(folder);
  const s = await revue.start({ folder });
  const gitRequests = recordRequests(page, /^\/api\/(diff|diffs|log)$/);
  await s.open(page);
  await expect(preview(page)).toContainText('Welcome home.');
  await folderRow(page, 'guide').click();
  await treeRow(page, 'guide/setup.txt').click();
  await expect(fileView(page)).toContainText('step 1');
  await focusMain(page);
  for (const k of ['j', 'k', 'f', 's', '1', '2', '3', 'Escape', 'r']) await page.keyboard.press(k);
  folder.write('a.txt', lines(5, 0));
  await treeRow(page, 'a.txt').click();
  await expect(fileView(page)).toContainText('edited 1');
  // The status card and the empty state's watching line ask /api/server, not the log.
  await page.locator('#live').hover();
  await expect(page.locator('#status-card')).toContainText('in memory');
  expect(gitRequests).toEqual([]);
});
