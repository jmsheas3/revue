// The markdown preview: which view a .md file opens in and how `f` cycles views, and that a preview of untrusted
// markdown loads nothing but the repo's own images (anything else it names stays text, with no request for it; a remote
// image is a link), runs nothing (scripts and on* handlers are stripped) and keeps links inside revue (relative links
// open the file, http links open a new tab, anchors scroll below the sticky file header, other schemes go nowhere) with
// read-only checkboxes. The repo's images come from the side the preview shows, follow a live update of the image
// alone, and <picture> sources and #gh-*-mode-only images follow the page's theme. One the browser can't load or decode
// is its placeholder, saying so. Headings get md- prefixed slug ids (numbered when repeated) that anchors find, and the
// document keeps only the classes the preview sets itself.
import { test, expect, lines, focusMain, hashParams, recordRequests, png, decodedImage, pickTheme, type Repo } from '../fixtures';
import type { Page } from '@playwright/test';

// Every line of the README past the payloads, so the anchor at the end is well below the first screen.
const filler = Array.from({ length: 80 }, (_, i) => `Paragraph ${i + 1} of filler text.\n`).join('\n');

const README = `# Project title

Pictures: ![logo](http://img.example.test/logo.png) and ![local pic](pic.png).

<img src="pic2.png">
<picture><source srcset="pic3.png"><img src="pic3.png"></picture>
<table background="pic4.png"><tr><td>cell</td></tr></table>
<input type="image" src="pic5.png">
<video poster="pic6.png"></video>

Links: [other doc](docs/other.md), [main code](src/main.js), [external](https://example.com/page),
[mail me](mailto:someone@example.com), [far away](#far-section).

- [ ] open task
- [x] done task

<div><input type="checkbox"> raw box</div>

${filler}
## Far section

${filler}
`;

// Payloads that would run script if they reached the page. on* handlers are in a file of their own, so their test
// also checks that they cause no CSP report. Nothing checks window.__mdPwned: the CSP lets no inline script, handler or
// javascript: URL run, so it can't be set. One that reached the page would be blocked and reported instead, and the
// fixture's guard fails the test on that report, in both browsers.
const SCRIPTS = `# Scripts

<script>window.__mdPwned = 'script'</script>

<iframe srcdoc="<script>parent.__mdPwned = 'iframe'</script>"></iframe>

<a href="javascript:window.__mdPwned = 'js'">js link</a>

<object data="javascript:window.__mdPwned = 'object'"></object>
`;

const HANDLERS = `# Handlers

<img src="pic.png" onerror="window.__mdPwned = 'img'">

<p id="hoverme" onmouseover="window.__mdPwned = 'hover'" onclick="window.__mdPwned = 'click'">hover me</p>

<svg><svg onload="window.__mdPwned = 'svg'"></svg></svg>

<details open ontoggle="window.__mdPwned = 'toggle'"><summary>more</summary>details body</details>

<p title="a > b onclick=x" onclick=window.__mdPwned='unquoted'>quoted gt</p>

<p title="glued"onclick="window.__mdPwned = 'glued'">no space after a quoted value</p>

<div><a/onmouseover="window.__mdPwned = 'slash'" href="#x">slash separated</a></div>

Inline <b onclick="window.__mdPwned = 'inline'">bold handler</b> in a sentence.
`;

function markdownRepo(repo: Repo): Repo {
  repo.write('README.md', README);
  repo.write('docs/other.md', '# Other document\n\nSecond page.\n');
  repo.write('src/main.js', 'export const answer = 42;\n');
  repo.write('notes.md', '# Notes\n\n' + lines(5));
  repo.write('scripts.md', SCRIPTS);
  repo.write('handlers.md', HANDLERS);
  repo.commit('initial');
  return repo;
}

