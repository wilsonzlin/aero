import { execFile as execFileCb } from "node:child_process";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { promisify } from "node:util";
import { performance } from "node:perf_hooks";
import { createRequire } from "node:module";
import { chromium } from "playwright-core";
import { summarize } from "./lib/stats.mjs";
import { formatOneLineUtf8 } from "../../src/text.js";

const execFile = promisify(execFileCb);
const require = createRequire(import.meta.url);
const playwrightCoreVersion = require("playwright-core/package.json").version;

const MAX_ERROR_MESSAGE_BYTES = 512;

function usage(exitCode) {
  const msg = `
Usage:
  node tools/perf/run.mjs --out-dir <dir> --iterations <n>

Options:
  --out-dir <dir>       Output directory (required)
  --iterations <n>      Iterations per benchmark (default: 3)
  --url <url>           URL to load (default: internal data: URL)
  --trace               Capture an Aero trace to <outDir>/trace.json (best-effort; opt-in)
  --trace-duration-ms <n>
                        Capture a trace for a fixed duration instead of running a trace workload
  --include-aero-bench  Include app-provided microbench suite (window.aero.bench.runMicrobenchSuite), if available
  --help                Show this help
`;
  console.log(msg.trim());
  process.exit(exitCode);
}

function parseArgs(argv) {
  const out = {
    outDir: undefined,
    iterations: 3,
    url: undefined,
    trace: false,
    traceDurationMs: undefined,
    includeAeroBench: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case "--out-dir":
        out.outDir = argv[++i];
        break;
      case "--iterations":
        out.iterations = Number.parseInt(argv[++i], 10);
        break;
      case "--url":
        out.url = argv[++i];
        break;
      case "--trace":
        out.trace = true;
        break;
      case "--trace-duration-ms":
        out.trace = true;
        out.traceDurationMs = Number.parseInt(argv[++i], 10);
        break;
      case "--include-aero-bench":
        out.includeAeroBench = true;
        break;
      case "--help":
        usage(0);
        break;
      default:
        if (arg.startsWith("-")) {
          console.error(`Unknown option: ${arg}`);
          usage(1);
        }
        break;
    }
  }

  if (!out.outDir) {
    console.error("--out-dir is required");
    usage(1);
  }
  if (!Number.isFinite(out.iterations) || out.iterations <= 0) {
    console.error("--iterations must be a positive integer");
    usage(1);
  }
  if (out.traceDurationMs !== undefined && (!Number.isFinite(out.traceDurationMs) || out.traceDurationMs <= 0)) {
    console.error("--trace-duration-ms must be a positive integer");
    usage(1);
  }

  return out;
}

async function gitValue(args) {
  try {
    const { stdout } = await execFile("git", args, { cwd: process.cwd() });
    return stdout.trim();
  } catch {
    return undefined;
  }
}

async function sleep(ms) {
  await new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function withRetries(label, attempts, fn) {
  let lastErr;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return await fn();
    } catch (err) {
      lastErr = err;
      const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
      console.warn(`[perf] ${label} failed (attempt ${attempt}/${attempts}): ${msg}`);
      if (attempt < attempts) {
        await sleep(250 * attempt);
      }
    }
  }
  throw lastErr;
}

