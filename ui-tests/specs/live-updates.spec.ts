// Live updates: what the page redraws when files or commits change on disk while it is open. An edit reloads only
// the edited file's overview section (the others keep their diff, open/closed state and the scroll position), new
// files take their place among the sections and removed ones go, an open diff whose change goes away turns into the
// view an unchanged file opens in, a burst of new files opens a bounded number of folders, a commit from the shell
// moves the view and the empty state's latest commit, and History ignores working-tree edits.
import * as fs from 'node:fs';
import * as path from 'node:path';
import {
  test, expect, lines, treeRow, folderRow, section, viewLabel, modeTab, commitList, recordRequests, removedLines, sectionPaths,
  recordEvents, addedLines, hashParams,
} from '../fixtures';
import type { Page } from '@playwright/test';


const files = Array.from({ length: 12 }, (_, i) => `f${String(i + 1).padStart(2, '0')}.txt`);

test('editing a file reloads only its overview section', async ({ page, repo, revue }) => {
  for (const f of files) repo.write(f, lines(10));
  repo.commit('initial');
  for (const f of files) repo.write(f, lines(10, 4));
  const s = await revue.start();
  await s.open(page);

  // Load every section: scroll to the end, where the last ones load, then back to the middle.
  const main = page.locator('#main');
  await main.evaluate(m => { m.scrollTop = m.scrollHeight; });
  await expect(page.locator('#main .section table')).toHaveCount(files.length);
  await main.evaluate(m => { m.scrollTop = 400; });
  const target = 'f05.txt';
  await expect(section(page, target)).toBeInViewport();

  // Close another section, and tag every diff table with a property the app doesn't know about: a table that is
  // drawn again loses it.
  await section(page, 'f04.txt').getByRole('button', { name: '▾' }).click();
  await expect(section(page, 'f04.txt').locator('.diff-body')).toBeHidden();
  await page.locator('#main .section table').evaluateAll(ts => ts.forEach(t => { (t as any)._tagged = true; }));
  const scrollTop = await main.evaluate(m => m.scrollTop);
  const diffRequests = recordRequests(page, /^\/api\/diffs?$/);

  // The same line counts, so only the file's version tag moves and its section's height stays.
  repo.write(target, lines(10, 4).replace('edited 5', 'rewritten 5'));
  await expect(section(page, target).getByRole('table')).toContainText('rewritten 5');

  const tagged = await page.locator('#main .section').evaluateAll(secs =>
    secs.map(s => [s.getAttribute('data-path'), !!(s.querySelector('table') as any)?._tagged]));
  expect(tagged).toEqual(files.map(f => [f, f !== target]));
  expect(diffRequests.flatMap(u => new URL(u).searchParams.getAll('path'))).toEqual([target]);
  await expect(section(page, 'f04.txt').locator('.diff-body')).toBeHidden();
  await expect(section(page, 'f04.txt').getByRole('button', { name: '▸' })).toBeVisible();
  expect(await main.evaluate(m => m.scrollTop)).toBe(scrollTop);
});

test('a new file appears in its place and a removed file disappears', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(5)).write('z.txt', lines(3)).commit('initial');
  repo.write('a.txt', lines(5, 1)).write('z.txt', lines(3, 0));
  const s = await revue.start();
  await s.open(page);
  await expect(viewLabel(page)).toHaveText('All changes · 2 files');
  await expect.poll(() => sectionPaths(page)).toEqual(['a.txt', 'z.txt']);

  // new.txt sorts between the two, so a section added at the end would be out of place.
  repo.write('new.txt', 'fresh\n');
  await expect(treeRow(page, 'new.txt').locator('.badge')).toHaveText('U');
  await expect(section(page, 'new.txt').getByRole('table')).toContainText('fresh');
  await expect(viewLabel(page)).toHaveText('All changes · 3 files');
  await expect.poll(() => sectionPaths(page)).toEqual(['a.txt', 'new.txt', 'z.txt']);

  repo.rm('new.txt');
  await expect(treeRow(page, 'new.txt')).toHaveCount(0);
  await expect(section(page, 'new.txt')).toHaveCount(0);
  await expect(viewLabel(page)).toHaveText('All changes · 2 files');
  await expect.poll(() => sectionPaths(page)).toEqual(['a.txt', 'z.txt']);

  // A tracked file deleted on disk stays listed, as a deletion: its section's letter and diff change with it.
  repo.rm('a.txt');
  await expect(treeRow(page, 'a.txt').locator('.badge')).toHaveText('D');
  const a = section(page, 'a.txt');
  await expect(a.locator('.file-head .letter')).toHaveText('D');
  await expect(removedLines(a)).toHaveText(['line 1', 'line 2', 'line 3', 'line 4', 'line 5']);
  await expect(a.getByRole('table')).not.toContainText('edited 2');
});

