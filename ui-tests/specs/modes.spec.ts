// The three modes (Changes, History, Branch): switching with the keys 1, 2, 3 and the tabs, the commit list
// (picking commits, "Load more", "all branches"), Branch mode's merge-base comparison, default base (or none) and
// indifference to working-tree edits, what a branch or commit that doesn't exist falls back to, the header's branch
// picker (view only), a detached HEAD, a repo with no commits yet, and "Check out" (git switch, refused with
// uncommitted tracked changes).
import {
  test, expect, lines, modeTab, overviewTitle, commitList, treeRow, section, viewLabel, hashParams, recordRequests,
  cleanEnv, sectionPaths, addedLines, removedLines, type Repo,
  recordEvents, focusMain, hold,
} from '../fixtures';
import type { Locator, Page, Request } from '@playwright/test';
import { execFileSync } from 'node:child_process';

// main: "base" (a.txt), then "main only" (b.txt); feature, from "base": "feature work" (c.txt). Checked out: `at`.
function forkedRepo(repo: Repo, at: 'main' | 'feature') {
  repo.write('a.txt', lines(5));
  repo.commit('base');
  repo.branch('feature');
  repo.write('b.txt', 'only on main\n');
  repo.commit('main only');
  repo.switch('feature');
  repo.write('c.txt', 'feature line\n');
  repo.commit('feature work');
  if (at === 'main') repo.switch('main');
}


// forkedRepo with feature checked out, plus a branch `older` at "feature work" and one more commit on feature, "more
// feature work" (d.txt). The two bases give different diffs: feature vs main is c.txt and d.txt, feature vs older only
// d.txt.
function twoBasesRepo(repo: Repo) {
  forkedRepo(repo, 'feature');
  repo.branch('older');
  repo.write('d.txt', 'more feature\n');
  repo.commit('more feature work');
}



test.describe('switching modes', () => {
  // forkedRepo with feature checked out, and a.txt edited, so each mode has one file to show.
  type Mode = 'Changes' | 'History' | 'Branch';
  function modesRepo(repo: Repo) {
    forkedRepo(repo, 'feature');
    repo.write('a.txt', lines(5, 2)); // an uncommitted change
    const tip = repo.rev('feature');
    const modes: Record<Mode, { title: string; path: string; hash: Record<string, string> }> = {
      Changes: { title: 'Uncommitted changes', path: 'a.txt', hash: { mode: 'working' } },
      // History opens at the branch's latest commit, and the URL names it by its full sha.
      History: { title: 'feature work', path: 'c.txt', hash: { mode: 'commit', ref: tip } },
      Branch: { title: 'feature vs main', path: 'c.txt', hash: { mode: 'branch', base: 'main' } },
    };
    // The whole page against the mode it should be in: which tab is current (in a narrow window, the mode select's
    // value), which of the commit list and "Compared with" is shown (a narrow window has both in the closed drawer), the
    // title, the diff and the URL.
    return async (page: Page, mode: Mode, narrow = false) => {
      if (narrow) await expect(page.getByRole('combobox', { name: 'Mode' })).toHaveValue(modes[mode].hash.mode);
      else for (const m of Object.keys(modes) as Mode[]) {
        await expect(modeTab(page, m)).toHaveAttribute('aria-current', m === mode ? 'page' : 'false');
      }
      await expect(commitList(page)).toBeVisible({ visible: mode === 'History' && !narrow });
      await expect(page.getByLabel('Compared with')).toBeVisible({ visible: mode === 'Branch' && !narrow });
      if (mode === 'Branch') await expect(page.getByLabel('Compared with')).toHaveValue('main');
      await expect(overviewTitle(page)).toHaveText(modes[mode].title);
      await expect(page.locator('#main .section')).toHaveCount(1);
      await expect(section(page, modes[mode].path)).toHaveCount(1);
      await expect.poll(() => hashParams(page)).toEqual(modes[mode].hash);
    };
  }

  // Each step switches with a key or a mode tab, and then the whole page is checked against the mode it should be in.
  test('the keys 1, 2 and 3 and the mode tabs switch between Changes, History and Branch', async ({ page, repo, revue }) => {
    const expectMode = modesRepo(repo);
    const s = await revue.start();
    await s.open(page);

    const steps: [string, Mode, () => Promise<void>][] = [
      ['as the page opens', 'Changes', async () => {}],
      ['the key 2', 'History', () => page.keyboard.press('2')],
      ['the key 3', 'Branch', () => page.keyboard.press('3')],
      ['the key 1', 'Changes', () => page.keyboard.press('1')],
      ['the History tab', 'History', () => modeTab(page, 'History').click()],
      ['the Branch tab', 'Branch', () => modeTab(page, 'Branch').click()],
      ['the Changes tab', 'Changes', () => modeTab(page, 'Changes').click()],
    ];
    for (const [how, mode, go] of steps) {
      await test.step(`${how}: ${mode}`, () => go().then(() => expectMode(page, mode)));
    }
  });

  // The page draws nothing until /api/info is in, but the mode keys, the tabs and a narrow window's mode select work
  // from the start: the mode picked last is drawn once info is in. Branch mode compares with info's default base, and
  // 3 used to throw (setMode read state.info.defaultBase) and leave the page blank. A page opened in Branch mode started
  // its mode only once info was in, so a key pressed before that was overridden by it.
  const early: { how: string; mode: Mode; go: (page: Page) => Promise<unknown>; hash?: string; narrow?: boolean }[] = [
    { how: 'the key 3', mode: 'Branch', go: p => p.keyboard.press('3') },
    { how: 'the key 2', mode: 'History', go: p => p.keyboard.press('2') },
    { how: 'the key 1 in a page opened in History', mode: 'Changes', hash: 'mode=commit', go: p => p.keyboard.press('1') },
    { how: 'the key 1 in a page opened in Branch mode', mode: 'Changes', hash: 'mode=branch', go: p => p.keyboard.press('1') },
    { how: 'the key 3, then 2', mode: 'History', go: async p => { await p.keyboard.press('3'); await p.keyboard.press('2'); } },
    { how: 'the key 2, then 3', mode: 'Branch', go: async p => { await p.keyboard.press('2'); await p.keyboard.press('3'); } },
    { how: 'the Branch tab', mode: 'Branch', go: p => modeTab(p, 'Branch').click() },
    { how: 'the History tab', mode: 'History', go: p => modeTab(p, 'History').click() },
    { how: 'the mode select', mode: 'Branch', narrow: true, go: p => p.getByRole('combobox', { name: 'Mode' }).selectOption('branch') },
  ];
  for (const { how, mode, go, hash, narrow } of early) {
    test(`${how} before /api/info answers: ${mode}`, async ({ page, repo, revue }) => {
      const expectMode = modesRepo(repo);
      const s = await revue.start();
      if (narrow) await page.setViewportSize({ width: 700, height: 800 });
      const info = await hold(page, u => u.pathname === '/api/info');
      await page.goto(s.url + (hash ? `#${hash}` : ''));
      await info.held;
      await go(page);
      await expect(page.locator('#main')).toBeEmpty(); // nothing is drawn yet
      info.release();
      await expectMode(page, mode, narrow);
    });
  }
});


