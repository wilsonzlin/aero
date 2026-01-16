import { execFileSync } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

import { SCHEMA_VERSION } from "./schema.js";
import { startStaticServer } from "./server.js";
import { SCENARIOS, getScenario } from "./scenarios/index.js";
import { computeStats } from "./util/stats.js";

const require = createRequire(import.meta.url);

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

/**
 * @typedef {Object} ScenarioRunContext
 * @property {import('playwright').Browser} browser
 * @property {string} baseUrl
 * @property {{ width: number, height: number }} viewport
 * @property {number} warmupIterations
 * @property {number} iterations
 * @property {number} idleSeconds
 * @property {number} timeoutMs
 */

/**
 * @typedef {Object} BenchOptions
 * @property {string[]} scenarios
 * @property {number} iterations
 * @property {number} warmupIterations
 * @property {number} idleSeconds
 * @property {string | undefined} url
 * @property {string | undefined} outputPath
 * @property {string | undefined} resultsDir
 * @property {string | undefined} runId
 * @property {number} timeoutMs
 * @property {number} unstableCov
 * @property {boolean} skipBuild
 * @property {boolean} headless
 */

function formatNumber(v) {
  if (!Number.isFinite(v)) return 'NaN';
  if (Math.abs(v) >= 100) return v.toFixed(1);
  if (Math.abs(v) >= 10) return v.toFixed(2);
  return v.toFixed(3);
}

function buildRunId() {
  const iso = new Date().toISOString();
  const safe = iso.replace(/[-:.TZ]/g, '');
  const rand = Math.random().toString(16).slice(2, 8);
  return `${safe}-${rand}`;
}

async function pathExists(p) {
  try {
    await fs.access(p);
    return true;
  } catch {
    return false;
  }
}

/**
 * @param {string} projectRoot
 * @param {{ skipBuild: boolean }} opts
 */
async function maybeBuild(projectRoot, opts) {
  if (opts.skipBuild) return;
  try {
    execFileSync("npm", ["run", "build"], { stdio: "inherit", cwd: projectRoot });
  } catch (err) {
    // For CI/dev ergonomics, prefer falling back to the built-in fixture over hard-failing.
    // If callers need to enforce a successful build, they can do so before invoking the runner.
    // eslint-disable-next-line no-console
    console.warn(`[bench] build failed; will attempt to continue.`, err?.message ?? err);
  }
}

/**
 * @param {ReturnType<typeof computeStats>} stats
 * @param {number} unstableCov
 */
function decorateStats(stats, unstableCov) {
  const unstable = stats.samples > 1 && stats.cov > unstableCov;
  return { ...stats, unstable };
}

function renderTable(summary) {
  /** @type {Array<[string, string, any]>} */
  const rows = [];
  for (const scen of summary.scenarios) {
    for (const [metric, stats] of Object.entries(scen.metrics)) {
      rows.push([scen.id, metric, stats]);
    }
  }

  rows.sort((a, b) => {
    if (a[0] !== b[0]) return a[0].localeCompare(b[0]);
    return a[1].localeCompare(b[1]);
  });

  const scenarioWidth = Math.max('scenario'.length, ...rows.map(([s]) => s.length));
  const metricWidth = Math.max('metric'.length, ...rows.map(([, m]) => m.length));

  const header = [
    'scenario'.padEnd(scenarioWidth),
    'metric'.padEnd(metricWidth),
    'median'.padStart(10),
    'stdev'.padStart(10),
    'CoV%'.padStart(8),
    'samples'.padStart(8),
    'stable'.padStart(8)
  ].join('  ');

  // eslint-disable-next-line no-console
  console.log(header);
  // eslint-disable-next-line no-console
  console.log('-'.repeat(header.length));

  for (const [scenario, metric, stats] of rows) {
    const stable = stats.unstable ? 'no' : 'yes';
    const line = [
      scenario.padEnd(scenarioWidth),
      metric.padEnd(metricWidth),
      formatNumber(stats.median).padStart(10),
      formatNumber(stats.stdev).padStart(10),
      formatNumber(stats.cov * 100).padStart(8),
      String(stats.samples).padStart(8),
      stable.padStart(8)
    ].join('  ');
    // eslint-disable-next-line no-console
    console.log(line);
  }
}

