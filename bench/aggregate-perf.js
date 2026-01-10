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
  if (raw?.schema_version === 1) {
    const embedded = raw.frame_time?.stats;
    if (embedded?.type === 'FrameTimeStats') return FrameTimeStats.fromJSON(embedded);

    const stats = new FrameTimeStats();
    for (const frame of raw.samples?.frames ?? []) {
      const us = frame?.durations_us?.frame;
      if (typeof us === 'number' && Number.isFinite(us) && us > 0) {
        stats.pushFrameTimeMs(us / 1000);
      }
    }
    if (stats.frames > 0) return stats;
  }
  if (raw?.kind === 'aero-perf-capture') {
    const embedded = raw.frameTime?.stats;
    if (embedded?.type === 'FrameTimeStats') return FrameTimeStats.fromJSON(embedded);

    const stats = new FrameTimeStats();
    for (const rec of raw.records ?? []) {
      const ms = rec?.frameTimeMs;
      if (typeof ms === 'number' && Number.isFinite(ms) && ms > 0) stats.pushFrameTimeMs(ms);
    }
    if (stats.frames > 0) return stats;
  }
  throw new Error(
    `Unsupported perf payload in ${path}`,
  );
}

function usage() {
  return `Usage: node bench/aggregate-perf.js <out.json> <in1.json> <in2.json> ...

Inputs may be:
- FrameTimeStats.toJSON()
- PerfReport produced by this tool
- PerfAggregator export (schema_version=1)
- Fallback perf export (kind=aero-perf-capture)
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