test.describe('History', () => {
  test('clicking a commit shows its diff', async ({ page, repo, revue }) => {
    repo.write('one.txt', 'first file\n');
    repo.commit('add one');
    repo.write('two.txt', 'second file\n');
    repo.commit('add two');
    repo.write('one.txt', 'first file, edited\n');
    repo.commit('edit one');
    const s = await revue.start();
    await s.open(page, 'mode=commit');

    const commits = commitList(page).getByRole('option');
    const selected = commitList(page).locator('[aria-selected=true]');
    const fileRows = page.locator('#tree [role="treeitem"][data-path]');
    await expect(commits).toHaveCount(3);
    // Each commit, newest first (the one History opens at): its diff, and the tree at that commit, with each file's
    // badge ('' for a file the commit didn't change). two.txt doesn't exist yet at "add one".
    const expected = [
      { subject: 'edit one', path: 'one.txt', text: 'first file, edited', tree: { 'one.txt': 'M', 'two.txt': '' } },
      { subject: 'add two', path: 'two.txt', text: 'second file', tree: { 'one.txt': '', 'two.txt': 'A' } },
      { subject: 'add one', path: 'one.txt', text: 'first file', tree: { 'one.txt': 'A' } },
    ];
    for (const [i, { subject, path, text, tree }] of expected.entries()) {
      await test.step(subject, async () => {
        const row = commitList(page).getByRole('option', { name: new RegExp(`^${subject}`) });
        if (i > 0) await row.click();
        await expect(row).toHaveAttribute('aria-selected', 'true');
        await expect(selected).toHaveCount(1);
        await expect(overviewTitle(page)).toHaveText(subject);
        await expect(page.locator('#main .section')).toHaveCount(1);
        await expect(section(page, path).locator('table.code')).toContainText(text);
        await expect(fileRows).toHaveCount(Object.keys(tree).length);
        for (const [file, badge] of Object.entries(tree)) {
          await expect(treeRow(page, file).locator('.badge')).toHaveText(badge);
        }
      });
    }
    await expect(page.locator('#main .section table.code')).not.toContainText('edited');
    await expect(treeRow(page, 'two.txt')).toHaveCount(0);
  });

  test('"Load more" appends the next page of commits', async ({ page, repo, revue }) => {
    // The log loads 200 commits at a time (loadCommits in app.js). One git fast-import makes the 205 commits, rather
    // than two git runs each, with the same increasing fixed dates Repo.commit gives.
    let stream = '';
    for (let i = 1; i <= 205; i++) {
      const who = `Test <test@example.com> ${1_700_000_000 + i * 60} +0000`;
      const msg = `commit ${i}\n`;
      stream += `commit refs/heads/main\nauthor ${who}\ncommitter ${who}\ndata ${msg.length}\n${msg}`;
      if (i === 1) stream += 'M 100644 inline a.txt\ndata 2\na\n';
      stream += '\n';
    }
    // Repo.git's environment, run directly because Repo.git gives git no stdin.
    execFileSync('git', ['fast-import', '--quiet'], { cwd: repo.dir, input: stream, env: cleanEnv(), stdio: 'pipe' });
    repo.git('reset', '-q', '--hard'); // the index and work tree, which fast-import leaves alone
    const s = await revue.start();
    await s.open(page, 'mode=commit');

    const commits = commitList(page).locator('li[data-sha]');
    const subject = (row: Locator) => row.locator('.subj');
    const more = commitList(page).getByRole('option', { name: 'Load more…' });
    await expect(commits).toHaveCount(200);
    await expect(more).toBeVisible();
    await expect(subject(commits.first())).toHaveText('commit 205');
    await expect(subject(commits.last())).toHaveText('commit 6');

    // The rows already shown stay (the list isn't redrawn): tag one and look for the tag afterwards.
    await commits.first().evaluate(el => { (el as any).__kept = true; });
    await more.click();
    await expect(commits).toHaveCount(205);
    await expect(more).toHaveCount(0);
    await expect(subject(commits.last())).toHaveText('commit 1');
    expect(await commits.first().evaluate(el => (el as any).__kept === true)).toBe(true);

    // A commit from the appended page can be picked.
    await commitList(page).getByRole('option', { name: /^commit 1\b/ }).click();
    await expect(overviewTitle(page)).toHaveText('commit 1');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'a.txt');
  });

  test('"all branches" lists the other branches\' commits, and turning it off drops them', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    const subjects = commitList(page).locator('li[data-sha] .subj');
    const all = page.getByRole('checkbox', { name: 'all branches' });
    await expect(all).not.toBeChecked();
    await expect(subjects).toHaveText(['feature work', 'base']);

    // main's own commit comes in, newest first (the fixed dates order the three), and can be picked.
    await all.check();
    await expect(subjects).toHaveText(['feature work', 'main only', 'base']);
    await commitList(page).getByRole('option', { name: /^main only/ }).click();
    await expect(overviewTitle(page)).toHaveText('main only');
    await expect.poll(() => sectionPaths(page)).toEqual(['b.txt']);

    await commitList(page).getByRole('option', { name: /^feature work/ }).click();
    await expect(overviewTitle(page)).toHaveText('feature work');
    await all.uncheck();
    await expect(subjects).toHaveText(['feature work', 'base']);
    await expect(commitList(page).getByRole('option', { name: /^feature work/ })).toHaveAttribute('aria-selected', 'true');
  });

  // r reloads everything a live update would, the commit list included, which matters most when there are no live
  // updates (the status card says to press r then). It used to draw the commit shown again and nothing else. The commit
  // picked stays picked, and the list where it was scrolled to.
  test('r lists a commit made from the shell, keeping the commit picked and the list\'s scroll position', async ({ page, repo, revue }) => {
    for (let i = 1; i <= 60; i++) {
      repo.write('a.txt', `${i}\n`);
      repo.commit(`commit ${i}`);
    }
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await s.debug({ watchError: 'off for this test' });
    const subjects = commitList(page).locator('li[data-sha] .subj');
    await expect(subjects).toHaveCount(60);
    const picked = commitList(page).getByRole('option', { name: /^commit 30\b/ });
    await picked.click();
    await expect(overviewTitle(page)).toHaveText('commit 30');
    const list = page.locator('#commits');
    await list.evaluate(el => { el.scrollTop = 600; });
    const top = await list.evaluate(el => el.scrollTop);
    expect(top).toBeGreaterThan(0);

    repo.write('a.txt', 'new\n');
    repo.commit('made from the shell');
    await focusMain(page);
    await page.keyboard.press('r');
    await expect(subjects.first()).toHaveText('made from the shell');
    await expect(subjects).toHaveCount(61);
    await expect(picked).toHaveAttribute('aria-selected', 'true');
    await expect(commitList(page).locator('[aria-selected=true]')).toHaveCount(1);
    await expect(overviewTitle(page)).toHaveText('commit 30');
    expect(await list.evaluate(el => el.scrollTop)).toBe(top);
  });

  // A link can name the commit by a branch (`ref=side`): it is that branch's latest commit, looked up on its own when
  // it isn't in History's list (here main's). The lookup used to be kept for the name, so once the branch moved the
  // title and the "linked" row still showed the commit it named before, over the new commit's files.
  for (const live of [true, false]) {
    test(`a commit linked by a branch's name follows the branch when it moves (${live ? 'live updates' : 'r with live updates off'})`, async ({ page, repo, revue }) => {
      repo.write('a.txt', lines(3));
      repo.commit('base');
      repo.switch('side', true);
      repo.write('s.txt', 'one\n');
      const one = repo.commit('side one');
      repo.switch('main');
      repo.write('a.txt', lines(3, 1));
      repo.commit('main work');
      const s = await revue.start();
      await s.open(page, 'mode=commit&ref=side');
      const linked = commitList(page).locator('li[data-pinned]');
      await expect(overviewTitle(page)).toHaveText('side one');
      await expect(linked).toHaveAttribute('data-sha', one);
      if (!live) await s.debug({ watchError: 'off for this test' });

      repo.switch('side');
      repo.write('s.txt', 'two\n');
      const two = repo.commit('side two');
      repo.switch('main');
      if (!live) {
        // With the watcher off the server keeps its answers for up to 3 s, but by the commits they list: one r is
        // enough, and the files it shows are those of the commit it names (they used to be the commit before's).
        await focusMain(page);
        await page.keyboard.press('r');
      }
      await expect(overviewTitle(page)).toHaveText('side two');
      await expect(page.locator('#main .overview-head .meta code')).toHaveText(two.slice(0, 7));
      await expect(linked).toHaveAttribute('data-sha', two);
      await expect(linked).toHaveAttribute('aria-selected', 'true');
      await expect(addedLines(section(page, 's.txt'))).toHaveText(['two']);
      await expect(page).toHaveURL(/#mode=commit&ref=side$/);
    });
  }

  // History opening at the latest commit fetches the branch tip's tree, by the branch's name, alongside the commit list,
  // so the tree needn't wait for the list. When the branch didn't move in between, that tree is the one shown.
  test('History opened at the latest commit shows the tree fetched for the branch tip, without asking again', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(3));
    repo.commit('first');
    repo.write('a.txt', lines(3, 1));
    repo.commit('second');
    const s = await revue.start();
    await s.open(page);
    const trees = recordRequests(page, /^\/api\/tree$/);
    await focusMain(page);
    await page.keyboard.press('2');
    await expect(overviewTitle(page)).toHaveText('second');
    await expect.poll(() => sectionPaths(page)).toEqual(['a.txt']);
    await expect(treeRow(page, 'a.txt').locator('.badge')).toHaveText('M');
    const refs = trees.map(u => new URL(u).searchParams).filter(q => q.get('mode') === 'commit').map(q => q.get('ref'));
    expect(refs).toEqual(['HEAD']);
  });

  // History opening at the latest commit fetches the branch tip's tree alongside the commit list. A commit made between
  // the tip's answer and the list's (held here, and answered after the commit) is the list's first, and the tip's tree
  // is the commit before's: it was shown under the new commit's title, and with live updates off nothing corrected it.
  // The tree says which commit it listed. The tip's notes, asked for with it, say which commit they are about too, but
  // can be answered after the commit (held here in the second case): only the tree's own word tells.
  for (const notes of ['before', 'after'] as const) {
    test(`a commit made between the branch tip's tree and the commit list is shown with its own files (tip's notes answered ${notes} it)`, async ({ page, repo, revue }) => {
      repo.write('a.txt', lines(3));
      repo.commit('first');
      repo.write('b.txt', 'b\n');
      const s = await revue.start();
      await s.open(page);
      await expect(section(page, 'b.txt')).toHaveCount(1);
      await s.debug({ watchError: 'off for this test' });
      const atTip = (path: string) => (u: URL) => u.pathname === path && u.searchParams.get('ref') === 'HEAD';
      const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
      const heldNotes = notes === 'after' ? await hold(page, atTip('/api/annotations')) : null;
      const tip = Promise.all([
        page.waitForResponse(r => atTip('/api/tree')(new URL(r.url()))),
        heldNotes ? heldNotes.held : page.waitForResponse(r => atTip('/api/annotations')(new URL(r.url()))),
      ]);
      await focusMain(page);
      await page.keyboard.press('2');
      await list.held;
      await tip;
      repo.commit('add b');
      heldNotes?.release();
      list.release();

      await expect(overviewTitle(page)).toHaveText('add b');
      await expect.poll(() => sectionPaths(page)).toEqual(['b.txt']);
      await expect(commitList(page).getByRole('option', { name: /^add b/ })).toHaveAttribute('aria-selected', 'true');
      await expect(treeRow(page, 'b.txt').locator('.badge')).toHaveText('A');
    });
  }

  // History opening at the latest commit fetches the branch tip's tree alongside the history (setMode), and then the
  // tree by the commit's sha if that failed, but awaits it only once /api/info is in. A tree that fails before then
  // fails the page's start once info is in, and is never an uncaught error first.
  test.describe('with a tree that fails before /api/info answers', () => {
    // /api/tree answers 500 on purpose (page.route), which the guard otherwise fails the test for.
    test.use({ allowServerErrors: true });

    test('the failure is shown once info is in, and nothing is uncaught before', async ({ page, repo, revue }) => {
      repo.write('a.txt', lines(3));
      const sha = repo.commit('first');
      const s = await revue.start();
      let release!: () => void;
      const held = new Promise<void>(r => { release = r; });
      await page.route(u => u.pathname === '/api/info', async route => { await held; await route.continue(); });
      await page.route(u => u.pathname === '/api/tree', route => route.fulfill({
        status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'tree failed' }),
      }));
      const treeFor = (ref: string) => page.waitForResponse(r => {
        const u = new URL(r.url());
        return u.pathname === '/api/tree' && u.searchParams.get('ref') === ref;
      });
      const tip = treeFor('HEAD'), bySha = treeFor(sha);
      await page.goto(s.url + '#mode=commit');
      await Promise.all([tip, bySha]);
      await expect(commitList(page).getByRole('option')).toHaveText([/^first/]);
      // The failure has been through the page meanwhile, with nothing awaiting it yet.
      await page.waitForTimeout(500);
      await expect(page.locator('#main')).toBeEmpty();
      release();
      await expect(page.locator('#main')).toHaveText('Failed to start: tree failed');
    });
  });
});

