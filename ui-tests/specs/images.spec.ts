// Images on the page: the file view shows one, a binary change that is an image shows its Before and After (or the one
// side an added or deleted file has) in its diff and in the overview, a live update fetches a changed image again, one
// its EXIF orientation turns is sized turned, a JPEG its EXIF resolution scales is sized scaled, one the browser gives
// another size than the server's (a PNG Firefox scales by its EXIF resolution) is drawn at the browser's once it has
// loaded, and an image the server won't show says why without being asked for. An SVG keeps its text views and gets an Image view that draws it,
// where its script never runs, and opened on its own it is a download; one that would draw too much, or embeds an image
// too large, or isn't well-formed XML (with its error and line), says so instead. What the server counts as an image,
// and how it serves one, is tests/images.rs's; status-and-security.spec.ts opens one on its own, and markdown.spec.ts
// has the preview's images.
import type { Locator, Page } from '@playwright/test';
import {
  test, expect, png, jpeg, decodedImage, section, treeRow, recordRequests, focusMain, hashParams, hold, addedLines,
} from '../fixtures';

const RED: [number, number, number, number] = [255, 0, 0, 255];
const BLUE: [number, number, number, number] = [0, 0, 255, 255];
const GREEN: [number, number, number, number] = [0, 128, 0, 255];

const drawn = (img: Locator) => img.evaluate(el => [el.clientWidth, el.clientHeight]);
const bytes = (b: Buffer) => b.length < 1024 ? `${b.length} bytes` : `${(b.length / 1024).toFixed(1)} KB`;

test('the file view shows an image at its own size, under what it is', async ({ page, repo, revue }) => {
  const logo = png(40, 30, RED);
  repo.write('pics/logo.png', logo);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=pics/logo.png');
  await expect(page.locator('#main figcaption')).toHaveText(`PNG · 40 × 30 · ${bytes(logo)}`);
  const img = page.getByRole('img', { name: 'pics/logo.png' });
  expect(await decodedImage(img)).toEqual({ size: [40, 30], pixel: RED });
  // At its own size: not enlarged to fill the pane.
  expect(await drawn(img)).toEqual([40, 30]);
});

test('a changed image shows Before and After in its diff, and either one in the file view', async ({ page, repo, revue }) => {
  const before = png(10, 10, RED), after = png(20, 10, BLUE);
  repo.write('a.png', before);
  repo.commit('initial');
  repo.write('a.png', after);
  const s = await revue.start();
  await s.open(page, 'file=a.png');
  await expect(page.locator('#main .image-diff figcaption'))
    .toHaveText([`Before · PNG · 10 × 10 · ${bytes(before)}`, `After · PNG · 20 × 10 · ${bytes(after)}`]);
  expect(await decodedImage(page.getByRole('img', { name: 'Before: a.png' }))).toEqual({ size: [10, 10], pixel: RED });
  expect(await decodedImage(page.getByRole('img', { name: 'After: a.png' }))).toEqual({ size: [20, 10], pixel: BLUE });

  await page.getByRole('button', { name: 'File', exact: true }).click();
  const one = page.getByRole('img', { name: 'a.png', exact: true });
  expect(await decodedImage(one)).toEqual({ size: [20, 10], pixel: BLUE });
  await page.getByRole('button', { name: 'Before', exact: true }).click();
  await expect(page.locator('#main figcaption')).toHaveText(`PNG · 10 × 10 · ${bytes(before)}`);
  expect(await decodedImage(one)).toEqual({ size: [10, 10], pixel: RED });
});

test('an added or a deleted image shows its one side', async ({ page, repo, revue }) => {
  const old = png(8, 8, RED), added = png(6, 4, BLUE);
  repo.write('gone.png', old);
  repo.write('README.md', 'images\n');
  repo.commit('initial');
  repo.rm('gone.png');
  repo.write('new.png', added);
  const s = await revue.start();
  await s.open(page, 'file=new.png');
  await expect(page.locator('#main .image-diff figcaption')).toHaveText([`PNG · 6 × 4 · ${bytes(added)}`]);
  expect(await decodedImage(page.getByRole('img', { name: 'new.png' }))).toEqual({ size: [6, 4], pixel: BLUE });
  await treeRow(page, 'gone.png').click();
  await expect(page.locator('#main .image-diff figcaption')).toHaveText([`PNG · 8 × 8 · ${bytes(old)}`]);
  expect(await decodedImage(page.getByRole('img', { name: 'gone.png' }))).toEqual({ size: [8, 8], pixel: RED });
});

