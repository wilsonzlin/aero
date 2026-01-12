# 16 - Trace-mode Profiling (Chrome Trace / Perfetto Export)

## Overview

Aero includes an **opt-in trace capture mode** that records:

- Nested span begin/end events
- Counter events (e.g. instructions, bytes, draw calls)
- Instant events for notable state transitions (boot milestones, tier switches)

The capture can be exported in **Chrome Trace Event** format and viewed in:

- `chrome://tracing`
- https://ui.perfetto.dev

This document focuses on trace capture/export. For the broader performance tooling workflows (Perf HUD, perf exports, benchmarks, CI perf jobs), see:

- [`docs/16-performance-tooling.md`](./16-performance-tooling.md)

## API

When running on the main thread, the profiler is exposed as:

```ts
window.aero.perf.traceStart();
window.aero.perf.traceStop();
const trace = await window.aero.perf.exportTrace(); // Chrome Trace JSON object
```

`exportTrace({ asString: true })` returns a JSON string.

For quick manual testing, `web/src/main.ts` also supports enabling tracing at boot via `?trace` in the URL.

## Workers / multi-thread timelines

To give each worker its own lane in the exported trace:

1) Register the worker on the main thread:

```ts
import { perf } from "./perf/perf";

const worker = new Worker(new URL("./workers/cpu.worker.ts", import.meta.url), { type: "module" });
perf.registerWorker(worker, { threadName: "cpu" });
```

`web/src/runtime/coordinator.ts` registers the default `cpu/gpu/io/jit` workers automatically.

2) Install trace handlers in the worker entrypoint:

```ts
import { installWorkerPerfHandlers } from "./perf/worker";

void installWorkerPerfHandlers();
```

## Bounded memory & dropped events

Trace recording uses fixed-size ring buffers. When buffers overflow, older events are overwritten and
the number of overwritten records is reported via `trace.otherData.aero.droppedRecordsByThread`.

## Minimal initial instrumentation

Current placeholder instrumentation points:

- Main thread: requestAnimationFrame present loops (`web/src/display/vga_presenter.ts`, plus any callers of `startFrameScheduler`)
- Main thread: `wasm:init` span for runtime WASM loader (`web/src/main.ts`)
- Workers: `wasm:init` + `worker:init` spans (`web/src/workers/*.worker.ts`, `web/src/workers/gpu-worker.ts`)

## Audio telemetry (ring buffer health)

When `startAudioPerfSampling()` is enabled for an `EnabledAudioOutput`, the trace will contain counter (`ph: "C"`) events with:

- `audio.bufferLevelFrames` — current queued frames in the output ring buffer
- `audio.underrunFrames` — total missing output frames rendered as silence due to underruns
- `audio.overrunFrames` — total frames dropped due to producer writes exceeding available capacity
- `audio.sampleRate` — AudioContext sample rate

These counters are what we typically want to capture during manual end-to-end audio validation (for example, the Windows 7
in-box HDA driver smoke test in [`docs/testing/audio-windows7.md`](./testing/audio-windows7.md)).

See:

- `web/src/platform/audio.ts` (`startAudioPerfSampling`, `getMetrics`)
- `web/src/main.ts` (demo UI auto-starts sampling when audio output is initialized and `window.aero.perf` is present)
