# 06 - Audio Subsystem

## Overview

Windows 7 uses **Intel HD Audio (HDA)** as the primary audio interface. Aero emulates guest audio devices (HDA + virtio-snd) and bridges them to the browser via **Web Audio / AudioWorklet**.

## Manual Windows 7 smoke test (in-box HDA driver)

Once the HDA controller is wired into the real worker runtime, use the manual checklist at:

- [`docs/testing/audio-windows7.md`](./testing/audio-windows7.md)

It covers Win7 boot, Device Manager enumeration (“High Definition Audio Controller” + “High Definition Audio Device”),
playback/recording validation, and the host-side metrics to capture (AudioWorklet ring buffer level + underrun/overrun counters).

Canonical implementation pointers (to avoid duplicated stacks):

- `crates/aero-audio/src/hda.rs` — canonical HDA device model (playback + capture) + codec/PCM glue.
- `crates/aero-audio/src/hda_pci.rs` — canonical PCI function wrapper for the HDA model (config space + BAR0 MMIO).
- `crates/aero-virtio/src/devices/snd.rs` — canonical virtio-snd device model.
- `docs/windows7-virtio-driver-contract.md` — definitive Windows 7 virtio device/transport contract (`AERO-W7-VIRTIO`, includes virtio-snd).
- `crates/platform/src/audio/worklet_bridge.rs` — playback `SharedArrayBuffer` ring layout + producer-side helper (`WorkletBridge`).
- `web/src/platform/audio.ts` — Web Audio output setup + JS ring producer helpers.
- `web/src/platform/audio-worklet-processor.js` — AudioWorklet playback ring consumer.
- `crates/platform/src/audio/mic_bridge.rs` — microphone `SharedArrayBuffer` ring layout + consumer-side helper (`MicBridge`).
- `web/src/audio/mic_ring.js` + `web/src/audio/mic-worklet-processor.js` — microphone ring helpers + AudioWorklet producer.
- `web/src/runtime/protocol.ts` + `web/src/runtime/coordinator.ts` — ring buffer attachment messages (`SetAudioRingBufferMessage`, `SetMicrophoneRingBufferMessage`).
- `web/src/workers/io.worker.ts` + `web/src/io/*` — worker runtime PCI/MMIO device registration (IO worker owns the guest device model layer).

The older `crates/emulator` audio stack is retained behind the `emulator/legacy-audio` feature for reference and targeted tests.

---

## Audio architecture

At a high level, audio flows like:

- Guest → (HDA/virtio-snd DMA) → device model → playback ring → AudioWorklet → speakers
- Microphone → mic ring → device model → guest capture DMA

---

## Worker runtime integration

This section describes the *canonical* browser runtime integration.

### Ownership model

- The **IO worker** owns the guest-visible device layer (PCI/MMIO/virtio) and is the intended home for guest audio devices (**HDA** and **virtio-snd**).
- The **AudioWorkletProcessor** runs on the browser’s audio rendering thread.
- The **main thread** owns the browser audio graph (`AudioContext` + `AudioWorkletNode`), typically gated by user gesture.
- The **coordinator** forwards `SharedArrayBuffer` attachments to workers via `postMessage`.

### Playback ring attachment (AudioWorklet output)

1. UI calls `createAudioOutput` (`web/src/platform/audio.ts`), which:
   - allocates a playback `SharedArrayBuffer` ring,
   - loads `web/src/platform/audio-worklet-processor.js`,
   - constructs an `AudioWorkletNode` with `processorOptions.ringBuffer = sab`.
2. The coordinator forwards the ring to the worker that will act as the **single producer** via `SetAudioRingBufferMessage`
   (`web/src/runtime/protocol.ts`, policy + dispatch in `web/src/runtime/coordinator.ts`).
   - The coordinator owns the attachment policy (`RingBufferOwner`) because the playback ring is SPSC (exactly one producer).
   - Default policy: CPU worker in demo mode (no disk), IO worker when running a real VM (disk present).
     - See `WorkerCoordinator.defaultAudioRingBufferOwner()` + `syncAudioRingBufferAttachments()`.
   - Optional override (use with care): `WorkerCoordinator.setAudioRingBufferOwner("cpu" | "io" | "none")`.
     - `"both"` is intentionally rejected to preserve the SPSC contract (multi-producer access corrupts the ring indices).
   - `ringBuffer`: `SharedArrayBuffer | null` (null detaches)
   - `capacityFrames` / `channelCount`: out-of-band layout parameters
   - `dstSampleRate`: the *actual* `AudioContext.sampleRate`