test.describe('Branch', () => {
  // The default base is the server's (info.defaultBase; tests/range.rs checks the order it picks it in). Each repo also
  // has a branch listed before it, so "the first branch that isn't checked out" would pick another one.
  test('compares the branch with its merge-base with the default base', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    repo.branch('archive', 'main');
    const s = await revue.start();
    await s.open(page, 'mode=branch');

    const compared = page.getByLabel('Compared with');
    await expect(compared.locator('option')).toHaveText(['archive', 'feature', 'main']);
    await expect(compared).toHaveValue('main');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    // Only the branch's own work: main's later b.txt is not shown as deleted, as a diff against main itself would.
    await expect(page.locator('#main .section')).toHaveCount(1);
    await expect(section(page, 'c.txt').locator('table.code')).toContainText('feature line');
    await expect(treeRow(page, 'b.txt')).toHaveCount(0);
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
  });

  test('without a main branch the default base is master', async ({ page, repo, revue }) => {
    repo.write('a.txt', 'a\n');
    repo.commit('base');
    repo.git('branch', '-m', 'main', 'master');
    repo.branch('develop');
    repo.switch('topic', true);
    repo.write('t.txt', 'topic\n');
    repo.commit('topic work');
    const s = await revue.start();
    await s.open(page);

    await page.keyboard.press('3');
    const compared = page.getByLabel('Compared with');
    await expect(compared.locator('option')).toHaveText(['develop', 'master', 'topic']);
    await expect(compared).toHaveValue('master');
    await expect(overviewTitle(page)).toHaveText('topic vs master');
    await expect(page.locator('#main .section')).toHaveCount(1);
    await expect(section(page, 't.txt')).toHaveCount(1);
    await expect(page).toHaveURL(/#mode=branch&base=master$/);
  });

  test('picking another base compares with that one', async ({ page, repo, revue }) => {
    twoBasesRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt', 'd.txt']);

    await page.getByLabel('Compared with').selectOption('older');
    await expect(overviewTitle(page)).toHaveText('feature vs older');
    await expect.poll(() => sectionPaths(page)).toEqual(['d.txt']);
    await expect(treeRow(page, 'c.txt').locator('.badge')).toHaveText('');
    await expect(treeRow(page, 'd.txt').locator('.badge')).toHaveText('A');
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
  });

  test('a base that isn\'t a branch (a tag or a commit) is shown in "Compared with"', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    repo.git('tag', 'v1', 'main~1');
    const sha = repo.rev('main~1');
    const s = await revue.start();
    const compared = page.getByLabel('Compared with');

    // From the URL the page opens with, then from a link (a new hash).
    await s.open(page, 'mode=branch&base=v1');
    await expect(overviewTitle(page)).toHaveText('feature vs v1');
    await expect(compared).toHaveValue('v1');
    await expect(compared.locator('option:checked')).toHaveText('v1');
    await page.evaluate(sha => { location.hash = `mode=branch&base=${sha}`; }, sha);
    await expect(overviewTitle(page)).toHaveText(`feature vs ${sha}`);
    await expect(compared).toHaveValue(sha);

    // The branches are still there to pick.
    await compared.selectOption('main');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
  });

  test('a base that doesn\'t exist falls back to the default base', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    repo.branch('older', 'feature~1');
    const s = await revue.start();
    const toast = page.getByRole('status');
    const compared = page.getByLabel('Compared with');

    // In the URL the page opens with.
    await s.open(page, 'mode=branch&base=nope');
    await expect(toast).toHaveText('Base \'nope\' doesn\'t exist; comparing with main');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'c.txt');
    await expect(compared).toHaveValue('main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);

    // From a link or Back (a new hash), in Branch mode already.
    await page.evaluate(() => { location.hash = 'mode=branch&base=gone'; });
    await expect(toast).toHaveText('Base \'gone\' doesn\'t exist; comparing with main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(compared).toHaveValue('main');
    await expect(overviewTitle(page)).toHaveText('feature vs main');

    // The page goes on working, and a bad base from another mode, with a file, is handled the same way.
    await modeTab(page, 'History').click();
    await expect(overviewTitle(page)).toHaveText('feature work');
    await page.evaluate(() => { location.hash = 'mode=branch&base=lost&file=c.txt'; });
    await expect(toast).toHaveText('Base \'lost\' doesn\'t exist; comparing with main');
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(viewLabel(page)).toHaveText('c.txt');
    await expect(page.locator('#main .file-head').getByRole('button', { name: 'Diff', exact: true })).toHaveClass(/\bon\b/);
    await expect(page).toHaveURL(/#mode=branch&base=main&file=c\.txt$/);
    await expect(compared).toHaveValue('main');

    // A base branch deleted while it is compared with (a live update).
    await compared.selectOption('older');
    await expect(overviewTitle(page)).toHaveText('feature vs older');
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
    repo.git('branch', '-D', 'older');
    await expect(toast).toHaveText('Base \'older\' doesn\'t exist; comparing with main');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(compared).toHaveValue('main');
    await expect(compared.locator('option')).toHaveText(['feature', 'main']);
  });

  test('a base that doesn\'t exist, with no default base to fall back to, asks for one', async ({ page, repo, revue }) => {
    repo.write('a.txt', 'a\n');
    repo.commit('base');
    repo.git('branch', '-m', 'main', 'trunk');
    repo.switch('topic', true);
    repo.write('t.txt', 'topic\n');
    repo.commit('topic work');
    const s = await revue.start();
    await s.open(page, 'mode=branch&base=nope');
    await expect(page.getByRole('status')).toHaveText('Base \'nope\' doesn\'t exist');
    await expect(overviewTitle(page)).toHaveText('Choose a base branch');
    await expect(page.getByLabel('Compared with')).toHaveValue('');
    await expect(page).toHaveURL(/#mode=branch$/);

    await page.getByLabel('Compared with').selectOption('trunk');
    await expect(overviewTitle(page)).toHaveText('topic vs trunk');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 't.txt');
  });

  test('with no default base it asks for one, and a picked base is compared with', async ({ page, repo, revue }) => {
    // No main, master, origin/main, origin/master or develop.
    repo.write('a.txt', 'a\n');
    repo.commit('base');
    repo.git('branch', '-m', 'main', 'trunk');
    repo.switch('topic', true);
    repo.write('t.txt', 'topic\n');
    repo.commit('topic work');
    const s = await revue.start();
    const trees = recordRequests(page, /^\/api\/tree$/);
    await s.open(page);
    await page.keyboard.press('3');

    const compared = page.getByLabel('Compared with');
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(overviewTitle(page)).toHaveText('Choose a base branch');
    await expect(page.locator('#main .overview-head .meta')).toHaveText(/^No default base branch .* was found\./);
    await expect(page.locator('#main .section')).toHaveCount(0);
    await expect(page.locator('#tree')).toHaveText('Choose a base branch');
    await expect(compared).toHaveValue('');
    await expect(compared.locator('option')).toHaveText(['choose a base…', 'topic', 'trunk']);
    await expect(compared.locator('option[disabled]')).toHaveText(['choose a base…']); // a prompt, not a base to pick
    await expect(page).toHaveURL(/#mode=branch$/);
    // Nothing was compared with: the server would have taken HEAD as the base.
    expect(trees.filter(u => new URL(u).searchParams.get('mode') === 'branch')).toEqual([]);

    await compared.selectOption('trunk');
    await expect(overviewTitle(page)).toHaveText('topic vs trunk');
    await expect.poll(() => sectionPaths(page)).toEqual(['t.txt']);
    await expect(treeRow(page, 't.txt').locator('.badge')).toHaveText('A');
    await expect(compared).toHaveValue('trunk');
    await expect(page).toHaveURL(/#mode=branch&base=trunk$/);
  });

  test('picking a branch to view with a file open goes back to the overview', async ({ page, repo, revue }) => {
    // topic, from feature, adds t.txt; feature is checked out.
    forkedRepo(repo, 'feature');
    repo.switch('topic', true);
    repo.write('t.txt', 'topic line\n');
    repo.commit('topic work');
    repo.switch('feature');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await treeRow(page, 'c.txt').click();
    await expect(viewLabel(page)).toHaveText('c.txt');

    // c.txt is in topic's comparison too, so only closing it brings the overview back.
    await page.getByLabel('Branch to view').selectOption('topic');
    await expect(overviewTitle(page)).toHaveText('topic vs main');
    await expect(viewLabel(page)).toHaveText('All changes · 2 files');
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt', 't.txt']);
    await expect(page).toHaveURL(/#mode=branch&base=main&head=topic$/);
  });

  test('ignores working-tree edits but shows a new commit', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    const table = section(page, 'c.txt').getByRole('table');
    await expect(table).toContainText('feature line');

    const messages = await recordEvents(page);
    // Tag what a redraw would replace, and record every API request from here on but the event streams.
    await table.evaluate(t => { (t as any)._tagged = true; });
    const requests = recordRequests(page, /^\/api\/(?!events$)/);

    repo.write('a.txt', lines(5, 3));
    repo.write('new.txt', 'untracked\n');
    await expect.poll(messages).toContain('worktree');
    // Nothing should happen: give the page's own delay (150 ms) and a refetch time to have acted if they were going to.
    await page.waitForTimeout(1000);
    expect(await messages(), 'only working-tree changes were reported').not.toContain('git');
    expect(requests).toEqual([]);
    expect(await table.evaluate(t => !!(t as any)._tagged)).toBe(true);

    // Positive control: a commit on the branch does reach the comparison.
    repo.commit('more feature work');
    await expect.poll(() => sectionPaths(page)).toEqual(['a.txt', 'c.txt', 'new.txt']);
    expect(requests.map(u => new URL(u).pathname)).toContain('/api/tree');
  });

  test('picking another base closes the open file even when a live update overtakes it', async ({ page, repo, revue }) => {
    twoBasesRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await treeRow(page, 'c.txt').click();
    await expect(viewLabel(page)).toHaveText('c.txt');

    // Hold the tree that the base change loads until a live update's newer tree load has been answered, so the live
    // update's refresh is the one that draws.
    const olderTree = (u: URL) => u.pathname === '/api/tree' && u.searchParams.get('base') === 'older';
    const olderTrees: Request[] = [];
    page.on('request', r => { if (olderTree(new URL(r.url()))) olderTrees.push(r); });
    let release!: () => void;
    const held = new Promise<void>(r => { release = r; });
    let holding = true;
    await page.route(olderTree, async route => {
      if (holding) { holding = false; await held; }
      await route.continue();
    });
    await page.getByLabel('Compared with').selectOption('older');
    await expect.poll(() => olderTrees.length).toBe(1);
    repo.branch('other'); // a ref change: a `git` live update, which refreshes the tree
    // (The watcher may report the ref change in more than one message, so there can be more than one.)
    await expect.poll(() => olderTrees.length).toBeGreaterThanOrEqual(2);
    await Promise.all(olderTrees.slice(1).map(r => r.response()));
    release();

    // The overview of the comparison with older: d.txt only (c.txt is in the comparison with main).
    await expect(viewLabel(page)).toHaveText('All changes · 1 file');
    await expect(overviewTitle(page)).toHaveText('feature vs older');
    await expect.poll(() => sectionPaths(page)).toEqual(['d.txt']);
    await expect(page).toHaveURL(/#mode=branch&base=older$/);
  });
});

// A branch to view (head) or a commit (ref) that doesn't exist: an old link, a typo, a branch deleted while shown.
test.describe('a branch or commit that doesn\'t exist', () => {
  const emptyTitle = (page: Page) => page.locator('#main .empty-state h2');

  test('a branch to view that doesn\'t exist falls back to the checked-out branch in Branch mode', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    repo.branch('topic');
    const s = await revue.start();
    const toast = page.getByRole('status');
    const branchName = page.locator('#branch-name');

    // In the URL the page opens with (no base: the default one).
    await s.open(page, 'mode=branch&head=nope');
    await expect(toast).toHaveText('Branch \'nope\' doesn\'t exist; showing feature');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'c.txt');
    await expect(branchName).toHaveText('feature');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    await expect(page.getByLabel('Branch to view')).toHaveValue('HEAD');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);

    // From a link or Back (a new hash), with a base.
    await page.evaluate(() => { location.hash = 'mode=branch&base=main&head=gone'; });
    await expect(toast).toHaveText('Branch \'gone\' doesn\'t exist; showing feature');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(branchName).toHaveText('feature');
    await expect(overviewTitle(page)).toHaveText('feature vs main');

    // A branch an agent showed, deleted while it is shown (a live update).
    await s.navigate({ mode: 'branch', head: 'topic' });
    await expect(overviewTitle(page)).toHaveText('topic vs main');
    await expect(page).toHaveURL(/#mode=branch&base=main&head=topic$/);
    repo.git('branch', '-D', 'topic');
    await expect(toast).toHaveText('Branch \'topic\' doesn\'t exist; showing feature');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(page.getByLabel('Branch to view').locator('option')).toHaveText(['⎇ feature (checked out)', 'main']);
  });

  test('a branch to view that doesn\'t exist falls back to the checked-out branch in History', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    const tip = repo.rev('feature');
    const s = await revue.start();
    const toast = page.getByRole('status');

    await s.open(page, 'mode=commit&head=nope');
    await expect(toast).toHaveText('Branch \'nope\' doesn\'t exist; showing feature');
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect(commitList(page).getByRole('option')).toHaveCount(2);
    await expect(commitList(page).getByRole('option', { name: /feature work/ })).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('#branch-name')).toHaveText('feature');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${tip}$`));

    // From a link or Back, from another mode.
    await modeTab(page, 'Changes').click();
    await expect(emptyTitle(page)).toHaveText('No uncommitted changes on feature');
    await page.evaluate(() => { location.hash = 'mode=commit&head=gone'; });
    await expect(toast).toHaveText('Branch \'gone\' doesn\'t exist; showing feature');
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${tip}$`));
  });

  test('a commit that doesn\'t exist falls back to the latest commit', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    repo.branch('topic', 'main');
    const tip = repo.rev('feature'), mainTip = repo.rev('main');
    const s = await revue.start();
    const toast = page.getByRole('status');
    const selected = commitList(page).locator('[aria-selected=true]');

    // In the URL the page opens with.
    await s.open(page, 'mode=commit&ref=nope');
    await expect(toast).toHaveText('Commit \'nope\' doesn\'t exist; showing the latest commit');
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'c.txt');
    await expect(selected).toHaveCount(1);
    await expect(selected).toContainText('feature work');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${tip}$`));

    // From a link or Back, in History already, with a file.
    await commitList(page).getByRole('option', { name: /^base/ }).click();
    await expect(overviewTitle(page)).toHaveText('base');
    await page.evaluate(() => { location.hash = 'mode=commit&ref=gone&file=c.txt'; });
    await expect(toast).toHaveText('Commit \'gone\' doesn\'t exist; showing the latest commit');
    await expect(viewLabel(page)).toHaveText('c.txt');
    await expect(page.locator('#main table.code')).toContainText('feature line');
    await expect(selected).toContainText('feature work');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${tip}&file=c\\.txt$`));

    // The latest commit of the branch shown, from another mode.
    await modeTab(page, 'Changes').click();
    await expect(emptyTitle(page)).toHaveText('No uncommitted changes on feature');
    await page.evaluate(() => { location.hash = 'mode=commit&ref=lost&head=main'; });
    await expect(toast).toHaveText('Commit \'lost\' doesn\'t exist; showing the latest commit');
    await expect(overviewTitle(page)).toHaveText('main only');
    await expect(selected).toContainText('main only');
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${mainTip}&head=main$`));

    // A commit named by a branch that is deleted while it is shown (a live update).
    await page.evaluate(() => { location.hash = 'mode=commit&ref=topic'; });
    await expect(overviewTitle(page)).toHaveText('main only');
    await expect(page).toHaveURL(/#mode=commit&ref=topic$/);
    repo.git('branch', '-D', 'topic');
    await expect(toast).toHaveText('Commit \'topic\' doesn\'t exist; showing the latest commit');
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect(selected).toContainText('feature work');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${tip}$`));
  });

  test('the fallback leaves a view that moved on alone, and other errors are still reported', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'feature');
    const first = repo.rev('feature~1');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('feature work');

    // The answers for a commit that doesn't exist come only once another commit was picked.
    const forNope = (u: URL) => ['/api/tree', '/api/annotations'].includes(u.pathname) && u.searchParams.get('ref') === 'nope';
    let release!: () => void;
    const held = new Promise<void>(r => { release = r; });
    await page.route(forNope, async route => { await held; await route.continue(); });
    const answered = page.waitForResponse(r => forNope(new URL(r.url())));
    await page.evaluate(() => { location.hash = 'mode=commit&ref=nope'; });
    await commitList(page).getByRole('option', { name: /^base/ }).click();
    await expect(overviewTitle(page)).toHaveText('base');
    release();
    await answered;
    await page.waitForTimeout(500);
    await expect(page.locator('#toast')).toHaveCount(0);
    await expect(overviewTitle(page)).toHaveText('base');
    await expect(page).toHaveURL(new RegExp(`#mode=commit&ref=${first}$`));

    // An error that isn't about the commit asked for is shown as it is, and the commit is kept.
    await page.route(u => u.pathname === '/api/tree' && u.searchParams.get('ref') === 'main',
      route => route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: 'fatal: Not a valid object name other' }) }));
    await page.evaluate(() => { location.hash = 'mode=commit&ref=main'; });
    await expect(page.getByRole('status')).toHaveText('fatal: Not a valid object name other');
    await expect(page).toHaveURL(/#mode=commit&ref=main$/);

    // Positive control: a commit that doesn't exist, with nothing held, still falls back.
    await page.evaluate(() => { location.hash = 'mode=commit&ref=gone'; });
    await expect(page.getByRole('status')).toHaveText('Commit \'gone\' doesn\'t exist; showing the latest commit');
    await expect(overviewTitle(page)).toHaveText('feature work');
  });
});

