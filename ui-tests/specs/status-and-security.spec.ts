// The live-updates status (the card under the dot, Offline when the server goes away, and reconnecting with a
// refresh when it comes back on the same port, the file watcher stopping, and the "Server out of date" badge), the
// Host check that blocks DNS rebinding as a real browser meets it (tests/host.rs checks every Host header case), that
// a page on another site learns nothing from the API, that an image opened on its own is sandboxed, and that the page
// and its views run without a single Content-Security-Policy violation.
import * as fs from 'node:fs';
import * as http from 'node:http';
import type { AddressInfo } from 'node:net';
import type { Page } from '@playwright/test';
import { test, expect, lines, png, treeRow, focusMain } from '../fixtures';

// The value of a row of the status card (the card must be open).
const cardValue = (page: Page, label: string) => page.locator(`#status-card dt:text-is("${label}") + dd`);

// Another website: a plain server of the test's own, with no CSP, serving an empty page at / and found.js, and a 404
// for anything else.
async function otherSite(): Promise<{ port: number; close: () => Promise<void> }> {
  const server = http.createServer((req, res) => {
    if (req.url === '/') res.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><title>another site</title>');
    else if (req.url === '/found.js') res.writeHead(200, { 'content-type': 'text/javascript' }).end('// found');
    else res.writeHead(404).end();
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  return {
    port: (server.address() as AddressInfo).port,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(() => resolve()); }),
  };
}

// Whether a <script> pointing at url fires load or error: what a page on any site learns about a URL it can't read.
const probe = (page: Page, url: string) => page.evaluate(u => new Promise<string>(resolve => {
  const s = document.createElement('script');
  s.onload = () => resolve('load');
  s.onerror = () => resolve('error');
  s.src = u;
  document.head.append(s);
}), url);

test('hovering the live dot shows the server status card', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page);
  const info = await s.json('/api/server');
  expect(info.stale).toBe(false);

  const card = page.locator('#status-card');
  await expect(card).toBeHidden();
  await page.locator('#live').hover();
  await expect(card).toBeVisible();
  await expect(page.locator('#live')).toHaveAttribute('aria-expanded', 'true');
  await expect(card.getByRole('heading', { name: 'revue server' })).toBeVisible();
  // Every row, in order, each value under its own label.
  const value = (label: string) => card.locator(`dt:text-is("${label}") + dd`);
  await expect(card.locator('dt')).toHaveText(['Live updates', 'Version', 'Server', 'Runs', 'Started', 'Tabs', 'Notes']);
  await expect(value('Live updates')).toHaveText('on');
  await expect(value('Version')).toHaveText(info.version + (info.commit ? ` · ${info.commit}${info.modified ? ' (modified)' : ''}` : ''));
  await expect(value('Server')).toHaveText(`port ${s.port} · pid ${s.proc.pid}`);
  await expect(value('Runs')).toHaveText('started by hand');
  await expect(value('Started')).toHaveText('just now');
  await expect(value('Tabs')).toHaveText('1 open, 1 following agent navigation');
  await expect(value('Notes')).toHaveText('0 kept in memory');

  await page.locator('#tree').hover();
  await expect(card).toBeHidden();
});

// A server whose code changed on disk after it started (a rebuild or upgrade) says so in /api/info and /api/server,
// which the page asks again on every `git` live update. REVUE_CODE_FILE stands in for the binary (as in
// tests/server.rs). layout.spec.ts checks the badge at narrower widths, where it shortens to "Restart".
test('the "Server out of date" badge and the status card show once the server\'s code changes on disk', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const code = test.info().outputPath('revue.bin');
  fs.writeFileSync(code, 'v1');
  const s = await revue.start({ env: { REVUE_CODE_FILE: code } });
  await s.open(page);
  const badge = page.locator('#stale');
  await expect(badge).toBeHidden();
  await page.locator('#live').hover();
  await expect(cardValue(page, 'Server')).toHaveText(`port ${s.port} · pid ${s.proc.pid}`);
  await expect(cardValue(page, 'Version')).not.toContainText('out of date');
  await page.locator('#tree').hover();

  // A new file in its place: another inode, size and mtime.
  fs.rmSync(code);
  fs.writeFileSync(code, 'v2, rebuilt');
  expect((await s.json('/api/info')).stale).toBe(true);
  repo.commit('a commit, for a live update');
  const STALE = 'Server out of date: restart revue';
  await expect(badge).toBeVisible();
  await expect(badge).toHaveText(STALE, { useInnerText: true });
  await expect(badge).toHaveAccessibleName(STALE);
  await page.locator('#live').hover();
  await expect(cardValue(page, 'Version')).toContainText('· out of date, restart revue');
});

