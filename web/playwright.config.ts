import { defineConfig } from '@playwright/test';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB_DIR = dirname(fileURLToPath(import.meta.url));
const DEV_URL = 'http://127.0.0.1:5173';

export default defineConfig({
  testDir: './tests',
  testMatch: ['**/*.spec.{ts,js,cjs,mjs}'],
  use: {
    baseURL: DEV_URL,
    headless: true,
    // Ensure WebGL/WebGPU have a deterministic software fallback in headless CI
    // environments (many are GPU-blocklisted).
    launchOptions: {
      args: [
        '--force-color-profile=srgb',
        '--ignore-gpu-blocklist',
        '--use-angle=swiftshader',
        '--use-gl=swiftshader',
        '--disable-gpu-sandbox',
      ],
    },
  },
  webServer: {
    // Use Vite directly instead of `npm run dev` so Playwright doesn't depend on
    // `predev` hooks that build wasm artifacts (tests in `web/tests/` are
    // presenter/worker focused and do not require the full wasm toolchain).
    command: 'npx vite --host 127.0.0.1 --port 5173 --strictPort',
    url: DEV_URL,
    reuseExistingServer: !process.env.CI,
    cwd: WEB_DIR,
  },
});