// A view whose tree fails to load is drawn as its error. Asked for from the page's own controls, its error also went
// unhandled (an uncaught error in the console, which the guard fails a test for); it is toasted now, as Back's is.
test.describe('a view asked for from the page whose tree fails to load', () => {
  // /api/tree answers 500 on purpose (page.route), which the guard otherwise fails the test for.
  test.use({ allowServerErrors: true });
  const failTrees = (page: Page, match: (u: URL) => boolean) => page.route(u => u.pathname === '/api/tree' && match(u),
    route => route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'tree failed' }) }));
  async function expectFailed(page: Page) {
    await expect(page.locator('#main')).toHaveText('Error: tree failed');
    await expect(page.getByRole('status')).toHaveText('tree failed');
  }

  test('by a mode key', async ({ page, repo, revue }) => {
    twoBasesRepo(repo);
    const s = await revue.start();
    await s.open(page);
    await failTrees(page, u => u.searchParams.get('mode') === 'commit');
    await focusMain(page);
    await page.keyboard.press('2');
    await expectFailed(page);
    // The mode tabs say which mode the page is in, as for a view drawn.
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(modeTab(page, 'Changes')).toHaveAttribute('aria-current', 'false');
  });

  test('by a commit picked', async ({ page, repo, revue }) => {
    twoBasesRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(overviewTitle(page)).toHaveText('more feature work');
    await failTrees(page, u => u.searchParams.get('ref') === repo.rev('older'));
    await commitList(page).getByRole('option', { name: /^feature work/ }).click();
    await expectFailed(page);
    await expect(commitList(page).getByRole('option', { name: /^feature work/ })).toHaveAttribute('aria-selected', 'true');
  });

  test('by a base picked', async ({ page, repo, revue }) => {
    twoBasesRepo(repo);
    const s = await revue.start();
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await failTrees(page, u => u.searchParams.get('base') === 'older');
    await page.getByLabel('Compared with').selectOption('older');
    await expectFailed(page);
  });
});