function loadPlaywright() {
  try {
    // Preferred: full Playwright API.
    return require('playwright');
  } catch {
    try {
      // Fallback: Playwright test runner package (commonly present in repos).
      return require('@playwright/test');
    } catch (err) {
      throw new Error(
        `Playwright is not installed. Install either 'playwright' or '@playwright/test'.\n` +
          `Original error: ${err?.message ?? err}`
      );
    }
  }
}

/**
 * @param {import('playwright').Browser} browser
 * @param {{ baseUrl: string, viewport: { width: number, height: number }, timeoutMs: number }} opts
 */
async function probeAeroApi(browser, opts) {
  const context = await browser.newContext({ viewport: opts.viewport, deviceScaleFactor: 1 });
  const page = await context.newPage();
  try {
    await page.goto(opts.baseUrl, { waitUntil: 'load' });
    const probeTimeoutMs = Math.min(5_000, Math.max(0, opts.timeoutMs));
    await page
      .waitForFunction(
        () =>
          // eslint-disable-next-line no-undef
          Boolean(window.aero?.perf && typeof window.aero.perf.export === 'function'),
        { timeout: probeTimeoutMs, polling: 50 }
      )
      .catch(() => undefined);
    return await page.evaluate(() => {
      // eslint-disable-next-line no-undef
      const aero = window.aero;
      return {
        hasAero: Boolean(aero),
        hasReady:
          typeof aero?.isReady === 'function' ||
          aero?.ready === true ||
          (aero?.whenReady && typeof aero.whenReady.then === 'function'),
        hasPerfExport: typeof aero?.perf?.export === 'function'
      };
    });
  } finally {
    await context.close();
  }
}

/**
 * @param {BenchOptions} opts
 */
