import { defineConfig, devices } from '@playwright/test';

const DEV_PORT = 5173;

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
    reuseExistingServer: !process.env.CI,
  },
  use: {
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
