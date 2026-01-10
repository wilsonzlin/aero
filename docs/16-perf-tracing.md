# 16 - Trace-mode Profiling (Chrome Trace / Perfetto Export)

## Overview

Aero includes an **opt-in trace capture mode** that records:

- Nested span begin/end events
- Counter events (e.g. instructions, bytes, draw calls)
- Instant events for notable state transitions (boot milestones, tier switches)

The capture can be exported in **Chrome Trace Event** format and viewed in:

- `chrome://tracing`
- https://ui.perfetto.dev

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
- Workers: `wasm:init` + `worker:init` spans (`web/src/workers/*.worker.ts`, `web/src/workers/gpu-worker.ts`, `web/src/workers/aero-gpu-worker.ts`)
