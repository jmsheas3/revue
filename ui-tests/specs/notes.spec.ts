// Agent notes (POST /api/annotations) in the page: placement under the annotated line on the side it is about (split
// and unified), a range of lines (under its last line, every line marked), an old-side note on a renamed file (under
// its old path, in the diff and the Before file view), the strip for notes whose lines aren't shown, the list above a
// markdown preview, notes that arrive while the page is open (put on it once, never on another view's rows, and never
// replaced by an older answer that comes in later), Dismiss and Clear all, the toolbar's count and Next, the "in other
// views" button, the outdated flag when the annotated lines change, and note text that is never parsed as HTML.
import {
  test, expect, hashParams, lines, reload, section, hold, focusMain, overviewTitle, addedLines, viewLabel, type Repo,
  type Server,
} from '../fixtures';
import type { Page } from '@playwright/test';

// a.txt: 20 lines, committed, then lines 5 and 15 edited in the working tree. With 3 lines of context the diff shows
// two hunks (lines 2–8 and 12–18), so lines 1 and 20 are not in it.
function edited(repo: Repo, edits: Record<number, string> = { 5: 'edited 5', 15: 'edited 15' }) {
  let s = '';
  for (let i = 1; i <= 20; i++) s += `${edits[i] ?? `line ${i}`}\n`;
  repo.write('a.txt', s);
}

function setup(repo: Repo) {
  edited(repo, {});
  repo.commit('init');
  edited(repo);
}

const note = (page: Page, text: string) => page.locator('#main .note-box', { hasText: text });

test('notes sit under their line on the side they are about, in split and unified layouts', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, side: 'old', text: 'old note' });
  await s.annotate({ file: 'a.txt', line: 5, text: 'new note' });
  await s.annotate({ file: 'a.txt', line: 15, text: 'only new' });
  // Split needs about 60 characters a side; at the default 1280 px the page draws unified ("narrow").
  await page.setViewportSize({ width: 1600, height: 900 });
  await s.open(page, 'mode=working&file=a.txt');

  // Split: line 5 is a changed pair, so its old and new notes share one row, each in its own side's half.
  const split = page.locator('#main table.split');
  await expect(split).toBeVisible();
  const shared = split.locator('tr[data-o="5"][data-n="5"] + tr.split-note');
  await expect(shared.locator('> td').nth(0)).toContainText('old note');
  await expect(shared.locator('> td').nth(1)).toContainText('new note');
  // Line 15's note is only about the new side: its half is filled, the old half is empty, and only the new number
  // is marked.
  const only = split.locator('tr[data-n="15"] + tr.split-note');
  await expect(only.locator('> td').nth(1)).toContainText('only new');
  await expect(only.locator('> td').nth(0)).toBeEmpty();
  const nums15 = split.locator('tr[data-n="15"] td.num');
  await expect(nums15.nth(0)).not.toHaveClass(/\bnoted\b/);
  await expect(nums15.nth(1)).toHaveClass(/\bnoted\b/);

  // Unified: each note spans the row right under its own side's line, with an old/new tag.
  await page.locator('#layout-seg').getByRole('button', { name: 'Unified' }).click();
  const unified = page.locator('#main table.unified');
  await expect(unified).toBeVisible();
  const oldNote = unified.locator('tr.del[data-o="5"] + tr.note');
  await expect(oldNote).toContainText('old note');
  await expect(oldNote.locator('.note-side')).toHaveText('old');
  const newNote = unified.locator('tr.add[data-n="5"] + tr.note');
  await expect(newNote).toContainText('new note');
  await expect(newNote.locator('.note-side')).toHaveText('new');
  await expect(unified.locator('tr.add[data-n="15"] + tr.note')).toContainText('only new');
  await expect(unified.locator('tr.note')).toHaveCount(3);
  await expect(page.locator('#main .notes-missing')).toHaveCount(0);
});

test('a note on lines the diff does not show goes in the strip at the top', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 20, text: 'about the last line' });
  await s.open(page, 'mode=working&file=a.txt');

  const strip = page.locator('#main .notes-missing');
  await expect(strip).toContainText('about the last line');
  await expect(strip).toContainText('line 20');
  await expect(page.locator('#main tr.note')).toHaveCount(0);
  // At the top: above the diff, not after it.
  await expect(page.locator('#main .file-body > :first-child')).toHaveClass(/\bnotes-missing\b/);

  // Its link opens the whole file, where the note is under line 20.
  await strip.getByRole('button', { name: 'show in file' }).click();
  await expect(page.locator('#main table.fileview tr[data-n="20"] + tr.note')).toContainText('about the last line');
  await expect(page.locator('#main .notes-missing')).toHaveCount(0);
});

