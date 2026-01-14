# 06 - Audio Subsystem

## Overview

Windows 7 uses **Intel HD Audio (HDA)** as the primary audio interface. Aero emulates guest audio devices (HDA + virtio-snd) and bridges them to the browser via **Web Audio / AudioWorklet**.

## Manual Windows 7 smoke test (in-box HDA driver)

To validate Windows 7 audio using the in-box HDA driver, use the manual checklist at:

- [`docs/testing/audio-windows7.md`](./testing/audio-windows7.md)

It covers Win7 boot, Device Manager enumeration (“High Definition Audio Controller” + “High Definition Audio Device”),
playback/recording validation, and the host-side metrics to capture (AudioWorklet ring buffer level + underrun/overrun counters).

Canonical implementation pointers (to avoid duplicated stacks):

- `crates/aero-audio/src/hda.rs` — canonical HDA device model (playback + capture) + codec/PCM glue.
- `crates/aero-audio/src/hda_pci.rs` — canonical PCI function wrapper for the HDA model (config space + BAR0 MMIO).
- `crates/aero-wasm/src/hda_controller_bridge.rs` — WASM-side bridge exported as `HdaControllerBridge` and used by the browser IO
  worker to expose HDA as a PCI/MMIO device (MMIO read/write, `process(frames)`, ring attachment).
- `crates/aero-virtio/src/devices/snd.rs` — canonical virtio-snd device model.
- `crates/aero-wasm/src/virtio_snd_pci_bridge.rs` — WASM-side bridge exported as `VirtioSndPciBridge` and used by the browser IO
  worker to expose virtio-snd as a virtio-pci device (BAR0 MMIO + `poll()`, ring attachment).
- `docs/windows7-virtio-driver-contract.md` — definitive Windows 7 virtio device/transport contract (`AERO-W7-VIRTIO`, includes virtio-snd).
- `crates/platform/src/audio/worklet_bridge.rs` — playback `SharedArrayBuffer` ring layout + producer-side helper (`WorkletBridge`).
- `web/src/platform/audio.ts` — Web Audio output setup + JS ring producer helpers.
- `web/src/platform/audio-worklet-processor.js` — AudioWorklet playback ring consumer.
- `web/src/audio/audio_frame_clock.ts` — deterministic time→audio-frame conversion helper used for device/worker tick scheduling (mirrors `crates/aero-audio/src/clock.rs`).
- `web/src/audio/audio_worklet_ring.ts` + `web/src/platform/audio_worklet_ring_layout.js` — playback ring header layout constants + helper math (shared between producers and the AudioWorklet consumer).
- `crates/platform/src/audio/mic_bridge.rs` — microphone `SharedArrayBuffer` ring layout + consumer-side helper (`MicBridge`).
- `web/src/audio/mic_ring.js` + `web/src/audio/mic-worklet-processor.js` — microphone ring helpers + AudioWorklet producer.
- `web/vite.config.ts` + `vite.harness.config.ts` — emit AudioWorklet dependency assets (`mic_ring.js`, `audio_worklet_ring_layout.js`) because Vite does not follow ESM imports from worklet modules loaded via `audioWorklet.addModule(new URL(...))`.
- `web/src/runtime/protocol.ts` + `web/src/runtime/coordinator.ts` — ring buffer attachment messages (`SetAudioRingBufferMessage`, `SetMicrophoneRingBufferMessage`).
- `web/src/workers/io.worker.ts` + `web/src/io/*` — worker runtime PCI/MMIO device registration (IO worker owns the guest device model layer).
  - `web/src/io/devices/hda.ts` — `HdaPciDevice` wrapper over `HdaControllerBridge` (MMIO + tick scheduling + ring attachment plumbing).
  - `web/src/io/devices/virtio_snd.ts` — `VirtioSndPciDevice` wrapper over `VirtioSndPciBridge` (virtio-pci BAR0 MMIO + ring attachment plumbing).
  - `web/src/workers/io_virtio_snd_init.ts` — IO worker virtio-snd init/registration helper.

The older `crates/emulator` audio stack is retained behind the `emulator/legacy-audio` feature for reference and targeted tests.

---

## Audio architecture

At a high level, audio flows like:

- Guest → (HDA/virtio-snd DMA) → device model → playback ring → AudioWorklet → speakers
- Microphone → mic ring → device model → guest capture DMA

