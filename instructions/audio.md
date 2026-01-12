# Workstream G: Audio

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns **audio emulation**: HD Audio (HDA) controller, codec emulation, and the Web Audio API integration that plays sound in the browser.

Audio is important for user experience but not on the critical boot path.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-audio/` | Audio subsystem (HDA controller, codecs) |
| `web/src/audio/` | TypeScript AudioWorklet integration |

---

## Essential Documentation

**Must read:**

- [`docs/06-audio-subsystem.md`](../docs/06-audio-subsystem.md) — Audio architecture

**Reference:**

- [`docs/11-browser-apis.md`](../docs/11-browser-apis.md) — Web Audio API usage
- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture

---

## Tasks

### Audio Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| AU-001 | HD Audio controller emulation | P0 | None | Very High |
| AU-002 | HDA codec emulation | P0 | AU-001 | High |
| AU-003 | Sample format conversion | P0 | None | Medium |
| AU-004 | AudioWorklet integration | P0 | None | Medium |
| AU-005 | Audio buffering/latency management | P0 | AU-004 | Medium |
| AU-006 | AC'97 fallback (legacy) | P2 | None | High |
| AU-007 | Audio input (microphone) | P2 | AU-004 | Medium |
| AU-008 | Audio test suite | P0 | AU-001 | Medium |

**Status (AU-004 / AU-008)**

- **AU-004 (AudioWorklet integration)**: AudioWorklet + SharedArrayBuffer rings exist (`web/src/platform/audio.ts`, `web/src/platform/audio-worklet-processor.js`, `crates/platform/src/audio/worklet_bridge.rs`). The HDA model exists (`crates/aero-audio/src/hda.rs`), but the remaining work is wiring the guest device into the PCI/MMIO device stack in the worker runtime.
- **AU-008 (Audio test suite)**: E2E coverage exists for the AudioWorklet + HDA demo (`tests/e2e/audio-worklet-hda-demo.spec.ts`). Remaining work is adding coverage for the full guest-driven PCI/MMIO device path once it is integrated.

---

## Audio Architecture

### Data Flow

```
┌─────────────────────────────────────────────┐
│            Windows 7 Guest                   │
│                 │                            │
│         HD Audio Driver (hdaudio.sys)       │
├─────────────────┼───────────────────────────┤
│                 ▼                            │
│        HDA Controller Emulation             │  ← AU-001
│                 │                            │
│                 ▼                            │
│         HDA Codec Emulation                 │  ← AU-002
│                 │                            │
│                 ▼                            │
│        Sample Format Conversion             │  ← AU-003
│                 │                            │
└─────────────────┼───────────────────────────┘
                  │ SharedArrayBuffer ring buffer
                  ▼
┌─────────────────────────────────────────────┐
│            Browser                           │
│                 │                            │
│         AudioWorklet Processor              │  ← AU-004
│                 │                            │
│                 ▼                            │
│         Web Audio API                        │
│                 │                            │
│                 ▼                            │
│         System Audio Output                  │
└─────────────────────────────────────────────┘
```

### Ring Buffer

Audio samples flow through a SharedArrayBuffer ring buffer:

```
Producer (Emulator)                    Consumer (AudioWorklet)
       │                                      │
       ▼                                      │
  ┌─────────────────────────────────────────┐ │
  │ [samples] [samples] [samples] [empty]  │ │
  │     ↑                           ↑       │ │
  │   read_ptr                  write_ptr   │ │
  └─────────────────────────────────────────┘ │
       │                                      ▼
       │                               Pull 128/256 samples
       │                               per render quantum
       └───────────────────────────────────────