test('the overview shows each changed image, and fetches it again when it changes', async ({ page, repo, revue }) => {
  repo.write('a.png', png(10, 10, RED));
  repo.write('data.bin', Buffer.from([0, 1, 2, 3]));
  repo.commit('initial');
  repo.write('a.png', png(10, 10, BLUE));
  repo.write('data.bin', Buffer.from([0, 1, 2, 4]));
  const s = await revue.start();
  await s.open(page);
  const sec = section(page, 'a.png');
  expect(await decodedImage(sec.getByRole('img', { name: 'Before: a.png' }))).toEqual({ size: [10, 10], pixel: RED });
  const after = sec.getByRole('img', { name: 'After: a.png' });
  expect(await decodedImage(after)).toEqual({ size: [10, 10], pixel: BLUE });
  // A binary file that isn't an image is only said to have changed.
  await expect(section(page, 'data.bin').locator('.diff-body')).toHaveText('Binary file changed');

  // Another picture of the same size in pixels: its version, and so its address, moves.
  const src = (await after.getAttribute('src'))!;
  repo.write('a.png', png(10, 10, GREEN));
  await expect(after).not.toHaveAttribute('src', src);
  expect(await decodedImage(after)).toEqual({ size: [10, 10], pixel: GREEN });
});

test("a commit's changed image shows that commit's Before and After", async ({ page, repo, revue }) => {
  repo.write('a.png', png(4, 4, RED));
  repo.commit('red');
  repo.write('a.png', png(4, 4, BLUE));
  const sha = repo.commit('blue');
  repo.write('a.png', png(4, 4, GREEN));
  repo.commit('green');
  const s = await revue.start();
  await s.open(page, `mode=commit&ref=${sha}&file=a.png`);
  expect(await decodedImage(page.getByRole('img', { name: 'Before: a.png' }))).toEqual({ size: [4, 4], pixel: RED });
  expect(await decodedImage(page.getByRole('img', { name: 'After: a.png' }))).toEqual({ size: [4, 4], pixel: BLUE });
});

test('the overview draws a tall image at most 360px tall, and Before and After stack where both don\'t fit side by side', async ({ page, repo, revue }) => {
  repo.write('tall.png', png(50, 900, RED));
  repo.commit('initial');
  repo.write('tall.png', png(60, 900, BLUE));
  const s = await revue.start();
  await s.open(page);
  const sec = section(page, 'tall.png');
  const before = sec.getByRole('img', { name: 'Before: tall.png' }), after = sec.getByRole('img', { name: 'After: tall.png' });
  await decodedImage(before);
  await decodedImage(after);
  expect(await drawn(before)).toEqual([20, 360]);
  expect(await drawn(after)).toEqual([24, 360]);

  // The diff view draws them whole, side by side while both fit.
  await sec.locator('.file-head .path').click();
  const [b, a] = [page.getByRole('img', { name: 'Before: tall.png' }), page.getByRole('img', { name: 'After: tall.png' })];
  await decodedImage(a);
  expect([await drawn(b), await drawn(a)]).toEqual([[50, 900], [60, 900]]);
  const top = async (img: Locator) => (await img.boundingBox())!.y;
  expect(await top(a)).toBe(await top(b));
  await page.setViewportSize({ width: 500, height: 800 });
  await expect.poll(async () => await top(a) - await top(b)).toBeGreaterThan(900);
});

// Browsers draw a PNG or a JPEG turned as its EXIF orientation says, so the size it is drawn at is the turned one; with
// the size as stored it would be drawn stretched to the stored width, and far taller.
test('an image its EXIF orientation turns is sized as it is drawn, turned', async ({ page, repo, revue }) => {
  // Stored 40 × 10 and 900 × 50, each turned a quarter turn (orientation 6).
  const photo = png(40, 10, RED, { orientation: 6 });
  repo.write('photo.png', photo);
  repo.commit('initial');
  repo.write('tall.png', png(900, 50, BLUE, { orientation: 6 }));
  const s = await revue.start();
  await s.open(page);
  // The overview draws it at most 360px tall.
  const tall = section(page, 'tall.png').getByRole('img', { name: 'tall.png' });
  expect(await decodedImage(tall)).toEqual({ size: [50, 900], pixel: BLUE });
  expect(await drawn(tall)).toEqual([20, 360]);

  // The file view draws it at its own size, as the caption says.
  await treeRow(page, 'photo.png').click();
  const img = page.getByRole('img', { name: 'photo.png' });
  expect(await decodedImage(img)).toEqual({ size: [10, 40], pixel: RED });
  await expect(page.locator('#main figcaption')).toHaveText(`PNG · 10 × 40 · ${bytes(photo)}`);
  expect(await drawn(img)).toEqual([10, 40]);
});

