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

function simulateCase({ sample_rate_hz, start_time_ns, now_ns }) {
  const sr = BigInt(sample_rate_hz);
  let last = BigInt(start_time_ns);
  let frac_fp = 0n;
  const expected_frames = [];

  for (const nowStr of now_ns) {
    const now = BigInt(nowStr);
    if (now <= last) {
      expected_frames.push(0);
      continue;
    }
    const delta = now - last;
    last = now;
    const total = frac_fp + delta * sr;
    const frames = total / NS_PER_SEC;
    frac_fp = total % NS_PER_SEC;
    expected_frames.push(Number(frames));
  }

  return {
    expected_frames,
    expected_end: { last_time_ns: last.toString(), frac_fp: frac_fp.toString() },
  };
}

const cases = [
  {
    name: "48kHz_small_deltas_and_duplicates",
    sample_rate_hz: 48000,
    start_time_ns: "0",
    now_ns: ["0", "1", "101", "20934", "20935", "41768", "61768", "61769", "62769", "62769", "30062769", "30062769", "30083602"],
  },
  {
    name: "48kHz_jittery_ticks_variable_frames",
    sample_rate_hz: 48000,
    start_time_ns: "0",
    now_ns: [
      "20833333",
      "41666666",
      "62500000",
      "83333333",
      "104166665",
      "125000000",
      "125000000",
      "145833333",
      "166666667",
      "187500000",
      "208333333",
    ],
  },
  {
    name: "48kHz_large_delta_500ms",
    sample_rate_hz: 48000,
    start_time_ns: "0",
    now_ns: ["500000001", "500000001", "1000000002", "1500000002", "2000000003"],
  },
  {
    name: "44_1kHz_mixed_small_and_large",
    sample_rate_hz: 44100,
    start_time_ns: "1000",
    now_ns: ["1000", "1000", "1001", "1101", "500001101", "500001102", "500001102", "500023676", "500023676", "500046351"],
  },
  {
    name: "44_1kHz_1ms_steps_accumulate_fraction",
    sample_rate_hz: 44100,
    start_time_ns: "0",
    now_ns: ["0", "1000000", "2000000", "3000000", "4000000", "5000000", "6000000", "7000000", "8000000", "9000000", "10000000"],
  },
];

const out = {
  version: 1,
  description:
    "Cross-language conformance vectors for AudioFrameClock (Rust crates/aero-audio/src/clock.rs vs TypeScript web/src/audio/audio_frame_clock.ts). If the clock behavior changes intentionally, regenerate this file (see tests/fixtures/generate_audio_frame_clock_vectors.mjs) and update both implementations together.",
  cases: cases.map((c) => ({ ...c, ...simulateCase(c) })),
};

process.stdout.write(JSON.stringify(out, null, 2) + "\n");