test('the toolbar counts the notes and Next steps through them in line order', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  // Added out of order: Next goes by file and line, not by when they were left.
  await s.annotate({ file: 'a.txt', line: 15, text: 'second' });
  await s.annotate({ file: 'a.txt', line: 5, text: 'first' });
  await s.open(page);

  const next = page.locator('#notes-next');
  await expect(next).toHaveText(/^2 notes/);
  await expect(note(page, 'first')).toContainText('1 of 2');
  await expect(note(page, 'second')).toContainText('2 of 2');

  const target = page.locator('#main tr.target');
  await next.click();
  await expect(page).toHaveURL(/[#&]file=a\.txt&.*line=5(&|$)/);
  await expect(target).toHaveAttribute('data-n', '5');
  await next.click();
  await expect(page).toHaveURL(/line=15(&|$)/);
  await expect(target).toHaveAttribute('data-n', '15');
  await next.click(); // and round again
  await expect(page).toHaveURL(/line=5(&|$)/);
  await expect(target).toHaveAttribute('data-n', '5');
});

test('notes left in other views are counted, and the button opens them', async ({ page, repo, revue }) => {
  setup(repo);
  const head = repo.rev('HEAD');
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 3, text: 'on the commit', mode: 'commit', ref: head });
  await s.open(page);

  // This view has none of its own: only the other-views button shows. Hidden is checked once the notes are in, since
  // the whole bar is hidden until then.
  const other = page.getByRole('button', { name: '1 in other view' });
  await expect(other).toBeVisible();
  await expect(page.locator('#notes-next')).toBeHidden();

  await other.click();
  await expect(page).toHaveURL(new RegExp(`mode=commit&ref=${head}&file=a\\.txt&line=3`));
  // The root commit adds every line, so line 3 is in its diff, with the note under it.
  await expect(page.locator('#main tr[data-n="3"] + tr.note')).toContainText('on the commit');
  await expect(page.locator('#notes-next')).toHaveText(/^1 note/);
});

test('editing the annotated lines marks the note outdated', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 15, text: 'watch this line' });
  await s.open(page, 'mode=working&file=a.txt');

  const box = note(page, 'watch this line');
  await expect(box).toBeVisible();
  await expect(box.locator('.note-stale')).toHaveCount(0);

  // Another line changes: the note still matches its code.
  edited(repo, { 5: 'edited 5', 6: 'also edited', 15: 'edited 15' });
  await expect(page.locator('#main tr[data-n="6"] td.src').last()).toHaveText('also edited');
  await expect(box.locator('.note-stale')).toHaveCount(0);

  // Now its own line changes.
  edited(repo, { 5: 'edited 5', 6: 'also edited', 15: 'rewritten 15' });
  await expect(box.locator('.note-stale')).toHaveText('outdated');
  await expect(box).toHaveClass(/\bstale\b/);
});

test('note text is shown as text, never as HTML', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.locator('#main table.code')).toBeVisible();

  // Added while the page is open, so it also arrives through the live `annotations` event.
  const text = '<img src=x onerror="document.title=\'pwned\'"><b>bold</b>';
  await s.annotate({ file: 'a.txt', line: 5, text });
  // Found by where it is (under new line 5), not by its text, so the checks below can't pass for want of a box. The
  // text comes first: it shows the box is there.
  const box = page.locator('#main tr.add[data-n="5"] + tr.note .note-box');
  await expect(box.locator('.note-text')).toHaveText(text);
  await expect(box.locator('img, b')).toHaveCount(0);
  // Nothing checks the title the onerror would set: the CSP blocks inline handlers, and the guard fails the test on
  // the report one that reached the page would cause.
});

