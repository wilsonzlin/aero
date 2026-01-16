/**
 * GPU benchmark runner.
 *
 * This file provides:
 * - `runGpuBenchmarksInPage(page, opts)` for Playwright tests/CI
 * - a small CLI (`node --experimental-strip-types bench/gpu_bench.ts`) for
 *   local execution which writes a JSON report to stdout or a file.
 */

import fs from "node:fs/promises";
import { execFile as execFileCb } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

import { transform } from "esbuild";
import { RunningStats } from "../packages/aero-stats/src/running-stats.js";
import { formatOneLineError, truncateUtf8 } from "../src/text.js";

const execFile = promisify(execFileCb);

export const GPU_BENCH_SCHEMA_VERSION = 2;

/** @typedef {any} GpuTelemetrySnapshot */

/**
 * @typedef {{
 *   fpsAvg: number|null,
 *   frameTimeMsP50: number|null,
 *   frameTimeMsP95: number|null,
 *   presentLatencyMsP95: number|null,
 *   shaderTranslationMsMean: number|null,
 *   shaderCompilationMsMean: number|null,
 *   pipelineCacheHitRate: number|null,
 *   textureUploadMBpsAvg: number|null,
 * }} GpuBenchDerivedMetrics
 */

/**
 * @typedef {{
 *   n: number,
 *   mean: number,
 *   stdev: number,
 *   cv: number|null,
 *   median: number,
 *   p50: number,
 *   p95: number,
 * }} GpuBenchMetricStats
 */

/**
 * @typedef {{
 *   iteration: number,
 *   status: "ok" | "skipped" | "error",
 *   api?: string|null,
 *   reason?: string|null,
 *   error?: string,
 *   durationMs: number,
 *   params: any,
 *   telemetry: GpuTelemetrySnapshot,
 *   derived: GpuBenchDerivedMetrics,
 * }} GpuBenchIterationSample
 */

/**
 * @typedef {{
 *   id: string,
 *   name: string,
 *   params: any,
 *   iterations: GpuBenchIterationSample[],
 * }} GpuBenchScenarioRaw
 */

/**
 * @typedef {{
 *   id: string,
 *   name: string,
 *   status: "ok" | "skipped" | "error",
 *   api?: string|null,
 *   reason?: string|null,
 *   error?: string|null,
 *   metrics: Record<string, GpuBenchMetricStats|null>,
 * }} GpuBenchScenarioSummary
 */

/**
 * @typedef {{
 *   schemaVersion: number,
 *   tool: string,
 *   startedAt: string,
 *   finishedAt: string,
 *   meta: { iterations: number, gitSha?: string, gitRef?: string, nodeVersion: string },
 *   environment: { userAgent: string, webgpu: boolean, webgl2: boolean },
 *   raw: { scenarios: Record<string, GpuBenchScenarioRaw> },
 *   summary: { scenarios: Record<string, GpuBenchScenarioSummary> },
 * }} GpuBenchReport
 */

/**
 * @param {GpuTelemetrySnapshot} telemetry
 */
function deriveMetrics(telemetry) {
  const ft = telemetry.frameTimeMs?.stats ?? null;
  const present = telemetry.presentLatencyMs?.stats ?? null;
  const dxbc = telemetry.shaderTranslationMs?.stats ?? null;
  const wgsl = telemetry.shaderCompilationMs?.stats ?? null;

  const wallTimeMs = telemetry.wallTimeTotalMs ?? null;
  const fpsAvg =
    wallTimeMs != null && wallTimeMs > 0 && (ft?.count ?? 0) > 0
      ? ft.count / (wallTimeMs / 1000)
      : ft?.mean
        ? 1000 / ft.mean
        : null;

  const textureBw = telemetry.textureUpload?.bandwidthBytesPerSecAvg ?? null;
  const textureUploadMBpsAvg =
    textureBw != null && Number.isFinite(textureBw) ? textureBw / (1024 * 1024) : null;

  return {
    fpsAvg,
    frameTimeMsP50: ft?.p50 ?? null,
    frameTimeMsP95: ft?.p95 ?? null,
    presentLatencyMsP95: present?.p95 ?? null,
    shaderTranslationMsMean: dxbc?.mean ?? null,
    shaderCompilationMsMean: wgsl?.mean ?? null,
    pipelineCacheHitRate: telemetry.pipelineCache?.hitRate ?? null,
    textureUploadMBpsAvg,
  };
}

