import assert from 'node:assert/strict';
import test from 'node:test';

import { FrameTimeStats } from '../src/frame-time-stats.js';

function approxEqual(actual, expected, eps = 1e-9) {
  assert.ok(
    Math.abs(actual - expected) <= eps,
    `expected ${expected} ±${eps}, got ${actual}`,
  );
}

test('FrameTimeStats: summary fields follow the documented definitions', () => {
  const stats = new FrameTimeStats({ keepLastNSamples: 10 });
  for (const ms of [16, 16, 16, 33]) stats.pushFrameTimeMs(ms);

  const s = stats.summary();

  assert.equal(s.frames, 4);
  approxEqual(s.totalTimeMs, 81);
  approxEqual(s.meanFrameTimeMs, 20.25);
  assert.equal(s.minFrameTimeMs, 16);
  assert.equal(s.maxFrameTimeMs, 33);

  approxEqual(s.fpsAvg, (4 * 1000) / 81, 1e-12);

  // Nearest-rank percentiles on frame times:
  // sorted = [16, 16, 16, 33]
  // p50 -> 2nd sample (16)
  // p99 -> 4th sample (33)
  assert.ok(Math.abs(s.frameTimeP50Ms - 16) < 0.1);
  assert.ok(Math.abs(s.frameTimeP99Ms - 33) < 0.5);
  assert.ok(Math.abs(s.fpsMedian - 62.5) < 0.5);
  assert.ok(Math.abs(s.fps1Low - 1000 / 33) < 0.5);

  assert.deepEqual(stats.getRecentFrameTimesMs(), [16, 16, 16, 33]);
});

test('FrameTimeStats: merge is equivalent to recording all samples in one instance', () => {
  const a = new FrameTimeStats();
  const b = new FrameTimeStats();
  const all = new FrameTimeStats();

  const frameTimes = [];
  let seed = 123456789;

  for (let i = 0; i < 50_000; i += 1) {
    // LCG for deterministic pseudo-random values in [5, 50).
    seed = (1103515245 * seed + 12345) % 0x80000000;
    const ms = 5 + (seed / 0x80000000) * 45;
    frameTimes.push(ms);
  }

  for (let i = 0; i < frameTimes.length; i += 1) {
    const ms = frameTimes[i];
    all.pushFrameTimeMs(ms);
    if (i % 2 === 0) a.pushFrameTimeMs(ms);
    else b.pushFrameTimeMs(ms);
  }

  a.merge(b);

  const merged = a.summary();
  const single = all.summary();

  assert.equal(merged.frames, single.frames);
  assert.ok(Math.abs(merged.totalTimeMs - single.totalTimeMs) < 1e-6);
  assert.ok(Math.abs(merged.meanFrameTimeMs - single.meanFrameTimeMs) < 1e-12);
  assert.ok(Math.abs(merged.varianceFrameTimeMs2 - single.varianceFrameTimeMs2) < 1e-9);
  assert.ok(Math.abs(merged.frameTimeP99Ms - single.frameTimeP99Ms) < 1e-9);
  assert.ok(Math.abs(merged.fps1Low - single.fps1Low) < 1e-9);
});

test('FrameTimeStats: toJSON/fromJSON roundtrips', () => {
  const original = new FrameTimeStats({ keepLastNSamples: 3 });
  for (const ms of [10, 11, 12, 13, 14]) original.pushFrameTimeMs(ms);

  const restored = FrameTimeStats.fromJSON(original.toJSON());

  assert.deepEqual(restored.getRecentFrameTimesMs(), [12, 13, 14]);

  const a = original.summary();
  const b = restored.summary();

  assert.equal(a.frames, b.frames);
  assert.ok(Math.abs(a.totalTimeMs - b.totalTimeMs) < 1e-9);
  assert.ok(Math.abs(a.frameTimeP99Ms - b.frameTimeP99Ms) < 1e-9);
});
