const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './web/tests',
  // `web/tests/` contains a mix of Playwright specs and Vitest unit tests. Keep
  // this config focused on the lightweight fallback demo spec so repo-root
  // `npm ci` is sufficient for this workflow.
  testMatch: ['webgl2-fallback.spec.cjs'],
  timeout: 30_000,
  retries: process.env.CI ? 2 : 0,
  use: {
    baseURL: 'http://127.0.0.1:4173',
    headless: true,
    ...devices['Desktop Chrome'],
  },
  webServer: {
    command: 'node web/scripts/serve.cjs --port 4173',
    port: 4173,
    reuseExistingServer: !process.env.CI,
    stdout: 'pipe',
    stderr: 'pipe',
  },
});
