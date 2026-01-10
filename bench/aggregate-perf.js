#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';

import { FrameTimeStats } from '../packages/aero-stats/src/frame-time-stats.js';

async function loadFrameTimeStats(path) {
  const raw = JSON.parse(await readFile(path, 'utf8'));
  if (raw?.type === 'PerfReport') {
    return FrameTimeStats.fromJSON(raw.stats);
  }
  if (raw?.type === 'FrameTimeStats') {
    return FrameTimeStats.fromJSON(raw);
  }
  throw new Error(
    `Unsupported perf payload in ${path} (expected {type:'FrameTimeStats'} or {type:'PerfReport'})`,
  );
}

function usage() {
  return `Usage: node bench/aggregate-perf.js <out.json> <in1.json> <in2.json> ...

Inputs must be JSON produced by FrameTimeStats.toJSON() or by this tool.
`;
}

async function main() {
  const [outPath, ...inPaths] = process.argv.slice(2);
  if (!outPath || inPaths.length === 0) {
    process.stderr.write(usage());
    process.exit(1);
  }

  const merged = new FrameTimeStats();
  for (const path of inPaths) merged.merge(await loadFrameTimeStats(path));

  const report = {
    type: 'PerfReport',
    summary: merged.summary(),
    stats: merged.toJSON(),
  };

  await writeFile(outPath, JSON.stringify(report, null, 2));
}

await main();

