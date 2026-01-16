import path from "node:path";

import { createServer } from "vite";

import { ScenarioSkippedError, type Scenario } from "./types.ts";

export const guestCpuScenario: Scenario = {
  id: "guest_cpu",
  name: "Guest CPU instruction throughput",
  kind: "micro",
  async run(ctx) {
    if (process.env.AERO_BENCH_SKIP_GUEST_CPU === "1") {
      throw new ScenarioSkippedError("AERO_BENCH_SKIP_GUEST_CPU=1");
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
      const benchUrl = new URL("guest_cpu_bench.html", url).toString();
      await page.goto(benchUrl, { waitUntil: "domcontentloaded" });

      await page.waitForFunction(() => Boolean(window.aero?.bench?.runGuestCpuBench));

      const run = (await page.evaluate(async () => {
        return await window.aero.bench.runGuestCpuBench({
          variant: "alu64",
          mode: "interpreter",
          seconds: 0.25,
        });
      })) as unknown;
      const runRecord = run && typeof run === "object" ? (run as Record<string, unknown>) : null;

      const formatChecksum = (value: unknown): string => {
        if (typeof value === "string") {
          const oneLine = value.replace(/\s+/g, " ").trim();
          return oneLine ? oneLine.slice(0, 128) : "<empty>";
        }
        if (typeof value === "number" || typeof value === "boolean" || typeof value === "bigint") {
          return String(value);
        }
        return "unknown";
      };

      if (
        runRecord &&
        "expected_checksum" in runRecord &&
        "observed_checksum" in runRecord &&
        runRecord.expected_checksum !== runRecord.observed_checksum
      ) {
        throw new Error(
          `Guest CPU checksum mismatch: expected=${formatChecksum(runRecord.expected_checksum)} observed=${formatChecksum(runRecord.observed_checksum)}`,
        );
      }

      const perfExport = await page.evaluate(() => window.aero.perf.export());
      const perfRecord = perfExport && typeof perfExport === "object" ? (perfExport as Record<string, unknown>) : null;
      const benchmarks =
        perfRecord && typeof perfRecord.benchmarks === "object" ? (perfRecord.benchmarks as Record<string, unknown>) : null;
      const exportedGuestCpu = benchmarks ? benchmarks.guest_cpu : null;
      if (!exportedGuestCpu) {
        throw new Error("Expected window.aero.perf.export() to include benchmarks.guest_cpu after guest CPU bench run");
      }

      await ctx.artifacts.writeJson("guest_cpu_bench.json", run, "other");
      await ctx.artifacts.writeJson("perf_export.json", perfExport, "perf_export");

      const mipsMean = runRecord ? runRecord.mips_mean ?? runRecord.mips : undefined;
      if (typeof mipsMean !== "number" || !Number.isFinite(mipsMean)) {
        throw new Error("Expected guest CPU bench run to include a numeric mips_mean (or mips)");
      }
      ctx.metrics.set({ id: "guest_cpu_alu64_mips", unit: "count", value: mipsMean });

      const variant = typeof runRecord?.variant === "string" ? runRecord.variant : "unknown";
      const mode = typeof runRecord?.mode === "string" ? runRecord.mode : "unknown";
      const checksum = formatChecksum(runRecord?.observed_checksum);
      ctx.log(
        `guest_cpu: variant=${variant} mode=${mode} mips_mean=${mipsMean.toFixed(2)} checksum=${checksum}`,
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