// With the file watcher stopped, the event stream is still connected (the dot says Live), but nothing will come
// through it: the card and the empty state say so, and point at r.
test('with the file watcher stopped, the status card and the empty state say live updates are off', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page);
  const watching = page.locator('#main .empty-state .watching');
  await expect(watching).toHaveText('Watching the working tree. Edits show up here as they happen.');
  await page.locator('#live').hover();
  await expect(cardValue(page, 'Server')).toHaveText(`port ${s.port} · pid ${s.proc.pid}`);
  await expect(cardValue(page, 'Live updates')).toHaveText('on');
  await page.locator('#tree').hover();

  await s.debug({ watchError: 'off for this test' });
  expect((await s.json('/api/server')).watcher).toBe('off');
  await page.locator('#live').hover();
  await expect(cardValue(page, 'Live updates')).toHaveText('off: the file watcher stopped, press r to refresh');
  await expect(page.locator('#live-label')).toHaveText('Live');
  await page.locator('#tree').hover();

  // No live update will redraw the empty state; r does.
  await focusMain(page);
  await page.keyboard.press('r');
  await expect(watching).toHaveText('Live updates are off. Press r to refresh.');
});

test('the page goes Offline when the server stops and refreshes when it is back on the same port', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  repo.write('a.txt', lines(3, 1));
  const s = await revue.start();
  await s.open(page);
  await expect(treeRow(page, 'a.txt')).toBeVisible();

  await s.stop();
  await expect(page.locator('#live-label')).toHaveText('Offline');
  await page.locator('#live').hover();
  await expect(page.locator('#status-card')).toContainText('disconnected, retrying');
  await page.locator('#tree').hover();

  // A change made while the server is down sends no event; the reconnect must pick it up.
  repo.write('later.txt', 'made while offline\n');
  const again = await revue.start({ port: s.port });
  expect(again.port).toBe(s.port);
  await expect(page.locator('#live-label')).toHaveText('Live', { timeout: 20_000 });
  await expect(treeRow(page, 'later.txt')).toBeVisible();
  await expect(page.locator('#summary')).toHaveText('2 files +2 −1');
});

test('a browser that reaches the server under another name gets 403', async ({ playwright, browserName, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  // Make rebind.test resolve to 127.0.0.1 in a browser of our own, as a rebound DNS name would.
  const browser = browserName === 'chromium'
    ? await playwright.chromium.launch({ args: ['--host-resolver-rules=MAP rebind.test 127.0.0.1'] })
    : await playwright.firefox.launch({ firefoxUserPrefs: { 'network.dns.localDomains': 'rebind.test' } });
  try {
    const page = await browser.newPage();
    const res = await page.goto(`http://rebind.test:${s.port}/`);
    expect(res!.status()).toBe(403);
    await expect(page.locator('body')).toHaveText('forbidden');
    // The control: the same browser reaches the page by its own name.
    const ok = await page.goto(s.url);
    expect(ok!.status()).toBe(200);
    await expect(page.locator('#live-label')).toHaveText('Live');
  } finally {
    await browser.close();
  }
});

test.describe('a page on another site', () => {
  // The browser logs each load the server refuses or the browser blocks, and refusing them is the point.
  test.use({ allowConsoleErrors: true });

  // Browsers already keep the JSON from such a page (opaque response blocking), but the server still ran the request:
  // read the file, refreshed the index, took the time it took. Now it refuses it before doing anything.
  test("can't tell from the API which files exist, nor make the server read them", async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(3));
    repo.commit('initial');
    const s = await revue.start();
    const site = await otherSite();
    const readsOfA = async () => (await s.debug()).reads.filter((r: { path: string }) => r.path.endsWith('a.txt'));
    try {
      // localhost is another site than 127.0.0.1, and 127.0.0.1 on another port is the same site but another origin.
      for (const origin of [`http://localhost:${site.port}`, `http://127.0.0.1:${site.port}`]) {
        await page.goto(origin + '/');
        // The control: the probe tells a file that is there from one that isn't, where the page may load both.
        expect(await probe(page, origin + '/found.js')).toBe('load');
        expect(await probe(page, origin + '/missing.js')).toBe('error');
        for (const path of ['a.txt', 'missing.txt']) {
          expect(await probe(page, `${s.url}api/file?mode=working&path=${path}`), `${origin}, ${path}`).toBe('error');
        }
      }
      expect(await readsOfA()).toEqual([]);
      // The control: the same request from revue's own page reads it.
      await s.open(page, 'file=a.txt&view=file');
      await expect(page.locator('#main tr[data-n="3"]')).toBeVisible();
      expect(await readsOfA()).not.toEqual([]);
    } finally {
      await site.close();
    }
  });
});