test.describe('the branch picker', () => {
  test('shows another branch without checking it out', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'main');
    const s = await revue.start();
    await s.open(page);
    const picker = page.getByLabel('Branch to view');
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    await expect(page.getByRole('button', { name: 'Check out' })).toBeHidden();

    // From Changes (which only exists for the checked-out branch), another branch opens in Branch mode.
    await picker.selectOption('feature');
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(page.locator('#branch-name')).toHaveText('feature');
    await expect(page.locator('#branch-note')).toHaveText('view only');
    await expect(page.getByRole('button', { name: 'Check out' })).toBeVisible();
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page.locator('#main .section')).toHaveAttribute('data-path', 'c.txt');
    await expect(page).toHaveURL(/head=feature/);

    // History follows the picked branch: its commits, and the diff of its latest commit (not main's).
    await page.keyboard.press('2');
    await expect(overviewTitle(page)).toHaveText('feature work');
    await expect(commitList(page).getByRole('option')).toHaveCount(2);
    await expect(commitList(page).getByRole('option', { name: /main only/ })).toHaveCount(0);
    await expect.poll(() => sectionPaths(page)).toEqual(['c.txt']);

    // Changes goes back to the checked-out branch.
    await page.keyboard.press('1');
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    await expect(picker).toHaveValue('HEAD');

    expect(repo.currentBranch()).toBe('main');
    expect(repo.exists('c.txt')).toBe(false);
  });
});

