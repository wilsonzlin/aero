import path from "node:path";

import { createServer } from "vite";

import { ScenarioSkippedError, type Scenario } from "./types.ts";

export const storageIoScenario: Scenario = {
  id: "storage_io",
  name: "Storage I/O (OPFS/IndexedDB)",
  kind: "micro",
  async run(ctx) {
    if (process.env.AERO_BENCH_SKIP_STORAGE_IO === "1") {
      throw new ScenarioSkippedError("AERO_BENCH_SKIP_STORAGE_IO=1");
    }

    let playwright: any;
    try {
      playwright = await import("playwright");
    } catch {
      throw new ScenarioSkippedError("playwright not available (install deps and retry)");
    }

    const server = await createServer({
      root: path.resolve("web"),
      configFile: path.resolve("web/vite.config.ts"),
      server: { port: 0 },
    });

    await server.listen();

    let url = server.resolvedUrls?.local[0];
    if (!url) {
      const address = server.httpServer?.address();
      if (address && typeof address === "object" && "port" in address) {
        url = `http://localhost:${address.port}/`;
      }
    }
    if (!url) {
      await server.close();
      throw new Error("Failed to determine Vite dev server URL");
    }

    let browser: any;

    try {
      browser = await playwright.chromium.launch({ headless: true });

      const page = await browser.newPage();
      const benchUrl = new URL("storage_bench.html", url).toString();
      await page.goto(benchUrl, { waitUntil: "domcontentloaded" });

      await page.waitForFunction(() => Boolean(window.aero?.bench?.runStorageBench));

      const storage = await page.evaluate(async () => {
        return await window.aero.bench.runStorageBench({
          seq_total_mb: 32,
          seq_chunk_mb: 4,
          seq_runs: 2,
          warmup_mb: 8,
          random_ops: 500,
          random_runs: 2,
          random_space_mb: 4,
          include_random_write: false,
        });
      });

      const perfExport = await page.evaluate(() => window.aero.perf.export());
      const exportedStorage = (perfExport as any)?.benchmarks?.storage;
      if (!exportedStorage) {
        throw new Error("Expected window.aero.perf.export() to include benchmarks.storage after storage bench run");
      }

      await ctx.artifacts.writeJson("storage_bench.json", storage, "other");
      await ctx.artifacts.writeJson("perf_export.json", perfExport, "perf_export");

      const p95 = storage?.random_read_4k?.mean_p95_ms;
      if (typeof p95 === "number" && Number.isFinite(p95)) {
        ctx.metrics.setMs("storage_random_read_p95_ms", p95);
      }

      ctx.log(
        `storage_io: backend=${storage?.backend ?? "unknown"} api_mode=${storage?.api_mode ?? "unknown"} ` +
          `seq_write=${storage?.sequential_write?.mean_mb_per_s?.toFixed?.(2) ?? "n/a"}MB/s ` +
          `seq_read=${storage?.sequential_read?.mean_mb_per_s?.toFixed?.(2) ?? "n/a"}MB/s ` +
          `rand_read_p95=${p95?.toFixed?.(2) ?? "n/a"}ms`,
      );
    } finally {
      try {
        await browser?.close();
      } finally {
      await server.close();
      }
    }
  },
};