// Browsers draw a JPEG whose EXIF gives a resolution other than 72 dpi, with pixel dimensions that agree, at that
// density: 144 dpi at half its size. (Chromium doesn't for a PNG's eXIf, though Firefox does: see below.)
test('a JPEG its EXIF resolution scales is sized as it is drawn, scaled', async ({ page, repo, revue }) => {
  // Stored 200 × 100 at 144 dpi: drawn at 100 × 50, and turned (orientation 6) at 50 × 100.
  const photo = jpeg(200, 100, { dpi: 144, pixels: [100, 50] });
  repo.write('photo.jpg', photo);
  repo.write('turned.jpg', jpeg(200, 100, { orientation: 6, dpi: 144, pixels: [100, 50] }));
  repo.commit('initial');
  // Stored 60 × 600: drawn at 30 × 300, under the overview's 360px, which its stored size is over.
  repo.write('tall.jpg', jpeg(60, 600, { dpi: 144, pixels: [30, 300] }));
  const s = await revue.start();
  await s.open(page);
  const tall = section(page, 'tall.jpg').getByRole('img', { name: 'tall.jpg' });
  expect((await decodedImage(tall)).size).toEqual([30, 300]);
  expect(await drawn(tall)).toEqual([30, 300]);

  // The file view draws it at its own size, as the caption says.
  const caption = page.locator('#main figcaption');
  await treeRow(page, 'photo.jpg').click();
  const img = page.getByRole('img', { name: 'photo.jpg' });
  expect((await decodedImage(img)).size).toEqual([100, 50]);
  await expect(caption).toHaveText(`JPEG · 100 × 50 · ${bytes(photo)}`);
  expect(await drawn(img)).toEqual([100, 50]);
  await treeRow(page, 'turned.jpg').click();
  const turned = page.getByRole('img', { name: 'turned.jpg' });
  expect((await decodedImage(turned)).size).toEqual([50, 100]);
  await expect(caption).toContainText('JPEG · 50 × 100');
  expect(await drawn(turned)).toEqual([50, 100]);
});

// ---------------------------------------------------------------- the browser's size
// The server's size is its guess of the size the browser draws an image at, which keeps its place while it loads.
// Where the browser gives it another, the page draws it at that one once it has loaded, and says so in its caption.

// Browsers disagree on a PNG whose EXIF gives a resolution other than 72 dpi: Firefox draws it at that density, as both
// do a JPEG, and Chromium at its stored size, which is the size the server gives.
test('a PNG its EXIF resolution scales in Firefox alone is drawn at the size the browser gives it', async ({ page, repo, revue, browserName }) => {
  const before = png(20, 10, BLUE), shot = png(200, 100, RED, { dpi: 144, pixels: [100, 50] });
  repo.write('shot.png', before);
  repo.write('README.md', '# Shot\n\n![shot](shot.png)\n');
  repo.commit('initial');
  repo.write('shot.png', shot);
  const s = await revue.start();
  await s.open(page);
  const [w, ht] = browserName === 'firefox' ? [100, 50] : [200, 100];
  const sec = section(page, 'shot.png'), after = sec.getByRole('img', { name: 'After: shot.png' });
  expect((await decodedImage(after)).size).toEqual([w, ht]);
  await expect(sec.locator('figcaption')).toHaveText([`Before · PNG · 20 × 10 · ${bytes(before)}`, `After · PNG · ${w} × ${ht} · ${bytes(shot)}`]);
  await expect.poll(() => drawn(after)).toEqual([w, ht]);

  // The file view.
  await treeRow(page, 'shot.png').click();
  await page.getByRole('button', { name: 'File', exact: true }).click();
  const one = page.getByRole('img', { name: 'shot.png', exact: true });
  expect((await decodedImage(one)).size).toEqual([w, ht]);
  await expect(page.locator('#main figcaption')).toHaveText(`PNG · ${w} × ${ht} · ${bytes(shot)}`);
  await expect.poll(() => drawn(one)).toEqual([w, ht]);

  // The preview, which has no caption.
  await treeRow(page, 'README.md').click();
  const pic = page.locator('#main .markdown').getByRole('img', { name: 'shot' });
  expect((await decodedImage(pic)).size).toEqual([w, ht]);
  await expect.poll(() => drawn(pic)).toEqual([w, ht]);
});