// The rendered document, in the preview's open shadow root: Playwright's locators reach into it, but a
// document.querySelector in the page has to go through the host's shadowRoot.
const preview = (page: Page) => page.locator('#main .markdown');
// The view in the hash, which leaves out the default (diff).
const viewOf = (page: Page) => hashParams(page).view ?? 'diff';

test('an unchanged markdown file opens in Preview and f cycles between file and preview', async ({ page, repo, revue }) => {
  markdownRepo(repo);
  const server = await revue.start();
  await server.open(page);

  await page.getByRole('treeitem', { name: /^README\.md/ }).click();
  await expect(preview(page).getByRole('heading', { name: 'Project title' })).toBeVisible();
  expect(viewOf(page)).toBe('preview');

  // With focus on a link inside the preview's shadow root: the key reaches the page's handler from its host.
  await preview(page).getByRole('link', { name: 'other doc' }).focus();
  await page.keyboard.press('f');
  await expect(page.locator('#main table.code').first()).toBeVisible();
  await expect(preview(page)).toHaveCount(0);
  expect(viewOf(page)).toBe('file');

  await page.keyboard.press('f');
  await expect(preview(page).getByRole('heading', { name: 'Project title' })).toBeVisible();
  expect(viewOf(page)).toBe('preview');
});

test('f cycles a changed markdown file through diff, file and preview', async ({ page, repo, revue }) => {
  markdownRepo(repo);
  repo.write('notes.md', '# Notes\n\n' + lines(5, 2));
  const s = await revue.start();
  await s.open(page, 'mode=working&file=notes.md');
  await expect(page.locator('#main table.code tr.add').first()).toBeVisible();
  expect(viewOf(page)).toBe('diff');

  await focusMain(page);
  await page.keyboard.press('f');
  await expect.poll(() => viewOf(page)).toBe('file');
  // The whole file, not just any code table: the diff, also a table.code, stays on the page until the file is drawn.
  await expect(page.locator('#main table.fileview')).toBeVisible();

  await page.keyboard.press('f');
  await expect.poll(() => viewOf(page)).toBe('preview');
  await expect(preview(page).getByRole('heading', { name: 'Notes' })).toBeVisible();
  await expect(preview(page)).toContainText('edited 3');

  await page.keyboard.press('f');
  await expect.poll(() => viewOf(page)).toBe('diff');
  await expect(page.locator('#main table.code tr.add').first()).toBeVisible();
});

test('images that are no file of the repo stay text placeholders and are never requested', async ({ page, repo, revue }) => {
  markdownRepo(repo);
  const s = await revue.start();
  // Every image the README names, none of which is in the repo. The server would answer 404 (it serves only revue's
  // own files): what matters is that the page never asks, for these or about them.
  const requested = recordRequests(page, /\/(pic\d?|logo)\.png$/), asked = recordRequests(page, /^\/api\/image$/);
  await s.open(page, 'mode=working&file=README.md&view=preview');

  await expect(preview(page)).toContainText('[image: logo]');
  await expect(preview(page)).toContainText('[image: local pic]');
  for (const sel of ['img', 'picture', 'source', 'video', 'input[type=image]', '[src]', '[srcset]', '[background]', '[poster]'])
    await expect(preview(page).locator(sel), sel).toHaveCount(0);

  // Give a request that was going to be made time to show up, then check that none did.
  await page.waitForTimeout(500);
  expect(requested).toEqual([]);
  expect(asked).toEqual([]);
  // The control: an image put on the page the ordinary way is requested and recorded.
  await page.evaluate(() => { const i = new Image(); i.src = 'pic.png'; document.body.append(i); });
  await expect.poll(() => requested.some(u => u.endsWith('/pic.png'))).toBe(true);
});

const RED: [number, number, number, number] = [255, 0, 0, 255];
const BLUE: [number, number, number, number] = [0, 0, 255, 255];
const GREEN: [number, number, number, number] = [0, 128, 0, 255];

