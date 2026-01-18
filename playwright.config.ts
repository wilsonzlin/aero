import { spawnSync } from 'node:child_process';

import { defineConfig, devices } from '@playwright/test';

function resolvePortFromEnv(name: string): number | null {
  const raw = process.env[name];
  if (!raw) return null;
  const port = Number.parseInt(raw, 10);
  if (!Number.isFinite(port) || port <= 0 || port >= 65536) {
    throw new Error(`${name} must be a TCP port number in 1..65535 (got ${raw})`);
  }
  return port;
}

function resolveOriginFromEnv(name: string): URL | null {
  const raw = process.env[name];
  if (!raw) return null;
  try {
    return new URL(raw);
  } catch (err) {
    throw new Error(`${name} must be a valid URL (got ${raw}): ${String(err)}`);
  }
}

function resolveFreePort(opts: {
  portEnv: string;
  originEnv: string;
  start: number;
  maxAttempts?: number;
}): number {
  const fromEnv = resolvePortFromEnv(opts.portEnv);
  if (fromEnv !== null) return fromEnv;

  const fromOrigin = resolveOriginFromEnv(opts.originEnv);
  if (fromOrigin) {
    const port = Number.parseInt(fromOrigin.port, 10);
    if (!Number.isFinite(port) || port <= 0 || port >= 65536) {
      throw new Error(`${opts.originEnv} must include an explicit TCP port (got ${fromOrigin.toString()})`);
    }
    return port;
  }

  const maxAttempts = opts.maxAttempts ?? 32;
  const script = `
    const net = require('node:net');
    const host = '127.0.0.1';
    const start = ${opts.start};
    const attempts = ${maxAttempts};

    function unrefBestEffort(handle) {
      let unref;
      try {
        unref = handle && handle.unref;
      } catch {
        return;
      }
      if (typeof unref !== 'function') return;
      try {
        unref.call(handle);
      } catch {
        // ignore
      }
    }

    function canListen(port) {
      return new Promise((resolve) => {
        const server = net.createServer();
        unrefBestEffort(server);
        server.once('error', () => resolve(false));
        server.listen(port, host, () => {
          server.close(() => resolve(true));
        });
      });
    }

    (async () => {
      for (let i = 0; i < attempts; i++) {
        const port = start + i;
        if (await canListen(port)) {
          process.stdout.write(String(port));
          return;
        }
      }
      process.stderr.write('No free port found');
      process.exit(1);
    })().catch((err) => {
      process.stderr.write(String(err && err.stack ? err.stack : err));
      process.exit(1);
    });
  `.trim();

  const proc = spawnSync(process.execPath, ['-e', script], { encoding: 'utf8' });
  if (proc.status !== 0) {
    throw new Error(
      `Failed to locate a free port (start=${opts.start} attempts=${maxAttempts}): ${proc.stderr || proc.stdout || 'unknown error'}`,
    );
  }

  const port = Number.parseInt(proc.stdout.trim(), 10);
  if (!Number.isFinite(port) || port <= 0 || port >= 65536) {
    throw new Error(`Port probe returned invalid output: ${JSON.stringify(proc.stdout)}`);
  }
  return port;
}

function applyPortStartOffset(baseStart: number, maxAttempts = 32): number {
  // Avoid port-selection races when multiple Playwright processes run concurrently (common in the
  // Grind worker swarm and local dev when iterating on a single spec in multiple terminals).
  //
  // `resolveFreePort` probes and returns the first available port in a fixed range, but it cannot
  // reserve that port; another process can claim it between the probe and Vite binding it. Spread
  // each Playwright process into a different search range by using a PID-derived offset.
  const stride = 100;
  const buckets = 500;
  const offset = (process.pid % buckets) * stride;
  const start = baseStart + offset;
  // Keep the scan window within the TCP port range; fall back to the base range if we would
  // overflow.
  if (start + maxAttempts >= 65536) return baseStart;
  return start;
}

// Some runner environments already have common Vite ports bound (e.g. 5173/4173). Probe for an
// available port so Playwright can still boot its web servers, and export the resolved origins
// for tests that need to hardcode them (e.g. COOP/COEP/CSP coverage).
const DEV_PORT = resolveFreePort({
  portEnv: 'AERO_PLAYWRIGHT_DEV_PORT',
  originEnv: 'AERO_PLAYWRIGHT_DEV_ORIGIN',
  start: applyPortStartOffset(5173),
});
const PREVIEW_PORT = resolveFreePort({
  portEnv: 'AERO_PLAYWRIGHT_PREVIEW_PORT',
  originEnv: 'AERO_PLAYWRIGHT_PREVIEW_ORIGIN',
  start: applyPortStartOffset(4173),
});
const CSP_POC_PORT = resolveFreePort({
  portEnv: 'AERO_PLAYWRIGHT_CSP_PORT',
  originEnv: 'AERO_PLAYWRIGHT_CSP_ORIGIN',
  start: applyPortStartOffset(4180),
});
const DEV_ORIGIN = process.env.AERO_PLAYWRIGHT_DEV_ORIGIN ?? `http://127.0.0.1:${DEV_PORT}`;
const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? `http://127.0.0.1:${PREVIEW_PORT}`;
const CSP_POC_ORIGIN = process.env.AERO_PLAYWRIGHT_CSP_ORIGIN ?? `http://127.0.0.1:${CSP_POC_PORT}`;
process.env.AERO_PLAYWRIGHT_DEV_ORIGIN = DEV_ORIGIN;
process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN = PREVIEW_ORIGIN;
process.env.AERO_PLAYWRIGHT_CSP_ORIGIN = CSP_POC_ORIGIN;
const EXPOSE_GC = process.env.AERO_PLAYWRIGHT_EXPOSE_GC === '1';
const REUSE_SERVER_SETTING = (process.env.AERO_PLAYWRIGHT_REUSE_SERVER ?? '').toLowerCase();
const REUSE_EXISTING_SERVER =
  !process.env.CI && (REUSE_SERVER_SETTING === '1' || REUSE_SERVER_SETTING === 'true');
const CHROMIUM_ARGS = [
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
  // WebGPU is guarded behind a dedicated project so non-WebGPU test runs don't depend on
  // Chromium's evolving WebGPU defaults (and to avoid flakiness/crashes on environments where
  // WebGPU init is unstable).
  '--enable-unsafe-webgpu',
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
      // Default to `false` locally to avoid accidentally reusing a different Vite
      // server on the same port (e.g. the legacy `web/` Vite app via `npm run dev:web`
      // or `npm -w web run dev`). Opt in explicitly when iterating on E2E:
      // `AERO_PLAYWRIGHT_REUSE_SERVER=1 npm run test:e2e`.
      reuseExistingServer: REUSE_EXISTING_SERVER,
    },
    {
      command: `npm run serve:coi:harness -- --host 127.0.0.1 --port ${PREVIEW_PORT} --strictPort`,
      port: PREVIEW_PORT,
      timeout: 300_000,
      reuseExistingServer: REUSE_EXISTING_SERVER,
    },
    {
      // Dedicated server for CSP (wasm-unsafe-eval) matrix tests.
      command: `node server/poc-server.mjs --port ${CSP_POC_PORT}`,
      port: CSP_POC_PORT,
      reuseExistingServer: REUSE_EXISTING_SERVER,
    },
  ],
});
