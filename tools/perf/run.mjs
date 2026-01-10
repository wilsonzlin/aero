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

const execFile = promisify(execFileCb);
const require = createRequire(import.meta.url);
const playwrightCoreVersion = require("playwright-core/package.json").version;

function usage(exitCode) {
  const msg = `
Usage:
  node tools/perf/run.mjs --out-dir <dir> --iterations <n>

Options:
  --out-dir <dir>       Output directory (required)
  --iterations <n>      Iterations per benchmark (default: 3)
  --url <url>           URL to load (default: internal data: URL)
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
      console.warn(`[perf] ${label} failed (attempt ${attempt}/${attempts}): ${err?.message ?? err}`);
      if (attempt < attempts) {
        await sleep(250 * attempt);
      }
    }
  }
  throw lastErr;
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
  try {
    try {
      await page.waitForFunction(() => {
        const perf = globalThis.aero?.perf;
        return perf && typeof perf === "object" && typeof perf.export === "function";
      }, null, { timeout: 2_000 });
    } catch {
      // Best-effort: if the app doesn't expose a perf API we still want the rest of the run to succeed.
    }

    return await page.evaluate(async () => {
      const aero = globalThis.aero;
      const perf = aero && typeof aero === "object" ? aero.perf : undefined;
      if (!perf || typeof perf !== "object") return null;
      if (typeof perf.captureStart !== "function" || typeof perf.captureStop !== "function") return null;
      if (typeof perf.export !== "function") return null;

      if (typeof perf.captureReset === "function") {
        perf.captureReset();
      }

      perf.captureStart();
      await new Promise((resolve) => setTimeout(resolve, 1000));
      perf.captureStop();

      try {
        return JSON.stringify(perf.export(), null, 2);
      } catch {
        return null;
      }
    });
  } catch {
    return null;
  }
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

async function runMicrobenchSamples(url, iterations) {
  const browser = await launchChromium();
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
    const page = await context.newPage();
    await gotoWithRetries(page, url);

    const aeroPerfExportJson = await tryCaptureAeroPerfExport(page);
    let jit = null;
    if (typeof aeroPerfExportJson === "string") {
      try {
        const parsed = JSON.parse(aeroPerfExportJson);
        if (parsed && typeof parsed === "object") {
          // Most perf exports: `{ jit: ... }`
          if ("jit" in parsed) {
            jit = parsed.jit ?? null;
          } else if ("capture" in parsed) {
            // Wrapped exports (see `web/src/runtime/aero_global.ts`): `{ capture: ..., benchmarks: ... }`
            const cap = parsed.capture;
            if (cap && typeof cap === "object" && "jit" in cap) {
              jit = cap.jit ?? null;
            }
          }
        }
      } catch {
        // Ignore parse errors; keep jit=null.
      }
    }

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

    return {
      samples,
      chromiumVersion: browser.version(),
      aeroPerfExportJson,
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

  const micro = await runMicrobenchSamples(url, iterations);

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
    chromiumVersion: micro.chromiumVersion,
    chromiumArgs: CHROMIUM_ARGS,
    targetUrl: url,
    iterations,
    aeroPerf: {
      jit: micro.jit ?? null,
    },
  };

  const raw = { meta, benchmarks };
  const summary = {
    meta,
    benchmarks: benchmarks.map((b) => ({
      name: b.name,
      unit: b.unit,
      stats: b.stats,
    })),
  };

  const writes = [
    fs.writeFile(path.join(outDir, "raw.json"), JSON.stringify(raw, null, 2)),
    fs.writeFile(path.join(outDir, "summary.json"), JSON.stringify(summary, null, 2)),
  ];

  if (typeof micro.aeroPerfExportJson === "string") {
    writes.push(fs.writeFile(path.join(outDir, "perf_export.json"), `${micro.aeroPerfExportJson}\n`));
  }

  await Promise.all(writes);

  console.log(`[perf] wrote ${path.relative(process.cwd(), outDir)}/raw.json and summary.json`);
}

await main();
