# AudioFrameClock time→frame conversion vectors

This fixture (`audio_frame_clock_vectors.json`) contains shared conformance vectors for deterministic
time→audio-frame conversion.

The same semantics are implemented in two places:

- Rust: `crates/aero-audio/src/clock.rs`
- TypeScript: `web/src/audio/audio_frame_clock.ts`

## Format

The fixture is a JSON array of vectors.

Each vector is:

- `name` (string): test case label.
- `sample_rate_hz` (integer): audio sample rate (frames/second).
- `start_time_ns` (integer): initial `last_time_ns` / `lastTimeNs` timestamp (nanoseconds).
- `steps` (integer[]): absolute `now_ns` timestamps to pass to `advance_to` / `advanceTo` in order.
- `expected_frames_per_step` (integer[]): frames returned by each step (same length as `steps`).
- `expected_final_frac` (integer): remainder accumulator after the final step
  (`AudioFrameClock.frac_fp` in Rust, `AudioFrameClock.fracNsTimesRate` in TS).

### Integer precision

The fixture stores all values as JSON numbers, so all timestamps must fit within
`Number.MAX_SAFE_INTEGER` to remain lossless when parsed by JS. The generator script validates this
invariant.

## Semantics

- If a step's `now_ns` is **earlier than or equal to** the previous accepted time, it must return
  `0` frames and **must not change** the internal clock state (no backwards time travel).

## Regenerating

If you intentionally change the conversion semantics, update:

1. Rust implementation (`clock.rs`)
2. TS implementation (`audio_frame_clock.ts`)
3. Regenerate the fixture:
   ```bash
   node tests/fixtures/generate_audio_frame_clock_vectors.mjs > tests/fixtures/audio_frame_clock_vectors.json
   ```

Both the Rust unit tests and the web (vitest) unit tests consume this fixture to prevent subtle
drift across languages.
