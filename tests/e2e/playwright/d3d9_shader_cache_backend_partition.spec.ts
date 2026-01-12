import { test, expect, chromium } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

type RunResult = {
  translateCalls: number;
  persistentHits: number;
  persistentMisses: number;
  cacheDisabled: boolean;
  backend: string;
  capsHash: string;
  logs: string[];
};

test("D3D9 shader cache partitions across WebGPU vs WebGL2 backends @webgpu", async ({}, testInfo) => {
  const baseUrl = testInfo.project.use.baseURL ?? "http://127.0.0.1:5173";

  // Only run under the dedicated WebGPU project (uses extra Chromium flags to make WebGPU available).
  if (testInfo.project.name !== "chromium-webgpu") test.skip();

  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "aero-d3d9-shader-cache-partition-"));

  async function runOnce(forceBackend: "webgl2_wgpu" | "webgpu"): Promise<RunResult> {
    const logs: string[] = [];
    const context = await chromium.launchPersistentContext(userDataDir, {
      headless: true,
      args: [
        "--enable-unsafe-webgpu",
        "--enable-features=WebGPU",
        "--ignore-gpu-blocklist",
        "--use-angle=swiftshader",
        "--use-gl=swiftshader",
        "--disable-gpu-sandbox",
        "--force-color-profile=srgb",
      ],
    });
    try {
      const page = await context.newPage();
      page.on("console", (msg) => logs.push(msg.text()));

      await page.goto(`${baseUrl}/web/gpu-worker-d3d9-shader-cache.html?backend=${forceBackend}`);
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
    const glFirst = await runOnce("webgl2_wgpu");
    if (glFirst.cacheDisabled) {
      test.skip(
        true,
        `persistent D3D9 shader cache is disabled/unavailable in this Chromium configuration\nlogs:\n${glFirst.logs.join("\n")}`,
      );
    }
    expect(glFirst.translateCalls).toBeGreaterThan(0);
    expect(glFirst.persistentMisses).toBeGreaterThan(0);
    expect(glFirst.capsHash).toContain("d3d9-wgpu-webgl2-");

    const glSecond = await runOnce("webgl2_wgpu");
    expect(glSecond.translateCalls).toBe(0);
    expect(glSecond.persistentHits).toBeGreaterThan(0);
    expect(glSecond.persistentMisses).toBe(0);
    expect(glSecond.capsHash).toContain("d3d9-wgpu-webgl2-");

    const webgpuFirst = await runOnce("webgpu");
    if (!webgpuFirst.capsHash.includes("d3d9-wgpu-webgpu-")) {
      test.skip(
        true,
        `WebGPU-backed D3D9 executor not available (capsHash=${webgpuFirst.capsHash || "empty"})\nlogs:\n${webgpuFirst.logs.join("\n")}`,
      );
    }
    // The key requirement: switching backends should not reuse cached WGSL from the other backend.
    expect(webgpuFirst.capsHash).not.toEqual(glFirst.capsHash);
    expect(webgpuFirst.translateCalls).toBeGreaterThan(0);
    expect(webgpuFirst.persistentMisses).toBeGreaterThan(0);
    expect(webgpuFirst.persistentHits).toBe(0);

    const webgpuSecond = await runOnce("webgpu");
    expect(webgpuSecond.translateCalls).toBe(0);
    expect(webgpuSecond.persistentHits).toBeGreaterThan(0);
    expect(webgpuSecond.persistentMisses).toBe(0);

    testInfo.attach("d3d9-shader-cache-caps-hash", {
      body: Buffer.from(
        [
          `glFirst=${glFirst.capsHash}`,
          `glSecond=${glSecond.capsHash}`,
          `webgpuFirst=${webgpuFirst.capsHash}`,
          `webgpuSecond=${webgpuSecond.capsHash}`,
          "",
        ].join("\n"),
      ),
      contentType: "text/plain",
    });
  } finally {
    try {
      fs.rmSync(userDataDir, { recursive: true, force: true });
    } catch {
      // Ignore cleanup failures; they are non-fatal.
    }
  }
});

