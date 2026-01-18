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

function resolveFreePort(opts: { portEnv: string; originEnv: string; start: number; maxAttempts?: number }): number {
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
  // See `playwright.config.ts`: avoid port-selection races when multiple Playwright processes run
  // concurrently by spreading each process into a different probe range.
  const stride = 100;
  const buckets = 500;
  const offset = (process.pid % buckets) * stride;
  const start = baseStart + offset;
  if (start + maxAttempts >= 65536) return baseStart;
  return start;
}

const DEV_PORT = resolveFreePort({
  portEnv: 'AERO_PLAYWRIGHT_DEV_PORT',
  originEnv: 'AERO_PLAYWRIGHT_DEV_ORIGIN',
  start: applyPortStartOffset(5173),
});
const DEV_ORIGIN = process.env.AERO_PLAYWRIGHT_DEV_ORIGIN ?? `http://127.0.0.1:${DEV_PORT}`;
process.env.AERO_PLAYWRIGHT_DEV_ORIGIN = DEV_ORIGIN;

const REUSE_SERVER_SETTING = (process.env.AERO_PLAYWRIGHT_REUSE_SERVER ?? '').toLowerCase();
const REUSE_EXISTING_SERVER =
  !process.env.CI && (REUSE_SERVER_SETTING === '1' || REUSE_SERVER_SETTING === 'true');

/**
 * Golden-image GPU correctness tests.
 *
 * Notes on stability:
 * - Microtests render only flat, integer-friendly colors.
 * - WebGPU test uses scissor rects (integer pixel bounds) instead of relying on edge rasterization.
 * - Image comparison is strict by default (0 differing pixels) because the scenes are designed to
 *   avoid GPU-dependent antialiasing / filtering.
 */
export default defineConfig({
  testDir: './tests/e2e/playwright',
  testMatch: ['**/gpu_golden.spec.ts'],
  fullyParallel: true,
  retries: process.env.CI ? 1 : 0,
  reporter: [['list'], ['html', { open: 'never' }]],
  outputDir: 'test-results',
  webServer: {
    command: `npm run dev:harness -- --host 127.0.0.1 --port ${DEV_PORT} --strictPort`,
    port: DEV_PORT,
    reuseExistingServer: REUSE_EXISTING_SERVER,
  },
  use: {
    baseURL: DEV_ORIGIN,
    headless: true,
    viewport: { width: 800, height: 600 },
    deviceScaleFactor: 1,
    colorScheme: 'light',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure'
  },
  projects: [
    {
      name: 'chromium-webgpu',
      use: {
        ...devices['Desktop Chrome'],
        launchOptions: {
          args: [
            // WebGPU is generally enabled by default in modern Chromium, but CI environments
            // are frequently configured more conservatively.
            '--enable-unsafe-webgpu',
            '--enable-features=WebGPU',
            // Prefer software paths for determinism and to avoid reliance on a host GPU.
            '--use-angle=swiftshader',
            '--use-gl=swiftshader',
            '--disable-gpu-sandbox'
          ]
        }
      }
    },
    {
      name: 'firefox-webgl2',
      use: {
        ...devices['Desktop Firefox']
      }
    }
  ]
});