test('a note on a range of lines sits under its last line, and each of its lines is marked on its side', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 4, endLine: 6, text: 'new range' });
  await s.annotate({ file: 'a.txt', line: 13, endLine: 16, side: 'old', text: 'old range' });
  await s.open(page, 'mode=working&file=a.txt');
  const table = page.locator('#main table.unified');

  // Under the range's last line (a context row, which shows both numbers), not under its first.
  const newNote = table.locator('tr[data-n="6"] + tr.note');
  await expect(newNote).toContainText('new range');
  await expect(newNote).toContainText('lines 4–6');
  const oldNote = table.locator('tr[data-o="16"] + tr.note');
  await expect(oldNote).toContainText('old range');
  await expect(oldNote).toContainText('lines 13–16 (before)');
  await expect(table.locator('tr.note')).toHaveCount(2);

  // A unified row has the old number, then the new one. Every line of a range is marked, on its own side only.
  const oldNum = (n: number) => table.locator(`tr[data-o="${n}"] > td.num`).nth(0);
  const newNum = (n: number) => table.locator(`tr[data-n="${n}"] > td.num`).nth(1);
  for (const n of [4, 5, 6]) await expect(newNum(n), `new line ${n}`).toHaveClass(/\bnoted\b/);
  for (const n of [3, 7]) await expect(newNum(n), `new line ${n}`).not.toHaveClass(/\bnoted\b/);
  for (const n of [4, 6]) await expect(oldNum(n), `old line ${n}`).not.toHaveClass(/\bnoted\b/);
  for (const n of [13, 14, 15, 16]) await expect(oldNum(n), `old line ${n}`).toHaveClass(/\bnoted\b/);
  for (const n of [12, 17]) await expect(oldNum(n), `old line ${n}`).not.toHaveClass(/\bnoted\b/);
  for (const n of [13, 16]) await expect(newNum(n), `new line ${n}`).not.toHaveClass(/\bnoted\b/);
});

// old.txt (20 lines) renamed to new.txt with line 5 edited: a staged rename with an unstaged edit, which Changes mode
// shows as R. The old side of a renamed file is filed under its old path, the new side under its new one.
async function renamed(repo: Repo, revue: { start(): Promise<Server> }): Promise<Server> {
  repo.write('old.txt', lines(20));
  repo.commit('init');
  repo.rename('old.txt', 'new.txt');
  repo.write('new.txt', lines(20, 4)); // line 5 becomes "edited 5"
  const s = await revue.start();
  await s.annotate({ file: 'old.txt', line: 5, side: 'old', text: 'about the old name' });
  await s.annotate({ file: 'new.txt', line: 5, text: 'about the new name' });
  return s;
}

test('an old-side note on a renamed file is shown under its old line, in the diff and in the Before file', async ({ page, repo, revue }) => {
  const s = await renamed(repo, revue);
  await s.open(page, 'mode=working&file=new.txt');
  const head = page.locator('#main .file-head');
  await expect(head).toContainText('old.txt → ');

  const diff = page.locator('#main table.unified');
  await expect(diff.locator('tr.del[data-o="5"] + tr.note')).toContainText('about the old name');
  await expect(diff.locator('tr.add[data-n="5"] + tr.note')).toContainText('about the new name');
  await expect(diff.locator('tr.note')).toHaveCount(2);

  // The whole file shows one side, and only that side's notes.
  await head.getByRole('button', { name: 'File', exact: true }).click();
  const after = page.locator('#main table.fileview[data-side="new"]');
  await expect(after.locator('tr[data-n="5"] + tr.note')).toContainText('about the new name');
  await expect(page.locator('#main tr.note')).toHaveCount(1);
  await expect(page.locator('#main .notes-missing')).toHaveCount(0);

  await head.getByRole('button', { name: 'Before', exact: true }).click();
  const before = page.locator('#main table.fileview[data-side="old"]');
  await expect(before.locator('tr[data-o="5"] + tr.note')).toContainText('about the old name');
  await expect(before.locator('tr[data-o="5"] td.num')).toHaveClass(/\bnoted\b/);
  await expect(page.locator('#main tr.note')).toHaveCount(1);
  await expect(page.locator('#main .notes-missing')).toHaveCount(0);

  // A link to the Before side (side=old, as an agent or a copied URL gives it) shows the same.
  const linked = await page.context().newPage();
  await s.open(linked, 'mode=working&file=new.txt&view=file&side=old');
  await expect(linked.locator('#main table.fileview[data-side="old"] tr[data-o="5"] + tr.note')).toContainText('about the old name');
  await expect(linked.locator('#main tr.note')).toHaveCount(1);
});