test("a preview shows the repo's own images and links remote ones, and loads nothing else", async ({ page, repo, revue }) => {
  repo.write('docs/img/logo.png', png(12, 8, RED));
  repo.write('top.png', png(30, 20, BLUE));
  repo.write('docs/guide.md', `# Guide

![logo](img/logo.png "The logo") and ![top](/top.png).

<img src="../top.png" alt="sized" width="15">

![missing](img/nope.png) ![badge](https://img.example.test/badge.svg) [![linked](https://img.example.test/b2.svg)](https://ci.example.test/) ![inline](data:image/png;base64,AAAA)
`);
  repo.commit('initial');
  const s = await revue.start();
  const images = recordRequests(page, /^\/api\/image$/), remote = recordRequests(page, /\.svg$/);
  await s.open(page, 'file=docs/guide.md&view=preview');
  const md = preview(page);
  // Paths from the document's folder, and from the repo's root.
  const logo = md.getByRole('img', { name: 'logo' });
  expect(await decodedImage(logo)).toEqual({ size: [12, 8], pixel: RED });
  await expect(logo).toHaveAttribute('title', 'The logo');
  expect(await decodedImage(md.getByRole('img', { name: 'top' }))).toEqual({ size: [30, 20], pixel: BLUE });
  // A raw <img> keeps the width the document gave it, and its own proportions.
  const sized = md.getByRole('img', { name: 'sized' });
  expect(await decodedImage(sized)).toEqual({ size: [30, 20], pixel: BLUE });
  expect(await sized.evaluate(el => [el.clientWidth, el.clientHeight])).toEqual([15, 10]);

  await expect(md.getByText('[image: missing]')).toHaveAttribute('title', 'img/nope.png (not an image in this view)');
  const badge = md.getByRole('link', { name: '[image: badge]' });
  await expect(badge).toHaveAttribute('href', 'https://img.example.test/badge.svg');
  await expect(badge).toHaveAttribute('target', '_blank');
  await expect(badge).toHaveAttribute('rel', 'noopener noreferrer');
  // In a link already, a remote image is text, and the link is the document's.
  await expect(md.getByRole('link', { name: '[image: linked]' })).toHaveAttribute('href', 'https://ci.example.test/');
  await expect(md.getByText('[image: inline]')).toBeVisible();
  await expect(md.locator('img')).toHaveCount(3);
  expect(new Set(images.map(u => new URL(u).searchParams.get('path')))).toEqual(new Set(['docs/img/logo.png', 'top.png']));
  expect(remote).toEqual([]);
});

// What the document writes is only ever a path looked up in the view: it can't make a placeholder of its own an image,
// lead out of the repo, or name an address of revue's.
test("a placeholder the document writes itself stays text, and an image's address can't leave the repo", async ({ page, repo, revue }) => {
  repo.write('pic.png', png(3, 3, RED));
  repo.write('docs/forged.md', `# Forged

<span class="md-img" data-md-img="0-0">[image: forged]</span>

![real](../pic.png) ![past the root](../../../../pic.png) ![api](/api/image?mode=working&path=pic.png)
`);
  repo.commit('initial');
  const s = await revue.start();
  const images = recordRequests(page, /^\/api\/image$/);
  await s.open(page, 'file=docs/forged.md&view=preview');
  const md = preview(page);
  expect(await decodedImage(md.getByRole('img', { name: 'real' }))).toEqual({ size: [3, 3], pixel: RED });
  // ../ past the root stays at the root.
  expect(await decodedImage(md.getByRole('img', { name: 'past the root' }))).toEqual({ size: [3, 3], pixel: RED });
  await expect(md.getByText('[image: forged]')).toBeVisible();
  await expect(md.getByText('[image: api]')).toHaveAttribute('title', '/api/image?mode=working&path=pic.png (not an image in this view)');
  await expect(md.locator('img')).toHaveCount(2);
  // Every image address is one this page built: the view's range, the path looked up, its side and version.
  expect(images.length).toBeGreaterThan(0);
  for (const u of images) expect([...new URL(u).searchParams.keys()].sort()).toEqual(['mode', 'path', 'side', 'v']);
});

