import { defineConfig } from '@playwright/test';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB_DIR = dirname(fileURLToPath(import.meta.url));
const DEV_URL = 'http://127.0.0.1:5173';

export default defineConfig({
  testDir: './tests',
  use: {
    baseURL: DEV_URL,
    headless: true,
  },
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 5173 --strictPort',
    url: DEV_URL,
    reuseExistingServer: !process.env.CI,
    cwd: WEB_DIR,
  },
});