3. The producer worker attaches the ring:
   - WASM: `WorkletBridge.fromSharedBuffer(...)` (Rust: `crates/platform/src/audio/worklet_bridge.rs`; JS bindings: `api.attach_worklet_bridge(...)`).

### Microphone ring attachment (AudioWorklet capture)

1. UI starts mic capture (`web/src/audio/mic_capture.ts`), which:
   - allocates a mic `SharedArrayBuffer` ring,
   - starts `web/src/audio/mic-worklet-processor.js` as the low-latency producer.
2. The coordinator forwards the mic ring via `SetMicrophoneRingBufferMessage`.
   - The coordinator owns the attachment policy (`RingBufferOwner`) because the mic ring is SPSC (exactly one consumer).
   - Default policy: CPU worker in demo mode, IO worker in VM mode.
     - See `WorkerCoordinator.defaultMicrophoneRingBufferOwner()` + `syncMicrophoneRingBufferAttachments()`.
   - Optional override (use with care): `WorkerCoordinator.setMicrophoneRingBufferOwner("cpu" | "io" | "none")`.
     - `"both"` is intentionally rejected to preserve the SPSC contract (multiple consumers will corrupt the read index / drop samples).
   - `ringBuffer`: `SharedArrayBuffer | null`
   - `sampleRate`: the *actual* capture graph sample rate
3. The consumer worker consumes mic samples via `MicBridge.fromSharedBuffer(...)` (`crates/platform/src/audio/mic_bridge.rs`).

### Device registration (PCI/MMIO)

Guest-visible devices are registered on the IO worker PCI bus:

- Bus/device plumbing: `web/src/io/device_manager.ts`, `web/src/io/bus/pci.ts`, `web/src/io/bus/mmio.ts`
- Worker wiring: `web/src/workers/io.worker.ts` (calls `DeviceManager.registerPciDevice(...)`)
  - See `web/src/io/devices/uhci.ts` for a concrete example of a WASM-backed PCI device wrapper (PIO + IRQ + tick scheduling).

### Ring producer/consumer constraints (SPSC)

Both rings are **SPSC** (single-producer / single-consumer). Only the “owner” thread should mutate the relevant indices/counters:

- **Playback ring** (`worklet_bridge.rs`, `audio-worklet-processor.js`)
  - Producer-owned fields: `writeFrameIndex`, `overrunCount`
  - Consumer-owned fields: `readFrameIndex`, `underrunCount`
- **Microphone ring** (`mic_bridge.rs`, `mic_ring.js`)
  - Producer-owned fields: `writePos`, `droppedSamples`, `capacitySamples`
  - Consumer-owned fields: `readPos`

Violating SPSC (e.g. having both JS and WASM write to the playback ring) will corrupt indices and cause audible glitches.

### Host-side telemetry (StatusIndex)

In addition to the raw ring header counters, the active producer worker publishes a few low-rate counters into the shared
status header (`web/src/runtime/shared_layout.ts`):

- `StatusIndex.AudioBufferLevelFrames`
- `StatusIndex.AudioUnderrunCount`
- `StatusIndex.AudioOverrunCount`

These are useful for perf HUDs / smoke tests without needing direct access to the underlying `SharedArrayBuffer` ring.

---

## Browser demo + CI coverage

To validate the *real* HDA DMA path end-to-end in the browser (guest PCM DMA → HDA model → `WorkletBridge` → AudioWorklet),
the repo includes a small demo harness:

- **UI button**: click `#init-audio-hda-demo` (“Init audio output (HDA demo)”) in either:
  - the repo-root harness (`src/main.ts`, used by Playwright at `http://127.0.0.1:4173/`), or
  - the production host (`web/src/main.ts`).
- **Implementation**:
  - The CPU worker (`web/src/workers/cpu.worker.ts`) instantiates the WASM export `HdaPlaybackDemo` and keeps the AudioWorklet ring buffer ~200ms full.
  - The demo programs a looping guest PCM buffer + BDL and uses the *real* HDA device model (`aero_audio::hda::HdaController`) to generate output.
