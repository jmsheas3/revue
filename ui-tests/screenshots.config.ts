// Makes the README's demo GIF and GitHub's social preview card (screenshots/readme.spec.ts, written to docs/ at the
// repo root; the GIF needs ffmpeg): npm run screenshots.
// A config of its own, so npm test never runs it. Chromium only, at 2x for sharp text, and 1440x760: at 1280 px wide
// the diff is too narrow for split and falls back to unified, and 760 high keeps the lines around the agent's note,
// which the card crops, on the first screen (the demo sets a taller page of its own).
import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './screenshots',
  globalSetup: require.resolve('./global-setup'),
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: 'list',
  use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 760 }, deviceScaleFactor: 2 },
});
