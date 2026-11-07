// The README's demo GIF and GitHub's social preview card, of revue on a small made-up shop project with uncommitted
// work. `npm run screenshots` (screenshots.config.ts) writes them to docs/: demo-{light,dark}.gif, in the light and the
// dark theme (see the demo's test), and social-preview.png, the card GitHub shows for the repo's links. `npm test`
// never runs this.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import type { Page } from '@playwright/test';
import { test, expect, Repo, REPO_ROOT, section, targetRows } from '../fixtures';

const SCHEMES = ['light', 'dark'] as const;
// The GIF's width: the 1440 px page, downscaled with Lanczos from its 2x screenshots, so it stays sharp at the width
// GitHub draws a README image (about 900 px) and the code is still readable.
const GIF_WIDTH = 1200;

const CART_BEFORE = `export interface LineItem {
  sku: string;
  name: string;
  price: number;
  quantity: number;
}

export interface Cart {
  id: string;
  items: LineItem[];
}

export function addItem(cart: Cart, item: LineItem): Cart {
  return { ...cart, items: [...cart.items, item] };
}
`;

const CART_AFTER = CART_BEFORE.replace('  price: number;', '  unitPrice: number;');

const PRICING_BEFORE = `import { Cart, LineItem } from './cart';
import { promoDiscount } from './promo';

// Sales tax for the store's home region.
export const TAX_RATE = 0.08;

export function lineTotal(item: LineItem): number {
  return item.price * item.quantity;
}

export function subtotal(items: LineItem[]): number {
  let sum = 0;
  for (const item of items) sum += lineTotal(item);
  return sum;
}

export function shipping(cart: Cart): number {
  if (cart.items.length === 0) return 0;
  return subtotal(cart.items) >= 50 ? 0 : 4.99;
}

export function total(cart: Cart): number {
  const base = subtotal(cart.items) - promoDiscount(cart);
  const tax = base * TAX_RATE;
  return round(base + tax + shipping(cart));
}

function round(amount: number): number {
  return Math.round(amount * 100) / 100;
}
`;

const PRICING_AFTER = `import { Cart, LineItem } from './cart';
import { Coupon, discountFor } from './coupons';

// Sales tax for the store's home region.
export const TAX_RATE = 0.0825;
export const FREE_SHIPPING_OVER = 50;

export function lineTotal(item: LineItem): number {
  return item.unitPrice * item.quantity;
}

export function subtotal(items: LineItem[]): number {
  let sum = 0;
  for (const item of items) sum += lineTotal(item);
  return sum;
}

export function shipping(cart: Cart): number {
  if (cart.items.length === 0) return 0;
  const free = subtotal(cart.items) >= FREE_SHIPPING_OVER;
  return free ? 0 : 4.99;
}

export function total(cart: Cart, coupon?: Coupon): number {
  const base = subtotal(cart.items);
  const discount = coupon ? discountFor(coupon, base) : 0;
  const tax = (base - discount) * TAX_RATE;
  return round(base - discount + tax + shipping(cart));
}

function round(amount: number): number {
  return Math.round(amount * 100) / 100;
}
`;

const PROMO = `import { Cart } from './cart';

// The spring sale: 10% off every cart.
export function promoDiscount(cart: Cart): number {
  return cart.items.length > 0 ? 0.1 : 0;
}
`;

const COUPONS = `export interface Coupon {
  code: string;
  percentOff?: number;
  amountOff?: number;
}

export function discountFor(coupon: Coupon, base: number): number {
  if (coupon.percentOff) return (base * coupon.percentOff) / 100;
  return Math.min(coupon.amountOff ?? 0, base);
}
`;

const TEST_BEFORE = `import { total } from '../src/pricing';

test('adds tax', () => {
  const cart = { id: 'c1', items: [{ sku: 'a', name: 'Mug', price: 60, quantity: 1 }] };
  expect(total(cart)).toBe(64.8);
});
`;

const TEST_AFTER = `import { total } from '../src/pricing';

test('adds tax', () => {
  const cart = { id: 'c1', items: [{ sku: 'a', name: 'Mug', unitPrice: 60, quantity: 1 }] };
  expect(total(cart)).toBe(64.95);
});

test('applies a coupon before tax', () => {
  const cart = { id: 'c1', items: [{ sku: 'a', name: 'Mug', unitPrice: 60, quantity: 1 }] };
  expect(total(cart, { code: 'TEN', percentOff: 10 })).toBe(58.46);
});
`;

const TAX_LINE = PRICING_AFTER.split('\n').indexOf('  const tax = (base - discount) * TAX_RATE;') + 1;
const NOTE = 'Tax is now charged on the discounted price. Check this against the checkout spec: some regions tax the '
  + 'full price before coupons.';