test.describe('a detached HEAD', () => {
  // main: "base", then "second" (line 2 edited). HEAD is detached at "base", with line 4 edited and not committed.
  // Returns the detached commit's short sha.
  function detachedRepo(repo: Repo): string {
    repo.write('a.txt', lines(5));
    repo.commit('base');
    repo.write('a.txt', lines(5, 1));
    repo.commit('second');
    repo.git('switch', '-q', '--detach', 'HEAD~1');
    repo.write('a.txt', lines(5, 3));
    expect(repo.currentBranch()).toBe('');
    return repo.rev('HEAD').slice(0, 7);
  }

  test('Changes and History show the detached commit, and the picker leaves it and comes back', async ({ page, repo, revue }) => {
    const short = detachedRepo(repo);
    const s = await revue.start();
    await s.open(page);
    const picker = page.getByLabel('Branch to view');
    const subjects = commitList(page).locator('li[data-sha] .subj');

    // The header names the detached commit, and the picker has it, selected.
    await expect(page.locator('#branch-name')).toHaveText(short);
    await expect(picker).toHaveValue('HEAD');
    await expect(picker.locator('option:checked')).toContainText(short);
    await expect(picker.locator('option')).toHaveCount(2); // it and main

    // Changes compares with the detached commit, not main's tip: only line 4 changed.
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    await expect(removedLines(section(page, 'a.txt'))).toHaveText(['line 4']);
    await expect(addedLines(section(page, 'a.txt'))).toHaveText(['edited 4']);

    // History lists the detached commit's history, without main's later commit.
    await page.keyboard.press('2');
    await expect(subjects).toHaveText(['base']);
    await expect(overviewTitle(page)).toHaveText('base');

    // main can be viewed, and the detached commit picked again.
    await picker.selectOption('main');
    await expect(page.locator('#branch-note')).toHaveText('view only');
    await expect(subjects).toHaveText(['second', 'base']);
    await picker.selectOption('HEAD');
    await expect(page.locator('#branch-name')).toHaveText(short);
    await expect(subjects).toHaveText(['base']);
    await expect(overviewTitle(page)).toHaveText('base');
    expect(repo.currentBranch()).toBe('');
  });

  // /api/info names a detached HEAD by its short sha, as it names a branch, and says it is detached: the header calls
  // it "detached", not "checked out", and the picker lists it without the branch sign. Checking out a branch by hand
  // makes the header say "checked out" again.
  test('the header calls a detached HEAD "detached", not "checked out"', async ({ page, repo, revue }) => {
    const short = detachedRepo(repo);
    const s = await revue.start();
    await s.open(page);
    const picker = page.getByLabel('Branch to view');
    await expect(page.locator('#branch-name')).toHaveText(short);
    await expect(page.locator('#branch-note')).toHaveText('detached');
    await expect(picker.locator('option:checked')).toHaveText(`${short} (detached)`);

    repo.git('switch', '-q', '--discard-changes', 'main');
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    await expect(picker.locator('option:checked')).toHaveText('⎇ main (checked out)');
    await expect(picker.locator('option')).toHaveCount(1);
  });

  // /api/info names a detached HEAD by its short sha, which reads as a branch's name in a sentence ("No uncommitted
  // changes on 1a2b3c4"): the sentences that name the checked-out branch call it a detached HEAD instead, as the header
  // does. Changes' empty state, Branch mode's title, and the toast for a branch to view that doesn't exist.
  test('sentences that name the checked-out branch call a detached HEAD one', async ({ page, repo, revue }) => {
    const short = detachedRepo(repo);
    repo.write('a.txt', lines(5)); // no uncommitted changes left
    const s = await revue.start();
    await s.open(page);
    await expect(page.locator('#main .empty-state h2')).toHaveText(`No uncommitted changes on detached HEAD ${short}`);

    await page.keyboard.press('3');
    await expect(overviewTitle(page)).toHaveText(`detached HEAD ${short} vs main`);

    await page.evaluate(() => { location.hash = 'mode=branch&base=main&head=gone'; });
    await expect(page.getByRole('status')).toHaveText(`Branch 'gone' doesn't exist; showing detached HEAD ${short}`);
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    await expect(overviewTitle(page)).toHaveText(`detached HEAD ${short} vs main`);

    // A branch is still named as one.
    repo.git('switch', '-q', 'main');
    await expect(overviewTitle(page)).toHaveText('main vs main');
    await page.keyboard.press('1');
    await expect(page.locator('#main .empty-state h2')).toHaveText('No uncommitted changes on main');
  });
});