test('the Before side of a file, with its old-side notes, is still shown after a reload', async ({ page, repo, revue }) => {
  const s = await renamed(repo, revue);
  await s.open(page, 'mode=working&file=new.txt&view=file');
  const head = page.locator('#main .file-head');
  await expect(page.locator('#main table.fileview[data-side="new"]')).toBeVisible();
  await head.getByRole('button', { name: 'Before', exact: true }).click();
  const before = page.locator('#main table.fileview[data-side="old"] tr[data-o="5"] + tr.note');
  await expect(before).toContainText('about the old name');

  await reload(page);
  await expect(before).toContainText('about the old name');
  await expect(head.getByRole('button', { name: 'Before', exact: true })).toHaveClass(/\bon\b/);
});

test('a markdown preview lists its notes above the document, with a link to the line in the file', async ({ page, repo, revue }) => {
  // Line 5 is "line 3".
  repo.write('doc.md', '# Doc\n\n' + lines(10));
  repo.commit('init');
  const s = await revue.start();
  await s.annotate({ file: 'doc.md', line: 5, text: 'about line 5' });
  await s.open(page, 'mode=working&file=doc.md&view=preview');
  await expect(page.locator('#main .markdown').getByRole('heading', { name: 'Doc' })).toBeVisible();

  // In the page, right above the preview's frame, not in the document's shadow root.
  const strip = page.locator('#main .file-body > .notes-missing');
  await expect(strip.locator('.note-box')).toHaveCount(1);
  await expect(strip).toContainText('about line 5');
  await expect(strip).toContainText('line 5');
  await expect(page.locator('#main .file-body > :first-child')).toHaveClass(/\bnotes-missing\b/);
  await expect(page.locator('#main .file-body > .notes-missing + .md-frame')).toHaveCount(1);
  expect(await page.evaluate(() => document.querySelector('#main .notes-missing .note-text')?.textContent)).toBe('about line 5');

  // One left while the preview is open joins the list.
  await s.annotate({ file: 'doc.md', line: 8, text: 'added later' });
  await expect(strip.locator('.note-box')).toHaveCount(2);
  await expect(strip.locator('.note-box').nth(1)).toContainText('added later');

  await strip.locator('.note-box', { hasText: 'about line 5' }).getByRole('button', { name: 'show in file' }).click();
  await expect(page.locator('#main table.fileview tr[data-n="5"] + tr.note')).toContainText('about line 5');
  await expect(page.locator('#main .notes-missing')).toHaveCount(0);
  expect(hashParams(page)).toMatchObject({ file: 'doc.md', view: 'file', line: '5' });
});

test('notes that arrive while a diff is open are put on it once, beside the ones already there', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'first note' });
  await s.annotate({ file: 'a.txt', line: 20, text: 'first strip note' });
  await s.open(page, 'mode=working&file=a.txt');
  const rows = page.locator('#main tr.note');
  const strip = page.locator('#main .notes-missing');
  await expect(rows).toHaveCount(1);
  await expect(strip.locator('.note-box')).toHaveCount(1);

  // Each arrives through the live `annotations` event, which puts all of this view's notes on the rows already drawn:
  // the ones there before are taken off first, so they aren't there twice.
  await s.annotate({ file: 'a.txt', line: 15, text: 'second note' });
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText('first note');
  await expect(rows.nth(1)).toContainText('second note');
  // Line 1 is outside the diff, like line 20: one strip, holding both.
  await s.annotate({ file: 'a.txt', line: 1, text: 'second strip note' });
  await expect(strip.locator('.note-box')).toHaveCount(2);
  await expect(strip).toHaveCount(1);
  await expect(rows).toHaveCount(2);
});

test('a note that arrives while the overview is open goes into its section once', async ({ page, repo, revue }) => {
  setup(repo);
  repo.write('b.txt', 'b\n');
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'first note' });
  await s.open(page);
  const a = section(page, 'a.txt');
  await expect(a.locator('tr.note')).toHaveCount(1);

  await s.annotate({ file: 'a.txt', line: 15, text: 'second note' });
  await expect(a.locator('tr.note')).toHaveCount(2);
  await expect(a.locator('tr.note').nth(1)).toContainText('second note');
  await s.annotate({ file: 'b.txt', line: 1, text: 'on b' });
  await expect(section(page, 'b.txt').locator('tr.note')).toContainText('on b');
  await expect(page.locator('#main tr.note')).toHaveCount(3);
});