// The made-up project with its uncommitted work, inside the temp folder so the header shows the project's name rather
// than a temp dir's.
function makeShop(dir: string): Repo {
  const repo = new Repo(path.join(dir, 'shop-api'));
  fs.mkdirSync(repo.dir);
  repo.init();
  repo
    .write('README.md', '# shop-api\n\nCart, pricing and checkout for the shop.\n')
    .write('package.json', '{\n  "name": "shop-api",\n  "private": true,\n  "scripts": { "test": "jest" }\n}\n')
    .write('tsconfig.json', '{\n  "compilerOptions": { "strict": true, "target": "ES2022" }\n}\n')
    .write('src/cart.ts', CART_BEFORE)
    .write('src/checkout.ts', "import { Cart } from './cart';\nimport { total } from './pricing';\n\nexport function amountDue(cart: Cart): number {\n  return total(cart);\n}\n")
    .write('src/pricing.ts', PRICING_BEFORE)
    .write('src/promo.ts', PROMO)
    .write('tests/pricing.test.ts', TEST_BEFORE);
  repo.commit('Price carts with tax and shipping');
  // The uncommitted work: a coupon replaces the spring promo (the new file staged, so the tree marks it A).
  repo
    .write('src/cart.ts', CART_AFTER)
    .write('src/pricing.ts', PRICING_AFTER)
    .write('src/coupons.ts', COUPONS)
    .rm('src/promo.ts')
    .write('tests/pricing.test.ts', TEST_AFTER)
    .add('src/coupons.ts');
  repo.settle();
  return repo;
}

// GitHub's social preview card (docs/social-preview.png, uploaded by hand in the repo's Settings → General → Social
// preview): the name and tagline beside the new side of src/pricing.ts's split diff around the agent's note, in the
// light theme, which reads on most sites' backgrounds. GitHub wants 2:1 (1280x640 is best) and under 1 MB. The 👀 needs
// an emoji font.
test('Social preview card', async ({ folder, revue, page }) => {
  const repo = makeShop(folder.dir);
  const server = await revue.start({ folder: repo });
  await server.annotate({ mode: 'working', file: 'src/pricing.ts', line: TAX_LINE, side: 'new', text: NOTE });

  await page.emulateMedia({ colorScheme: 'light' });
  await server.open(page, 'mode=working&file=src/pricing.ts');
  await expect(page.locator('#main table.code.split')).toBeVisible();
  await expect(page.locator('#main .note-box')).toContainText('discounted price');
  await page.evaluate(() => document.fonts.ready);
  await page.mouse.move(0, 0);
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  const out = path.join(REPO_ROOT, 'docs');
  fs.mkdirSync(out, { recursive: true });
  const note = await page.locator('#main .note-box').boundingBox();
  expect(note, 'the note is on the page').not.toBeNull();
  const crop = { x: note!.x, y: note!.y - 330, width: note!.width, height: note!.height + 400 };
  const shot = await page.screenshot({ clip: crop, animations: 'disabled', caret: 'hide' });
  // A page of its own: revue's page has a CSP that would refuse the card's inline styles and data: image.
  const card = await page.context().newPage();
  await card.setViewportSize({ width: 1280, height: 640 });
  await card.setContent(`<!doctype html><meta charset="utf-8"><style>
    html, body { margin: 0; }
    body { box-sizing: border-box; width: 1280px; height: 640px; overflow: hidden; display: flex; align-items: center;
      gap: 44px; padding-left: 72px; font-family: system-ui, sans-serif; color: #1f2328;
      background: linear-gradient(135deg, #f6f8fa 0%, #e8f0fb 100%); }
    .text { flex: 0 0 460px; }
    h1 { margin: 0 0 22px; font-size: 76px; line-height: 1; font-weight: 800; letter-spacing: -1px; }
    .tag { margin: 0 0 22px; font-size: 34px; line-height: 1.25; font-weight: 650; }
    .sub { margin: 0; font-size: 21px; line-height: 1.5; color: #57606a; }
    .url { margin-top: 34px; font-size: 18px; color: #57606a; }
    img { flex: none; width: ${Math.round(crop.width)}px; border-radius: 12px;
      box-shadow: 0 18px 50px rgba(31, 35, 40, 0.22), 0 0 0 1px rgba(31, 35, 40, 0.08); }
    </style>
    <div class="text">
      <h1>👀 revue</h1>
      <p class="tag">Let your coding agent show you what it changed.</p>
      <p class="sub">Live git diffs in your browser, with a built-in MCP server. One binary that needs only git.</p>
      <div class="url">github.com/jmsheas3/revue</div>
    </div>
    <img alt="" src="data:image/png;base64,${shot.toString('base64')}">`);
  await card.evaluate(() => document.fonts.ready);
  const preview = path.join(out, 'social-preview.png');
  await card.screenshot({ path: preview, scale: 'css', animations: 'disabled' });
  expect(fs.statSync(preview).size, 'GitHub refuses a social preview of 1 MB or more').toBeLessThan(1_000_000);
  await card.close();
});