// An open diff whose change a live update takes away (the edit is reverted) has nothing left to show. The file goes to
// the view an unchanged file opens in, as picking it would: the file view, or for markdown its Preview. The hash says
// so without a new history entry, and the diff settings go with the diff. When the change comes back, the file stays
// in the view it is in.
const fileTab = (page: Page, name: 'Diff' | 'File' | 'Preview') =>
  page.locator('#main .file-head').getByRole('button', { name, exact: true });
async function expectDiffControls(page: Page, visible: boolean) {
  for (const control of [page.locator('#context-ctl'), page.locator('#layout-seg')]) await expect(control).toBeVisible({ visible });
}

test('an open diff whose change is reverted turns into the file view, and stays there when it comes back', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(20)).commit('initial');
  repo.write('a.txt', lines(20, 9));
  const s = await revue.start();
  await s.open(page, 'file=a.txt');
  await expect(addedLines(page.locator('#main'))).toHaveText(['edited 10']);
  await expect(fileTab(page, 'Diff')).toHaveClass(/\bon\b/);
  await expectDiffControls(page, true);
  const entries = await page.evaluate(() => history.length);

  repo.write('a.txt', lines(20));
  await expect(fileTab(page, 'File')).toHaveClass(/\bon\b/);
  await expect(fileTab(page, 'Diff')).not.toHaveClass(/\bon\b/);
  await expect(fileTab(page, 'Diff')).toBeDisabled();
  await expect(page.locator('#main table.fileview tr[data-n]')).toHaveCount(20);
  await expect(page.locator('#main table.fileview #L10')).toContainText('line 10');
  await expect(page.locator('#main table.fileview tr.changed')).toHaveCount(0);
  expect(hashParams(page)).toEqual({ mode: 'working', file: 'a.txt', view: 'file' });
  expect(await page.evaluate(() => history.length)).toBe(entries);
  await expectDiffControls(page, false);

  // The change comes back: its line is marked in the file view, and the Diff tab is there to pick.
  repo.write('a.txt', lines(20, 9));
  await expect(page.locator('#main table.fileview tr.changed td.src')).toHaveText(['edited 10']);
  await expect(fileTab(page, 'Diff')).toBeEnabled();
  await expect(fileTab(page, 'File')).toHaveClass(/\bon\b/);
  expect(hashParams(page)).toEqual({ mode: 'working', file: 'a.txt', view: 'file' });
  await expectDiffControls(page, false);
  await fileTab(page, 'Diff').click();
  await expect(addedLines(page.locator('#main'))).toHaveText(['edited 10']);
  await expectDiffControls(page, true);
});

test('an open markdown diff whose change is reverted turns into its Preview', async ({ page, repo, revue }) => {
  repo.write('doc.md', '# Title\n\nSome text.\n').commit('initial');
  repo.write('doc.md', '# Title\n\nSome other text.\n');
  const s = await revue.start();
  await s.open(page, 'file=doc.md');
  await expect(addedLines(page.locator('#main'))).toHaveText(['Some other text.']);
  await expect(fileTab(page, 'Diff')).toHaveClass(/\bon\b/);
  await expectDiffControls(page, true);
  const entries = await page.evaluate(() => history.length);

  repo.write('doc.md', '# Title\n\nSome text.\n');
  await expect(fileTab(page, 'Preview')).toHaveClass(/\bon\b/);
  await expect(fileTab(page, 'Diff')).toBeDisabled();
  const doc = page.locator('#main .markdown');
  await expect(doc.getByRole('heading', { name: 'Title' })).toBeVisible();
  await expect(doc.getByText('Some text.')).toBeVisible();
  expect(hashParams(page)).toEqual({ mode: 'working', file: 'doc.md', view: 'preview' });
  expect(await page.evaluate(() => history.length)).toBe(entries);
  await expectDiffControls(page, false);

  // The change comes back: the Preview stays, and shows it.
  repo.write('doc.md', '# Title\n\nSome other text.\n');
  await expect(fileTab(page, 'Diff')).toBeEnabled();
  await expect(doc.getByText('Some other text.')).toBeVisible();
  await expect(fileTab(page, 'Preview')).toHaveClass(/\bon\b/);
  expect(hashParams(page)).toEqual({ mode: 'working', file: 'doc.md', view: 'preview' });
});

