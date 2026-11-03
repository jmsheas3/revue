// The theme menu (Auto/Light/Dark): the choice persists across a reload and applies before first paint, code colors
// (the preview's and revue's own diff and file views) follow it, and the menu works from the keyboard. And the page
// under emulated media: in forced colors (Windows high contrast) the selected tree row and commit, the target line,
// the current mode, the layout drawn and a blocked layout keep a visible marker in system colors, and with reduced
// motion nothing transitions.
import { test, expect, lines, focusMain, reload, modeTab, commitList, viewLabel, pickTheme } from '../fixtures';
import type { Locator, Page } from '@playwright/test';

// Records data-theme on <html> when <body> is first inserted (before anything of the page can be painted) and at
// DOMContentLoaded. null means no attribute (Auto).
async function recordEarlyTheme(page: Page) {
  await page.addInitScript(() => {
    const w = window as any;
    w.__themeAtBody = undefined;
    w.__themeAtDCL = undefined;
    const read = () => document.documentElement.getAttribute('data-theme');
    const mo = new MutationObserver(() => {
      if (document.body) { w.__themeAtBody = read(); mo.disconnect(); }
    });
    mo.observe(document, { childList: true, subtree: true });
    document.addEventListener('DOMContentLoaded', () => { w.__themeAtDCL = read(); });
  });
}

const early = (page: Page) => page.evaluate(() => [(window as any).__themeAtBody, (window as any).__themeAtDCL]);
const bodyBg = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor);

test('the chosen theme persists across a reload and applies before first paint', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  // The OS says light, so a dark page can only come from the saved choice.
  await page.emulateMedia({ colorScheme: 'light' });
  await recordEarlyTheme(page);
  await s.open(page);
  expect(await early(page)).toEqual([null, null]);
  const lightBg = await bodyBg(page);

  await pickTheme(page, 'Dark');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  const darkBg = await bodyBg(page);
  expect(darkBg).not.toBe(lightBg);

  await reload(page);
  expect(await early(page)).toEqual(['dark', 'dark']);
  await expect(page.locator('#theme-btn')).toHaveAttribute('aria-label', 'Theme: Dark');
  expect(await bodyBg(page)).toBe(darkBg);

  await pickTheme(page, 'Light');
  await page.emulateMedia({ colorScheme: 'dark' });
  await reload(page);
  expect(await early(page)).toEqual(['light', 'light']);
  expect(await bodyBg(page)).toBe(lightBg);

  // Auto drops the attribute and follows the OS setting.
  await pickTheme(page, 'Auto');
  await reload(page);
  expect(await early(page)).toEqual([null, null]);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /.*/);
  expect(await bodyBg(page)).toBe(darkBg);
  await page.emulateMedia({ colorScheme: 'light' });
  await expect.poll(() => bodyBg(page)).toBe(lightBg);
});

test('code in a markdown preview follows the theme', async ({ page, repo, revue }) => {
  repo.write('doc.md', '# Doc\n\n```js\nconst x = 1;\n```\n');
  repo.commit('initial');
  const s = await revue.start();
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'mode=working&file=doc.md&view=preview');
  // The preview's shadow root has its own copies of the highlight themes (GitHub light and dark: #d73a49, #ff7b72).
  const keyword = page.locator('#main .markdown pre code .hljs-keyword');
  const LIGHT = 'rgb(215, 58, 73)', DARK = 'rgb(255, 123, 114)';
  await expect(keyword).toHaveText('const');
  await expect(keyword).toHaveCSS('color', LIGHT);

  await pickTheme(page, 'Dark');
  await expect(keyword).toHaveCSS('color', DARK);
  await pickTheme(page, 'Light');
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(keyword).toHaveCSS('color', LIGHT);
  // Auto follows the OS.
  await pickTheme(page, 'Auto');
  await expect(keyword).toHaveCSS('color', DARK);
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(keyword).toHaveCSS('color', LIGHT);
});