test('a preview shows its images from the side it shows, and again when only an image changed', async ({ page, repo, revue }) => {
  repo.write('pic.png', png(4, 4, RED));
  repo.write('README.md', '# Doc\n\n![pic](pic.png)\n');
  repo.commit('initial');
  repo.write('pic.png', png(4, 4, BLUE));
  repo.write('README.md', '# Doc, edited\n\n![pic](pic.png)\n');
  const s = await revue.start();
  await s.open(page, 'file=README.md&view=preview');
  const md = preview(page), pic = md.getByRole('img', { name: 'pic' });
  expect(await decodedImage(pic)).toEqual({ size: [4, 4], pixel: BLUE });
  await page.getByRole('button', { name: 'Before', exact: true }).click();
  await expect(md.getByRole('heading', { name: 'Doc', exact: true })).toBeVisible();
  expect(await decodedImage(pic)).toEqual({ size: [4, 4], pixel: RED });
  await page.getByRole('button', { name: 'After', exact: true }).click();
  await expect(md.getByRole('heading', { name: 'Doc, edited' })).toBeVisible();
  // The document is unchanged since: only its image moves.
  repo.write('pic.png', png(4, 4, GREEN));
  await expect.poll(async () => (await decodedImage(pic)).pixel).toEqual(GREEN);
});

test("<picture> sources and #gh-*-mode-only images follow the page's theme, not the OS's", async ({ page, repo, revue }) => {
  repo.write('light.png', png(5, 5, RED));
  repo.write('dark.png', png(6, 6, BLUE));
  repo.write('README.md', `# Themed

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="dark.png">
  <img src="light.png" alt="shot">
</picture>

![for dark](dark.png#gh-dark-mode-only) ![for light](light.png#gh-light-mode-only)
`);
  repo.commit('initial');
  const s = await revue.start();
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'file=README.md&view=preview');
  const md = preview(page), shot = md.getByRole('img', { name: 'shot' });
  const shows = async (theme: 'light' | 'dark') => {
    // An OS change is reported at the browser's next rendering step, so the image is waited for.
    await expect.poll(() => decodedImage(shot)).toEqual(theme === 'dark' ? { size: [6, 6], pixel: BLUE } : { size: [5, 5], pixel: RED });
    await expect(md.getByRole('img', { name: `for ${theme}` })).toBeVisible();
    await expect(md.getByRole('img', { name: `for ${theme === 'dark' ? 'light' : 'dark'}` })).toBeHidden();
  };
  await shows('light');
  // The theme picked in the header, while the OS stays light.
  await pickTheme(page, 'Dark');
  await shows('dark');
  // Auto follows the OS, also when it changes while the page is open.
  await pickTheme(page, 'Auto');
  await shows('light');
  await page.emulateMedia({ colorScheme: 'dark' });
  await shows('dark');
});

// A PNG cut off after its header: revue reads its size there and serves it, and browsers can't decode it (Chromium and
// Firefox log nothing for it).
const undecodable = (width: number, height: number) => png(width, height).subarray(0, 8 + 25);