async function withTimeout(label, timeoutMs, promise) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) return promise;

  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${label} timed out after ${timeoutMs}ms`));
    }, timeoutMs);
  });

  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

const CHROMIUM_ARGS = [
  "--disable-dev-shm-usage",
  "--disable-gpu",
  "--disable-features=WebGPU",
  "--no-first-run",
  "--no-default-browser-check",
  "--disable-background-networking",
  "--disable-background-timer-throttling",
  "--disable-renderer-backgrounding",
  "--disable-backgrounding-occluded-windows",
  "--disable-extensions",
  "--disable-sync",
];

const TEST_HTML = [
  "<!doctype html>",
  "<meta charset='utf-8' />",
  "<title>aero-perf</title>",
  "<body>perf</body>",
].join("");

const TEST_URL = `data:text/html,${encodeURIComponent(TEST_HTML)}`;

async function gotoWithRetries(page, url) {
  await withRetries("page.goto", 3, async () => {
    await page.goto(url, { waitUntil: "load", timeout: 30_000 });
    await page.waitForFunction(() => document.readyState === "complete", null, { timeout: 5_000 });
  });
}

async function tryCaptureAeroPerfExport(page) {
  let apiTimedOut = false;

  try {
    try {
      await page.waitForFunction(() => {
        const perf = globalThis.aero?.perf;
        return perf && typeof perf === "object" && typeof perf.export === "function";
      }, null, { timeout: 2_000 });
    } catch {
      apiTimedOut = true;
      // Best-effort: if the app doesn't expose a perf API we still want the rest of the run to succeed.
    }

    const res = await page.evaluate(async () => {
      const aero = globalThis.aero;
      const perf = aero && typeof aero === "object" ? aero.perf : undefined;
      if (!perf || typeof perf !== "object") return { available: false, json: null };
      if (typeof perf.export !== "function") return { available: false, json: null };

      // Prefer capturing a short window so the export is meaningful, but fall back
      // to `export()` if capture APIs aren't present in the build.
      if (typeof perf.captureStart === "function" && typeof perf.captureStop === "function") {
        try {
          if (typeof perf.captureReset === "function") {
            perf.captureReset();
          }
          perf.captureStart();
          await new Promise((resolve) => setTimeout(resolve, 1000));
          perf.captureStop();
        } catch {
          // Ignore capture errors; still attempt export().
        }
      }

      try {
        return { available: true, json: JSON.stringify(perf.export(), null, 2) };
      } catch {
        return { available: true, json: null };
      }
    });

    return {
      available: Boolean(res?.available),
      apiTimedOut,
      json: typeof res?.json === "string" ? res.json : null,
    };
  } catch (err) {
    const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
    return {
      available: false,
      apiTimedOut,
      json: null,
      error: msg,
    };
  }
}

async function hasAeroTraceApi(page) {
  try {
    return await page.evaluate(() => {
      const perf = globalThis.aero?.perf;
      return Boolean(
        perf &&
          typeof perf === "object" &&
          typeof perf.traceStart === "function" &&
          typeof perf.traceStop === "function" &&
          typeof perf.exportTrace === "function",
      );
    });
  } catch {
    return false;
  }
}

async function tryCaptureAeroTrace(page, opts) {
  const res = {
    requested: Boolean(opts.trace),
    durationMs: Number.isFinite(opts.traceDurationMs) ? opts.traceDurationMs : null,
    available: false,
    timedOut: false,
    captured: false,
    error: null,
    json: null,
  };

  res.available = await hasAeroTraceApi(page);
  if (!res.requested || !res.available) return res;

  let traceStarted = false;
  try {
    await withTimeout("aero.perf.traceStart", 5_000, page.evaluate(() => globalThis.aero.perf.traceStart()));
    traceStarted = true;

    if (Number.isFinite(opts.traceDurationMs)) {
      await sleep(opts.traceDurationMs);
    } else if (typeof opts.traceWorkload === "function") {
      await opts.traceWorkload();
    }

    await withTimeout("aero.perf.traceStop", 5_000, page.evaluate(() => globalThis.aero.perf.traceStop()));
    traceStarted = false;

    const traceJson = await withTimeout(
      "aero.perf.exportTrace",
      30_000,
      page.evaluate(async () => {
        const perf = globalThis.aero?.perf;
        if (!perf || typeof perf !== "object") return null;
        if (typeof perf.exportTrace !== "function") return null;

        // Prefer string export to avoid double-encoding.
        try {
          const asString = await perf.exportTrace({ asString: true });
          if (typeof asString === "string") return asString;
        } catch {
          // Fall back to object export below.
        }

        try {
          const data = await perf.exportTrace();
          if (typeof data === "string") return data;
          return JSON.stringify(data);
        } catch {
          return null;
        }
      }),
    );

    if (typeof traceJson === "string") {
      res.json = traceJson;
      res.captured = true;
    }
  } catch (err) {
    const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
    res.error = msg;
    if (msg.includes("timed out")) res.timedOut = true;
  } finally {
    if (traceStarted) {
      try {
        await page.evaluate(() => {
          try {
            globalThis.aero?.perf?.traceStop?.();
          } catch {
            // ignored
          }
        });
      } catch {
        // ignored
      }
    }
  }

  return res;
}

async function launchChromium() {
  return withRetries("chromium.launch", 3, async () =>
    chromium.launch({
      headless: true,
      args: CHROMIUM_ARGS,
    }),
  );
}

async function runChromiumStartupOnce(url) {
  const t0 = performance.now();
  const browser = await launchChromium();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    const page = await context.newPage();
    await gotoWithRetries(page, url);
  } finally {
    await browser.close();
  }
  return performance.now() - t0;
}

async function runMicrobenchSamples(url, iterations, opts) {
  const browser = await launchChromium();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    const page = await context.newPage();
    await gotoWithRetries(page, url);

    let jit = null;
    const aeroMicrobenchSuite = {
      requested: Boolean(opts.includeAeroBench),
      available: false,
      status: opts.includeAeroBench ? "skipped" : "disabled",
      reason: opts.includeAeroBench ? "window.aero.bench.runMicrobenchSuite unavailable" : null,
      samples: [],
    };

    const microbench = () => {
      const t0 = performance.now();
      const buf = new Uint32Array(1_000_000);
      let acc = 0;
      for (let i = 0; i < buf.length; i += 1) {
        buf[i] = i;
        acc += buf[i];
      }
      const t1 = performance.now();
      return { ms: t1 - t0, acc };
    };

    await page.evaluate(microbench);

    const samples = [];
    for (let i = 0; i < iterations; i += 1) {
      const result = await page.evaluate(microbench);
      samples.push(result.ms);
    }

    if (opts.includeAeroBench) {
      const runAeroMicrobenchSuiteOnce = async () => {
        const fn = globalThis.aero?.bench?.runMicrobenchSuite;
        if (typeof fn !== "function") return null;
        const t0 = performance.now();
        await fn();
        const t1 = performance.now();
        return t1 - t0;
      };

      try {
        aeroMicrobenchSuite.available = await page.evaluate(() => typeof globalThis.aero?.bench?.runMicrobenchSuite === "function");
      } catch {
        aeroMicrobenchSuite.available = false;
      }

      if (aeroMicrobenchSuite.available) {
        aeroMicrobenchSuite.status = "ok";
        aeroMicrobenchSuite.reason = null;

        try {
          // Warm-up (best-effort) so the first sample isn't dominated by one-time init/JIT.
          await page.evaluate(runAeroMicrobenchSuiteOnce);
        } catch (err) {
          aeroMicrobenchSuite.status = "skipped";
          const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
          aeroMicrobenchSuite.reason = `window.aero.bench.runMicrobenchSuite warmup failed: ${msg}`;
        }

        if (aeroMicrobenchSuite.status === "ok") {
          for (let i = 0; i < iterations; i += 1) {
            try {
              const ms = await page.evaluate(runAeroMicrobenchSuiteOnce);
              if (typeof ms !== "number" || !Number.isFinite(ms)) {
                aeroMicrobenchSuite.status = "skipped";
                aeroMicrobenchSuite.reason = "window.aero.bench.runMicrobenchSuite returned a non-finite duration";
                aeroMicrobenchSuite.samples = [];
                break;
              }
              aeroMicrobenchSuite.samples.push(ms);
            } catch (err) {
              aeroMicrobenchSuite.status = "skipped";
              const msg = formatOneLineUtf8(err?.message ?? err, MAX_ERROR_MESSAGE_BYTES) || "Error";
              aeroMicrobenchSuite.reason = `window.aero.bench.runMicrobenchSuite failed: ${msg}`;
              aeroMicrobenchSuite.samples = [];
              break;
            }
          }
        }
      }
    }

    // Trace capture happens after timed benchmarks to keep benchmark numbers stable.
    const trace = await tryCaptureAeroTrace(page, {
      trace: opts.trace,
      traceDurationMs: opts.traceDurationMs,
      traceWorkload:
        Number.isFinite(opts.traceDurationMs) || !opts.trace
          ? undefined
          : async () => {
              try {
                if (opts.includeAeroBench && aeroMicrobenchSuite.available) {
                  await page.evaluate(async () => {
                    const fn = globalThis.aero?.bench?.runMicrobenchSuite;
                    if (typeof fn === "function") {
                      await fn();
                    }
                  });
                } else {
                  await page.evaluate(microbench);
                }
              } catch {
                // Best-effort trace capture should not fail the run.
              }
            },
    });

    const aeroPerfExport = await tryCaptureAeroPerfExport(page);
    if (typeof aeroPerfExport?.json === "string") {
      try {
        const parsed = JSON.parse(aeroPerfExport.json);
        if (parsed && typeof parsed === "object") {
          const extractJit = (value) => {
            if (!value || typeof value !== "object") return null;
            if ("jit" in value) return value.jit ?? null;
            // Legacy wrappers sometimes nest the payload under `capture` or `exported`.
            if ("capture" in value) return extractJit(value.capture);
            if ("exported" in value) return extractJit(value.exported);
            return null;
          };

          jit = extractJit(parsed);
        }
      } catch {
        // Ignore parse errors; keep jit=null.
      }
    }

    const pageUrl = page.url();
    let pageTimeOrigin = null;
    try {
      pageTimeOrigin = await page.evaluate(() => performance.timeOrigin);
    } catch {
      // ignored
    }

    return {
      samples,
      aeroMicrobenchSuite,
      chromiumVersion: browser.version(),
      pageUrl,
      pageTimeOrigin,
      aeroPerfExport,
      trace,
      // PF-006: surface key JIT metrics in the benchmark output so regressions
      // can be attributed without digging through raw exports.
      jit,
    };
  } finally {
    await browser.close();
  }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const outDir = path.resolve(process.cwd(), opts.outDir);
  await fs.mkdir(outDir, { recursive: true });
  const url = opts.url ?? TEST_URL;

  const [gitSha, gitRef] = await Promise.all([
    gitValue(["rev-parse", "HEAD"]),
    gitValue(["rev-parse", "--abbrev-ref", "HEAD"]),
  ]);

  const iterations = opts.iterations;
  const startupSamples = [];
  for (let i = 0; i < iterations; i += 1) {
    startupSamples.push(await runChromiumStartupOnce(url));
  }

  const micro = await runMicrobenchSamples(url, iterations, opts);

  const benchmarks = [
    {
      name: "chromium_startup_ms",
      unit: "ms",
      samples: startupSamples,
      stats: summarize(startupSamples),
    },
    {
      name: "microbench_ms",
      unit: "ms",
      samples: micro.samples,
      stats: summarize(micro.samples),
    },
  ];

  if (opts.includeAeroBench) {
    if (micro.aeroMicrobenchSuite.status === "ok") {
      benchmarks.push({
        name: "aero_microbench_suite_ms",
        unit: "ms",
        samples: micro.aeroMicrobenchSuite.samples,
        stats: summarize(micro.aeroMicrobenchSuite.samples),
      });
    } else {
      benchmarks.push({
        name: "aero_microbench_suite_ms",
        unit: "ms",
        skipped: true,
        reason: micro.aeroMicrobenchSuite.reason,
      });
    }
  }

  const meta = {
    collectedAt: new Date().toISOString(),
    gitSha,
    gitRef,
    nodeVersion: process.version,
    os: {
      platform: os.platform(),
      release: os.release(),
      arch: os.arch(),
      cpuModel: os.cpus()?.[0]?.model,
      cpuCount: os.cpus()?.length,
    },
    playwrightCoreVersion,
    browserId: "chromium",
    chromiumVersion: micro.chromiumVersion,
    chromiumArgs: CHROMIUM_ARGS,
    targetUrl: url,
    pageUrl: micro.pageUrl,
    pageTimeOrigin: micro.pageTimeOrigin,
    iterations,
    runner: {
      includeAeroBench: opts.includeAeroBench,
      trace: opts.trace,
      traceDurationMs: opts.traceDurationMs ?? null,
    },
    aeroPerf: {
      jit: micro.jit ?? null,
      exportAvailable: micro.aeroPerfExport?.available ?? false,
      exportApiTimedOut: micro.aeroPerfExport?.apiTimedOut ?? false,
      trace: {
        requested: micro.trace?.requested ?? false,
        available: micro.trace?.available ?? false,
        captured: micro.trace?.captured ?? false,
        timedOut: micro.trace?.timedOut ?? false,
        durationMs: micro.trace?.durationMs ?? null,
      },
    },
  };

  const raw = { meta, benchmarks };
  const summary = {
    meta,
    benchmarks: benchmarks
      .filter((b) => b.stats && typeof b.stats === "object")
      .map((b) => ({
        name: b.name,
        unit: b.unit,
        stats: b.stats,
      })),
  };

  const writes = [
    fs.writeFile(path.join(outDir, "raw.json"), JSON.stringify(raw, null, 2)),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(summary, null, 2)),
  ];

  writes.push(
    fs.writeFile(
      path.join(outDir, "perf_export.json"),
      typeof micro.aeroPerfExport?.json === "string" ? `${micro.aeroPerfExport.json}\n` : "null\n",
    ),
  );

  writes.push(
    fs.writeFile(path.join(outDir, "trace.json"), typeof micro.trace?.json === "string" ? `${micro.trace.json}\n` : "null\n"),
  );

  await Promise.all(writes);

  console.log(`[perf] wrote ${path.relative(process.cwd(), outDir)}/raw.json and summary.json`);
}

await main();
