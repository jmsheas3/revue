// A markdown preview can't style revue or load anything through styles. Each defense is tested on its own here:
// the sanitizer (style attributes, <style>, <link>, <meta> and <base> never reach the page), the CSP (no inline
// styles at all, whatever puts them on the page), and the preview's isolation (its shadow root and contained frame keep
// any styling that got in to the preview's own box).
import { test, expect, lines } from '../fixtures';

// Each payload would load an image, restyle revue or take the page away if it reached the page.
const STYLES = `# Styles

<p style="background-image: url(pic7.png)">styled paragraph</p>

Inline <span style="background: url(pic7.png)">styled span</span> in a sentence.

<style>.markdown { border-image: url(pic8.png) 30 } h1 { background: url(pic9.png) } #top { display: none }</style>

An inline <style>#side { display: none }</style>style tag in a sentence.

<link rel="stylesheet" href="style.css?from-preview">

<meta http-equiv="refresh" content="0;url=about:blank">

<base href="http://example.test/">

[a relative link](other.md)
`;

test('style attributes, <style>, <link>, <meta> and <base> are stripped from a preview', async ({ page, repo, revue }) => {
  repo.write('styles.md', STYLES);
  repo.write('other.md', '# Other\n');
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=other.md&view=preview');
  await expect(page.locator('#main .markdown').getByRole('heading', { name: 'Other' })).toBeVisible();

  // revue's own header as drawn before the preview, to compare with after.
  const topStyle = () => page.locator('#top').evaluate(el => {
    const cs = getComputedStyle(el), r = el.getBoundingClientRect();
    return { display: cs.display, visibility: cs.visibility, background: cs.backgroundImage, border: cs.borderImageSource, box: [r.x, r.y, r.width, r.height] };
  });
  const before = await topStyle();
  const requested: string[] = [];
  page.on('request', r => requested.push(r.url()));
  await page.evaluate(() => { (window as any).__samePage = true; }); // gone if the page navigates away

  await page.getByRole('treeitem', { name: /^styles\.md/ }).click();
  const md = page.locator('#main .markdown');
  await expect(md.getByRole('heading', { name: 'Styles' })).toBeVisible();
  await expect(md.getByText('styled paragraph')).toBeVisible();
  await expect(md.getByText('styled span')).toBeVisible();

  await expect(md.locator('style, link, meta, base, [style]')).toHaveCount(0);
  // A <style> goes with its content, inline too.
  await expect(md.getByText('An inline style tag in a sentence.')).toBeVisible();
  await expect(md).not.toContainText('border-image');
  await expect(md).not.toContainText('#side');
  // Relative URLs still resolve against revue, not the <base>.
  expect(new URL(await page.evaluate(() => document.baseURI)).origin).toBe(new URL(s.url).origin);
  expect(await md.getByRole('link', { name: 'a relative link' }).evaluate(a => (a as HTMLAnchorElement).href)).toMatch(new RegExp(`^${s.url}#`));

  // Give a request or the meta refresh time to happen, then check that none did.
  await page.waitForTimeout(500);
  expect(page.url()).toMatch(new RegExp(`^${s.url}#.*file=styles\\.md`));
  expect(await page.evaluate(() => (window as any).__samePage), 'the page navigated').toBe(true);
  expect(requested.filter(u => /pic[789]\.png|from-preview/.test(u))).toEqual([]);
  expect(await topStyle()).toEqual(before);
  await expect(page.locator('#top')).toBeVisible();
  // The control: a stylesheet put on the page the ordinary way is requested and recorded.
  await page.evaluate(() => document.head.append(Object.assign(document.createElement('link'), { rel: 'stylesheet', href: 'style.css?control' })));
  await expect.poll(() => requested.some(u => u.endsWith('/style.css?control'))).toBe(true);
});

test('popover attributes are stripped, so nothing in a preview reaches the top layer', async ({ page, repo, revue }) => {
  // DOMPurify keeps these by default. A popover is drawn above everything, outside the preview's contained frame.
  repo.write('pop.md', '# Popover\n\n<div popover id="pop">popover content</div>\n\n<p popovertarget="pop" popovertargetaction="show">opener</p>\n');
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=pop.md&view=preview');
  const md = page.locator('#main .markdown');
  await expect(md.getByRole('heading', { name: 'Popover' })).toBeVisible();
  await expect(md.getByText('opener')).toBeVisible();
  await expect(md.locator('[popover], [popovertarget], [popovertargetaction]')).toHaveCount(0);
  // Without the attribute it is an ordinary block, shown in place rather than hidden until opened.
  await expect(md.getByText('popover content')).toBeVisible();
});