// An `annotations` event loads the notes of the view shown when it came. The page may have gone to another view before
// they are in, and they used to be put on that view's rows: here Changes' notes, held, came in over the commit's a.txt,
// which has a line 5 too, and replaced the commit's own note there.
test('notes that come in after the page went to another view are not put on its rows', async ({ page, repo, revue }) => {
  setup(repo);
  const head = repo.rev('HEAD');
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'on the commit', mode: 'commit', ref: head });
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.getByRole('button', { name: '1 in other view' })).toBeVisible();
  const notes = page.locator('#main tr.note');

  const load = await hold(page, u => u.pathname === '/api/annotations' && u.searchParams.get('mode') === 'working');
  await s.annotate({ file: 'a.txt', line: 5, text: 'uncommitted' });
  await load.held;
  await focusMain(page);
  await page.keyboard.press('2');
  await expect(section(page, 'a.txt').locator('tr[data-n="5"] + tr.note')).toContainText('on the commit');
  load.release();
  await load.answered();
  await page.waitForTimeout(500);
  await expect(note(page, 'uncommitted')).toHaveCount(0);
  await expect(notes).toHaveCount(1);
  await expect(note(page, 'on the commit')).toBeVisible();
  await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/);

  // The controls: the commit's next note arrives the same way and is put on its rows, and Changes shows its own note.
  await s.annotate({ file: 'a.txt', line: 15, text: 'next on the commit', mode: 'commit', ref: head });
  await expect(section(page, 'a.txt').locator('tr[data-n="15"] + tr.note')).toContainText('next on the commit');
  await expect(notes).toHaveCount(2);
  await page.keyboard.press('1');
  await expect(section(page, 'a.txt').locator('tr[data-n="5"] + tr.note')).toContainText('uncommitted');
  await expect(notes).toHaveCount(1);
});

// The notes come with every tree (loadTree) and on their own when they change (loadNotes, on an `annotations` event),
// and the answers can come in in any order: the request made last is the one whose answer is kept. An agent edits a
// file and annotates it at once: the live update's tree, slower (held here), used to come in after the notes had been
// loaded again for the new note, and the notes loaded with it, from before the note, replaced them. The note was gone
// until the next load, and the toolbar said "0 notes".
test('a note added while a live update loads is still shown once the update is in', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=a.txt');
  const code = page.locator('#main table.code');
  await expect(code).toContainText('edited 15');

  const tree = await hold(page, u => u.pathname === '/api/tree');
  const notesWithTree = page.waitForResponse(r => new URL(r.url()).pathname === '/api/annotations');
  edited(repo, { 5: 'edited 5', 10: 'edited 10', 15: 'edited 15' });
  await tree.held;
  await notesWithTree; // answered before the note is added
  await s.annotate({ file: 'a.txt', line: 5, text: 'right after the edit' });
  await expect(note(page, 'right after the edit')).toBeVisible();
  tree.release();

  await expect(code).toContainText('edited 10'); // the live update is drawn
  await expect(note(page, 'right after the edit')).toBeVisible();
  await expect(page.locator('#main tr.note')).toHaveCount(1);
  await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/);
});

// Notes added one after the other each load the notes again. The first load's answer, from before the second note
// (held here), used to replace the second load's when it came in last: the second note was gone until the next load.
test('of two loads of the notes, the later one\'s answer is kept when the earlier one comes in last', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.locator('#main table.code')).toContainText('edited 15');
  const rows = page.locator('#main tr.note');

  const first = await hold(page, u => u.pathname === '/api/annotations', { answer: true });
  await s.annotate({ file: 'a.txt', line: 5, text: 'first note' });
  await first.held; // answered with the first note only
  await s.annotate({ file: 'a.txt', line: 15, text: 'second note' });
  await expect(note(page, 'second note')).toBeVisible();
  await expect(rows).toHaveCount(2);
  first.release();
  await first.answered();
  await page.waitForTimeout(500);
  await expect(note(page, 'second note')).toBeVisible();
  await expect(rows).toHaveCount(2);
  await expect(page.locator('#notes-next')).toHaveText(/^2 notes/);

  // The control: the next note's load is put on the page.
  await s.annotate({ file: 'a.txt', line: 8, text: 'third note' });
  await expect(note(page, 'third note')).toBeVisible();
  await expect(rows).toHaveCount(3);
  await expect(page.locator('#notes-next')).toHaveText(/^3 notes/);
});