```

Key considerations:
- AudioWorklet runs on a separate high-priority thread
- Ring buffer must be lock-free (Atomics for pointers)
- Underflow handling (output silence, don't block)
- Sample rate conversion if needed (guest may use 44.1kHz, browser 48kHz)

---

## HD Audio Implementation Notes

Intel High Definition Audio is the standard Windows 7 audio controller. Key components:

1. **Controller Registers** — PCI BAR0 MMIO
2. **CORB/RIRB** — Command/Response ring buffers
3. **Stream Descriptors** — DMA buffer pointers
4. **Codec Nodes** — Audio widgets (DAC, ADC, mixer, etc.)

Windows 7 uses the inbox `hdaudio.sys` and `HdAudBus.sys` drivers.

Reference: Intel HDA specification (publicly available).

---

## AudioWorklet Integration

Canonical implementation:

- Output setup (main thread): `web/src/platform/audio.ts` (`createAudioOutput`)
- Ring consumer (AudioWorklet): `web/src/platform/audio-worklet-processor.js`
- Ring layout/constants + WASM producer bridge: `crates/platform/src/audio/worklet_bridge.rs`

```typescript
// Simplified: AudioWorklet consumer for the SAB playback ring.
//
// Notes:
// - Indices are monotonic *frame counters* (u32 wrapping at 2^32), not modulo indices.
// - Samples are interleaved f32: [L0, R0, L1, R1, ...].
// - The consumer (AudioWorklet) increments the underrun counter by the number of
//   missing frames it had to render as silence.
const READ_FRAME_INDEX = 0;
const WRITE_FRAME_INDEX = 1;
const UNDERRUN_COUNT_INDEX = 2;
const HEADER_U32_LEN = 4;
const HEADER_BYTES = HEADER_U32_LEN * Uint32Array.BYTES_PER_ELEMENT;

class AeroAudioProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const sab = options.processorOptions.ringBuffer;
    this.header = new Uint32Array(sab, 0, HEADER_U32_LEN);
    this.samples = new Float32Array(sab, HEADER_BYTES);
    this.channelCount = options.processorOptions.channelCount;
    this.capacityFrames = options.processorOptions.capacityFrames;
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    const framesNeeded = output[0].length;

    const read = Atomics.load(this.header, READ_FRAME_INDEX) >>> 0;
    const write = Atomics.load(this.header, WRITE_FRAME_INDEX) >>> 0;
    const available = Math.min((write - read) >>> 0, this.capacityFrames);
    const framesToRead = Math.min(framesNeeded, available);

    // Copy framesToRead frames from `this.samples` into `output` (wrap-around omitted).
    // Zero-fill any missing frames and Atomics.add(UNDERRUN_COUNT_INDEX, missingFrames).

    Atomics.store(this.header, READ_FRAME_INDEX, read + framesToRead);
    return true;
  }
}
```

---

## Sample Format Conversion

Windows may output various formats:
- 16-bit signed integer (common)
- 24-bit signed integer
- 32-bit signed integer
- 32-bit float

Web Audio API requires 32-bit float in [-1.0, 1.0]:

```rust
// 16-bit signed to float
fn s16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

// 32-bit signed to float
fn s32_to_f32(sample: i32) -> f32 {
    sample as f32 / 2147483648.0
}
```

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: HDA registers accessed via `CpuBus`
- **Integration (H)**: Controller wired into PCI bus

### What Other Workstreams Need From You

- Working audio for user experience testing
- System sounds for boot verification

---

## Testing

```bash
# Run audio tests
./scripts/safe-run.sh cargo test -p aero-audio --locked

# Manual testing
# Boot Windows 7 and play a sound file or system sound
```

Audio is hard to test automatically. Focus on:
- Controller initialization (no guest crash)
- Sample flow (ring buffer fills, doesn't overflow)
- Codec response to commands

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/06-audio-subsystem.md`](../docs/06-audio-subsystem.md)
4. ☐ Explore `crates/aero-audio/src/`
5. ☐ Run existing tests to establish baseline
6. ☐ Pick a task from the tables above and begin

---

*Audio brings the emulator to life. System sounds tell you it's working.*
