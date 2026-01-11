import { defineConfig, devices } from '@playwright/test';

const DEV_PORT = 5173;
const PREVIEW_PORT = 4173;
const CSP_POC_PORT = 4180;
const DEV_ORIGIN = `http://127.0.0.1:${DEV_PORT}`;
const EXPOSE_GC = process.env.AERO_PLAYWRIGHT_EXPOSE_GC === '1';
const CHROMIUM_ARGS = [
  '--enable-unsafe-webgpu',
  // Keep screenshot colors deterministic across environments.
  '--force-color-profile=srgb',
  ...(EXPOSE_GC ? ['--js-flags=--expose-gc'] : []),
];

/**
 * Extra Chromium flags used for the `chromium-webgpu` project.
 *
 * WebGPU availability is highly dependent on OS, drivers, and whether the
 * browser is running headless. CI runners are also frequently GPU-blocklisted.
 * These flags aim to maximize the chance that `navigator.gpu` is present and
 * that Dawn can create an adapter/device.
 */
const CHROMIUM_WEBGPU_ARGS = [
  ...CHROMIUM_ARGS,
  // WebGPU is generally enabled by default in modern Chromium, but CI
  // environments can be configured more conservatively.
  '--enable-features=WebGPU',
  // CI VMs are often GPU-blocklisted; allow Chromium to try initializing GPU
  // features (including WebGPU) anyway.
  '--ignore-gpu-blocklist',
  // Prefer software paths for determinism and to avoid reliance on a host GPU.
  '--use-angle=swiftshader',
  '--use-gl=swiftshader',
  // Required in some containerized CI environments; harmless elsewhere.
  '--disable-gpu-sandbox',
];

export default defineConfig({
  // Canonical Playwright suites live under `tests/e2e/`.
  timeout: 30_000,
  expect: {
    timeout: 5_000,
    // Default tolerance for screenshot comparisons.
    // Keep this low so that real visual regressions are caught,
    // while allowing for tiny anti-aliasing diffs in CI.
    toHaveScreenshot: {
      maxDiffPixelRatio: 0.005,
    },
  },
  testDir: './tests/e2e',
  testMatch: ['**/*.spec.ts'],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [
        ['dot'],
        ['junit', { outputFile: 'test-results/junit.xml' }],
        ['html', { open: 'never', outputFolder: 'playwright-report' }],
      ]
    : [
        ['list'],
        ['html', { open: 'never', outputFolder: 'playwright-report' }],
      ],
  outputDir: 'test-results',
  // Keep screenshot baselines in a dedicated, predictable location next to the spec.
  snapshotPathTemplate:
    '{testDir}/{testFileDir}/__screenshots__/{testFileName}/{arg}{-projectName}{-platform}{ext}',
  use: {
    baseURL: DEV_ORIGIN,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    contextOptions: {
      reducedMotion: 'reduce',
    },
    colorScheme: 'light',
    locale: 'en-US',
    timezoneId: 'UTC',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        browserName: 'chromium',
        launchOptions: {
          args: CHROMIUM_ARGS,
        },
      },
      grepInvert: /@webgpu/,
    },
    {
      name: 'firefox',
      use: { ...devices['Desktop Firefox'], browserName: 'firefox' },
      grepInvert: /@webgpu/,
    },
    {
      name: 'webkit',
      use: { ...devices['Desktop Safari'], browserName: 'webkit' },
      grepInvert: /@webgpu/,
    },
    {
      name: 'chromium-webgpu',
      use: {
        ...devices['Desktop Chrome'],
        browserName: 'chromium',
        launchOptions: {
          args: CHROMIUM_WEBGPU_ARGS,
        },
      },
      grep: /@webgpu/,
    },
  ],
  webServer: [
    {
      command: `npm run dev:harness -- --host 127.0.0.1 --port ${DEV_PORT} --strictPort`,
      port: DEV_PORT,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'npm run serve:coi:harness',
      port: PREVIEW_PORT,
      timeout: 300_000,
      reuseExistingServer: !process.env.CI,
    },
    {
      // Dedicated server for CSP (wasm-unsafe-eval) matrix tests.
      command: `node server/poc-server.mjs --port ${CSP_POC_PORT}`,
      port: CSP_POC_PORT,
      reuseExistingServer: !process.env.CI,
    },
  ],
});
