import { test, expect } from "@playwright/test";
import { build } from "esbuild";
import { fileURLToPath } from "node:url";

let bundledApp = "";

async function buildBrowserBundle(): Promise<string> {
  const entryPoint = fileURLToPath(new URL("./frame_pacing_app.ts", import.meta.url));
  const result = await build({
    entryPoints: [entryPoint],
    bundle: true,
    format: "iife",
    platform: "browser",
    target: "es2020",
    write: false,
    outfile: "bundle.js",
  });

  const outputFile = result.outputFiles?.[0];
  if (!outputFile) {
    throw new Error("esbuild did not produce any output files");
  }

  return outputFile.text;
}

async function getJsHeapUsedSizeBytes(cdp: any) {
  const result = await cdp.send("Performance.getMetrics");
  const metrics = Array.isArray(result?.metrics) ? result.metrics : [];
  const used = metrics.find((metric: { name: string }) => metric.name === "JSHeapUsedSize")?.value;
  return typeof used === "number" ? used : 0;
}

test.beforeAll(async () => {
  bundledApp = await buildBrowserBundle();
});

test("frame pacing bounds frames-in-flight and avoids unbounded growth", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "CDP-backed memory metrics require Chromium");

  await page.setContent(`<!doctype html>
    <html>
      <body>
        <canvas id="c" style="width: 320px; height: 240px"></canvas>
      </body>
    </html>`);

  await page.addScriptTag({ content: bundledApp });

  const cdp = await page.context().newCDPSession(page);
  await cdp.send("HeapProfiler.enable");
  await cdp.send("Performance.enable");

  await cdp.send("HeapProfiler.collectGarbage");
  const heapBefore = await getJsHeapUsedSizeBytes(cdp);

  const result = await page.evaluate(async () => {
    return await window.__runFramePacingStressTest?.({
      durationMs: 2000,
      producerIntervalMs: 0,
      maxFramesInFlight: 2,
      simulateWorkDoneDelayMs: 20,
    });
  });

  await cdp.send("HeapProfiler.collectGarbage");
  const heapAfter = await getJsHeapUsedSizeBytes(cdp);

  expect(result).toBeTruthy();
  if (!result || typeof result !== "object") {
    throw new Error("Unexpected stress test result");
  }

  const { config, produced, telemetry } = result as any;

  expect(typeof produced).toBe("number");
  expect(produced).toBeGreaterThan(0);
  expect(produced).toBeGreaterThan(120);

  expect(telemetry.framesDropped).toBeGreaterThan(0);
  expect(telemetry.maxFramesInFlightObserved).toBeLessThanOrEqual(config.maxFramesInFlight);
  expect(telemetry.averageEnqueueToSubmitLatencyMs).toBeLessThan(50);
  expect(telemetry.maxEnqueueToSubmitLatencyMs).toBeLessThan(200);
  expect(telemetry.averageWorkDoneLatencyMs).toBeGreaterThan(0);
  expect(telemetry.maxWorkDoneLatencyMs).toBeLessThan(1000);

  const heapDelta = heapAfter - heapBefore;
  expect(heapDelta).toBeLessThan(30 * 1024 * 1024);
});

test("webgpu backend smoke test (if available)", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "WebGPU smoke test only runs on Chromium");

  await page.setContent(`<!doctype html>
    <html>
      <body>
        <canvas id="c" style="width: 64px; height: 64px"></canvas>
      </body>
    </html>`);

  await page.addScriptTag({ content: bundledApp });

  const result = await page.evaluate(async () => {
    return await window.__runWebGpuFramePacingSmokeTest?.();
  });

  if (!result || typeof result !== "object" || !(result as any).supported) {
    test.skip(true, "WebGPU not available in this environment");
  }

  const { telemetry } = result as any;
  expect(telemetry.maxFramesInFlightObserved).toBeLessThanOrEqual(2);
});
