import { test, expect, chromium } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

test("D3D9 shader translation is persisted and skipped on next run", async ({}, testInfo) => {
  const baseUrl = testInfo.project.use.baseURL ?? "http://127.0.0.1:5173";

  // Use Chromium only: relies on OffscreenCanvas + worker + IDB/OPFS persistence,
  // and we only need to validate the behavior once.
  if (testInfo.project.name !== "chromium") test.skip();

  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-d3d9-shader-cache-"));

  async function runOnce(): Promise<{
    translateCalls: number;
    persistentHits: number;
    persistentMisses: number;
    cacheDisabled: boolean;
    backend: string;
    capsHash: string;
    logs: string[];
  }> {
    const logs: string[] = [];
    const context = await chromium.launchPersistentContext(userDataDir, {
      headless: true,
      args: [
        // Keep aligned with `playwright.config.ts` so workers using WebGPU/WebGL2 backends behave consistently.
        "--enable-unsafe-webgpu",
        "--force-color-profile=srgb",
      ],
    });
    try {
      const page = await context.newPage();
      page.on("console", (msg) => logs.push(msg.text()));

      await page.goto(`${baseUrl}/web/gpu-worker-d3d9-shader-cache.html`);
      await page.waitForFunction(() => (window as any).__d3d9ShaderCacheDemo !== undefined);

      const result = await page.evaluate(() => (window as any).__d3d9ShaderCacheDemo);
      if (result?.error) {
        throw new Error(`demo page failed: ${result.error}`);
      }

      return {
        translateCalls: Number(result.translateCalls),
        persistentHits: Number(result.persistentHits),
        persistentMisses: Number(result.persistentMisses),
        cacheDisabled: Boolean(result.cacheDisabled),
        backend: String(result.backend),
        capsHash: typeof result.capsHash === "string" ? result.capsHash : "",
        logs,
      };
    } catch (err) {
      throw new Error(`${String(err)}\nlogs:\n${logs.join("\n")}`);
    } finally {
      try {
        await context.close();
      } catch {
        // Ignore.
      }
    }
  }

  try {
    const first = await runOnce();
    expect(first.backend).toBe("webgl2_wgpu");
    if (first.cacheDisabled) {
      test.skip(
        true,
        `persistent D3D9 shader cache is disabled/unavailable in this Chromium configuration\nlogs:\n${first.logs.join("\n")}`,
      );
    }
    expect(first.translateCalls).toBeGreaterThan(0);
    expect(first.persistentMisses).toBeGreaterThan(0);

    const second = await runOnce();
    expect(second.backend).toBe("webgl2_wgpu");
    expect(second.translateCalls).toBe(0);
    expect(second.persistentHits).toBeGreaterThan(0);
    expect(second.persistentMisses).toBe(0);
    if (first.capsHash || second.capsHash) {
      testInfo.attach("d3d9-shader-cache-caps-hash", {
        body: Buffer.from(`first=${first.capsHash}\nsecond=${second.capsHash}\n`),
        contentType: "text/plain",
      });
    }
  } finally {
    try {
      fs.rmSync(userDataDir, { recursive: true, force: true });
    } catch {
      // Ignore cleanup failures; they are non-fatal.
    }
  }
});
