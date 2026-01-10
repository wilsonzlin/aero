#!/usr/bin/env node

import { readFile } from 'node:fs/promises';

import { FrameTimeStats } from '../packages/aero-stats/src/frame-time-stats.js';

async function loadSummary(path) {
  const raw = JSON.parse(await readFile(path, 'utf8'));
  if (raw?.type === 'PerfReport') return raw.summary;
  if (raw?.type === 'FrameTimeStats') return FrameTimeStats.fromJSON(raw).summary();
  throw new Error(
    `Unsupported perf payload in ${path} (expected {type:'FrameTimeStats'} or {type:'PerfReport'})`,
  );
}

function usage() {
  return `Usage: node bench/compare-perf.js <baseline.json> <current.json> [--tolerance=0.05]

Compares current metrics against baseline and exits non-zero on regression.
`;
}

function parseArgs(argv) {
  const positional = [];
  let tolerance = 0.05;

  for (const arg of argv) {
    if (arg.startsWith('--tolerance=')) {
      tolerance = Number(arg.slice('--tolerance='.length));
      continue;
    }
    positional.push(arg);
  }

  if (!Number.isFinite(tolerance) || tolerance < 0) {
    throw new Error(`Invalid --tolerance value: ${tolerance}`);
  }

  return { positional, tolerance };
}

function formatPct(x) {
  if (!Number.isFinite(x)) return 'n/a';
  return `${(x * 100).toFixed(2)}%`;
}

async function main() {
  const { positional, tolerance } = parseArgs(process.argv.slice(2));
  const [baselinePath, currentPath] = positional;
  if (!baselinePath || !currentPath) {
    process.stderr.write(usage());
    process.exit(1);
  }

  const baseline = await loadSummary(baselinePath);
  const current = await loadSummary(currentPath);

  const metrics = ['fpsAvg', 'fpsMedian', 'fpsP95', 'fps1Low', 'fps0_1Low'];
  const failures = [];

  for (const key of metrics) {
    const base = baseline[key];
    const cur = current[key];
    if (!Number.isFinite(base) || base === 0 || !Number.isFinite(cur)) continue;

    const change = (cur - base) / base;
    if (change < -tolerance) failures.push({ key, base, cur, change });

    process.stdout.write(
      `${key}: baseline=${base.toFixed(3)} current=${cur.toFixed(3)} change=${formatPct(change)}\n`,
    );
  }

  if (failures.length !== 0) {
    process.stdout.write(
      `\nRegression detected (tolerance=${formatPct(tolerance)}): ${failures
        .map((f) => f.key)
        .join(', ')}\n`,
    );
    process.exit(2);
  }
}

await main();