// A caption over the page, set through the CSSOM (revue's CSP allows no inline styles). It is the demo's narration, not
// part of revue: dark on the light theme and light on the dark one (light-dark() follows the page's color-scheme), and
// above the spot where revue's own toasts appear, so the toast an agent's navigation shows stays readable.
async function caption(page: Page, text: string) {
  await page.evaluate((t) => {
    let el = document.getElementById('demo-caption');
    if (!el) {
      el = document.createElement('div');
      el.id = 'demo-caption';
      Object.assign(el.style, {
        position: 'fixed', left: '50%', bottom: '64px', transform: 'translateX(-50%)', zIndex: '1000',
        width: 'max-content', maxWidth: '80vw', padding: '12px 22px', borderRadius: '12px', textAlign: 'center',
        pointerEvents: 'none', font: '600 20px/1.35 system-ui, sans-serif',
        background: 'light-dark(rgba(20, 22, 28, 0.94), rgba(244, 245, 248, 0.96))',
        color: 'light-dark(#fff, #14161c)', boxShadow: '0 6px 24px rgba(0, 0, 0, 0.35)',
      });
      document.body.append(el);
    }
    el.textContent = t;
  }, text);
}

// The README's demo: the user asks their agent to show its change, the agent opens it in the tab and leaves a note,
// then the user's own edit lands live and the note is marked outdated. revue's jumps and updates are instant, so each
// step is a screenshot (sharp, unlike a video of code) held for a few seconds, taken in both themes and joined by
// ffmpeg, which `npm run screenshots` therefore needs, into docs/demo-light.gif and docs/demo-dark.gif.
test('README demo', async ({ folder, revue, page }) => {
  const repo = makeShop(folder.dir);
  const server = await revue.start({ folder: repo });
  const frames: { light: string; dark: string; seconds: number }[] = [];
  const work = path.join(folder.dir, 'frames'); // outside the repo, so the watcher never sees them
  fs.mkdirSync(work);
  const frame = async (seconds: number) => {
    const shots = { light: '', dark: '' };
    for (const scheme of SCHEMES) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
      shots[scheme] = path.join(work, `${scheme}-${frames.length}.png`);
      await page.screenshot({ path: shots[scheme], animations: 'disabled', caret: 'hide' });
    }
    frames.push({ ...shots, seconds });
  };

  // Taller than the screenshots, so the caption sits below the lines the agent points at rather than over them.
  await page.setViewportSize({ width: 1440, height: 860 });
  await page.emulateMedia({ colorScheme: 'light' });
  await server.open(page, 'mode=working');
  await expect(section(page, 'src/pricing.ts').locator('table.code')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await page.mouse.move(0, 0);
  await caption(page, 'Your coding agent just changed five files. revue shows every diff, live.');
  await frame(3);

  await caption(page, 'You ask it: “Show me the coupon change.”');
  await frame(1.8);
  const total = PRICING_AFTER.split('\n').indexOf('export function total(cart: Cart, coupon?: Coupon): number {') + 1;
  const nav = await server.navigate({ mode: 'working', file: 'src/pricing.ts', view: 'diff', line: total, endLine: total + 4 });
  expect(nav.tabs, 'the page follows the agent').toBeGreaterThan(0);
  await expect(targetRows(page).first()).toBeVisible();
  await expect(page.locator('#toast.on')).toContainText('Showing src/pricing.ts');
  await caption(page, 'The agent opens it in your browser and points at the lines.');
  await frame(3);
  // In real time the toast is gone by the next step; this script gets there in a moment.
  await expect(page.locator('#toast.on')).toHaveCount(0);

  await server.annotate({ mode: 'working', file: 'src/pricing.ts', line: TAX_LINE, side: 'new', text: NOTE });
  await expect(page.locator('#main .note-box')).toContainText('discounted price');
  await caption(page, 'It leaves a note on the line it wants you to check.');
  await frame(3.5);

  await caption(page, 'You fix it in your editor and save…');
  await frame(1.8);
  repo.write('src/pricing.ts', PRICING_AFTER.replace('(base - discount) * TAX_RATE', 'base * TAX_RATE'));
  await expect(page.locator('#main td.src', { hasText: 'const tax = base * TAX_RATE;' }).first()).toBeVisible();
  await expect(page.locator('#main .note-stale')).toBeVisible();
  await caption(page, '…and the diff updates as you save. The note is now marked outdated.');
  await frame(4);

  for (const scheme of SCHEMES) {
    // The concat demuxer holds each frame for its duration; the last is listed again, or its duration is dropped.
    const list = path.join(work, `${scheme}.txt`);
    fs.writeFileSync(list, frames.map(f => `file '${f[scheme]}'\nduration ${f.seconds}\n`).join('')
      + `file '${frames[frames.length - 1][scheme]}'\n`);
    const gif = path.join(REPO_ROOT, 'docs', `demo-${scheme}.gif`);
    const ff = spawnSync('ffmpeg', [
      '-hide_banner', '-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', list,
      '-vf', `scale=${GIF_WIDTH}:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=none`,
      '-fps_mode', 'vfr', '-loop', '0', gif,
    ], { encoding: 'utf8' });
    expect(ff.error?.message ?? '', 'ffmpeg must be installed to make the demo GIF').toBe('');
    expect(ff.status, ff.stderr).toBe(0);
  }
});
