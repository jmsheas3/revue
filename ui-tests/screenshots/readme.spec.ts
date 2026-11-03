// The README's screenshots: revue on a small made-up shop project with uncommitted work, showing one file's split diff
// with an agent note on it, in the light and the dark theme. `npm run screenshots` (screenshots.config.ts) writes them
// to docs/screenshot-light.png and docs/screenshot-dark.png; `npm test` never runs this.
import fs from 'node:fs';
import path from 'node:path';
import { test, expect, Repo, REPO_ROOT, treeRow } from '../fixtures';

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

test('README screenshots', async ({ folder, revue, page }) => {
  // Inside the temp folder, so the header shows the project's name rather than a temp dir's.
  const repo = new Repo(path.join(folder.dir, 'shop-api'));
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

  const server = await revue.start({ folder: repo });
  const taxLine = PRICING_AFTER.split('\n').indexOf('  const tax = (base - discount) * TAX_RATE;') + 1;
  await server.annotate({
    mode: 'working', file: 'src/pricing.ts', line: taxLine, side: 'new',
    text: 'Tax is now charged on the discounted price. Check this against the checkout spec: some regions tax the full price before coupons.',
  });

  await page.emulateMedia({ colorScheme: 'light' });
  await server.open(page, 'mode=working&file=src/pricing.ts');
  await expect(page.locator('#main table.code.split')).toBeVisible();
  await expect(page.locator('#main .note-box')).toContainText('discounted price');
  await expect(treeRow(page, 'src/coupons.ts')).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await page.mouse.move(0, 0);

  const out = path.join(REPO_ROOT, 'docs');
  fs.mkdirSync(out, { recursive: true });
  for (const scheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
    await page.screenshot({ path: path.join(out, `screenshot-${scheme}.png`), animations: 'disabled', caret: 'hide' });
  }
});