// Every image's size comes back from /api/file as `sizes` gives it by path, as from a server whose guess is wrong.
async function misreport(page: Page, sizes: Record<string, [number, number]>) {
  await page.route(u => u.pathname === '/api/file', async route => {
    const response = await route.fetch();
    const json = await response.json();
    const size = sizes[new URL(route.request().url()).searchParams.get('path')!];
    if (json.image && size) [json.image.width, json.image.height] = size;
    await route.fulfill({ status: response.status(), json }); // not the server's headers: its content-length is the old body's
  });
}

test('an image the server sizes wrong is drawn, once it has loaded, at the size the browser gives it', async ({ page, repo, revue }) => {
  const small = png(10, 10, RED), pic = png(40, 30, RED), tall = png(60, 900, BLUE), wide = png(100, 50, BLUE);
  repo.write('pic.png', pic);
  repo.write('tall.png', small);
  repo.write('wide.png', small);
  repo.commit('initial');
  repo.write('tall.png', tall);
  repo.write('wide.png', wide);
  const s = await revue.start();
  // tall.png said to be short (so not held to 360px in the overview), wide.png said to be tall (so held to it).
  await misreport(page, { 'pic.png': [123, 45], 'tall.png': [90, 60], 'wide.png': [50, 900] });
  await s.open(page);
  // The overview holds each to 360px tall by the browser's size.
  const tallSec = section(page, 'tall.png'), wideSec = section(page, 'wide.png');
  const tallAfter = tallSec.getByRole('img', { name: 'After: tall.png' }), wideAfter = wideSec.getByRole('img', { name: 'After: wide.png' });
  expect((await decodedImage(tallAfter)).size).toEqual([60, 900]);
  expect((await decodedImage(wideAfter)).size).toEqual([100, 50]);
  await expect(tallSec.locator('figcaption')).toHaveText([`Before · PNG · 10 × 10 · ${bytes(small)}`, `After · PNG · 60 × 900 · ${bytes(tall)}`]);
  await expect(wideSec.locator('figcaption')).toHaveText([`Before · PNG · 10 × 10 · ${bytes(small)}`, `After · PNG · 100 × 50 · ${bytes(wide)}`]);
  await expect.poll(() => drawn(tallAfter)).toEqual([24, 360]);
  await expect.poll(() => drawn(wideAfter)).toEqual([100, 50]);
  await expect.poll(() => drawn(wideSec.getByRole('img', { name: 'Before: wide.png' }))).toEqual([10, 10]);

  // The diff view draws Before and After whole.
  await treeRow(page, 'tall.png').click();
  const after = page.getByRole('img', { name: 'After: tall.png' });
  await decodedImage(after);
  await expect(page.locator('#main figcaption')).toHaveText([`Before · PNG · 10 × 10 · ${bytes(small)}`, `After · PNG · 60 × 900 · ${bytes(tall)}`]);
  await expect.poll(() => drawn(after)).toEqual([60, 900]);
  await expect(after).toHaveAttribute('width', '60');
  await expect(after).toHaveAttribute('height', '900');

  // The file view.
  await treeRow(page, 'pic.png').click();
  const one = page.getByRole('img', { name: 'pic.png' });
  expect((await decodedImage(one)).size).toEqual([40, 30]);
  await expect(page.locator('#main figcaption')).toHaveText(`PNG · 40 × 30 · ${bytes(pic)}`);
  await expect.poll(() => drawn(one)).toEqual([40, 30]);
});

test('a preview draws an image the server sizes wrong at the size the browser gives it, in the width the document gave', async ({ page, repo, revue }) => {
  repo.write('pic.png', png(40, 20, RED));
  repo.write('README.md', '# Pics\n\n![pic](pic.png)\n\n<img src="pic.png" alt="sized" width="30">\n');
  repo.commit('initial');
  const s = await revue.start();
  await misreport(page, { 'pic.png': [123, 45] });
  await s.open(page, 'file=README.md&view=preview');
  const md = page.locator('#main .markdown');
  const pic = md.getByRole('img', { name: 'pic' }), sized = md.getByRole('img', { name: 'sized' });
  expect((await decodedImage(pic)).size).toEqual([40, 20]);
  await expect(pic).toHaveAttribute('width', '40');
  await expect(pic).toHaveAttribute('height', '20');
  await expect.poll(() => drawn(pic)).toEqual([40, 20]);
  // Its width and height give its proportions; the width the document gave stays.
  await decodedImage(sized);
  await expect(sized).toHaveAttribute('width', '40');
  await expect(sized).toHaveAttribute('height', '20');
  expect(await sized.evaluate(el => el.style.width)).toBe('30px');
  expect(await drawn(sized)).toEqual([30, 15]);
});