// revue's own code views use the page's highlight themes (#hl-light, #hl-dark), whose media applyTheme sets: the
// theme picked, not the OS, decides. The same GitHub light and dark keyword colors as in the preview.
test('code in the diff and file views follows the theme', async ({ page, repo, revue }) => {
  repo.write('a.js', 'const x = 1;\n');
  repo.commit('initial');
  repo.write('a.js', 'const x = 2;\n');
  const s = await revue.start();
  const LIGHT = 'rgb(215, 58, 73)', DARK = 'rgb(255, 123, 114)';
  const inDiff = page.locator('#main table.code:not(.fileview) .hljs-keyword').first();
  const inFile = page.locator('#main table.code.fileview .hljs-keyword').first();
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'mode=working&file=a.js');
  await expect(inDiff).toHaveText('const');
  await expect(inDiff).toHaveCSS('color', LIGHT);

  // Dark while the OS is light: in the diff, in the file view, and after a reload (theme.js).
  await pickTheme(page, 'Dark');
  await expect(inDiff).toHaveCSS('color', DARK);
  await focusMain(page);
  await page.keyboard.press('f');
  await expect(inFile).toHaveText('const');
  await expect(inFile).toHaveCSS('color', DARK);
  await reload(page);
  await expect(inFile).toHaveCSS('color', DARK);

  // Light while the OS is dark.
  await pickTheme(page, 'Light');
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(inFile).toHaveCSS('color', LIGHT);
  await reload(page);
  await expect(inFile).toHaveCSS('color', LIGHT);
  await focusMain(page);
  await page.keyboard.press('f'); // the file view has no preview for a.js: back to the diff
  await expect(inDiff).toHaveCSS('color', LIGHT);

  // Auto follows the OS.
  await pickTheme(page, 'Auto');
  await expect(inDiff).toHaveCSS('color', DARK);
});

// Up and Down move between the themes, wrapping at the ends, without picking one; Enter picks. Esc closes the menu with
// focus back on its button and goes no further (on its own, Esc leaves the open file for all changes). Tab and a click
// outside close it too.
test('in the theme menu the arrows move between the themes, and Esc, Tab or a click outside closes it', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  repo.write('a.txt', lines(3, 1));
  const s = await revue.start();
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'mode=working&file=a.txt');
  const btn = page.locator('#theme-btn'), menu = page.locator('#theme-menu');
  const item = (name: string) => page.getByRole('menuitemradio', { name });

  await btn.click();
  await expect(menu).toBeVisible();
  await expect(btn).toHaveAttribute('aria-expanded', 'true');
  await expect(item('Auto')).toBeFocused();
  for (const [key, next] of [['ArrowDown', 'Light'], ['ArrowDown', 'Dark'], ['ArrowDown', 'Auto'], ['ArrowUp', 'Dark'], ['ArrowUp', 'Light']] as const) {
    await page.keyboard.press(key);
    await expect(item(next), key).toBeFocused();
  }
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /.*/);

  await page.keyboard.press('Escape');
  await expect(menu).toBeHidden();
  await expect(btn).toBeFocused();
  await expect(btn).toHaveAttribute('aria-expanded', 'false');
  await expect(viewLabel(page)).toHaveText('a.txt');

  await page.keyboard.press('Enter');
  await expect(item('Auto')).toBeFocused();
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('Enter');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(menu).toBeHidden();
  await expect(btn).toBeFocused();

  await btn.click();
  await expect(item('Dark')).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(menu).toBeHidden();
  await expect(btn).toHaveAttribute('aria-expanded', 'false');

  await btn.click();
  await expect(menu).toBeVisible();
  await viewLabel(page).click();
  await expect(menu).toBeHidden();
  await expect(btn).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});

// A system color as forced colors mode draws it: a probe that opts out of the forcing (set through the CSSOM, which
// the CSP allows) keeps the color as its own.
const systemColor = (page: Page, name: 'Highlight' | 'GrayText') => page.evaluate(name => {
  const probe = document.createElement('span');
  probe.style.setProperty('forced-color-adjust', 'none');
  probe.style.color = name;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}, name);
const systemHighlight = (page: Page) => systemColor(page, 'Highlight');

const outline = (l: Locator) => l.evaluate(el => {
  const cs = getComputedStyle(el);
  return { style: cs.outlineStyle, width: parseFloat(cs.outlineWidth), color: cs.outlineColor };
});

test('in forced colors the selected row and the target line keep a visible marker', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(10));
  repo.write('b.txt', lines(3));
  repo.commit('initial');
  repo.write('a.txt', lines(10, 4));
  repo.write('b.txt', lines(3, 0));
  const s = await revue.start();
  await page.emulateMedia({ forcedColors: 'active' });
  await s.open(page, 'mode=working&file=a.txt&line=5');
  expect(await page.evaluate(() => matchMedia('(forced-colors: active)').matches)).toBe(true);
  const highlight = await systemHighlight(page);

  const selected = page.getByRole('treeitem', { selected: true });
  await expect(selected).toHaveAttribute('data-path', 'a.txt');
  const other = page.getByRole('treeitem', { name: /^b\.txt/ });
  const sel = await outline(selected);
  expect(sel.style).not.toBe('none');
  expect(sel.width).toBeGreaterThan(0);
  expect(sel.color).toBe(highlight);
  expect((await outline(other)).style).toBe('none');

  // The target line's number cell carries an inset bar in Highlight; other rows' don't. Outside forced colors the
  // bar is the accent color, which table.code keeps (it opts out of the forcing), so the color tells the two apart.
  const target = page.locator('#main table.code tr.target td.num').first();
  await expect(target).toBeVisible();
  const shadow = (l: Locator) => l.evaluate(el => getComputedStyle(el).boxShadow);
  const targetShadow = await shadow(target);
  expect(targetShadow).toMatch(/inset/);
  expect(targetShadow.startsWith(`${highlight} `), `${targetShadow} is drawn in Highlight, ${highlight}`).toBe(true);
  const plain = page.locator('#main table.code tr:not(.target) td.num').first();
  expect(await shadow(plain)).toBe('none');
});

