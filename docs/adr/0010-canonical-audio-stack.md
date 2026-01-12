# ADR 0010: Canonical audio stack (aero-audio + aero-virtio; legacy emulator audio gated)

## Context

The repo accumulated two overlapping audio implementations:

1. **Newer (browser/WASM path)**:
   - `crates/aero-audio` (HDA device model: playback + capture; PCM helpers)
   - `crates/aero-virtio` (virtio device models, including virtio-snd)
   - `crates/platform::audio::*` (SharedArrayBuffer ring-buffer layouts used by AudioWorklets)
   - `crates/aero-wasm` (wasm-pack exports used by the browser runtime, including the IO-worker-facing
     `HdaControllerBridge` that exposes HDA as a PCI/MMIO device and attaches the AudioWorklet/mic rings)

2. **Legacy/parallel**:
   - `crates/emulator/src/io/audio/*` (AC97, HDA, DSP, capture plumbing)
   - `crates/emulator/src/io/virtio/devices/snd.rs` (a second virtio-snd model)

The browser demo/runtime code interacts with `crates/aero-wasm` exports and uses the
newer stack; the legacy `crates/emulator` audio code is not on the default path.

Maintaining two stacks increases the risk of:

- inconsistent device behavior and wire formats
- duplicated bug fixes and test effort
- diverging AudioWorklet / SharedArrayBuffer layouts

## Decision

We standardize on the **newer audio stack** as the canonical implementation:

- **HDA device model**: `crates/aero-audio` (`aero_audio::hda`)
- **virtio-snd device model**: `crates/aero-virtio` (`aero_virtio::devices::snd`)
- **AudioWorklet + microphone SharedArrayBuffer bridges**: `crates/platform` (`aero_platform::audio::{worklet_bridge,mic_bridge}`)
- **Browser runtime WASM exports**: `crates/aero-wasm` (exports device bridges like `HdaControllerBridge` used by the IO worker)

The legacy `crates/emulator` audio implementation is retained short-term, but is
**not built by default**. It is gated behind the `emulator/legacy-audio` feature
to keep it available for reference and targeted tests.

## Migration plan

1. **Docs + references**
   - Treat `aero-audio` / `aero-virtio` / `aero-platform::audio` as canonical in docs.
   - Clearly label `crates/emulator` audio code as legacy/deprecated.

2. **Code**
   - Keep legacy `crates/emulator` audio modules behind `emulator/legacy-audio`.
   - Prefer `aero-virtio::devices::snd` everywhere new virtio-snd work is done.

3. **Follow-ups**
   - If additional functionality is needed (multi-stream, format support, better resampling/mixing),
     implement it in the canonical crates and delete legacy code once unreferenced.

## Alternatives considered

1. **Keep the legacy `crates/emulator` stack as canonical**
   - Pros: already includes AC97 and alternate device models / DSP utilities.
   - Cons: not used by the browser/WASM runtime path; would require migrating `aero-wasm`
      and the web stack, increasing risk and effort.

2. **Keep both stacks active**
   - Pros: preserves optionality.
   - Cons: guarantees split effort and inconsistent behavior over time.

## Consequences

- The default build and documentation now have a single, canonical audio stack.
- The legacy audio implementation remains available behind `emulator/legacy-audio` for now.
- Targeted legacy tests/benches require opting in via:
  - `cargo test --locked -p emulator --features legacy-audio`
  - `cargo bench --locked -p emulator --features legacy-audio`