---

## Worker runtime integration

This section describes the *canonical* browser runtime integration.

> Runtime note: the details below describe the legacy device-stack integration (`vmRuntime=legacy`), where guest audio devices
> live in the I/O worker. The canonical machine runtime (`vmRuntime=machine`) runs `api.Machine` in the CPU worker and does not
> currently expose guest audio devices via the I/O worker host stack.

### Ownership model

- The **IO worker** owns the guest-visible device layer (PCI/MMIO/virtio) and is the intended home for guest audio devices (**HDA** and **virtio-snd**).
  - Both devices can be registered in IO-worker browser builds (depending on which WASM exports are available):
    - HDA: `HdaControllerBridge` (`crates/aero-wasm/src/hda_controller_bridge.rs`) + `HdaPciDevice` (`web/src/io/devices/hda.ts`)
    - virtio-snd: `VirtioSndPciBridge` (`crates/aero-wasm/src/virtio_snd_pci_bridge.rs`) + `VirtioSndPciDevice` (`web/src/io/devices/virtio_snd.ts`)
  - **Ring attachment policy** (SPSC rings): the IO worker attaches the host audio rings to **HDA when present**, and falls back to
    attaching them to **virtio-snd** only when HDA is unavailable (see `attachAudioRingBuffer` / `attachMicRingBuffer` in
    `web/src/workers/io.worker.ts`).
- The **AudioWorkletProcessor** runs on the browser’s audio rendering thread.
- The **main thread** owns the browser audio graph (`AudioContext` + `AudioWorkletNode`), typically gated by user gesture.
- The **coordinator** forwards `SharedArrayBuffer` attachments to workers via `postMessage`.

### Playback ring attachment (AudioWorklet output)

1. UI calls `createAudioOutput` (`web/src/platform/audio.ts`), which:
    - allocates a playback `SharedArrayBuffer` ring,
    - loads `web/src/platform/audio-worklet-processor.js`,
    - constructs an `AudioContext` (`AudioContext`/`webkitAudioContext` fallback) and applies a few startup/latency-smoothing policies (see `createAudioOutput` options below),
    - constructs an `AudioWorkletNode` with `processorOptions.ringBuffer = sab`.
2. The coordinator forwards the ring to the worker that will act as the **single producer** via `SetAudioRingBufferMessage`
    (`web/src/runtime/protocol.ts`, policy + dispatch in `web/src/runtime/coordinator.ts`).
    - The coordinator owns the attachment policy (`RingBufferOwner`) because the playback ring is SPSC (exactly one producer).
   - Default policy: CPU worker in demo mode (no disk), IO worker when running a real VM (disk present).
     - See `WorkerCoordinator.defaultAudioRingBufferOwner()` + `syncAudioRingBufferAttachments()`.
   - Optional override (use with care): `WorkerCoordinator.setAudioRingBufferOwner("cpu" | "io" | "none" | null)`.
     - Use `null` to clear an override and return to the default policy.
     - Note: `RingBufferOwner` includes `"both"` for compatibility, but the coordinator intentionally rejects it (throws) because it
       violates the SPSC contract (multi-producer access corrupts the ring indices).
   - `ringBuffer`: `SharedArrayBuffer | null` (null detaches)
   - `capacityFrames` / `channelCount`: out-of-band layout parameters
   - `dstSampleRate`: the *actual* `AudioContext.sampleRate`
3. The producer worker attaches the ring:
   - Generic WASM producer: `WorkletBridge.fromSharedBuffer(...)` (Rust: `crates/platform/src/audio/worklet_bridge.rs`; WASM export: `api.attach_worklet_bridge(...)`).
   - IO worker HDA path: `HdaPciDevice.setAudioRingBuffer(...)` (`web/src/io/devices/hda.ts`) forwards to the WASM-side
     `HdaControllerBridge.attach_audio_ring(...)` + `set_output_rate_hz(...)` (`crates/aero-wasm/src/hda_controller_bridge.rs`).

### Microphone ring attachment (AudioWorklet capture)

1. UI starts mic capture (`web/src/audio/mic_capture.ts`), which:
   - allocates a mic `SharedArrayBuffer` ring,
   - starts `web/src/audio/mic-worklet-processor.js` as the low-latency producer.