// Outside forced colors the current mode's tab is underlined by a box shadow and the layout drawn has a background and
// a shadow, all of which forced colors drops; a blocked layout button is only faded. The forced-colors block gives each
// a marker of its own: a bar and an outline in Highlight, and GrayText.
test('in forced colors the current mode, the layout drawn and a blocked layout keep a visible marker', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(10));
  repo.commit('initial');
  repo.write('a.txt', lines(10, 4));
  const s = await revue.start();
  // Wide enough for split (see layout.spec.ts).
  await page.setViewportSize({ width: 1600, height: 900 });
  await page.emulateMedia({ forcedColors: 'active' });
  await s.open(page);
  const highlight = await systemColor(page, 'Highlight'), grayText = await systemColor(page, 'GrayText');
  expect(grayText).not.toBe(highlight);

  const bar = (l: Locator) => l.evaluate(el => {
    const cs = getComputedStyle(el);
    return { style: cs.borderBottomStyle, width: parseFloat(cs.borderBottomWidth), color: cs.borderBottomColor };
  });
  expect(await bar(modeTab(page, 'Changes'))).toEqual({ style: 'solid', width: 3, color: highlight });
  expect((await bar(modeTab(page, 'History'))).style).toBe('none');

  const layout = (name: string) => page.locator('#layout-seg').getByRole('button', { name: new RegExp(`^${name}`) });
  const color = (l: Locator) => l.evaluate(el => getComputedStyle(el).color);
  await expect(layout('Split')).toHaveClass(/\bon\b/);
  expect(await outline(layout('Split'))).toEqual({ style: 'solid', width: 2, color: highlight });
  expect((await outline(layout('Unified'))).style).toBe('none');

  // No wrap blocks split: Unified is drawn, and Split is grayed out.
  await focusMain(page);
  await page.keyboard.press('w');
  await expect(layout('Split')).toHaveAttribute('aria-disabled', 'true');
  await expect(layout('Unified')).toHaveClass(/\bon\b/);
  expect(await color(layout('Split'))).toBe(grayText);
  expect(await color(layout('Unified'))).not.toBe(grayText);
  expect(await outline(layout('Unified'))).toEqual({ style: 'solid', width: 2, color: highlight });
  expect((await outline(layout('Split'))).style).toBe('none');
});

// The selected commit is drawn with a background and an accent edge outside forced colors; the outline is what's left.
test('in forced colors the selected commit keeps a visible marker', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(5));
  repo.commit('first');
  repo.write('a.txt', lines(5, 1));
  repo.commit('second');
  const s = await revue.start();
  await page.emulateMedia({ forcedColors: 'active' });
  await s.open(page, 'mode=commit');
  const highlight = await systemHighlight(page);
  const selected = commitList(page).getByRole('option', { selected: true });
  await expect(selected).toHaveAttribute('title', 'second');
  expect(await outline(selected)).toEqual({ style: 'solid', width: 2, color: highlight });
  expect((await outline(commitList(page).getByRole('option', { name: /^first\b/ }))).style).toBe('none');
});

test('with reduced motion nothing transitions', async ({ page, repo, revue }) => {
  repo.write('a.txt', lines(3));
  repo.commit('initial');
  const s = await revue.start();
  // Whether the element has a transition that takes any time.
  const moves = (l: Locator) => l.evaluate(el => getComputedStyle(el).transitionDuration.split(',').some(d => parseFloat(d) > 0));
  const durations = () => page.evaluate(() => {
    const found = new Set<string>();
    for (const el of document.querySelectorAll('*'))
      for (const pseudo of [null, '::before', '::after'])
        for (const d of getComputedStyle(el, pseudo).transitionDuration.split(',')) found.add(d.trim());
    return [...found].sort();
  });
  const side = page.locator('#side'), toast = page.locator('#toast');

  // The controls: without the preference the toast fades (`w` shows one), and at 600 px the sidebar is a drawer that
  // slides in and out.
  await page.setViewportSize({ width: 600, height: 800 });
  await s.open(page);
  expect(await moves(side)).toBe(true);
  await focusMain(page);
  await page.keyboard.press('w');
  await expect(toast).toHaveText(/long lines/i);
  expect(await moves(toast)).toBe(true);

  await page.emulateMedia({ reducedMotion: 'reduce' });
  await expect.poll(() => moves(side)).toBe(false);
  expect(await moves(toast)).toBe(false);
  expect(await durations()).toEqual(['0s']);
});
