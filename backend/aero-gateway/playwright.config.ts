import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './test/e2e',
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    ...devices['Desktop Chrome'],
    headless: true,
    // The backend E2E suite runs a local HTTPS reverse proxy with a
    // self-signed certificate in order to exercise WSS connectivity.
    ignoreHTTPSErrors: true,
  },
  projects: [
    {
      name: 'chromium',
    },
  ],
});
