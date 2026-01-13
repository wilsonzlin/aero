#!/usr/bin/env node
/**
 * Deterministically regenerate `audio_frame_clock_vectors.json`.
 *
 * Usage:
 *   node tests/fixtures/generate_audio_frame_clock_vectors.mjs > tests/fixtures/audio_frame_clock_vectors.json
 *
 * The reference math here mirrors the integer fixed-point algorithm used by:
 * - Rust: `crates/aero-audio/src/clock.rs`
 * - TS:   `web/src/audio/audio_frame_clock.ts`
 */
const NS_PER_SEC = 1_000_000_000n;
const MAX_SAFE_INTEGER = BigInt(Number.MAX_SAFE_INTEGER);

function stepsFromDeltas(deltasNs) {
  let now = 0;
  const steps = [];
  for (const delta of deltasNs) {
    now += delta;
    steps.push(now);
  }
  return steps;
}

function assertSafeInteger(value, label) {
  if (!Number.isSafeInteger(value)) throw new Error(`${label} must be a JS safe integer, got: ${String(value)}`);
  if (value < 0) throw new Error(`${label} must be >= 0, got: ${String(value)}`);
  return value;
}

function toSafeNumber(value, label) {
  if (value > MAX_SAFE_INTEGER) throw new Error(`${label} overflows JS safe integer range: ${value.toString()}`);
  return Number(value);
}

function simulateVector({ sample_rate_hz, start_time_ns, steps }) {
  const sr = BigInt(sample_rate_hz);
  let last = BigInt(start_time_ns);
  let frac_fp = 0n;
  const expected_frames_per_step = [];

  for (const nowNs of steps) {
    const now = BigInt(nowNs);
    if (now <= last) {
      expected_frames_per_step.push(0);
      continue;
    }
    const delta = now - last;
    last = now;
    const total = frac_fp + delta * sr;
    const frames = total / NS_PER_SEC;
    frac_fp = total % NS_PER_SEC;
    expected_frames_per_step.push(toSafeNumber(frames, "frames"));
  }

  return {
    expected_frames_per_step,
    expected_final_frac: toSafeNumber(frac_fp, "expected_final_frac"),
  };
}

const vectors = [
  {
    name: "48kHz_small_deltas_and_duplicates",
    sample_rate_hz: 48000,
    start_time_ns: 0,
    steps: [0, 1, 101, 20_934, 20_935, 41_768, 61_768, 61_769, 62_769, 62_769, 30_062_769, 30_062_769, 30_083_602],
  },
  {
    name: "48kHz_backwards_time_is_ignored",
    sample_rate_hz: 48000,
    start_time_ns: 1_000,
    // Includes a backwards step (`21000` < `21834`). This must return 0 frames and must not
    // modify `last_time_ns` or `frac_fp`.
    steps: [21_833, 21_834, 21_000, 21_835],
  },
  {
    name: "48kHz_jittery_ticks_60Hz_16666666_16666667",
    sample_rate_hz: 48000,
    start_time_ns: 0,
    // ~60Hz jitter pattern where 1/60s (16_666_666.666...) is represented via a mix of
    // 16_666_666 and 16_666_667 ns steps. Frame output is intentionally non-uniform to verify
    // remainder carry semantics (799/800/801 frames at 48kHz).
    steps: [16_666_666, 33_333_333, 50_000_000, 66_666_666, 83_333_333, 100_000_000, 116_666_666],
  },
  {
    name: "48kHz_jittery_60Hz_40x16666667_20x16666666_full_second",
    sample_rate_hz: 48000,
    start_time_ns: 0,
    // 1s split into 60 ticks with the classic 40/20 distribution:
    // - 40 ticks of 16_666_667ns
    // - 20 ticks of 16_666_666ns
    //
    // The order here is chosen to produce non-uniform per-tick frame counts while still summing to
    // exactly 1_000_000_000ns, exercising remainder-carry semantics over a longer sequence.
    steps: stepsFromDeltas([
      ...Array.from({ length: 20 }, () => 16_666_667).flatMap((d) => [d, 16_666_666]),
      ...Array.from({ length: 20 }, () => 16_666_667),
    ]),
  },
  {
    name: "48kHz_jittery_ticks_variable_frames",
    sample_rate_hz: 48000,
    start_time_ns: 0,
    steps: [
      20_833_333,
      41_666_666,
      62_500_000,
      83_333_333,
      104_166_665,
      125_000_000,
      125_000_000,
      145_833_333,
      166_666_667,
      187_500_000,
      208_333_333,
    ],
  },
  {
    name: "48kHz_large_delta_500ms",
    sample_rate_hz: 48000,
    start_time_ns: 0,
    steps: [500_000_001, 500_000_001, 1_000_000_002, 1_500_000_002, 2_000_000_003],
  },
  {
    name: "44_1kHz_mixed_small_and_large",
    sample_rate_hz: 44100,
    start_time_ns: 1_000,
    steps: [1_000, 1_000, 1_001, 1_101, 500_001_101, 500_001_102, 500_001_102, 500_023_676, 500_023_676, 500_046_351],
  },
  {
    name: "44_1kHz_1ms_steps_accumulate_fraction",
    sample_rate_hz: 44100,
    start_time_ns: 0,
    steps: Array.from({ length: 11 }, (_, i) => i * 1_000_000),
  },
  {
    name: "96kHz_multi_second_deltas_with_remainder",
    sample_rate_hz: 96000,
    start_time_ns: 0,
    // Large multi-second deltas plus a 1ns step to ensure remainder carry is preserved across
    // very different step sizes.
    steps: [2_000_000_000, 2_000_000_001, 5_000_000_000, 5_000_000_123],
  },
];

for (const [i, v] of vectors.entries()) {
  assertSafeInteger(v.sample_rate_hz, `vectors[${i}].sample_rate_hz`);
  assertSafeInteger(v.start_time_ns, `vectors[${i}].start_time_ns`);
  if (!Array.isArray(v.steps) || v.steps.length === 0) throw new Error(`vectors[${i}].steps must be a non-empty array`);
  for (const [j, step] of v.steps.entries()) {
    assertSafeInteger(step, `vectors[${i}].steps[${j}]`);
  }
}

const out = vectors.map((v) => ({ ...v, ...simulateVector(v) }));

process.stdout.write(JSON.stringify(out, null, 2) + "\n");