test("an image the browser can't decode is the placeholder, saying so, and a themed one is drawn again on a theme change", async ({ page, repo, revue }) => {
  repo.write('good.png', png(7, 7, RED));
  repo.write('broken.png', undecodable(9, 9));
  repo.write('light.png', png(5, 5, RED));
  repo.write('dark.png', undecodable(6, 6));
  repo.write('README.md', `# Broken

![good](good.png) ![broken](broken.png)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="dark.png">
  <img src="light.png" alt="shot">
</picture>
`);
  repo.commit('initial');
  const s = await revue.start();
  const images = recordRequests(page, /^\/api\/image$/);
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'file=README.md&view=preview');
  const md = preview(page), shot = md.getByRole('img', { name: 'shot' });
  // The server served it (it was no refusal), and the browser couldn't draw it.
  const broken = md.getByText('[image: broken]');
  await expect(broken).toHaveAttribute('class', 'md-img');
  await expect(broken).toHaveAttribute('title', "broken.png (couldn't be loaded)");
  expect(images.some(u => new URL(u).searchParams.get('path') === 'broken.png')).toBe(true);
  await expect(md.getByRole('img', { name: 'broken' })).toHaveCount(0);
  // The control: the other images of the same preview load.
  expect(await decodedImage(md.getByRole('img', { name: 'good' }))).toEqual({ size: [7, 7], pixel: RED });
  expect(await decodedImage(shot)).toEqual({ size: [5, 5], pixel: RED });

  // The <picture>'s dark source fails, and is the placeholder, still drawn again when the theme changes.
  await pickTheme(page, 'Dark');
  const failed = md.getByText('[image: shot]');
  await expect(failed).toHaveAttribute('title', "dark.png (couldn't be loaded)");
  await expect(failed).toBeVisible();
  await expect(shot).toHaveCount(0);
  await pickTheme(page, 'Light');
  await expect(shot).toBeVisible();
  expect(await decodedImage(shot)).toEqual({ size: [5, 5], pixel: RED });
  await expect(failed).toHaveCount(0);
});

test.describe('an image whose request fails', () => {
  // Chromium logs the failed request (net::ERR_FAILED), which the test causes on purpose; Firefox logs nothing.
  test.use({ allowConsoleErrors: true });

  test('is the placeholder in a preview, saying so', async ({ page, repo, revue }) => {
    repo.write('good.png', png(7, 7, RED));
    repo.write('gone.png', png(8, 8, BLUE));
    repo.write('README.md', '# Gone\n\n![good](good.png) ![gone](gone.png)\n');
    repo.commit('initial');
    const s = await revue.start();
    await page.route(u => u.pathname === '/api/image' && u.searchParams.get('path') === 'gone.png', route => route.abort());
    await s.open(page, 'file=README.md&view=preview');
    const md = preview(page);
    await expect(md.getByText('[image: gone]')).toHaveAttribute('title', "gone.png (couldn't be loaded)");
    await expect(md.getByRole('img', { name: 'gone' })).toHaveCount(0);
    // The control: the other image of the same preview loads.
    expect(await decodedImage(md.getByRole('img', { name: 'good' }))).toEqual({ size: [7, 7], pixel: RED });
  });
});