// History opened by its key fetches the branch tip's tree and notes before its commit list (held here), while the commit
// shown last (S, on another branch) is still in state. A note left on S meanwhile used to load S's notes into the
// toolbar over Changes, until the tip's replaced them. Notes now wait for History's range, and S's note is counted among
// the other views'.
test('the notes History opens with are its first commit\'s, even when the commit shown last gets a note while it loads', async ({ page, repo, revue }) => {
  repo.write('a.txt', 'one\n');
  repo.commit('base');
  repo.switch('side', true);
  repo.write('a.txt', 'side\n');
  const S = repo.commit('on side');
  repo.switch('main');
  repo.write('a.txt', 'main\n');
  repo.commit('on main');
  const s = await revue.start();
  await s.open(page, `mode=commit&ref=${S}`);
  await expect(overviewTitle(page)).toHaveText('on side');
  await focusMain(page);
  await page.keyboard.press('1');
  await expect(page.locator('#main .empty-state')).toBeVisible();

  const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
  await page.keyboard.press('2');
  await list.held;
  await s.annotate({ file: 'a.txt', line: 1, text: 'on side', mode: 'commit', ref: S });
  await page.waitForTimeout(500);
  await expect(page.locator('#notes-next')).toBeHidden();
  list.release();

  await expect(overviewTitle(page)).toHaveText('on main');
  await expect(addedLines(section(page, 'a.txt'))).toHaveText(['main']); // its notes are put on its rows as it is drawn
  await expect(note(page, 'on side')).toHaveCount(0);
  await expect(page.locator('#notes-next')).toBeHidden();
  await expect(page.locator('#notes-other')).toHaveText('1 in other view');
});

// The page holds History's range as soon as 2 is pressed, while the view shown is still Changes' until the commit list
// (held here) is in. An `annotations` event meanwhile loaded History's notes (HEAD's, with no commit chosen yet) and
// put them on the rows still shown: Changes' overview has a line 5 of a.txt too.
test('notes that arrive while History waits for its commit list are not put on the view it replaces', async ({ page, repo, revue }) => {
  setup(repo);
  const head = repo.rev('HEAD');
  const s = await revue.start();
  await s.open(page);
  await expect(addedLines(section(page, 'a.txt'))).toHaveText(['edited 5', 'edited 15']);

  const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
  await focusMain(page);
  await page.keyboard.press('2');
  await list.held;
  await s.annotate({ file: 'a.txt', line: 5, text: 'on the commit', mode: 'commit', ref: head });
  await page.waitForTimeout(500);
  await expect(note(page, 'on the commit')).toHaveCount(0);

  // The control: History, once drawn, has the note under its line 5.
  list.release();
  await expect(section(page, 'a.txt').locator('tr[data-n="5"] + tr.note')).toContainText('on the commit');
  await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/);
});

// History's branch tip's notes come with its tree, which is asked for before the commit list (held here). Their
// summary of the other views' notes used to be the last one stored, so a note left in another view meanwhile wasn't
// counted until the notes were loaded again.
test('a note left in another view while History waits for its commit list is counted once History is in', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.locator('#main table.code')).toContainText('edited 5');

  const list = await hold(page, u => u.pathname === '/api/log' && u.searchParams.get('limit') !== '1');
  const tipNotes = page.waitForResponse(r => new URL(r.url()).pathname === '/api/annotations'
    && new URL(r.url()).searchParams.get('ref') === 'HEAD');
  await focusMain(page);
  await page.keyboard.press('2');
  await list.held;
  await tipNotes;
  await s.annotate({ file: 'a.txt', line: 5, text: 'uncommitted' });
  list.release();
  await expect(overviewTitle(page)).toHaveText('init');
  await expect(page.locator('#notes-other')).toHaveText('1 in other view');
});

test('Dismiss removes a note for good, and the count and line mark go with it', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'dismiss me' });
  await s.annotate({ file: 'a.txt', line: 15, text: 'keep me' });
  await s.open(page, 'mode=working&file=a.txt');
  const count = page.locator('#notes-next');
  await expect(count).toHaveText(/^2 notes/);
  const num5 = page.locator('#main tr.add[data-n="5"] > td.num').nth(1);
  await expect(num5).toHaveClass(/\bnoted\b/);

  await note(page, 'dismiss me').getByRole('button', { name: 'Dismiss', exact: true }).click();
  await expect(note(page, 'dismiss me')).toHaveCount(0);
  await expect(count).toHaveText(/^1 note(?!s)/);
  await expect(num5).not.toHaveClass(/\bnoted\b/);
  await expect(note(page, 'keep me')).toBeVisible();
  await expect(page.locator('#main tr.note')).toHaveCount(1);

  // The server dropped it, so a reload doesn't bring it back. The note that stays shows the notes are in.
  await reload(page);
  await expect(note(page, 'keep me')).toBeVisible();
  await expect(note(page, 'dismiss me')).toHaveCount(0);
  await expect(count).toHaveText(/^1 note(?!s)/);
});