// New changes open their folders, but at most 200 of them (AUTO_EXPAND) per refresh, so a burst of new files (an
// unignored node_modules) doesn't unfold the whole tree.
test('a burst of new files in new folders opens at most 200 of the folders', async ({ page, repo, revue }) => {
  repo.write('README', 'readme\n').commit('initial');
  const s = await revue.start();
  await s.open(page);
  await expect(page.getByRole('heading', { name: 'No uncommitted changes on main' })).toBeVisible();

  // 201 new files, one per folder. Each refresh has its own budget, so they must all arrive in one: they are written
  // outside the repo (on the same file system) and moved in with a single rename.
  const staging = fs.mkdtempSync(path.join(path.dirname(repo.dir), 'revue-ui-staging-'));
  try {
    for (let i = 1; i <= 201; i++) {
      const dir = path.join(staging, 'pkg', `d${String(i).padStart(3, '0')}`);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'new.txt'), `new ${i}\n`);
    }
    fs.renameSync(path.join(staging, 'pkg'), repo.abs('pkg'));
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }

  const folders = page.locator('#tree [role="treeitem"][data-dir^="pkg/"]');
  await expect(folders).toHaveCount(201);
  await expect(folderRow(page, 'pkg')).toHaveAttribute('aria-expanded', 'true');
  await expect(folders.and(page.locator('[aria-expanded="true"]'))).toHaveCount(200);
  await expect(folders.and(page.locator('[aria-expanded="false"]'))).toHaveCount(1);
  await expect(page.locator('#tree [role="treeitem"][data-path^="pkg/"]')).toHaveCount(200);
});

test('a commit made from the shell moves the view', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(5)).commit('initial');
  repo.write('a.txt', lines(5, 1));
  const s = await revue.start();
  await s.open(page);
  await expect(section(page, 'a.txt').getByRole('table')).toContainText('edited 2');

  const sha = repo.commit('Edit line two');
  // The change is committed: Changes is empty and points at the new commit.
  await expect(page.getByRole('heading', { name: 'No uncommitted changes on main' })).toBeVisible();
  await expect(section(page, 'a.txt')).toHaveCount(0);
  await expect(treeRow(page, 'a.txt').locator('.badge')).toHaveText('');
  const latest = page.locator('#main .latest');
  await expect(latest).toContainText('Edit line two');
  await expect(latest).toContainText(sha.slice(0, 7));

  // Its link opens the commit in History, with the change.
  await latest.getByRole('button', { name: /Open in History/ }).click();
  await expect(commitList(page).getByRole('option', { selected: true })).toHaveAttribute('data-sha', sha);
  await expect(section(page, 'a.txt').getByRole('table')).toContainText('edited 2');
});

// The empty state keeps the latest commit it fetched until the next `git` live update, so each commit from the shell
// replaces it. The commits are empty: the tree the page gets stays exactly the same, and only the `git` message says
// that the latest commit moved.
test('the empty state\'s latest commit follows each new commit', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(5));
  const first = repo.commit('initial');
  const s = await revue.start();
  await s.open(page);
  await expect(page.getByRole('heading', { name: 'No uncommitted changes on main' })).toBeVisible();
  const latest = page.locator('#main .latest');
  // The subject line is the short sha and the subject; its title is the whole subject.
  await expect(latest.locator('.subj')).toHaveAttribute('title', 'initial');
  await expect(latest.locator('code')).toHaveText(first.slice(0, 7));

  for (const msg of ['Second from the shell', 'Third from the shell']) {
    const sha = repo.commit(msg);
    await expect(latest.locator('.subj')).toHaveAttribute('title', msg);
    await expect(latest.locator('code')).toHaveText(sha.slice(0, 7));
  }
});

test('History ignores working-tree edits but shows a new commit', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(5)).commit('first');
  repo.write('a.txt', lines(5, 0)).commit('second');
  const s = await revue.start();
  await s.open(page);
  await modeTab(page, 'History').click();
  const commits = commitList(page).getByRole('option');
  const table = section(page, 'a.txt').getByRole('table');
  await expect(commits).toHaveCount(2);
  await expect(commits.first()).toContainText('second');
  await expect(table).toContainText('edited 1');

  // Once the second stream has the edits' `worktree` message, the page's own stream has had it too.
  const messages = await recordEvents(page);
  // Tag what a redraw would replace, and record every API request from here on but the event streams.
  await commits.evaluateAll(els => els.forEach(e => { (e as any)._tagged = true; }));
  await table.evaluate(t => { (t as any)._tagged = true; });
  const requests = recordRequests(page, /^\/api\/(?!events$)/);

  repo.write('a.txt', lines(5, 3));
  repo.write('b.txt', 'untracked\n');
  await expect.poll(messages).toContain('worktree');
  // Nothing should happen: give the page's own delay (150 ms) and a refetch time to have acted if they were going to.
  await page.waitForTimeout(1000);
  expect(await messages(), 'only working-tree changes were reported').not.toContain('git');
  expect(requests).toEqual([]);
  expect(await commits.evaluateAll(els => els.map(e => !!(e as any)._tagged))).toEqual([true, true]);
  expect(await table.evaluate(t => !!(t as any)._tagged)).toBe(true);

  // Positive control: a commit does reach History.
  repo.commit('third');
  await expect(commits).toHaveCount(3);
  await expect(commits.first()).toContainText('third');
  expect(requests.map(u => new URL(u).pathname)).toContain('/api/log');
});