test.describe('the CSP', () => {
  // The violations are the point here; the test checks them itself.
  test.use({ allowCspViolations: true });

  test('blocks and reports a <style> and a style attribute put on the page directly', async ({ page, repo, revue }) => {
    repo.write('a.txt', lines(3));
    repo.commit('initial');
    const s = await revue.start();
    await page.addInitScript(() => {
      const w = window as any;
      w.__csp = [];
      document.addEventListener('securitypolicyviolation', e => w.__csp.push(e.effectiveDirective));
    });
    await s.open(page);
    await expect(page.locator('#top')).toBeVisible();
    await expect(page.locator('#side')).toBeVisible();

    // Not through the preview: straight onto the page, as if every other layer had failed. Each would hide what it
    // targets.
    await page.evaluate(() => {
      document.head.append(Object.assign(document.createElement('style'), { textContent: '#top { display: none }' }));
      document.querySelector('#side')!.setAttribute('style', 'display: none');
    });
    await expect.poll(() => page.evaluate(() => [...(window as any).__csp].sort())).toEqual(['style-src-attr', 'style-src-elem']);
    await expect(page.locator('#top')).toBeVisible();
    await expect(page.locator('#side')).toBeVisible();
  });
});

test('styles inside the preview stay in its shadow root and its frame', async ({ page, repo, revue }) => {
  repo.write('doc.md', '# Isolated heading\n\nBody text.\n');
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'mode=working&file=doc.md&view=preview');
  const heading = page.locator('#main .markdown').getByRole('heading', { name: 'Isolated heading' });
  await expect(heading).toBeVisible();

  // The document's ids are the shadow root's, not the page's.
  expect(await page.evaluate(() => document.getElementById('md-isolated-heading'))).toBeNull();
  expect(await page.evaluate(() => document.querySelector('#main .md-host')?.shadowRoot?.getElementById('md-isolated-heading')?.textContent))
    .toBe('Isolated heading');

  // revue's header and sidebar: where they are, how they look, and what is under their middle.
  const look = () => page.evaluate(() => Object.fromEntries(['#top', '#side'].map(sel => {
    const el = document.querySelector(sel)!, r = el.getBoundingClientRect(), cs = getComputedStyle(el);
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return [sel, { box: [r.x, r.y, r.width, r.height], display: cs.display, color: cs.color, hit: !!hit?.closest(sel) }];
  })));
  const before = await look();
  expect(before['#top'].hit && before['#side'].hit).toBe(true);

  // Rules inside the preview, as if every other layer had let them in: a constructed sheet, which the CSP allows. They
  // try to cover the whole window and restyle anything they can match.
  await page.evaluate(() => {
    const root = document.querySelector('#main .md-host')!.shadowRoot!;
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`:host { position: fixed !important; inset: 0 !important; z-index: 9999 !important; contain: none !important;
        overflow: visible !important; background: red !important }
      h1 { font-size: 200px !important } * { color: rgb(255, 0, 0) !important } #top, #side, header, nav { display: none !important }`);
    root.adoptedStyleSheets = [...root.adoptedStyleSheets, sheet];
  });
  // The control: they apply inside the preview.
  await expect(heading).toHaveCSS('font-size', '200px');
  await expect(page.locator('#main .md-host')).toHaveCSS('position', 'fixed');

  // Nothing outside changed, and the header and sidebar are still on top where they were.
  expect(await look()).toEqual(before);
  // The fixed host is placed in the frame, not the window, and painted only inside it.
  const [host, frame] = await page.evaluate(() => ['#main .md-host', '#main .md-frame'].map(sel => {
    const r = document.querySelector(sel)!.getBoundingClientRect();
    return [r.x, r.y, r.width, r.height];
  }));
  expect(host).toEqual(frame);
  const below = await page.evaluate(([x, y, w, h]) => document.elementFromPoint(x + w / 2, y + h + 20)?.className, frame);
  expect(below).not.toBe('md-host');
});