test('Clear all asks first, then removes every note in this view and leaves other views alone', async ({ page, repo, revue }) => {
  setup(repo);
  const head = repo.rev('HEAD');
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'first' });
  await s.annotate({ file: 'a.txt', line: 15, text: 'second' });
  await s.annotate({ file: 'a.txt', line: 3, text: 'on the commit', mode: 'commit', ref: head });
  await s.open(page, 'mode=working&file=a.txt');
  const count = page.locator('#notes-next');
  await expect(count).toHaveText(/^2 notes/);
  const other = page.getByRole('button', { name: '1 in other view' });
  await expect(other).toBeVisible();

  const asked: string[] = [];
  const clearAll = async (accept: boolean) => {
    page.once('dialog', d => { asked.push(d.message()); void (accept ? d.accept() : d.dismiss()); });
    await page.getByRole('button', { name: 'Notes menu' }).click();
    await page.getByRole('menuitem', { name: 'Clear all notes in this view' }).click();
  };

  // Cancelled: nothing goes. Given time to (wrongly) clear, then the control below.
  await clearAll(false);
  expect(asked).toEqual(['Clear all 2 notes in this view?']);
  await page.waitForTimeout(500);
  await expect(page.locator('#main tr.note')).toHaveCount(2);
  await expect(count).toHaveText(/^2 notes/);

  await clearAll(true);
  expect(asked).toHaveLength(2);
  await expect(page.locator('#main tr.note')).toHaveCount(0);
  await expect(count).toBeHidden();
  await expect(other).toBeVisible();

  // Cleared on the server: after a reload this view still has none, and the other view still has its note.
  await reload(page);
  await expect(other).toBeVisible();
  await expect(count).toBeHidden();
  await expect(page.locator('#main tr.note')).toHaveCount(0);
});

// A view with no range of its own (Branch mode with no base to compare with, History on a branch with no commits yet) has
// no notes, and every view's are another's. Its notes used to be stored as none at all, which hid "N in other views".
test.describe('a view with no range of its own counts the notes in every view', () => {
  test('Branch mode with no base to compare with', async ({ page, repo, revue }) => {
    // No main, master, origin/main, origin/master or develop; a.txt is edited, for a note in Changes.
    repo.write('a.txt', 'a\n');
    repo.commit('base');
    repo.git('branch', '-m', 'main', 'trunk');
    repo.switch('topic', true);
    repo.write('t.txt', 'topic\n');
    repo.commit('topic work');
    repo.write('a.txt', 'edited\n');
    const s = await revue.start();
    await s.annotate({ file: 'a.txt', line: 1, text: 'uncommitted' });
    await s.open(page, 'mode=branch');
    await expect(overviewTitle(page)).toHaveText('Choose a base branch');
    await expect(page.locator('#notes-other')).toHaveText('1 in other view');
    await expect(page.locator('#notes-next')).toBeHidden();

    // It opens the note's view, as anywhere else.
    await page.locator('#notes-other').click();
    await expect(page).toHaveURL(/#mode=working&file=a\.txt&line=1$/);
    await expect(note(page, 'uncommitted')).toBeVisible();
  });

  test('History on a branch with no commits yet', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(3));
    const first = repo.commit('first');
    repo.git('switch', '-q', '--orphan', 'new');
    const s = await revue.start();
    await s.annotate({ file: 'a.txt', line: 2, text: 'on the first commit', mode: 'commit', ref: first });
    await s.open(page, 'mode=commit');
    await expect(page.getByRole('heading', { name: 'No commits on new yet' })).toBeVisible();
    await expect(page.locator('#notes-other')).toHaveText('1 in other view');
    await expect(page.locator('#notes-next')).toBeHidden();
  });
});