2. The coordinator forwards the mic ring via `SetMicrophoneRingBufferMessage`.
   - The coordinator owns the attachment policy (`RingBufferOwner`) because the mic ring is SPSC (exactly one consumer).
   - Default policy:
     - `vmRuntime=legacy`: CPU worker in demo mode, IO worker in VM mode.
     - `vmRuntime=machine`: CPU worker (canonical Machine) in VM mode.
     - See `WorkerCoordinator.defaultMicrophoneRingBufferOwner()` + `syncMicrophoneRingBufferAttachments()`.
   - Optional override (use with care): `WorkerCoordinator.setMicrophoneRingBufferOwner("cpu" | "io" | "none" | null)`.
     - Use `null` to clear an override and return to the default policy.
     - Note: `RingBufferOwner` includes `"both"` for compatibility, but the coordinator intentionally rejects it (throws) because it
       violates the SPSC contract (multiple consumers would advance `readPos` and effectively double-consume/drop samples).
   - `ringBuffer`: `SharedArrayBuffer | null`
   - `sampleRate`: the *actual* capture graph sample rate
3. The consumer worker consumes mic samples via `MicBridge.fromSharedBuffer(...)` (`crates/platform/src/audio/mic_bridge.rs`).
   - IO worker HDA path: the IO worker forwards the attachment into the WASM-side `HdaControllerBridge` so it can consume samples
     via an internal `MicBridge` while running the guest capture DMA path.

### Device registration (PCI/MMIO)

In the legacy worker runtime (`vmRuntime=legacy`), guest-visible devices are registered on the IO worker PCI bus:

- Bus/device plumbing: `web/src/io/device_manager.ts`, `web/src/io/bus/pci.ts`, `web/src/io/bus/mmio.ts`
- Worker wiring (legacy runtime, `vmRuntime=legacy`): `web/src/workers/io.worker.ts` (calls `DeviceManager.registerPciDevice(...)`)
  - HDA PCI function wrapper: `web/src/io/devices/hda.ts` (`HdaPciDevice`, backed by `HdaControllerBridge`).
    - Registration entrypoint: `maybeInitHdaDevice()` in `web/src/workers/io.worker.ts`.
  - virtio-snd PCI function wrapper: `web/src/io/devices/virtio_snd.ts` (`VirtioSndPciDevice`, backed by `VirtioSndPciBridge`).
    - Registration entrypoint: `tryInitVirtioSndDevice()` in `web/src/workers/io_virtio_snd_init.ts` (invoked by `maybeInitVirtioSndDevice()` in `web/src/workers/io.worker.ts`).
  - See `web/src/io/devices/uhci.ts` for a concrete example of a WASM-backed PCI device wrapper (PIO + IRQ + tick scheduling).

Note: In `vmRuntime=machine`, guest audio devices live inside the canonical `api.Machine` runtime owned by
`web/src/workers/machine_cpu.worker.ts`; the IO worker runs in host-only mode and does not register guest PCI devices.

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
  - In `vmRuntime=legacy`, the CPU worker (`web/src/workers/cpu.worker.ts`) instantiates the WASM export `HdaPlaybackDemo` and keeps the AudioWorklet ring buffer ~200ms full.
  - The demo programs a looping guest PCM buffer + BDL and uses the *real* HDA device model (`aero_audio::hda::HdaController`) to generate output.
- **E2E test**: `tests/e2e/audio-worklet-hda-demo.spec.ts` asserts that:
   - `AudioContext` reaches `running`,
   - the ring buffer write index advances over time,
   - underruns stay bounded and overruns remain 0.

Note: this demo is a *test harness* for the HDA audio pipeline. In `vmRuntime=legacy`, the production VM device stack is
owned by the IO worker. In `vmRuntime=machine`, guest audio devices are owned by the Machine CPU worker and the IO worker runs
in host-only mode.

### End-to-end IO-worker HDA PCI/MMIO device path

Legacy runtime note: this section applies to `vmRuntime=legacy` (IO-worker-owned guest devices). In `vmRuntime=machine`,
guest devices are owned by the Machine CPU worker.

The CPU-worker HDA demo above is useful for validating the core HDA audio model + ring-buffer plumbing, but it does **not**
exercise the *real* worker runtime device stack (PCI config space, BAR0 MMIO, IO-worker-owned HDA PCI function).

To validate that full path, the repo-root harness exposes:

- **UI button**: `#init-audio-hda-pci-device` (“Init audio output (HDA PCI device)”) in `src/main.ts`
- **Implementation**:
  - The main thread allocates an AudioWorklet output ring buffer and attaches it to the **IO worker** via
    `WorkerCoordinator.setAudioRingBufferOwner("io")` + `setAudioRingBuffer(...)`.
  - The CPU worker programs the **IO worker's** HDA PCI function (8086:2668) using the real:
    - PCI config ports (0xCF8/0xCFC)
    - BAR0 MMIO reads/writes
  - The IO worker's WASM-backed `HdaPciDevice` then DMA-reads guest PCM and writes `f32` samples into the AudioWorklet ring.

Note: the CPU worker continuously publishes a shared framebuffer demo into guest RAM (see
`CPU_WORKER_DEMO_FRAMEBUFFER_OFFSET_BYTES` / `DEMO_FB_OFFSET`). Any audio harness that places guest-physical scratch buffers
(CORB/RIRB/BDL/PCM) in guest RAM must keep them disjoint from those regions (prefer allocating from the end of guest RAM),
otherwise the framebuffer publish loop will corrupt device state and cause flaky tests.
- **E2E test**: `tests/e2e/audio-hda-pci-snapshot-resume.spec.ts` asserts that:
  - ring read + write indices advance,
  - samples are **non-silent** (not just index movement),
  - underruns/overruns stay bounded, and
  - playback does not burst/fast-forward after snapshot restore.

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

### `createAudioOutput` options (latency vs robustness)

`createAudioOutput` (`web/src/platform/audio.ts`) is the canonical Web Audio output entrypoint. In addition to basic setup
(`sampleRate`, `latencyHint`, `ringBufferFrames`, etc.), it exposes a few tuning knobs to trade off:

- **Robustness** (tolerating IO worker stalls, slow startup, or browser suspensions), vs
- **Latency** (time from guest audio generation to speakers).

Key options (in addition to the existing `sampleRate`/`latencyHint`/`ringBufferFrames`/`ringBuffer`):

- `startupPrefillFrames?: number`
  - **What it does:** if the playback ring is empty at graph startup, pre-fills it with *silence* up to this many frames.
    (Default: `512` frames.)
    - Frames are **per-channel** (same unit as `ringBufferFrames`) and are clamped to the ring’s `capacityFrames`.
    - Avoid setting this too close to `ringBufferFrames` (capacity): a near-full ring leaves little headroom for real audio writes
      and can transiently increase backpressure/overrun counts at startup.
  - **Why it exists:** avoids an initial “startup underrun” window between `AudioWorkletNode` start and the first producer write,
    and gives slow-starting producers a small grace period.
  - **Trade-off:** increases time-to-first-audible-sample by roughly `startupPrefillFrames / AudioContext.sampleRate` seconds.
- `discardOnResume?: boolean`
  - **What it does:** when the `AudioContext` resumes after being suspended (tab backgrounding, iOS/Safari interruptions, etc.),
    discards any buffered playback frames by advancing the consumer `readFrameIndex` to the current `writeFrameIndex`
    (i.e. “flush the ring”).
    (Default: `true`.)
  - **Why it exists:** prevents *stale latency* where the VM kept producing while the browser was suspended; without discarding,
    the AudioWorklet would play back old buffered audio first, making the user hear audio seconds late after resume.
  - **Trade-off:** drops buffered frames across the suspension boundary (you get an audio discontinuity, but latency stays bounded).
  - **Implementation detail:** the discard happens on the **AudioWorklet consumer** (via a control message) so we do not violate
    the playback ring’s SPSC ownership rules (normally only the worklet advances `readFrameIndex`).
    - The discard is intentionally *not* applied to the **first** transition to `AudioContext.state === "running"` so that the
      startup silence prefill can still mask initial underruns.
    - If you need to flush explicitly (or you are in a browser that doesn’t reliably support `AudioContext` `statechange`
      listeners), `createAudioOutput` also exposes `audioOutput.discardBufferedFrames()` to manually request a ring reset.
    - Control message: `{ type: "ring.reset" }` posted to `AudioWorkletNode.port` (the worklet atomically applies
      `readFrameIndex := writeFrameIndex`).
  - **Worklet-side heuristic (optional):** the AudioWorkletProcessor also supports
    `processorOptions.discardOnResume === true` (default: `false`) to auto-reset the ring after detecting a large wall-clock gap
    between `process()` callbacks (a proxy for suspend/resume or extreme scheduling stalls). This is a best-effort self-healing
    mechanism for custom integrations that manually construct an `AudioWorkletNode`; `createAudioOutput` does **not** enable it.
  - **CI coverage:** `tests/e2e/audio-worklet-suspend-resume-discard.spec.ts` validates in a real Chromium browser that an
    `AudioContext` suspend/resume cycle triggers a prompt playback ring discard (prevents stale buffered playback after resume).