test('script tags, iframes and javascript: links are stripped from a preview', async ({ page, repo, revue }) => {
  markdownRepo(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=scripts.md&view=preview');
  const md = preview(page);
  await expect(md.getByRole('heading', { name: 'Scripts' })).toBeVisible();

  await expect(md.locator('script, iframe, object, embed')).toHaveCount(0);
  await expect(md.getByText('js link')).not.toHaveAttribute('href', /.*/);
  // Were the javascript: URL still there, the CSP would block it on this click and the guard would fail the test.
  await md.getByText('js link').click();
});

test('on* event handler attributes are stripped from a preview without a CSP report', async ({ page, repo, revue }) => {
  // Two layers take the handlers off: stripRawHtml cuts them from marked's raw HTML before DOMPurify parses it, and
  // DOMPurify drops any that get through. The checks below see the result in both browsers. The strip itself is checked
  // only in Chromium, and only by the fixture's guard: Chromium reports a CSP violation for an on* attribute in markup
  // it parses even into DOMPurify's inert document, so a handler the strip misses fails the test there. Firefox reports
  // nothing for that document and DOMPurify then drops the handler, so there a miss passes.
  markdownRepo(repo);
  const s = await revue.start();
  await s.open(page, 'mode=working&file=handlers.md&view=preview');
  const md = preview(page);
  await expect(md.getByText('hover me')).toBeVisible();
  // The slash-separated tag is markup that reaches the strip (an HTML block), not text that marked escaped.
  await expect(md.getByRole('link', { name: 'slash separated' })).toBeVisible();

  await expect(md.locator('img, svg')).toHaveCount(0);
  const handlers = await md.evaluate(root => [...root.querySelectorAll('*')]
    .flatMap(el => [...el.attributes].filter(a => /^on/i.test(a.name)).map(a => `${el.tagName}.${a.name}`)));
  expect(handlers).toEqual([]);
  // The id from the source is dropped too, so the document can't take over one of revue's own ids.
  await expect(page.locator('#hoverme')).toHaveCount(0);

  await md.getByText('hover me').hover();
  await md.getByText('hover me').click();
  await md.getByText('more', { exact: true }).click(); // fires toggle
  for (const text of ['quoted gt', 'no space after a quoted value', 'slash separated', 'bold handler']) {
    await md.getByText(text).hover();
    await md.getByText(text).click();
  }
  // A quoted value is skipped whole: neither its '>' nor the handler-like text inside it is cut.
  await expect(md.getByText('quoted gt')).toHaveAttribute('title', 'a > b onclick=x');
  await expect(md.getByText('no space after a quoted value')).toHaveAttribute('title', 'glued');
});

test('links open files in revue, open http in a new tab and scroll to anchors below the header', async ({ page, repo, revue }) => {
  markdownRepo(repo);
  // Changed, so opening it without asking for a view would show its diff.
  repo.write('docs/other.md', '# Other document\n\nSecond page, edited.\n');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=README.md&view=preview');
  const md = preview(page);
  await expect(md.getByRole('heading', { name: 'Project title' })).toBeVisible();

  const ext = md.getByRole('link', { name: 'external' });
  await expect(ext).toHaveAttribute('href', 'https://example.com/page');
  await expect(ext).toHaveAttribute('target', '_blank');
  await expect(ext).toHaveAttribute('rel', /noopener/);
  await expect(md.getByRole('link', { name: 'main code' })).toHaveAttribute('href', /[#&]file=src%2Fmain\.js(&|$)/);
  // Any other scheme stays inert: the link keeps its text and shows its target, but goes nowhere.
  const mail = md.locator('a', { hasText: 'mail me' });
  await expect(mail).toHaveAttribute('title', 'mailto:someone@example.com');
  await expect(mail).not.toHaveAttribute('href', /.*/);

  // Checkboxes show their state but can't be changed: the task list's (which marked writes disabled) and a raw one.
  await expect(md.getByText('raw box')).toBeVisible();
  const boxes = md.getByRole('checkbox');
  await expect(boxes).toHaveCount(3);
  await expect(boxes.nth(0)).not.toBeChecked();
  await expect(boxes.nth(1)).toBeChecked();
  for (const b of await boxes.all()) await expect(b).toBeDisabled();

  // An anchor scrolls the heading to just below the sticky file header.
  const far = md.getByRole('heading', { name: 'Far section' });
  await expect(far).not.toBeInViewport();
  await md.getByRole('link', { name: 'far away' }).click();
  await expect(far).toBeInViewport();
  const belowHead = () => page.evaluate(() => {
    const main = document.querySelector('#main')!.getBoundingClientRect();
    const head = document.querySelector('#main .file-head')!.getBoundingClientRect();
    const doc = document.querySelector('#main .md-host')!.shadowRoot!;
    const h = [...doc.querySelectorAll('.markdown h2')].find(e => e.textContent === 'Far section')!.getBoundingClientRect();
    return h.top - main.top - head.height;
  });
  // Below the header (scrollBelowHead leaves 8px of air), not hidden under it; a pixel either way is rounding.
  await expect(async () => {
    const px = await belowHead();
    expect(px).toBeGreaterThanOrEqual(7);
    expect(px).toBeLessThanOrEqual(9);
  }).toPass();
  expect(viewOf(page)).toBe('preview');

  // A relative link to another markdown file opens its preview inside revue, though the file changed.
  await md.getByRole('link', { name: 'other doc' }).click();
  await expect(preview(page).getByRole('heading', { name: 'Other document' })).toBeVisible();
  await expect(preview(page)).toContainText('Second page, edited.');
  expect(hashParams(page)).toMatchObject({ file: 'docs/other.md', view: 'preview' });
  expect(new URL(page.url()).origin).toBe(new URL(s.url).origin);
});

// A heading named like revue's main pane, and one heading three times, a screen of filler apart.
const HEADINGS = `# Main

[the second setup](#setup-1)

## Setup

${filler}
## Setup

${filler}
## Setup

${filler}`;

test('headings get md- prefixed slug ids, numbered when repeated, and an anchor scrolls to the right one', async ({ page, repo, revue }) => {
  repo.write('headings.md', HEADINGS);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=headings.md&view=preview');
  const md = preview(page);
  await expect(md.getByRole('heading', { name: 'Main' })).toBeVisible();

  expect(await md.evaluate(root => [...root.querySelectorAll('h1, h2')].map(h => h.id)))
    .toEqual(['md-main', 'md-setup', 'md-setup-1', 'md-setup-2']);
  // The ids are the shadow root's: the document's #main is still revue's main pane, the one holding the preview.
  expect(await page.evaluate(() => {
    const main = document.getElementById('main')!, host = main.querySelector('.md-host');
    return { tag: main.tagName, h1: host?.shadowRoot?.getElementById('md-main')?.textContent, outside: !!document.getElementById('md-main') };
  })).toEqual({ tag: 'MAIN', h1: 'Main', outside: false });

  // The link names the second Setup (#setup-1), and that one is where it goes.
  const setups = md.getByRole('heading', { name: 'Setup' });
  await expect(setups).toHaveCount(3);
  await expect(setups.nth(1)).not.toBeInViewport();
  await md.getByRole('link', { name: 'the second setup' }).click();
  await expect(setups.nth(1)).toBeInViewport();
  await expect(setups.nth(0)).not.toBeInViewport();
  await expect(setups.nth(2)).not.toBeInViewport();
});

// Classes from the source that would borrow the page's or the preview's styles, next to the ones the preview sets.
const CLASSES = `# Classes

![a picture](pic.png)

<div class="note-box toast">borrowed classes</div>

<pre><code class="language-js toast">raw code</code></pre>

\`\`\`js
const fenced = 1;
\`\`\`
`;

test('a preview keeps only the classes it sets itself', async ({ page, repo, revue }) => {
  repo.write('classes.md', CLASSES);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=classes.md&view=preview');
  const md = preview(page);
  await expect(md.getByText('borrowed classes')).toBeVisible();

  await expect(md.getByText('borrowed classes')).not.toHaveAttribute('class', /.*/);
  // A code element keeps its class only when it is one language-* class, which picks its highlighting.
  await expect(md.locator('code', { hasText: 'raw code' })).not.toHaveAttribute('class', /.*/);
  const fenced = md.locator('code', { hasText: 'fenced' });
  await expect(fenced).toHaveAttribute('class', 'language-js');
  await expect(fenced.locator('.hljs-keyword')).toHaveText('const');
  await expect(md.getByText('[image: a picture]')).toHaveAttribute('class', 'md-img');
  // Nothing else has a class, apart from the highlighter's spans, which are added after the filter.
  const classes = await md.evaluate(root => [...root.querySelectorAll('[class]')]
    .map(el => `${el.tagName.toLowerCase()}.${el.getAttribute('class')}`).filter(c => !/^span\.hljs-/.test(c)));
  expect(classes).toEqual(['span.md-img', 'code.language-js']);
});