// What the page changes of an image (its attributes) and its figure (the caption) once it has loaded: `show` puts it on
// the page, whose request for it is held until the watch has started.
async function changesOnLoad(page: Page, img: Locator, show: () => Promise<unknown>): Promise<string[]> {
  const request = await hold(page, u => u.pathname === '/api/image');
  await show();
  await request.held;
  await img.evaluate((el: HTMLImageElement & { changed?: Promise<string[]> }) => {
    const changes: string[] = [];
    const seen = (records: MutationRecord[]) => changes.push(...records.map(r => r.attributeName ?? r.type));
    const watch = new MutationObserver(seen);
    watch.observe(el.closest('figure') ?? el, { attributes: true, characterData: true, childList: true, subtree: true });
    // A listener added after the page's runs after it.
    el.changed = new Promise(loaded => el.addEventListener('load', loaded, { once: true }))
      .then(() => { seen(watch.takeRecords()); watch.disconnect(); return changes; });
  });
  request.release();
  return img.evaluate((el: HTMLImageElement & { changed?: Promise<string[]> }) => el.changed!);
}

// The control: an image whose size the server has right is left as it is, and so is an SVG with only a viewBox, which
// the server sizes by it, though the browser reports a default size for it (300 × 150 here) as its natural size.
test('an image the server sizes right is left as it is once it has loaded, and so is an SVG', async ({ page, repo, revue }) => {
  const ok = png(40, 30, RED), icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50" fill="#00f"/></svg>\n';
  repo.write('ok.png', ok);
  repo.write('icon.svg', icon);
  repo.write('off.png', png(40, 30, BLUE));
  repo.commit('initial');
  const s = await revue.start();
  await misreport(page, { 'off.png': [123, 45] });
  const caption = page.locator('#main figcaption');

  const okImg = page.getByRole('img', { name: 'ok.png' });
  expect(await changesOnLoad(page, okImg, () => s.open(page, 'file=ok.png'))).toEqual([]);
  await expect(caption).toHaveText(`PNG · 40 × 30 · ${bytes(ok)}`);
  expect(await drawn(okImg)).toEqual([40, 30]);

  const iconImg = page.getByRole('img', { name: 'icon.svg' });
  expect(await changesOnLoad(page, iconImg, () => treeRow(page, 'icon.svg').click())).toEqual([]);
  expect(await iconImg.evaluate((el: HTMLImageElement) => [el.naturalWidth, el.naturalHeight])).toEqual([300, 150]);
  await expect(caption).toHaveText(`SVG · 100 × 50 · ${icon.length} bytes`);
  expect(await drawn(iconImg)).toEqual([100, 50]);

  // One the server sizes wrong is changed, as the watch sees.
  const offImg = page.getByRole('img', { name: 'off.png' });
  expect(await changesOnLoad(page, offImg, () => treeRow(page, 'off.png').click())).toEqual(expect.arrayContaining(['width', 'height', 'childList']));
  expect(await drawn(offImg)).toEqual([40, 30]);
});

test('an image too large to show says why, and is never asked for', async ({ page, repo, revue }) => {
  // A PNG whose header claims 10,000 × 10,000 pixels, 400 MB decoded.
  const huge = png(1, 1);
  huge.writeUInt32BE(10_000, 16);
  huge.writeUInt32BE(10_000, 20);
  repo.write('huge.png', huge);
  repo.write('README.md', 'images\n');
  repo.commit('initial');
  const s = await revue.start();
  const asked = recordRequests(page, /^\/api\/image$/);
  await s.open(page, 'file=huge.png');
  await expect(page.locator('#main figure.image')).toHaveText(
    `PNG · 10000 × 10000 · ${bytes(huge)}Image too large to show (10000 × 10000 pixels; the most is 50 million)`);
  expect(asked).toEqual([]);
});

test('a plain folder opens its images in the file view', async ({ page, folder, revue }) => {
  folder.write('pic.png', png(5, 7, BLUE));
  const s = await revue.start({ folder });
  await s.open(page, 'file=pic.png');
  expect(await decodedImage(page.getByRole('img', { name: 'pic.png' }))).toEqual({ size: [5, 7], pixel: BLUE });
  await expect(page.locator('#main .file-head .seg')).toHaveCount(0);
});