Related diagnostics knobs (not latency controls, but useful when tuning):

- `sendUnderrunMessages?: boolean` / `underrunMessageIntervalMs?: number`
  - When enabled, the AudioWorklet posts periodic `type: "underrun"` messages to the main thread for debugging/telemetry.
  - These are disabled by default because posting every render quantum can be expensive under persistent underrun.

#### AudioContext construction fallbacks (Safari/WebKit)

`createAudioOutput` must treat Web Audio as a runtime capability:

- It probes `globalThis.AudioContext`, and falls back to `globalThis.webkitAudioContext` when needed (Safari).
- Some WebKit/Safari variants are stricter about `AudioContext` constructor options; the implementation includes fallbacks so that
  “requested” settings (like `sampleRate`/`latencyHint`) do not hard-fail audio output initialization.
- Some browsers also differ in which `AudioWorkletNode` constructor options they accept; `createAudioOutput` retries node creation
  without `outputChannelCount` when needed for compatibility.
- Autoplay policies vary by browser: `createAudioOutput` makes a best-effort early `AudioContext.resume()`, but callers should
  still expect to call/await `audioOutput.resume()` from a user gesture handler (and may need to retry after a rejected resume).
- Always treat `audioOutput.context.sampleRate` as authoritative (Safari/iOS may ignore the requested sample rate).

#### Quick tuning summary

| Knob | Default | Primary trade-off | When to change |
|------|---------|-------------------|----------------|
| `startupPrefillFrames` | `512` | higher value ⇒ more startup robustness, more time-to-first-sound | VM mode / slow-starting producers |
| `discardOnResume` | `true` | `true` ⇒ drops buffered audio across suspend/resume to keep latency bounded | Almost always keep `true` for interactive UX |
| `ringBufferFrames` | ~200ms capacity (derived from actual sample rate) | larger ring ⇒ fewer underruns, higher worst-case latency | VM mode / IO-worker stalls |
| `latencyHint` | `"interactive"` | `"playback"`/`"balanced"` can increase buffering and stability | if underruns persist even with larger rings |

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

#### Attach/resume semantics (stale latency avoidance)

Microphone input is a host resource and is **not serialized in VM snapshots**. The AudioWorklet producer may continue writing
into the ring while the VM is snapshot-paused, or before the guest capture device has attached as the ring consumer.

To avoid replaying *stale* mic samples (which would manifest as large capture latency after resume), consumers discard any
already-buffered samples by advancing `readPos` to the current `writePos` (i.e. `readPos := writePos`) on:

- ring attachment (late attach while the producer is already running)
- snapshot resume / other pause boundaries where the producer may have continued writing while the VM was stopped

### HDA capture exposure (guest)

The canonical `aero-audio` HDA model exposes one capture stream and a microphone pin widget:

- Stream DMA: `SD1` (input stream 0) DMA-writes captured PCM bytes into guest memory via BDL entries.
- Codec topology: an input converter widget (`NID 4`) plus a mic pin widget (`NID 5`) so Windows can enumerate a recording endpoint.

Host code provides microphone samples via `aero_audio::capture::AudioCaptureSource` (implemented for
`aero_platform::audio::mic_bridge::MicBridge` on wasm) and advances the device model via the HDA capture processing path.

### HDA pin/power gating semantics (playback + capture)

The minimal HDA codec model enforces a subset of widget **power state** + **pin widget control** semantics that Windows uses to
mute endpoints. This primarily affects whether the guest hears audio / records audio, while keeping DMA timing consistent.

#### Playback gating (line-out)

Playback is forced to **silence** when any of the following are true:

- AFG power state (`NID 1`) is not `D0`
- output pin (`NID 3`) `pin_ctl == 0`
- output pin (`NID 3`) power state is not `D0`