// Open image in new tab: the image is a document of its own, served with a CSP that sandboxes it, so it has an opaque
// origin rather than revue's and runs no script. The browser's own image view still works under it, with no violation.
test.describe('an image opened on its own', () => {
  // Chromium logs each of the test's own scripts (the guard's, Playwright's) that the sandbox keeps from running, which
  // is the sandbox at work. Any other console error still fails the test, and CSP violations are still the guard's.
  test.use({ allowConsoleErrors: true });

  test('is shown, in a sandbox', async ({ page, repo, revue }) => {
    const errors: string[] = [];
    page.on('console', m => { if (m.type() === 'error' && !/because the document's frame is sandboxed/.test(m.text())) errors.push(m.text()); });
    repo.write('dot.png', png(3, 2));
    repo.commit('initial');
    const s = await revue.start();
    const res = await page.goto(`${s.url}api/image?mode=working&path=dot.png`);
    expect(res!.status()).toBe(200);
    expect(res!.headers()['content-security-policy']).toBe("default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox");
    const img = page.locator('img');
    await expect(img).toBeVisible();
    await expect.poll(() => img.evaluate((el: HTMLImageElement) => [el.naturalWidth, el.naturalHeight])).toEqual([3, 2]);
    expect(await img.evaluate(el => el.ownerDocument.defaultView!.origin)).toBe('null');
    expect(errors).toEqual([]);
  });
});

test('the page and its views cause no CSP violations or console errors', async ({ page, repo, revue }) => {
  repo.write('a.js', 'export const a = 1;\nexport function f() {\n  return a;\n}\n');
  repo.write('doc.md', '# Doc\n\nSome *text* with `code`, a [link](a.js) and\n\n```js\nconst x = 1;\n```\n');
  repo.commit('initial');
  repo.write('a.js', 'export const a = 2;\nexport function f() {\n  return a;\n}\n');
  repo.write('doc.md', '# Doc\n\nChanged *text* with `code`, a [link](a.js) and\n\n```js\nconst x = 1;\n```\n');
  const s = await revue.start();

  // The guard fails the test on a CSP violation (its console message or its securitypolicyviolation event) or a
  // console error once the test ends. This checks the console after each step as well, so a failure names the view
  // that caused it, and it counts every console error, including the 4xx answers the guard lets through: none of
  // these views should get one.
  const errors: string[] = [];
  page.on('console', m => { if (m.type() === 'error') errors.push(m.text()); });
  page.on('pageerror', e => errors.push(e.message));
  const check = (what: string) => expect(errors, `console errors or CSP violations after ${what}`).toEqual([]);

  await s.open(page);
  await expect(page.locator('#main table.code').first()).toBeVisible(); // the overview's diffs
  check('load');

  await treeRow(page, 'a.js').click();
  await expect(page.locator('#main table.code tr.add')).toHaveCount(1);
  check('opening a diff');

  await page.getByRole('button', { name: 'File', exact: true }).click();
  await expect(page.locator('#main table.code').getByText('export function f')).toBeVisible();
  check('the file view');

  await treeRow(page, 'doc.md').click();
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(page.locator('#main .markdown').getByRole('heading', { name: 'Doc' })).toBeVisible();
  await expect(page.locator('#main .markdown code.language-js')).toBeVisible();
  check('a markdown preview');
});
