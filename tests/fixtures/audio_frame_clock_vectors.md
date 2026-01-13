# AudioFrameClock time→frame conversion vectors

This fixture (`audio_frame_clock_vectors.json`) contains shared conformance vectors for deterministic
time→audio-frame conversion.

The same semantics are implemented in two places:

- Rust: `crates/aero-audio/src/clock.rs`
- TypeScript: `web/src/audio/audio_frame_clock.ts`

## Format

The fixture is a single JSON object:

- `version` (number): format version.
- `description` (string, optional): human-readable notes.
- `cases` (array): conformance cases.

Each case is:

- `name` (string): test case label.
- `sample_rate_hz` (number): audio sample rate (frames/second).
- `start_time_ns` (string): initial `last_time_ns` / `lastTimeNs` timestamp (`u64` encoded as a
  decimal string).
- `now_ns` (string[]): absolute `now_ns` timestamps to pass to `advance_to` / `advanceTo` in order
  (`u64` encoded as decimal strings).
- `expected_frames` (number[]): frames returned by each step (same length as `now_ns`).
- `expected_end`:
  - `last_time_ns` (string): expected final `last_time_ns` / `lastTimeNs` (`u64` as decimal string).
  - `frac_fp` (string): expected final remainder accumulator (`AudioFrameClock.frac_fp` in Rust,
    `AudioFrameClock.fracNsTimesRate` in TS) as a decimal string.

### Why strings?

JSON has no `bigint` type, and decoding large integers as JS `number` can lose precision. All
nanosecond timestamps and the remainder accumulator are therefore stored as **decimal strings**, and
the web unit tests parse them with `BigInt(...)`.

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