// A view whose tree fails to load, with nothing to fall back to, is drawn as its error. Before, the last view's rows
// stayed (with a toast), and the view never counted as shown, so Clear all stayed disabled. The notes bar is the failed
// view's own, so Clear all clears those.
test.describe('a view whose tree fails to load', () => {
  // /api/tree answers 500 on purpose (page.route), which the guard otherwise fails the test for.
  test.use({ allowServerErrors: true });

  test('is drawn as its error, with its own notes, which Clear all clears', async ({ page, repo, revue }) => {
    setup(repo);
    const head = repo.rev('HEAD');
    const s = await revue.start();
    await s.annotate({ file: 'a.txt', line: 5, text: 'uncommitted' });
    await s.annotate({ file: 'a.txt', line: 3, text: 'on the commit', mode: 'commit', ref: head });
    await s.annotate({ file: 'a.txt', line: 4, text: 'also on the commit', mode: 'commit', ref: head });
    await s.open(page, 'mode=working&file=a.txt');
    await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/);
    await page.route(u => u.pathname === '/api/tree' && u.searchParams.get('mode') === 'commit', route => route.fulfill({
      status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'tree failed' }),
    }));

    await page.evaluate(h => { location.hash = h; }, `mode=commit&ref=${head}`);
    await expect(page.locator('#main')).toHaveText('Error: tree failed');
    await expect(page.locator('#notes-next')).toHaveText(/^2 notes/);
    await expect(page.locator('#notes-other')).toHaveText('1 in other view');
    const asked: string[] = [];
    page.once('dialog', d => { asked.push(d.message()); void d.accept(); });
    await page.getByRole('button', { name: 'Notes menu' }).click();
    const clearAll = page.getByRole('menuitem', { name: 'Clear all notes in this view' });
    await expect(clearAll).toBeEnabled();
    await clearAll.click();
    await expect(page.locator('#notes-next')).toBeHidden();
    expect(asked).toEqual(['Clear all 2 notes in this view?']);
    // The commit's notes went, and the working tree's stayed.
    expect((await s.json('/api/annotations', { mode: 'commit', ref: head })).notes).toEqual([]);
    expect((await s.json('/api/annotations', { mode: 'working' })).notes).toHaveLength(1);
  });
});

test('Clear all asks about a single note in the singular', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'the only one' });
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.locator('#notes-next')).toHaveText(/^1 note(?!s)/);
  const asked: string[] = [];
  page.once('dialog', d => { asked.push(d.message()); void d.accept(); });
  await page.getByRole('button', { name: 'Notes menu' }).click();
  await page.getByRole('menuitem', { name: 'Clear all notes in this view' }).click();
  await expect(page.locator('#main tr.note')).toHaveCount(0);
  expect(asked).toEqual(['Clear the 1 note in this view?']);
});

// Clear all clears the notes of the view the page holds, which is the next one as soon as it is asked for, while the
// notes shown are still the last view's. So it is disabled until the next view is drawn (its tree is held here).
test('Clear all is disabled while another view loads', async ({ page, repo, revue }) => {
  setup(repo);
  const s = await revue.start();
  await s.annotate({ file: 'a.txt', line: 5, text: 'first' });
  await s.annotate({ file: 'a.txt', line: 15, text: 'second' });
  await s.open(page, 'mode=working&file=a.txt');
  await expect(page.locator('#notes-next')).toHaveText(/^2 notes/);
  const asked: string[] = [];
  page.on('dialog', d => { asked.push(d.message()); void d.dismiss(); });
  const menu = page.getByRole('button', { name: 'Notes menu' });
  const clearAll = page.getByRole('menuitem', { name: 'Clear all notes in this view' });

  const tree = await hold(page, u => u.pathname === '/api/tree');
  await page.evaluate(() => { location.hash = 'mode=working'; });
  await tree.held;
  await menu.click();
  await expect(clearAll).toBeDisabled();
  await clearAll.click({ force: true }); // past Playwright's wait for it to be enabled
  await page.waitForTimeout(500);
  expect(asked).toEqual([]);
  await page.keyboard.press('Escape');

  // The control: once the overview is drawn, it is enabled and asks first.
  tree.release();
  await expect(viewLabel(page)).toHaveText('All changes · 1 file');
  await menu.click();
  await expect(clearAll).toBeEnabled();
  await clearAll.click();
  await expect.poll(() => asked).toEqual(['Clear all 2 notes in this view?']);
});