- **E2E test**: `tests/e2e/audio-worklet-hda-demo.spec.ts` asserts that:
  - `AudioContext` reaches `running`,
  - the ring buffer write index advances over time,
  - underruns stay bounded and overruns remain 0.

Note: this demo is a *test harness* for the HDA audio pipeline; the production VM device stack is owned by the IO worker.

---

## Playback: AudioWorklet output ring

### Semantics

Playback uses a `SharedArrayBuffer` ring buffer consumed by an `AudioWorkletProcessor`.

- Indices are monotonic `u32` **frame counters** (wrap naturally at `2^32`) to avoid “read == write” ambiguity.
- Overrun/backpressure policy is **drop-new**:
  - the producer never advances the consumer-owned `readFrameIndex` to “make room”,
  - writes are truncated to the available free space,
  - dropped frames are counted in `overrunCount`.

Canonical semantics live in:

- `crates/platform/src/audio/worklet_bridge.rs` (WASM producer)
  - Re-exported from `crates/aero-audio/src/lib.rs` as `aero_audio::worklet_bridge`.
- `web/src/platform/audio.ts` (JS producer used by demos/fallbacks)
- `web/src/platform/audio-worklet-processor.js` (consumer)

### Playback ring buffer layout

Header (`Uint32Array`, little-endian) + payload (`Float32Array`):

| Byte offset | Type | Meaning |
|------------:|------|---------|
| 0           | u32  | `readFrameIndex` (monotonic frame counter, consumer-owned) |
| 4           | u32  | `writeFrameIndex` (monotonic frame counter, producer-owned) |
| 8           | u32  | `underrunCount` (total missing output frames rendered as silence; wraps at 2^32) |
| 12          | u32  | `overrunCount` (frames dropped by producer due to buffer full; wraps at 2^32) |
| 16..        | f32[]| Interleaved PCM samples (`L0, R0, L1, R1, ...`) |

Important: `capacityFrames` and `channelCount` are passed out-of-band (they are *not* stored in the header) and must match
both the producer and the AudioWorklet `processorOptions`.

### Sample rate mismatches

Browsers may ignore a requested `AudioContext` sample rate (Safari/iOS commonly runs at 44.1kHz). The AudioWorklet consumes
frames at `AudioContext.sampleRate`, so the device models must be configured to produce audio at that *actual* rate.

- HDA (`aero_audio::hda::HdaController`): set `output_rate_hz` to the output `AudioContext.sampleRate`.
- virtio-snd (`aero_virtio::devices::snd::VirtioSnd`): set `host_sample_rate_hz` to the output `AudioContext.sampleRate`.

---

## Capture: microphone ring

Microphone capture is bridged from the browser to the guest via a `SharedArrayBuffer` ring buffer:

- **Main thread**: requests permission on explicit user action and manages lifecycle/UI state (`web/src/audio/mic_capture.ts`).
- **AudioWorklet** (preferred): pulls mic PCM frames with low latency and writes them into the ring (`web/src/audio/mic-worklet-processor.js`).
- **IO worker**: reads from the ring and feeds the guest capture device (HDA input pin or virtio-snd capture stream).

Rust bridge helpers live in `crates/platform/src/audio/mic_bridge.rs` and are re-exported as `aero_audio::mic_bridge`
(via `crates/aero-audio/src/lib.rs`).

### Microphone ring buffer layout

The microphone ring buffer is a mono `Float32Array` backed by a `SharedArrayBuffer`:

| Byte offset | Type | Meaning |
|------------:|------|---------|
| 0           | u32  | `writePos` (monotonic sample counter) |
| 4           | u32  | `readPos` (monotonic sample counter) |
| 8           | u32  | `droppedSamples` (samples dropped due to buffer pressure; wraps at 2^32) |
| 12          | u32  | `capacitySamples` (samples in the data section; constant) |
| 16..        | f32[]| PCM samples, index = `(writePos % capacitySamples)` |

Backpressure policy for mic capture is **keep-latest** (drop the oldest part of the *current block* when partially writing)
so the guest sees the most recent microphone audio.

### HDA capture exposure (guest)

