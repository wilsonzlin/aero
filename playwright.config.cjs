const { defineConfig, devices } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './web/tests',
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