// ---------------------------------------------------------------- SVG
const svg = (fill: string, inside = '', attrs = '') =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6" viewBox="0 0 8 6"${attrs}>${inside}<rect width="8" height="6" fill="${fill}"/></svg>\n`;
const viewOf = (page: import('@playwright/test').Page) => hashParams(page).view ?? 'diff';

test('an SVG opens drawn in its Image view, and f goes between that and its text', async ({ page, repo, revue }) => {
  const logo = svg('#ff0000');
  repo.write('logo.svg', logo);
  repo.write('README.md', 'images\n');
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=logo.svg');
  await expect(page.getByRole('button', { name: 'Image', exact: true })).toHaveClass(/\bon\b/);
  await expect(page.locator('#main figcaption')).toHaveText(`SVG · 8 × 6 · ${logo.length} bytes`);
  expect(await decodedImage(page.getByRole('img', { name: 'logo.svg' }))).toEqual({ size: [8, 6], pixel: RED });
  await focusMain(page);
  await page.keyboard.press('f');
  await expect.poll(() => viewOf(page)).toBe('file');
  await expect(page.locator('#main table.code tr[data-n="1"] td.src')).toContainText('<svg xmlns');
  await page.keyboard.press('f');
  await expect.poll(() => viewOf(page)).toBe('preview');
  await expect(page.getByRole('img', { name: 'logo.svg' })).toBeVisible();
});

test('a changed SVG shows its text diff, and its Image view draws either side', async ({ page, repo, revue }) => {
  repo.write('a.svg', svg('#ff0000'));
  repo.commit('initial');
  repo.write('a.svg', svg('#0000ff'));
  const s = await revue.start();
  await s.open(page, 'file=a.svg');
  await expect(addedLines(page.locator('#main')).first()).toContainText('#0000ff');
  await page.getByRole('button', { name: 'Image', exact: true }).click();
  const img = page.getByRole('img', { name: 'a.svg' });
  expect(await decodedImage(img)).toEqual({ size: [8, 6], pixel: BLUE });
  await page.getByRole('button', { name: 'Before', exact: true }).click();
  await expect.poll(async () => (await decodedImage(img)).pixel).toEqual(RED);
});

// The page takes an SVG's size from the server, never the browser's (which it makes up for one with only a viewBox),
// so the server's has to be the size the browser draws it at: here in em, the default font's 16px, and in mm, with the
// height the viewBox's proportions give.
test('an SVG sized in other units, or by one side and its viewBox, is drawn at the size the browser gives it', async ({ page, repo, revue }) => {
  const em = '<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="5em" viewBox="0 0 100 50"><rect width="100" height="50" fill="#00f"/></svg>\n';
  const side = '<svg xmlns="http://www.w3.org/2000/svg" width="25.4mm" viewBox="0 0 100 50"><rect width="100" height="50" fill="#f00"/></svg>\n';
  repo.write('em.svg', em);
  repo.write('side.svg', side);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page);
  for (const [path, text, size] of [['em.svg', em, [160, 80]], ['side.svg', side, [96, 48]]] as const) {
    await treeRow(page, path).click();
    const img = page.getByRole('img', { name: path });
    await decodedImage(img);
    // The size the browser draws it at with no width or height of its own, out of the page's layout.
    const own = await img.evaluate(async (el: HTMLImageElement) => {
      const free = new Image();
      free.src = el.src;
      await free.decode();
      free.style.position = 'absolute';
      document.body.append(free);
      const { width, height } = free.getBoundingClientRect();
      free.remove();
      return [width, height];
    });
    expect(own).toEqual(size);
    await expect(page.locator('#main figcaption')).toHaveText(`SVG · ${size[0]} × ${size[1]} · ${text.length} bytes`);
    expect(await drawn(img)).toEqual(size);
  }
});

// In an <img> a browser runs none of an SVG's script and loads no other file it names; opened on its own, revue hands
// it over as a download, so it is never a document of revue's origin either.
test("an SVG's script never runs, in its Image view or opened on its own", async ({ page, repo, revue }) => {
  repo.write('evil.svg', svg('#ff0000', `<script>fetch('/api/info?from=svg-script')</script><image href="/api/info?from=svg-image"/>`,
    ` onload="fetch('/api/info?from=svg-onload')"`));
  repo.commit('initial');
  const s = await revue.start();
  const probes = recordRequests(page, /^\/api\/info$/);
  const fromSvg = () => probes.filter(u => new URL(u).searchParams.has('from'));
  await s.open(page, 'file=evil.svg');
  const img = page.getByRole('img', { name: 'evil.svg' });
  expect(await decodedImage(img)).toEqual({ size: [8, 6], pixel: RED });
  const src = new URL((await img.getAttribute('src'))!, s.url).href;
  const download = page.waitForEvent('download');
  await page.goto(src).catch(() => {}); // the navigation ends in a download, which goto reports as an error
  expect((await download).url()).toBe(src);
  // Give a request that was going to be made time to show up, then check that none did, with a control.
  await page.waitForTimeout(500);
  expect(fromSvg()).toEqual([]);
  await page.evaluate(() => fetch('/api/info?from=control'));
  await expect.poll(fromSvg).toHaveLength(1);
});