export async function runBench(opts) {
  const { chromium } = loadPlaywright();

  const projectRoot = path.resolve(__dirname, '..');
  const resultsDir = path.resolve(projectRoot, opts.resultsDir ?? path.join('bench', 'results'));
  await fs.mkdir(resultsDir, { recursive: true });

  const runId = opts.runId ?? buildRunId();
  const startedAt = new Date().toISOString();

  const viewport = { width: 1280, height: 720 };

  const requested = opts.scenarios.length ? opts.scenarios : SCENARIOS.map((s) => s.id);
  for (const s of requested) {
    if (!getScenario(s)) throw new Error(`Unknown scenario: ${s}. Available: ${SCENARIOS.map((x) => x.id).join(', ')}`);
  }

  const scenarios = requested.map((id) => getScenario(id)).filter(Boolean);
  if (scenarios.length === 0) {
    throw new Error(`No scenarios selected. Available: ${SCENARIOS.map((s) => s.id).join(', ')}`);
  }

  /** @type {string | undefined} */
  let baseUrl = opts.url;
  /** @type {null | { close: () => Promise<void> }} */
  let server = null;
  /** @type {'url' | 'dist' | 'fixture'} */
  let siteKind = baseUrl ? 'url' : 'dist';
  const distDir = path.join(projectRoot, 'dist');
  const fixtureDir = path.join(projectRoot, 'bench', 'fixture');

  if (!baseUrl) {
    await maybeBuild(projectRoot, { skipBuild: opts.skipBuild });
    const distExists = await pathExists(path.join(distDir, 'index.html'));
    const rootDir = distExists ? distDir : fixtureDir;
    siteKind = distExists ? 'dist' : 'fixture';

    const started = await startStaticServer({ rootDir });
    baseUrl = started.baseUrl;
    server = { close: started.close };
  }

  const launchArgs = [
    '--no-sandbox',
    '--disable-dev-shm-usage',
    '--disable-background-timer-throttling',
    '--disable-backgrounding-occluded-windows',
    '--disable-renderer-backgrounding',
    '--disable-breakpad',
    '--disable-component-extensions-with-background-pages',
    '--disable-features=TranslateUI',
    '--metrics-recording-only',
    '--mute-audio',
    `--window-size=${viewport.width},${viewport.height}`
  ];

  /** @type {import('playwright').Browser | null} */
  let browser = null;
  /** @type {any} */
  let rawResults;
  try {
    browser = await chromium.launch({ headless: opts.headless, args: launchArgs });

    if (siteKind === 'dist' && server && (await pathExists(path.join(fixtureDir, 'index.html')))) {
      const probe = await probeAeroApi(browser, { baseUrl, viewport, timeoutMs: opts.timeoutMs });
      if (!probe.hasAero || !probe.hasReady || !probe.hasPerfExport) {
        // eslint-disable-next-line no-console
        console.warn(
          `[bench] dist/ does not expose expected window.aero perf APIs; falling back to bench/fixture.\n` +
            `Detected: ${JSON.stringify(probe)}`
        );
        await server.close();
        const started = await startStaticServer({ rootDir: fixtureDir });
        baseUrl = started.baseUrl;
        server = { close: started.close };
        siteKind = 'fixture';
      }
    }

    /** @type {ScenarioRunContext} */
    const baseCtx = {
      browser,
      baseUrl,
      viewport,
      warmupIterations: opts.warmupIterations,
      iterations: opts.iterations,
      idleSeconds: opts.idleSeconds,
      timeoutMs: opts.timeoutMs
    };

    const scenarioResults = [];
    for (const scenario of scenarios) {
      const res = await scenario.run(baseCtx);
      scenarioResults.push(res);
    }

    rawResults = {
      schemaVersion: SCHEMA_VERSION,
      runId,
      startedAt,
      options: {
        scenarios: requested,
        iterations: opts.iterations,
        warmupIterations: opts.warmupIterations,
        idleSeconds: opts.idleSeconds,
        url: opts.url,
        headless: opts.headless,
        siteKind
      },
      environment: {
        node: process.version,
        platform: process.platform,
        arch: process.arch,
        ci: Boolean(process.env.CI),
        browserVersion: browser.version()
      },
      scenarios: scenarioResults
    };

    const summary = {
      schemaVersion: SCHEMA_VERSION,
      runId,
      generatedAt: new Date().toISOString(),
      unstableCov: opts.unstableCov,
      scenarios: [],
      unstable: false
    };

    for (const scenario of scenarioResults) {
      /** @type {Record<string, number[]>} */
      const metricValues = {};
      const measuredRuns = scenario.runs.filter((r) => !r.warmup);
      for (const run of measuredRuns) {
        for (const [k, v] of Object.entries(run.metrics ?? {})) {
          if (typeof v !== 'number' || !Number.isFinite(v)) continue;
          metricValues[k] ??= [];
          metricValues[k].push(v);
        }
      }

      /** @type {Record<string, any>} */
      const metrics = {};
      let scenarioUnstable = false;
      for (const [metric, values] of Object.entries(metricValues)) {
        const stats = decorateStats(computeStats(values), opts.unstableCov);
        metrics[metric] = stats;
        if (stats.unstable) scenarioUnstable = true;
      }

      summary.scenarios.push({
        id: scenario.id,
        name: scenario.name,
        metrics,
        unstable: scenarioUnstable
      });

      if (scenarioUnstable) summary.unstable = true;
    }

    renderTable(summary);

    const rawPath = path.join(resultsDir, `${runId}.json`);
    const summaryPath = path.join(resultsDir, `${runId}.summary.json`);

    await fs.writeFile(rawPath, JSON.stringify(rawResults, null, 2));
    await fs.writeFile(summaryPath, JSON.stringify(summary, null, 2));

    if (opts.outputPath) {
      const outPath = path.resolve(projectRoot, opts.outputPath);
      await fs.writeFile(outPath, JSON.stringify(summary, null, 2));
    }
  } finally {
    if (browser) await browser.close();
    if (server) await server.close();
  }
}
