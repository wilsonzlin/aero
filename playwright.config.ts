import { defineConfig, devices } from '@playwright/test';

const DEV_PORT = 5173;
const PREVIEW_PORT = 4173;

export default defineConfig({
  // Keep Playwright tests under `tests/`, but only run the dedicated browser suites.
  // (We also have Node/Vitest unit tests elsewhere under `tests/`.)
  timeout: 30_000,
  expect: {
    timeout: 5_000,
  },
  fullyParallel: true,
  reporter: process.env.CI ? 'dot' : 'list',
  testDir: './tests',
  testMatch: ['e2e/**/*.spec.ts', 'playwright/**/*.spec.ts'],
  use: {
    trace: 'on-first-retry',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        browserName: 'chromium',
        launchOptions: {
          args: ['--enable-unsafe-webgpu'],
        },
      },
    },
    {
      name: 'firefox',
      use: { ...devices['Desktop Firefox'], browserName: 'firefox' },
    },
    {
      name: 'webkit',
      use: { ...devices['Desktop Safari'], browserName: 'webkit' },
    },
  ],
  webServer: [
    {
      command: `npm run dev -- --host 127.0.0.1 --port ${DEV_PORT} --strictPort`,
      port: DEV_PORT,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'npm run serve:coi',
      port: PREVIEW_PORT,
      reuseExistingServer: !process.env.CI,
    },
  ],
});