test("an SVG that isn't one, by its text, is shown as text and not drawn", async ({ page, repo, revue }) => {
  repo.write('page.svg', '<!doctype html><html><body><svg></svg></body></html>\n');
  repo.write('README.md', 'images\n');
  repo.commit('initial');
  const s = await revue.start();
  const asked = recordRequests(page, /^\/api\/image$/);
  await s.open(page, 'file=page.svg');
  await expect(page.locator('#main .file-body .placeholder')).toHaveText(
    "Not drawn: an SVG's text has to start with an <svg> element, with no doctype that defines entities");
  await page.getByRole('button', { name: 'File', exact: true }).click();
  await expect(page.locator('#main table.code tr[data-n="1"] td.src')).toContainText('<!doctype html>');
  expect(asked).toEqual([]);
});

// A few <use>s, each a copy of a group of <use>s, draw many elements: 5 levels of 10 draw 100,000 squares, in about a
// kilobyte, which took browsers seconds to draw. The server refuses it, so neither the Image view nor a preview asks.
test('an SVG that would draw too much says why, and is never asked for', async ({ page, repo, revue }) => {
  const uses = (n: number) => `<use href="#l${n}"/>`.repeat(10);
  let defs = '<rect id="l0" width="1" height="1"/>';
  for (let n = 1; n < 5; n++) defs += `<g id="l${n}">${uses(n - 1)}</g>`;
  const deep = svg('#ff0000', `<defs>${defs}</defs><g>${uses(4)}</g>`);
  repo.write('deep.svg', deep);
  repo.write('README.md', '# Deep\n\n![deep](deep.svg)\n');
  repo.commit('initial');
  const s = await revue.start();
  const asked = recordRequests(page, /^\/api\/image$/);
  const why = 'Image too complex to draw (over 50,000 elements, counting the copies its <use>s draw)';
  await s.open(page, 'file=deep.svg');
  await expect(page.getByRole('button', { name: 'Image', exact: true })).toHaveClass(/\bon\b/);
  // No size: it was read only up to the refusal, and what came after could change it.
  await expect(page.locator('#main figure.image')).toHaveText(`SVG · ${bytes(Buffer.from(deep))}${why}`);
  // Its text is there as for any SVG.
  await page.getByRole('button', { name: 'File', exact: true }).click();
  await expect(page.locator('#main table.code tr[data-n="1"] td.src')).toContainText('<svg xmlns');
  // A markdown preview leaves it a placeholder, which says why.
  await treeRow(page, 'README.md').click();
  await expect(page.locator('#main .markdown').getByText('[image: deep]')).toHaveAttribute('title', why);
  expect(asked).toEqual([]);
});

// An SVG is text, so the most it may be is the text limit, 2 MB, not the 20 MB of other images.
test('an SVG too large to show says the most is 2 MB', async ({ page, repo, revue }) => {
  const big = svg('#ff0000', `<!-- ${'x'.repeat(2.1 * (1 << 20))} -->`);
  repo.write('big.svg', big);
  repo.write('README.md', 'images\n');
  repo.commit('initial');
  const s = await revue.start();
  const asked = recordRequests(page, /^\/api\/image$/);
  await s.open(page, 'file=big.svg');
  await expect(page.getByRole('button', { name: 'Image', exact: true })).toHaveClass(/\bon\b/);
  const mb = `${(big.length / (1 << 20)).toFixed(1)} MB`;
  await expect(page.locator('#main figure.image')).toHaveText(`SVG · ${mb}Image too large to show (${mb}; the most is 2 MB)`);
  expect(asked).toEqual([]);
});

