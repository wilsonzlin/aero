import { test, expect, chromium } from "@playwright/test";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";

import { startStaticServer } from "./utils/static_server";

test("shader translation is persisted and skipped on next run", async ({}, testInfo) => {
  // This test uses a Chromium persistent context to validate IndexedDB persistence.
  // Skip in other projects to avoid running the same coverage multiple times.
  if (testInfo.project.name !== "chromium") test.skip();

  const rootDir = path.resolve(process.cwd(), "web");
  const server = await startStaticServer(rootDir);

  try {
    // Use a persistent browser profile to ensure IndexedDB survives across browser restarts.
    const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-shader-cache-"));

    async function runOnce(): Promise<{
      cacheHit: boolean;
      translationMs: number;
      telemetry: any;
      logs: string[];
    }> {
      const logs: string[] = [];
      const context = await chromium.launchPersistentContext(userDataDir, {
        headless: true,
        // WebGPU may be behind a flag in some Chromium configurations.
        args: ["--enable-unsafe-webgpu"],
      });
      const page = await context.newPage();
      page.on("console", (msg) => logs.push(msg.text()));

      await page.goto(`${server.baseUrl}/shader_cache_demo.html`);
      await page.waitForFunction(() => (window as any).__shaderCacheDemo?.translationMs !== undefined);

      const result = await page.evaluate(() => (window as any).__shaderCacheDemo);
      await context.close();

      if (result?.error) {
        throw new Error(`demo page failed: ${result.error}`);
      }

      return {
        cacheHit: !!result.cacheHit,
        translationMs: Number(result.translationMs),
        telemetry: result.telemetry,
        logs,
      };
    }

    const first = await runOnce();
    expect(first.cacheHit).toBe(false);
    expect(first.logs.some((l) => l.includes("shader_translate: begin"))).toBe(true);
    expect(first.telemetry?.shader?.misses ?? 0).toBeGreaterThan(0);
    expect(first.telemetry?.shader?.bytesWritten ?? 0).toBeGreaterThan(0);

    const second = await runOnce();
    expect(second.cacheHit).toBe(true);
    expect(second.logs.some((l) => l.includes("shader_translate: begin"))).toBe(false);
    expect(second.telemetry?.shader?.hits ?? 0).toBeGreaterThan(0);
    expect(second.telemetry?.shader?.bytesRead ?? 0).toBeGreaterThan(0);

    // Timing assertion: the simulated translation takes ~300ms on cache miss.
    // Allow a lot of variance for CI load, but ensure the second run is
    // significantly faster.
    expect(first.translationMs).toBeGreaterThan(150);
    expect(second.translationMs).toBeLessThan(first.translationMs / 3);
  } finally {
    await server.close();
  }
});