The canonical `aero-audio` HDA model exposes one capture stream and a microphone pin widget:

- Stream DMA: `SD1` (input stream 0) DMA-writes captured PCM bytes into guest memory via BDL entries.
- Codec topology: an input converter widget (`NID 4`) plus a mic pin widget (`NID 5`) so Windows can enumerate a recording endpoint.

Host code provides microphone samples via `aero_audio::capture::AudioCaptureSource` (implemented for
`aero_platform::audio::mic_bridge::MicBridge` on wasm) and advances the device model via the HDA capture processing path.

### virtio-snd capture exposure (guest)

Guest-visible virtio-snd behaviour (stream ids, queues, formats) is specified by the
[`AERO-W7-VIRTIO` contract](./windows7-virtio-driver-contract.md#34-virtio-snd-audio).

The canonical `aero-virtio` virtio-snd device model exposes an additional fixed-format capture stream:

- Stream id `1`, S16_LE mono @ 48kHz.
- Captured data is delivered to the guest via the virtio-snd RX queue (`VIRTIO_SND_QUEUE_RX`).

### Capture constraints (echo/noise)

Capture uses `getUserMedia` audio constraints to expose user-facing toggles:

- `echoCancellation`
- `noiseSuppression`
- `autoGainControl`

These are applied when the stream is created (and may be updated later via `MediaStreamTrack.applyConstraints` when supported).

---

## Snapshot/Restore (Save States)

Audio snapshots must capture guest-visible progress (DMA positions, buffer state) while treating the Web Audio pipeline as a
host resource that may need reinitialization.

### What must be captured

- Guest-visible device state (HDA registers, CORB/RIRB, stream descriptors, codec verb-visible state, DMA progress, etc).
- Host sample rates used by the device model (e.g. HDA `output_rate_hz` / `capture_sample_rate_hz`) so resampler state can be restored deterministically.
- Host-side ring **indices** (but not audio content):
  - playback ring `readFrameIndex` / `writeFrameIndex` + `capacityFrames`
  - helpers: `aero_platform::audio::worklet_bridge::{WorkletBridge, InterleavedRingBuffer}::snapshot_state()`

Implementation references:

- Snapshot schema: `crates/aero-io-snapshot/src/io/audio/state.rs` (`HdaControllerState`, `AudioWorkletRingState`).
- HDA device snapshot/restore: `crates/aero-audio/src/hda.rs` (`HdaController::snapshot_state` / `HdaController::restore_state`,
  behind the `io-snapshot` feature).
- Playback ring snapshot/restore: `crates/platform/src/audio/worklet_bridge.rs` (`WorkletBridge::snapshot_state` / `restore_state`).
- Roundtrip tests: `crates/aero-io-snapshot/tests/state_roundtrip.rs`.

### Restore semantics / limitations

- The browser `AudioContext` / `AudioWorkletNode` is not serializable; on restore the host audio graph is recreated.
- Ring buffer **contents** are not restored. Producers clear the ring to silence on restore to avoid replaying stale samples.
- Any host-time-derived audio clocks (e.g. `AudioFrameClock`-driven schedulers) must be reset on snapshot resume so devices do not
  "fast-forward" by wall-clock time spent paused during save/restore.
- The goal is *guest-visible determinism*: after restore, Windows should see consistent HDA/virtio-snd state and DMA position evolution.

---

## Latency management

- Ring buffer sizing defaults are derived from the *actual* `AudioContext.sampleRate`:
  - `web/src/platform/audio.ts`: `getDefaultRingBufferFrames()` defaults to ~200ms of capacity.
- Producers should generally target a smaller steady-state fill level (tens of ms) and adapt if underruns occur:
  - `web/src/platform/audio.ts`: `createAdaptiveRingBufferTarget()`.

---

## AC'97 fallback (legacy)

AC'97 is **not** part of the canonical Aero audio stack. The only AC'97 device model in this repository lives in the legacy
`crates/emulator` audio stack, which is gated behind `emulator/legacy-audio` (see ADR 0010).

---

## Next steps

- See [Networking](./07-networking.md) for network stack emulation.
- See [Browser APIs](./11-browser-apis.md) for Web Audio details.
- See [Task Breakdown](./15-agent-task-breakdown.md) for audio tasks.