test.describe('a repo with no commits yet', () => {
  test('Changes lists every file as new, and the empty states say why', async ({ page, repo, revue }) => {
    const s = await revue.start();
    const logs = recordRequests(page, /^\/api\/log$/);
    await s.open(page);
    const empty = page.locator('#main .empty-state');
    await expect(empty.locator('h2')).toHaveText('No uncommitted changes on main');
    await expect(empty.locator('.watching')).toHaveText('Watching the working tree. Edits show up here as they happen.');
    await expect(empty.locator('.latest')).toHaveCount(0);
    expect(logs, 'there is no latest commit to ask for').toEqual([]);
    await expect(page.locator('#branch-name')).toHaveText('main');
    await expect(page.locator('#branch-note')).toHaveText('checked out');

    // Files show up against the empty tree: a staged one as added, an untracked one as untracked.
    repo.write('a.txt', lines(3)).add('a.txt');
    repo.write('b.txt', 'b\n');
    await expect.poll(() => sectionPaths(page)).toEqual(['a.txt', 'b.txt']);
    await expect(overviewTitle(page)).toHaveText('Uncommitted changes');
    await expect(treeRow(page, 'a.txt').locator('.badge')).toHaveText('A');
    await expect(treeRow(page, 'b.txt').locator('.badge')).toHaveText('U');
    await expect(addedLines(section(page, 'a.txt'))).toHaveText(['line 1', 'line 2', 'line 3']);
    await expect(addedLines(section(page, 'b.txt'))).toHaveText(['b']);
    await expect(removedLines(page.locator('#main'))).toHaveCount(0);

    // Branch mode has nothing to compare, and no branch to compare with: it says why, as History does, rather than ask
    // for a base there is none of (see 'a branch with no commits yet').
    await page.keyboard.press('3');
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(empty.locator('h2')).toHaveText('No commits on main yet');
    await expect(page.getByLabel('Compared with').locator('option')).toHaveText(['choose a base…']);
    expect(logs).toEqual([]);
  });

  // There is no commit to show the tree of: History says so (noCommitsState) instead of asking the server for HEAD's
  // tree. That request used to fail: the page showed "Failed to start: fatal: Not a valid object name HEAD" when it
  // opened in History, and pressing 2 threw the same as an uncaught error. The first commit shows up live.
  test('History says there are no commits yet, and shows the first one once it is made', async ({ page, context, repo, revue }) => {
    repo.write('a.txt', 'a\n');
    const s = await revue.start();
    await s.open(page);
    await expect(section(page, 'a.txt')).toHaveCount(1);
    const empty = (p: Page) => p.locator('#main .empty-state');
    const trees = recordRequests(page, /^\/api\/tree$/);

    await page.keyboard.press('2');
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(commitList(page)).toHaveText('No commits yet');
    await expect(empty(page).locator('h2')).toHaveText('No commits on main yet');
    await expect(empty(page).locator('.watching')).toHaveText('Watching the repository. Commits show up here as they are made.');
    await expect(page.locator('#main')).not.toContainText(/fatal|Failed/);
    await expect(page).toHaveURL(/#mode=commit$/);
    expect(trees, 'HEAD has no tree to ask for').toEqual([]);
    await page.keyboard.press('1');
    await expect(section(page, 'a.txt')).toHaveCount(1);

    // A page that opens in History (a new page: going to another hash of the same page doesn't load it again), and one
    // that opens at a file there: with no commit there is no file to show either.
    const fresh = await context.newPage();
    await s.open(fresh, 'mode=commit');
    await expect(commitList(fresh)).toHaveText('No commits yet');
    await expect(empty(fresh).locator('h2')).toHaveText('No commits on main yet');
    await expect(fresh.locator('#main')).not.toContainText(/fatal|Failed/);
    const linked = await context.newPage();
    await s.open(linked, 'mode=commit&file=a.txt');
    await expect(empty(linked).locator('h2')).toHaveText('No commits on main yet');
    await expect(linked.locator('#main')).not.toContainText(/fatal|Failed|Error/);
    await expect(linked).toHaveURL(/#mode=commit$/);
    await linked.close();

    // The first commit, made from the shell, is listed, selected and shown.
    const sha = repo.commit('first');
    await expect(commitList(fresh).locator('li[data-sha] .subj')).toHaveText(['first']);
    await expect(commitList(fresh).locator('[aria-selected=true]')).toHaveAttribute('data-sha', sha);
    await expect(overviewTitle(fresh)).toHaveText('first');
    await expect(section(fresh, 'a.txt')).toHaveCount(1);
    await expect(fresh).toHaveURL(new RegExp(`#mode=commit&ref=${sha}$`));
  });

  // With the file watcher stopped no live update brings the first commit, so the empty state says how to look again: r,
  // which loads the commit list again (it used to say 2, since r didn't). The server keeps answers for up to 3 s when
  // its watcher can't see every change, so r is pressed until it shows.
  test('with the watcher stopped, History says to press r for the first commit, which then shows it', async ({ page, repo, revue }) => {
    repo.write('a.txt', 'a\n');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    const watching = page.locator('#main .empty-state .watching');
    await expect(watching).toHaveText('Watching the repository. Commits show up here as they are made.');
    await s.debug({ watchError: 'off for this test' });
    expect((await s.json('/api/server')).watcher).toBe('off');
    // The status card fetches /api/server again, which the empty state then reuses.
    await page.locator('#live').hover();
    await expect(page.locator('#status-card')).toContainText('the file watcher stopped');
    await page.locator('#tree').hover();
    await focusMain(page);
    await page.keyboard.press('r');
    await expect(watching).toHaveText('Live updates are off. Press r to refresh.');

    const sha = repo.commit('first');
    await expect(async () => {
      await page.keyboard.press('r');
      await expect(commitList(page).locator('[aria-selected=true]')).toHaveAttribute('data-sha', sha, { timeout: 1000 });
    }).toPass();
    await expect(modeTab(page, 'History')).toHaveAttribute('aria-current', 'page');
    await expect(overviewTitle(page)).toHaveText('first');
    await expect(section(page, 'a.txt')).toHaveCount(1);
  });
});

test.describe('a branch with no commits yet', () => {
  // main has a commit (a.txt), and `other` points at it too. `new` is made with git switch --orphan, which leaves the
  // working tree and the index empty, and b.txt is written there, untracked.
  function orphanRepo(repo: Repo) {
    repo.write('a.txt', lines(3));
    repo.commit('first');
    repo.branch('other');
    repo.git('switch', '-q', '--orphan', 'new');
    repo.write('b.txt', 'b\n');
    expect(repo.currentBranch()).toBe('new');
  }

  // Branch mode compares what the branch has added since it left its base: a branch with no commits has nothing to
  // compare, whatever the base. It says so, as History does (noCommitsState), instead of asking the server for HEAD's
  // tree, which failed with "fatal: Not a valid object name HEAD" (pressing 3 threw it as an uncaught error). The base
  // can still be picked, and the branch's first commit is compared once it is made. In a repo with no commits at all
  // Branch mode says the same (see 'a repo with no commits yet'): there is no base to pick there either.
  test('Branch mode says the branch has no commits yet, keeps the base picker, and compares the first commit', async ({ page, context, repo, revue }) => {
    orphanRepo(repo);
    const s = await revue.start();
    await s.open(page);
    await expect(section(page, 'b.txt')).toHaveCount(1);
    await expect(page.locator('#branch-name')).toHaveText('new');
    const empty = (p: Page) => p.locator('#main .empty-state');
    const asked = recordRequests(page, /^\/api\/(tree|file|diffs?|annotations)$/);

    await page.keyboard.press('3');
    await expect(modeTab(page, 'Branch')).toHaveAttribute('aria-current', 'page');
    await expect(empty(page).locator('h2')).toHaveText('No commits on new yet');
    await expect(empty(page).locator('.watching')).toHaveText('Watching the repository. Commits show up here as they are made.');
    const base = page.getByLabel('Compared with');
    await expect(base).toHaveValue('main');
    await expect(base.locator('option')).toHaveText(['main', 'other']);
    await expect(page).toHaveURL(/#mode=branch&base=main$/);

    // Another base: still nothing to compare. The URL is written once the (empty) tree is in, so any request for it
    // would have gone out by then.
    await base.selectOption('other');
    await expect(page).toHaveURL(/#mode=branch&base=other$/);
    await expect(empty(page).locator('h2')).toHaveText('No commits on new yet');
    await expect(base).toHaveValue('other');
    // (The notes of every view, all=1, which the notes bar counts there as other views', are no view's: HEAD's least.)
    const forHead = asked.filter(u => new URL(u).searchParams.get('all') !== '1');
    expect(forHead, 'HEAD has no tree, file or notes to ask for').toEqual([]);

    // History (the branch's own commits, not all branches') says the same, and Branch mode keeps the base picked.
    await page.keyboard.press('2');
    await expect(commitList(page)).toHaveText('No commits yet');
    await expect(empty(page).locator('h2')).toHaveText('No commits on new yet');
    await page.keyboard.press('3');
    await expect(page).toHaveURL(/#mode=branch&base=other$/);
    await expect(empty(page).locator('h2')).toHaveText('No commits on new yet');

    // A page opened at a file in Branch mode: there is no file to show either.
    const linked = await context.newPage();
    await s.open(linked, 'mode=branch&file=a.txt');
    await expect(empty(linked).locator('h2')).toHaveText('No commits on new yet');
    await expect(linked.locator('#main')).not.toContainText(/fatal|Failed|Error/);
    await expect(linked).toHaveURL(/#mode=branch&base=main$/);

    // The first commit, made from the shell, is compared with the base: the orphan branch shares no history with it,
    // so the comparison is with the base's tip (a.txt gone, b.txt added).
    repo.commit('first on new');
    for (const [p, b] of [[page, 'other'], [linked, 'main']] as const) {
      await expect(overviewTitle(p)).toHaveText(`new vs ${b}`);
      await expect.poll(() => sectionPaths(p)).toEqual(['a.txt', 'b.txt']);
      await expect(addedLines(section(p, 'b.txt'))).toHaveText(['b']);
    }
  });
});

test.describe('Check out', () => {
  test('runs git switch once confirmed, and does nothing when cancelled', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'main');
    const s = await revue.start();
    await s.open(page);
    // No live update follows the switch, so what the page shows afterwards is checkout()'s own doing.
    await s.debug({ watchError: 'off for this test' });
    expect((await s.json('/api/server')).watcher).toBe('off');
    const switches = recordRequests(page, /^\/api\/switch$/);
    await page.getByLabel('Branch to view').selectOption('feature');
    const checkout = page.getByRole('button', { name: 'Check out' });
    await expect(checkout).toBeVisible();
    await expect(page).toHaveURL(/#mode=branch&base=main&head=feature$/);

    const dialogs: string[] = [];
    page.once('dialog', d => { dialogs.push(`${d.type()}: ${d.message()}`); d.dismiss(); });
    await checkout.click();
    await expect.poll(() => dialogs).toEqual([expect.stringMatching(/^confirm: Check out "feature"\?/)]);
    await expect(page.locator('#branch-note')).toHaveText('view only');
    // This catches a switch only if it has already finished; the request count at the end doesn't depend on timing.
    expect(repo.currentBranch()).toBe('main');

    page.once('dialog', d => { dialogs.push(`${d.type()}: ${d.message()}`); d.accept(); });
    await checkout.click();
    await expect.poll(() => repo.currentBranch()).toBe('feature');
    expect(dialogs).toHaveLength(2);
    await expect(page.locator('#branch-name')).toHaveText('feature');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    await expect(checkout).toBeHidden();
    await expect(page.getByLabel('Branch to view')).toHaveValue('HEAD');
    // The view is drawn again for the checked-out branch: the same comparison, now without head= in the URL.
    await expect(overviewTitle(page)).toHaveText('feature vs main');
    await expect(page).toHaveURL(/#mode=branch&base=main$/);
    expect(repo.exists('c.txt')).toBe(true);
    // The cancelled click sent nothing: the one switch is the confirmed one.
    expect(switches).toEqual([expect.stringMatching(/\/api\/switch\?branch=feature$/)]);
  });

  // /api/info belongs to the page, and the newest answer asked for is the one kept. An r pressed before Check out asks
  // for it too, and its answer (held here) from before the switch used to replace Check out's own when it came in last:
  // the page then took main for the checked-out branch.
  test('info from before the switch that comes in after it is not kept', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'main');
    const s = await revue.start();
    await s.open(page);
    await s.debug({ watchError: 'off for this test' });
    await page.getByLabel('Branch to view').selectOption('feature');
    const checkout = page.getByRole('button', { name: 'Check out' });
    await expect(checkout).toBeVisible();

    const info = await hold(page, u => u.pathname === '/api/info', { answer: true });
    await focusMain(page);
    await page.keyboard.press('r');
    await info.held; // answered while main is checked out
    page.once('dialog', d => d.accept());
    await checkout.click();
    await expect.poll(() => repo.currentBranch()).toBe('feature');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
    info.release();
    await info.answered();
    await page.waitForTimeout(500);
    await expect(page.locator('#branch-name')).toHaveText('feature');

    // The control: Changes, drawn now, names the branch from the info the page kept.
    await page.keyboard.press('1');
    await expect(page.getByRole('heading', { name: /^No uncommitted changes on / })).toHaveText('No uncommitted changes on feature');
    await expect(page.locator('#branch-name')).toHaveText('feature');
    await expect(page.locator('#branch-note')).toHaveText('checked out');
  });

  test('is refused while tracked files have uncommitted changes', async ({ page, repo, revue }) => {
    forkedRepo(repo, 'main');
    repo.write('a.txt', lines(5, 0));
    const s = await revue.start();
    await s.open(page);
    await page.getByLabel('Branch to view').selectOption('feature');

    const dialogs: string[] = [];
    page.on('dialog', d => { dialogs.push(`${d.type()}: ${d.message()}`); d.accept(); });
    await page.getByRole('button', { name: 'Check out' }).click();
    await expect.poll(() => dialogs).toEqual([
      expect.stringMatching(/^confirm: Check out "feature"/),
      expect.stringMatching(/^alert: .*\bfeature\b.*uncommitted changes/),
    ]);
    expect(repo.currentBranch()).toBe('main');
    expect(repo.read('a.txt')).toBe(lines(5, 0));
    await expect(page.locator('#branch-note')).toHaveText('view only');
    await expect(page.getByRole('button', { name: 'Check out' })).toBeEnabled();
  });
});

test.describe('History on a branch with no commits yet', () => {
  // `git switch --orphan` checks out a branch with no commits while main has some. Its own history is empty, but "all
  // branches" lists main's: the server used to answer every log request with an empty list while HEAD had no commits.
  test('"all branches" lists the other branches\' commits, which can be picked', async ({ page, repo, revue }) => {
    repo.write('a.txt', 'a\n');
    repo.commit('base');
    repo.write('b.txt', 'b\n');
    repo.commit('main only');
    repo.git('switch', '-q', '--orphan', 'new');
    const s = await revue.start();
    await s.open(page, 'mode=commit');
    await expect(commitList(page)).toHaveText('No commits yet');

    await page.getByRole('checkbox', { name: 'all branches' }).check();
    await expect(commitList(page).locator('li[data-sha] .subj')).toHaveText(['main only', 'base']);
    await commitList(page).getByRole('option', { name: /^main only/ }).click();
    await expect(overviewTitle(page)).toHaveText('main only');
    await expect.poll(() => sectionPaths(page)).toEqual(['b.txt']);
  });
});