This is implemented by applying 0 gain at the codec output stage (guest playback DMA still advances as normal).

Unit tests:

- [`crates/aero-audio/tests/hda_volume_mute.rs`](../crates/aero-audio/tests/hda_volume_mute.rs)
  (`afg_power_state_d3_silences_output`, `pin_ctl_zero_silences_output`, `output_pin_power_state_d3_silences_output`)

#### Capture gating (microphone)

The capture stream DMA engine still advances, but the guest receives **silence** when any of the following are true:

- AFG power state (`NID 1`) is not `D0`
- mic pin (`NID 5`) power state is not `D0`
- mic pin (`NID 5`) `pin_ctl == 0`

In the capture-muted cases above, the device model must **not** consume microphone samples from the host ring
(`AudioCaptureSource`). (This avoids dropping mic audio while the guest endpoint is disabled.)

Unit tests:

- [`crates/aero-audio/tests/hda_capture_pin_gating.rs`](../crates/aero-audio/tests/hda_capture_pin_gating.rs)
  (`capture_pin_ctl_zero_writes_silence_without_consuming`, `capture_pin_power_state_d3_writes_silence_without_consuming`,
  `capture_afg_power_state_d3_writes_silence_without_consuming`)

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

### Outer snapshot identity

In the outer `aero-snapshot` `DEVICES` table, HDA audio state is stored under:

- `DeviceId::HDA` (`18`)
- JS kind string: `"audio.hda"` (worker snapshot glue)

`DeviceState.data` is an `aero-io-snapshot` TLV blob (`DEVICE_ID = HDA0`) and `DeviceState.version/flags` mirror the inner
`SnapshotVersion (major, minor)`.

### What must be captured

- Guest-visible device state (HDA registers, CORB/RIRB, stream descriptors, codec verb-visible state, DMA progress, etc).
  - Includes codec pin/power gating state (AFG `power_state`, pin widget `pin_ctl`/`power_state`), which controls whether
    playback/capture are silenced and (for capture) whether the device model consumes host microphone samples.
- Host sample rates used by the device model (e.g. HDA `output_rate_hz` / `capture_sample_rate_hz`) so resampler state can be restored deterministically.
- Host-side ring **indices** (but not audio content):
   - playback ring `readFrameIndex` / `writeFrameIndex` + `capacityFrames`
   - helpers:
      - `WorkletBridge::snapshot_state()` / `WorkletBridge::restore_state()` (wasm; the real SAB-backed ring)
     - `InterleavedRingBuffer::snapshot_state()` (pure Rust helper used by unit tests)
     - `web/src/platform/audio_ring_restore.ts`: `restoreAudioWorkletRing()` (JS helper used when restoring rings on the web side)
   - Note: ring underrun/overrun counters are host-side telemetry and are not part of the snapshot; restore intentionally leaves
     them untouched (and they may reset if the ring is recreated).

Implementation references:

- Snapshot schema: `crates/aero-io-snapshot/src/io/audio/state.rs` (`HdaControllerState`, `AudioWorkletRingState`).
- HDA device snapshot/restore: `crates/aero-audio/src/hda.rs` (`HdaController::snapshot_state` / `HdaController::restore_state`,
  behind the `io-snapshot` feature).
- Playback ring snapshot/restore: `crates/platform/src/audio/worklet_bridge.rs` (`WorkletBridge::snapshot_state` / `restore_state`).
- Roundtrip tests: `crates/aero-io-snapshot/tests/state_roundtrip.rs`.

### Restore semantics / limitations

- The browser `AudioContext` / `AudioWorkletNode` is not serializable; on restore the host audio graph is recreated.
- Ring buffer **contents** are not restored. Producers clear the ring to silence on restore to avoid replaying stale samples.
- Host microphone rings are not serializable and may continue producing while the VM is paused; capture consumers are expected to
  discard stale data on attach/resume boundaries. If the guest capture endpoint is gated (pin/power state), the HDA model will DMA
  silence without consuming any host mic samples until the endpoint is re-enabled.
- Any host-time-derived audio clocks (e.g. `AudioFrameClock`-driven schedulers) must be reset on snapshot resume so devices do not
  "fast-forward" by wall-clock time spent paused during save/restore.
