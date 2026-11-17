// Mermaid diagrams: a markdown preview draws its ```mermaid blocks, and a .mmd or .mermaid file opens drawn in its
// Diagram view. mermaid runs in a sandboxed frame of its own (an opaque origin, which can reach neither the page nor the
// API) and the page shows each drawing as an <img> of a data: URL, sized by the renderer so its place is kept. One that
// can't be drawn shows its source and why; a live edit draws it again, and so does a theme switch, in the other color
// scheme. Nothing a diagram names is ever requested, and nothing here causes a CSP report (the fixture's guard).
import { test, expect, hashParams, recordRequests, pickTheme, focusMain } from '../fixtures';
import type { Locator, Page } from '@playwright/test';

const preview = (page: Page) => page.locator('#main .markdown');
const FLOW = 'flowchart LR\n  accTitle: Build flow\n  A[Start] --> B[Done]\n';
const mermaid = (src: string) => '```mermaid\n' + src + '```\n';

// An <img> that has loaded: its natural size, and the size its width and height attributes give it.
const sizes = (img: Locator) => img.evaluate(async (el: HTMLImageElement) => {
  await el.decode();
  return { natural: [el.naturalWidth, el.naturalHeight], attrs: [+el.getAttribute('width')!, +el.getAttribute('height')!] };
});

test('a preview draws its mermaid blocks as images at their own size, and other code stays code', async ({ page, repo, revue }) => {
  repo.write('README.md', `# Diagrams\n\n${mermaid(FLOW)}\n${mermaid('sequenceDiagram\n  Alice->>Bob: Hi\n')}\n\`\`\`js\nconst x = 1;\n\`\`\`\n`);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=README.md&view=preview');
  const md = preview(page);
  // Named by its accTitle, or "Mermaid diagram" without one.
  const flow = md.getByRole('img', { name: 'Build flow' }), seq = md.getByRole('img', { name: 'Mermaid diagram' });
  await expect(flow).toBeVisible();
  await expect(seq).toBeVisible();
  await expect(flow).toHaveAttribute('src', /^data:image\/svg\+xml;/);
  for (const img of [flow, seq]) {
    const { natural, attrs } = await sizes(img);
    expect(natural[0]).toBeGreaterThan(50);
    expect(natural[1]).toBeGreaterThan(20);
    expect(attrs).toEqual(natural);
  }
  // The diagrams' sources aren't shown; the other code block is, as code.
  await expect(md.locator('pre')).toHaveCount(1);
  await expect(md.locator('pre code')).toHaveText('const x = 1;');
  await expect(md).not.toContainText('flowchart LR');
});

test("a diagram that can't be drawn shows its source and why", async ({ page, repo, revue }) => {
  // A syntax error, and a mindmap, which the bundled build of mermaid leaves out.
  repo.write('README.md', `# Broken\n\n${mermaid('flowchart LR\n  A -->\n')}\n${mermaid('mindmap\n  root\n')}`);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=README.md&view=preview');
  const [bad, mindmap] = [preview(page).getByRole('figure').nth(0), preview(page).getByRole('figure').nth(1)];
  await expect(bad).toContainText('Diagram not drawn: Parse error');
  await expect(bad.locator('pre')).toHaveText('flowchart LR\n  A -->\n');
  await expect(mindmap).toContainText('Diagram not drawn: No diagram type detected');
  await expect(mindmap.locator('pre')).toHaveText('mindmap\n  root\n');
  await expect(preview(page).getByRole('img')).toHaveCount(0);
});

test('a live edit draws a diagram again, and a theme switch draws it in the other color scheme', async ({ page, repo, revue }) => {
  repo.write('README.md', `# Live\n\n${mermaid(FLOW)}`);
  repo.commit('initial');
  const s = await revue.start();
  await page.emulateMedia({ colorScheme: 'light' });
  await s.open(page, 'file=README.md&view=preview');
  const md = preview(page);
  await expect(md.getByRole('img', { name: 'Build flow' })).toBeVisible();

  repo.write('README.md', `# Live\n\n${mermaid(FLOW.replace('Build flow', 'Ship flow'))}`);
  const img = md.getByRole('img', { name: 'Ship flow' });
  await expect(img).toBeVisible();
  const light = await img.getAttribute('src');

  await pickTheme(page, 'Dark');
  await expect.poll(() => img.getAttribute('src')).not.toBe(light);
  const dark = await img.getAttribute('src');
  // The drawing's own colors changed: the dark theme's are another set.
  expect(decodeURIComponent(dark!)).not.toEqual(decodeURIComponent(light!));
  // Back to light: the same drawing as before.
  await pickTheme(page, 'Light');
  await expect.poll(() => img.getAttribute('src')).toBe(light);
});

test('a mermaid file opens drawn in its Diagram view, and f goes to its text and back', async ({ page, repo, revue }) => {
  repo.write('flow.mmd', FLOW);
  repo.write('a.txt', 'a\n');
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=a.txt&view=file');
  await page.getByRole('treeitem', { name: /^flow\.mmd/ }).click();
  const view = page.locator('#main .diagram-view');
  await expect(view.getByRole('img', { name: 'Build flow' })).toBeVisible();
  expect(hashParams(page).view).toBe('preview');
  await expect(page.getByRole('button', { name: 'Diagram', exact: true })).toHaveClass('on');

  await focusMain(page);
  await page.keyboard.press('f');
  await expect(page.locator('#main table.fileview')).toContainText('accTitle: Build flow');
  expect(hashParams(page).view).toBe('file');
  await page.keyboard.press('f');
  await expect(view.getByRole('img', { name: 'Build flow' })).toBeVisible();
});

test('a .mermaid file in a plain folder opens drawn', async ({ page, folder, revue }) => {
  folder.write('chart.mermaid', 'pie title Pets\n  "Dogs" : 3\n  "Cats" : 2\n');
  const s = await revue.start({ folder });
  await s.open(page, 'file=chart.mermaid');
  await expect(page.locator('#main .diagram-view').getByRole('img', { name: 'Mermaid diagram' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Diagram', exact: true })).toHaveClass('on');
});

test('the renderer is a sandbox the page keeps apart, and nothing a diagram names is requested', async ({ page, repo, revue }) => {
  // Labels that would load or run something if they were markup, and click handlers, which the strict security level
  // drops.
  const names = recordRequests(page, /pwn|evil/);
  repo.write('README.md', `# Sandbox\n\n${mermaid(`flowchart LR
  accTitle: Payloads
  A["<img src='/pwn.png'>"] --> B["<script>fetch('/pwn-script')</script>"]
  click A href "http://evil.example.test/x"
  click B call fetch('/pwn-call')
`)}`);
  repo.commit('initial');
  const s = await revue.start();
  await s.open(page, 'file=README.md&view=preview');
  const img = preview(page).getByRole('img', { name: 'Payloads' });
  await expect(img).toBeVisible();
  // The renderer: sandboxed with scripts only, so its origin is opaque and the page can't see into it.
  const frame = page.locator('iframe.diagram-frame');
  await expect(frame).toHaveAttribute('sandbox', 'allow-scripts');
  expect(await frame.evaluate((el: HTMLIFrameElement) => el.contentDocument)).toBeNull();
  expect(await frame.evaluate((el: HTMLIFrameElement) => { try { return !!el.contentWindow!.document; } catch { return 'blocked'; } }))
    .toBe('blocked');
  await img.click();
  await page.waitForTimeout(300);
  expect(names).toEqual([]);
  // The control: the drawing is there, and drawn.
  expect((await sizes(img)).natural[0]).toBeGreaterThan(50);
});