/**
 * @param {number[]} values
 * @param {number} q
 */
function quantile(values, q) {
  if (values.length === 0) return null;
  if (q <= 0) return Math.min(...values);
  if (q >= 1) return Math.max(...values);
  const sorted = [...values].sort((a, b) => a - b);
  const idx = (sorted.length - 1) * q;
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  const t = idx - lo;
  return sorted[lo] * (1 - t) + sorted[hi] * t;
}

/**
 * @param {number[]} values
 * @returns {GpuBenchMetricStats|null}
 */
function summarize(values) {
  if (values.length === 0) return null;
  const stats = new RunningStats();
  for (const v of values) stats.push(v);

  const mean = stats.mean;
  const stdev = stats.stdevPopulation;
  const median = quantile(values, 0.5);
  const p95 = quantile(values, 0.95);
  const cv = Number.isFinite(mean) && mean !== 0 ? stdev / mean : null;
  if (
    !Number.isFinite(mean) ||
    !Number.isFinite(stdev) ||
    median == null ||
    p95 == null ||
    !Number.isFinite(median) ||
    !Number.isFinite(p95)
  ) {
    return null;
  }

  return {
    n: stats.count,
    mean,
    stdev,
    cv,
    median,
    p50: median,
    p95,
  };
}

async function gitValue(args) {
  try {
    const { stdout } = await execFile("git", args, { cwd: process.cwd() });
    return stdout.trim();
  } catch {
    return undefined;
  }
}

function resolveRepoRoot() {
  const here = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(here, "..");
}

/**
 * @param {any} page Playwright Page
 * @param {{
 *   scenarios?: string[],
 *   scenarioParams?: Record<string, any>,
 *   iterations?: number,
 * }=} opts
 * @returns {Promise<GpuBenchReport>}
 */
