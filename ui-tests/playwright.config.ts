// Browser tests for public/. Each test gets its own temp repo, its own revue server (the debug binary, built once
// by global-setup.ts) and a fresh browser context, so everything runs in parallel. WebKit is left out: on Linux it
// needs system libraries (libevent, libavif, libmanette) that only `sudo apt-get` can install.
import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './specs',
  globalSetup: require.resolve('./global-setup'),
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 30_000,
  // On CI, give up on the whole run after 15 minutes (it takes about 4): a worker stuck outside any test, such as on a
  // browser that can't start, is otherwise never timed out.
  globalTimeout: process.env.CI ? 15 * 60_000 : 0,
  // Live updates go through a 250 ms debounce and a refetch; leave room for a loaded machine. toPass() has no timeout
  // of its own by default, so a failing one would use up the whole test's 30 s.
  expect: { timeout: 10_000, toPass: { timeout: 10_000 } },
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    trace: 'retain-on-failure',
    // Never bypassCSP: a Content-Security-Policy regression must fail these tests.
    viewport: { width: 1280, height: 800 },
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'], viewport: { width: 1280, height: 800 } } },
    { name: 'firefox', use: { ...devices['Desktop Firefox'], viewport: { width: 1280, height: 800 } } },
  ],
});
