import { test, expect } from "@playwright/test";
import fs from "node:fs/promises";
import http from "node:http";

import { runGpuBenchmarksInPage } from "../../bench/gpu_bench.ts";

async function startServer(html: string) {
  const server = http.createServer((req, res) => {
    res.statusCode = 200;
    res.setHeader("content-type", "text/html; charset=utf-8");
    res.end(html);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  if (!addr || typeof addr === "string") {
    server.close();
    throw new Error("Failed to start test server");
  }
  const url = `http://127.0.0.1:${addr.port}/`;
  return { url, close: () => new Promise<void>((resolve) => server.close(() => resolve())) };
}

test("gpu benchmark suite emits JSON report (smoke)", async ({ page }, testInfo) => {
  const html = `<!doctype html>
  <meta charset="utf-8" />
  <title>Aero GPU Bench (Playwright)</title>
  <canvas id="bench-canvas" width="800" height="600"></canvas>`;

  const server = await startServer(html);
  try {
    await page.goto(server.url, { waitUntil: "load" });

    // Reduced subset for CI: keep runtime short and avoid relying on WebGPU
    // availability on all runners (scenarios fall back to WebGL2 where possible).
    const report = await runGpuBenchmarksInPage(page, {
      scenarios: ["vga_text_scroll", "vbe_lfb_blit"],
      scenarioParams: {
        vga_text_scroll: { frames: 120 },
        vbe_lfb_blit: { frames: 60, width: 256, height: 256 },
      },
    });

    expect(report.schemaVersion).toBe(2);
    expect(report.tool).toBe("aero-gpu-bench");

    const outPath = testInfo.outputPath("gpu_bench.json");
    await fs.writeFile(outPath, JSON.stringify(report, null, 2), "utf8");
    await testInfo.attach("gpu_bench.json", { path: outPath, contentType: "application/json" });
  } finally {
    await server.close();
  }
});