export async function runGpuBenchmarksInPage(page, opts = {}) {
  const repoRoot = resolveRepoRoot();
  const startedAt = new Date().toISOString();
  const iterations = opts.iterations ?? 1;
  if (!Number.isInteger(iterations) || iterations <= 0) {
    throw new Error("runGpuBenchmarksInPage: iterations must be a positive integer");
  }

  const scenarioIds = opts.scenarios ?? [
    "vga_text_scroll",
    "vbe_lfb_blit",
    "webgpu_triangle_batch",
    "d3d9_state_churn",
  ];

  const scriptPaths = [
    path.join(repoRoot, "web/gpu-cache/persistent_cache.ts"),
    path.join(repoRoot, "web/src/gpu/telemetry.ts"),
    path.join(repoRoot, "bench/scenarios/vga_text_scroll.ts"),
    path.join(repoRoot, "bench/scenarios/vbe_lfb_blit.ts"),
    path.join(repoRoot, "bench/scenarios/webgpu_triangle_batch.ts"),
    path.join(repoRoot, "bench/scenarios/d3d9_state_churn.ts"),
  ];

  for (const p of scriptPaths) {
    // Benchmark scripts are authored as `.ts` (for editor tooling), but this runner
    // injects them into a real browser page without Vite/tsc. Strip TypeScript
    // syntax before injection so the module can execute in the browser.
    const source = await fs.readFile(p, "utf8");
    const loader = p.endsWith(".ts") ? "ts" : "js";
    const result = await transform(source, {
      loader,
      format: "esm",
      target: "es2020",
      sourcefile: p,
    });
    await page.addScriptTag({ type: "module", content: result.code });
  }

  const { environment, scenarios } = await page.evaluate(
    async ({ scenarioIds, scenarioParams, iterations }) => {
      const initialCanvas = /** @type {HTMLCanvasElement | null} */ (document.getElementById("bench-canvas"));
      if (!initialCanvas) {
        throw new Error("Benchmark page missing #bench-canvas");
      }

      const g = /** @type {any} */ (globalThis);
      const ScenarioRegistry = g.__aeroGpuBenchScenarios;
      const Telemetry = g.AeroGpuTelemetry?.GpuTelemetry;
      if (!ScenarioRegistry) {
        throw new Error("Scenario registry missing: expected globalThis.__aeroGpuBenchScenarios");
      }
      if (!Telemetry) {
        throw new Error("Telemetry missing: expected globalThis.AeroGpuTelemetry.GpuTelemetry");
      }

      const host = initialCanvas.parentElement ?? document.body;

      // Creating a context (2d/webgl2/webgpu) permanently "claims" a canvas.
      // Each scenario needs a fresh canvas to avoid context-type conflicts.
      function createFreshCanvas() {
        const old = /** @type {HTMLCanvasElement | null} */ (document.getElementById("bench-canvas"));
        const next = document.createElement("canvas");
        next.id = "bench-canvas";
        next.width = old?.width ?? 800;
        next.height = old?.height ?? 600;
        if (old) old.replaceWith(next);
        else host.appendChild(next);
        return next;
      }

      const probe = document.createElement("canvas");
      const env = {
        userAgent: navigator.userAgent,
        webgpu: !!navigator.gpu,
        webgl2: !!probe.getContext("webgl2"),
      };

      /** @type {Record<string, any>} */
      const results = {};

      for (const id of scenarioIds) {
        const scenario = ScenarioRegistry[id];
        const params = scenarioParams?.[id] ?? {};
        /** @type {any[]} */
        const samples = [];
        for (let iteration = 0; iteration < iterations; iteration += 1) {
          const telemetry = new Telemetry();
          telemetry.reset();
          const t0 = performance.now();
          const canvas = createFreshCanvas();

          try {
            if (!scenario) {
              samples.push({
                iteration,
                status: "error",
                error: `Unknown scenario: ${id}`,
                durationMs: 0,
                params,
                telemetry: telemetry.snapshot(),
              });
              continue;
            }

            const out = await scenario.run({ canvas, telemetry, params });
            const t1 = performance.now();

            samples.push({
              iteration,
              status: out?.status ?? "ok",
              api: out?.api ?? null,
              reason: out?.reason ?? null,
              durationMs: t1 - t0,
              params: out?.params ?? params,
              telemetry: telemetry.snapshot(),
            });
          } catch (e) {
            const t1 = performance.now();
            samples.push({
              iteration,
              status: "error",
              api: null,
              error: formatOneLineError(e, 512),
              durationMs: t1 - t0,
              params,
              telemetry: telemetry.snapshot(),
            });
          }
        }

        results[id] = {
          id,
          name: scenario?.name ?? id,
          params,
          iterations: samples,
        };
      }

      return { environment: env, scenarios: results };
    },
    { scenarioIds, scenarioParams: opts.scenarioParams ?? {}, iterations },
  );

  /** @type {Record<string, GpuBenchScenarioRaw>} */
  const rawScenarios = {};
  /** @type {Record<string, GpuBenchScenarioSummary>} */
  const summaryScenarios = {};

  for (const [id, raw] of Object.entries(scenarios)) {
    const iterationsRaw = raw.iterations.map((sample) => ({
      ...sample,
      derived: deriveMetrics(sample.telemetry),
    }));

    rawScenarios[id] = {
      id,
      name: raw.name ?? id,
      params: raw.params ?? {},
      iterations: iterationsRaw,
    };

    const okIterations = iterationsRaw.filter((r) => r.status === "ok");
    /** @type {Record<string, number[]>} */
    const metricSamples = {};
    for (const r of okIterations) {
      for (const [metric, value] of Object.entries(r.derived ?? {})) {
        if (typeof value !== "number" || !Number.isFinite(value)) continue;
        (metricSamples[metric] ??= []).push(value);
      }
    }

    /** @type {Record<string, GpuBenchMetricStats|null>} */
    const metrics = {};
    for (const metric of Object.keys(deriveMetrics(okIterations[0]?.telemetry ?? {}))) {
      metrics[metric] = summarize(metricSamples[metric] ?? []);
    }

    const status = iterationsRaw.some((r) => r.status === "error")
      ? "error"
      : okIterations.length > 0
        ? "ok"
        : "skipped";

    const firstOk = okIterations[0] ?? null;
    const firstSkipped = iterationsRaw.find((r) => r.status === "skipped") ?? null;
    const firstError = iterationsRaw.find((r) => r.status === "error") ?? null;

    summaryScenarios[id] = {
      id,
      name: raw.name ?? id,
      status,
      api: firstOk?.api ?? firstSkipped?.api ?? null,
      reason: firstSkipped?.reason ?? null,
      error: firstError?.error ?? null,
      metrics,
    };
  }

  const [gitSha, gitRef] = await Promise.all([gitValue(["rev-parse", "HEAD"]), gitValue(["rev-parse", "--abbrev-ref", "HEAD"])]);

  return {
    schemaVersion: GPU_BENCH_SCHEMA_VERSION,
    tool: "aero-gpu-bench",
    startedAt,
    finishedAt: new Date().toISOString(),
    meta: {
      iterations,
      gitSha,
      gitRef,
      nodeVersion: process.version,
    },
    environment,
    raw: { scenarios: rawScenarios },
    summary: { scenarios: summaryScenarios },
  };
}