// An <img> loads no other file, but it does load data: URLs, so an SVG can draw a picture written into it, as design
// tools write them. One whose header claims 100,000 × 100,000 pixels is refused as the file would be, so neither the
// Image view nor a preview asks for the SVG.
test('an SVG that embeds an image too large to show says why, and is never asked for', async ({ page, repo, revue }) => {
  const embedding = (pic: Buffer) => '<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6" viewBox="0 0 8 6">'
    + `<image width="8" height="6" preserveAspectRatio="none" href="data:image/png;base64,${pic.toString('base64')}"/></svg>\n`;
  const huge = png(1, 1);
  huge.writeUInt32BE(100_000, 16);
  huge.writeUInt32BE(100_000, 20);
  const holder = embedding(huge);
  repo.write('small.svg', embedding(png(8, 6, BLUE)));
  repo.write('holder.svg', holder);
  repo.write('README.md', '# Holder\n\n![holder](holder.svg)\n');
  repo.commit('initial');
  const s = await revue.start();
  // A small one is drawn, its picture with it.
  await s.open(page, 'file=small.svg');
  expect(await decodedImage(page.getByRole('img', { name: 'small.svg' }))).toEqual({ size: [8, 6], pixel: BLUE });

  const asked = recordRequests(page, /^\/api\/image$/);
  const why = "Image not shown: it embeds an image revue can't check, or images of over 50 million pixels in all";
  await treeRow(page, 'holder.svg').click();
  await expect(page.getByRole('button', { name: 'Image', exact: true })).toHaveClass(/\bon\b/);
  await expect(page.locator('#main figure.image')).toHaveText(`SVG · ${bytes(Buffer.from(holder))}${why}`);
  await treeRow(page, 'README.md').click();
  await expect(page.locator('#main .markdown').getByText('[image: holder]')).toHaveAttribute('title', why);
  expect(asked).toEqual([]);
});

// Browsers draw nothing of an SVG that isn't well-formed XML, which the page could only say it couldn't load. The
// server refuses it with its first error and that error's line, which the Image view says on the side that has it, and
// a preview's placeholder too, again once an edit moves the error. Its side is never asked for.
test("an SVG that isn't well formed says why and where, and is never asked for", async ({ page, repo, revue }) => {
  repo.write('a.svg', svg('#ff0000'));
  repo.write('README.md', '# A\n\n![a](a.svg)\n');
  repo.commit('initial');
  const broken = '<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6">\n<g>\n</h>\n</svg>\n';
  repo.write('a.svg', broken);
  const s = await revue.start();
  const asked = recordRequests(page, /^\/api\/image$/);
  const why = "Not a well-formed SVG: </h> doesn't close <g> (line 3)";
  const placeholder = page.locator('#main .markdown').getByText('[image: a]');
  await s.open(page, 'file=README.md');
  await expect(placeholder).toHaveAttribute('title', why);
  await treeRow(page, 'a.svg').click();
  await expect(addedLines(page.locator('#main')).first()).toContainText('<svg');
  await page.getByRole('button', { name: 'Image', exact: true }).click();
  await expect(page.locator('#main figure.image')).toHaveText(`SVG · ${bytes(Buffer.from(broken))}${why}`);
  // Before the change, it is drawn.
  await page.getByRole('button', { name: 'Before', exact: true }).click();
  expect(await decodedImage(page.getByRole('img', { name: 'a.svg' }))).toEqual({ size: [8, 6], pixel: RED });
  // Back in the preview, drawn with the preview's code loaded (it loads with the first one), an edit that moves the
  // error draws the placeholder again, though the image is refused as it was.
  await treeRow(page, 'README.md').click();
  await expect(placeholder).toHaveAttribute('title', why);
  repo.write('a.svg', '<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6">\n<rect width=8/>\n</svg>\n');
  await expect(placeholder).toHaveAttribute('title', "Not a well-formed SVG: the value of width on <rect> isn't quoted (line 2)");
  expect(asked.filter(u => new URL(u).searchParams.get('side') !== 'old')).toEqual([]);
});

test("a markdown preview draws the repo's SVGs, and a plain folder's SVG opens drawn", async ({ page, folder, revue }) => {
  folder.write('logo.svg', svg('#0000ff'));
  folder.write('README.md', '# Folder\n\n![logo](logo.svg)\n');
  const s = await revue.start({ folder });
  await s.open(page);
  expect(await decodedImage(page.locator('#main .markdown').getByRole('img', { name: 'logo' }))).toEqual({ size: [8, 6], pixel: BLUE });
  await treeRow(page, 'logo.svg').click();
  await expect(page.locator('#main .file-head .seg button')).toHaveText(['File', 'Image']);
  expect(await decodedImage(page.getByRole('img', { name: 'logo.svg' }))).toEqual({ size: [8, 6], pixel: BLUE });
});