- virtio-snd snapshot/restore is supported in the browser runtime under kind `"audio.virtio_snd"` (`DeviceId::VIRTIO_SND = 22`).
  The snapshot captures virtio-pci transport state + virtio-snd stream state + AudioWorklet ring indices, but not host audio
  contents (rings are cleared to silence on restore).
- The goal is *guest-visible determinism*: after restore, Windows should see consistent HDA state and virtio-snd state (when
  present) and DMA position evolution.

---

## Latency management

- Ring buffer sizing defaults are derived from the *actual* `AudioContext.sampleRate`:
  - `web/src/platform/audio.ts`: `getDefaultRingBufferFrames()` defaults to ~200ms of capacity.
- Producers should generally target a smaller steady-state fill level (tens of ms) and adapt if underruns occur:
  - `web/src/platform/audio.ts`: `createAdaptiveRingBufferTarget()`.

### Measuring/estimating latency

For rough tuning, you can break “audio latency” into:

- **Ring-buffer backlog** (producer → AudioWorklet): approximately
  `bufferLevelFrames / AudioContext.sampleRate` seconds.
- **Browser output pipeline latency** (AudioContext → speakers): best-effort fields exposed by some browsers:
  - `AudioContext.baseLatency` — estimated base latency of the audio graph (seconds)
  - `AudioContext.outputLatency` — estimated latency from the audio graph to the audio output device (seconds)

These `AudioContext` values are **browser/OS dependent** and may be missing. Aero exposes them (when available) via
`AudioOutputMetrics` returned from `audioOutput.getMetrics()`:

- `baseLatencySeconds`
- `outputLatencySeconds`

and also emits them as trace counters (`audio.baseLatencySeconds` / `audio.outputLatencySeconds`) when
`startAudioPerfSampling()` is enabled.

When available, a rough “total” playback latency estimate is:

```
totalSeconds ≈ (bufferLevelFrames / sampleRate) + baseLatencySeconds + outputLatencySeconds
```

Treat this as an **approximation**: browser scheduling, device buffering, and platform policies vary, and not all browsers expose
`outputLatency`.

### Recommended defaults (demo mode vs VM mode)

Audio tuning is inherently workload-dependent, but the following presets are good starting points:

- **Demo mode** (repo harness / interactive demos; producer is typically the CPU worker):
  - keep `startupPrefillFrames` small to minimize “click → sound” delay:
    - `startupPrefillFrames: 0..512` (default is `512`, ~10.7ms @ 48kHz).
  - prefer `latencyHint: "interactive"` and a smaller steady-state buffer target.
  - `discardOnResume: true` is still recommended if you care about interactive latency after tab/background resumes.
- **VM mode** (real guest VM; producer is typically the IO worker and may stall on host IO/GC):
  - use a larger `startupPrefillFrames` so initial device init / IO-worker stalls don’t immediately underrun:
    - `startupPrefillFrames: 2048..4096` (~43–85ms @ 48kHz) is a reasonable starting range if you see cold-start underruns.
  - consider a less aggressive `latencyHint` (e.g. `"balanced"`/`"playback"`) and/or a larger ring capacity if you see frequent underruns:
    - default `ringBufferFrames` is ~200ms capacity (`sampleRate / 5`), but VM mode can justify ~400–500ms capacity
      if the IO worker frequently stalls.
  - enable `discardOnResume` to avoid resuming with a large backlog of buffered guest audio (stale latency).

Concrete examples:

```ts
// Demo mode: low startup latency, still avoids stale latency after tab resumes.
await createAudioOutput({
  latencyHint: "interactive",
  startupPrefillFrames: 0, // or 512 (default)
  discardOnResume: true,
});

// VM mode: tolerate IO-worker stalls (higher robustness), at the cost of more buffering.
await createAudioOutput({
  latencyHint: "balanced",
  startupPrefillFrames: 4096,
  discardOnResume: true,
});
```

---

## AC'97 fallback (legacy)

AC'97 is **not** part of the canonical Aero audio stack. The only AC'97 device model in this repository lives in the legacy
`crates/emulator` audio stack, which is gated behind `emulator/legacy-audio` (see ADR 0010).

---

## Next steps

- See [Networking](./07-networking.md) for network stack emulation.
- See [Browser APIs](./11-browser-apis.md) for Web Audio details.
- See [Task Breakdown](./15-agent-task-breakdown.md) for audio tasks.