/**
 * Start a minimal local server to ensure a secure context (localhost is
 * considered "potentially trustworthy"), which is required by WebGPU.
 *
 * @param {{width:number, height:number}} viewport
 */
async function startBenchServer(viewport) {
  const html = `<!doctype html>
<meta charset="utf-8" />
<title>Aero GPU Bench</title>
<style>
  html, body { margin: 0; padding: 0; background: #000; }
  canvas { display: block; width: ${viewport.width}px; height: ${viewport.height}px; }
</style>
<canvas id="bench-canvas" width="${viewport.width}" height="${viewport.height}"></canvas>
`;

  const server = http.createServer((req, res) => {
    res.statusCode = 200;
    res.setHeader("content-type", "text/html; charset=utf-8");
    res.end(html);
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  const addr = server.address();
  if (!addr || typeof addr === "string") {
    server.close();
    throw new Error("Failed to bind benchmark server");
  }
  const url = `http://127.0.0.1:${addr.port}/`;
  return { url, close: () => new Promise((resolve) => server.close(resolve)) };
}

function parseArgs(argv) {
  /** @type {Record<string, string>} */
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (!a.startsWith("--")) continue;
    const k = a.slice(2);
    const v = argv[i + 1];
    if (v && !v.startsWith("--")) {
      out[k] = v;
      i += 1;
    } else {
      out[k] = "true";
    }
  }
  return out;
}

async function runCli() {
  const args = parseArgs(process.argv.slice(2));
  const scenarios = args.scenarios ? args.scenarios.split(",").map((s) => s.trim()).filter(Boolean) : undefined;
  const outPath = args.output ?? null;
  const headless = args.headless !== "false";
  const swiftshader = args.swiftshader === "true";
  const iterations = args.iterations ? Number.parseInt(args.iterations, 10) : 1;
  const scenarioParamsPath = args["scenario-params"] ?? null;
  const scenarioParamsJson = args["scenario-params-json"] ?? null;

  if (!Number.isFinite(iterations) || iterations <= 0) {
    throw new Error("--iterations must be a positive integer");
  }

  /** @type {Record<string, any> | undefined} */
  let scenarioParams;
  if (scenarioParamsPath) {
    const text = await fs.readFile(scenarioParamsPath, "utf8");
    scenarioParams = JSON.parse(text);
  } else if (scenarioParamsJson) {
    scenarioParams = JSON.parse(scenarioParamsJson);
  }

  // Lazy import so this file can be imported without Playwright installed.
  /** @type {any} */
  let playwright;
  try {
    playwright = await import("playwright");
  } catch (e) {
    throw new Error(
      "Playwright is required to run GPU benchmarks. Install it (e.g. `npm i -D playwright`) and retry.",
    );
  }

  const browser = await playwright.chromium.launch({
    headless,
    args: [
      "--enable-unsafe-webgpu",
      "--disable-gpu-sandbox",
      "--disable-dev-shm-usage",
      ...(swiftshader ? ["--use-gl=swiftshader"] : []),
    ],
  });

  const context = await browser.newContext({ viewport: { width: 800, height: 600 } });
  const page = await context.newPage();
  const server = await startBenchServer({ width: 800, height: 600 });

  try {
    await page.goto(server.url, { waitUntil: "load" });
    const report = await runGpuBenchmarksInPage(page, { scenarios, scenarioParams, iterations });
    const json = JSON.stringify(report, null, 2);
    if (outPath) {
      await fs.mkdir(path.dirname(outPath), { recursive: true });
      await fs.writeFile(outPath, json, "utf8");
    } else {
      process.stdout.write(json);
      process.stdout.write("\n");
    }
  } finally {
    await server.close();
    await context.close();
    await browser.close();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  runCli().catch((err) => {
    let stack: string | null = null;
    if (err && typeof err === "object") {
      try {
        const raw = (err as { stack?: unknown }).stack;
        if (typeof raw === "string" && raw) stack = raw;
      } catch {
        // ignore getters throwing
      }
    }
    console.error(stack ? truncateUtf8(stack, 8 * 1024) : formatOneLineError(err, 512));
    process.exitCode = 1;
  });
}
