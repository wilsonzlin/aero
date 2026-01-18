# 16 - Performance Tooling (Profiling, HUD, Traces, Benchmarks)

## Overview

Aero ships with **first-party performance tooling** that contributors should use for:

- **Interactive profiling** while developing locally (Perf HUD)
- **Bug reports / PR evidence** (JSON exports + traces)
- **Regression detection** (benchmarks + baselines in CI)

This doc describes the expected workflows and how to interpret the outputs.

---

## Perf tooling unit tests

Perf tooling source lives in `web/src/perf/`, and its unit tests live alongside the implementation as `web/src/perf/**/*.test.ts`.

These tests are executed in CI by the **root** Vitest suite. To run them locally (including coverage):

```bash
npm run test:unit:coverage
```

If a perf unit test needs DOM APIs (e.g. HUD tests), opt into `jsdom` per-file:

```ts
// @vitest-environment jsdom
```

## Perf HUD

The Perf HUD is a lightweight overlay that shows a rolling snapshot of key emulator metrics while the system is running.

### Toggling the HUD

- **In-app UI:** Click the **Perf HUD** button in the developer menu.
- **Keyboard shortcut:** `F2` or `Ctrl+Shift+P` (when focus is *not* in a text input).

If you don’t see the toggle, make sure you’re running a build that includes the developer menu / HUD.

### Update frequency / sampling window

To avoid the HUD itself becoming a source of overhead:

- The HUD **refreshes at a fixed rate** (currently **5 Hz**, i.e. every **200ms**) while visible.
- Most values are computed over a **rolling window** (a fixed-size ring buffer of recent samples).

### Metrics (what they mean)

The exact fields may evolve, but contributors should expect the HUD to include:

| Metric | Meaning | How to use it |
| --- | --- | --- |
| **FPS (avg / 1% low)** | Average FPS plus “1% low” FPS (tail latency) | Tail drops are often more important than averages for “jank” |
| **Frame time (avg / p95)** | Mean and p95 frame time | Compare to 16.7ms (60Hz) / 33.3ms (30Hz); p95 captures stutters |
| **MIPS (avg / p95)** | Estimated guest throughput (million instructions/s), reported as avg / p95 | Useful for CPU/JIT regressions independent of rendering; p95 captures stutters |
| **CPU / GPU / IO / JIT (avg)** | Accounted time buckets (ms) | Use to attribute slow frames; buckets may not sum to frame time if work overlaps |
| **Draw calls (avg/frame)** | Approximate rendering work submitted | Spikes often correlate with state churn or missed batching |
| **IO throughput** | Bytes/sec of emulated I/O | Correlate stutters with disk/network activity |
| **Host heap** | JS heap used/total | Watch for leaks and GC-triggering spikes |
| **Guest RAM** | Guest memory size (if known) | Helps validate test configurations |
| **Capture** | Recording state + sample counts | Use Start/Stop to capture a window and Download to export it |

Interpretation tip: **one bad frame is not a regression**. Look for sustained changes in the rolling averages and for increases in long-frame counts.

### Capture controls (Start / Stop / Reset / Download)

The HUD includes basic capture tooling:

- **Start / Stop:** begin/end a capture window (aim for ~5–15s around the issue).
- **Reset:** clears the current capture buffer.
- **Download:** downloads the current `window.aero.perf.export()` payload as JSON.

---

## Capturing and downloading JSON exports

JSON exports are meant to be attached to issues/PRs. They should include:

- Build metadata (commit SHA, build mode, feature flags)
- Environment metadata (browser version, OS, device info)
- Aggregated counters and histograms for the current run

### Capturing a window of data

`window.aero.perf.export()` exports the **most recent capture buffer**. To collect a useful per-frame trace:

- Start a capture
- Reproduce the workload for ~5–15 seconds
- Stop the capture
- Export / download

You can do this either:

- via the Perf HUD buttons (**Start**, **Stop**, then **Download**), or
- via the console API:

```js
window.aero.perf.captureStart();
// Reproduce the workload for ~5–15 seconds.
window.aero.perf.captureStop();
const data = window.aero.perf.export();
```

### Export from the DevTools console

Run this in the page console:

```js
const data = window.aero.perf.export();
```

Expected behavior:

- `export()` returns a plain JSON-serializable object.
- In builds that include the Perf HUD, clicking **Download** triggers a JSON download using this same payload.

### Manual download (if your build doesn’t auto-download)

```js
const data = window.aero.perf.export();
const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
const a = document.createElement("a");
a.href = URL.createObjectURL(blob);
a.download = `aero-perf-${new Date().toISOString()}.json`;
a.click();
```

### Perf export schema (canonical, v2)

`window.aero.perf.export()` returns a **versioned**, **JSON-serializable** object with:

- `kind: "aero-perf-capture"`
- `version: 2`

Top-level fields (v2):

- `build`: build metadata (`git_sha`, `mode`, and optional `features` when available)
- `env`: environment metadata (`now_epoch_ms`, `userAgent`, `platform`, `hardwareConcurrency`, `devicePixelRatio`, `webgpu`)
- `capture`: capture timing (`startUnixMs`, `endUnixMs`, `durationMs`)
- `capture_control`: capture bounds/counters (`startFrameId`, `endFrameId`, `droppedRecords`, `records`)
- `summary`: `{ frameTime, mipsAvg }` (frame-time summary + MIPS average)
- `frameTime`: `{ summary, stats }` (histogram payload; `stats` is `FrameTimeStats.toJSON()`)
- `records[]`: per-frame samples:
  - `tMs`: milliseconds since `captureStart()`
  - `frameTimeMs`
  - `instructions` (number when safe, otherwise string; `null` if unavailable)
  - `cpuMs`, `gpuMs`, `ioMs`, `jitMs`, `drawCalls`, `ioBytes` (nullable)
- `memory`: memory telemetry (`MemoryTelemetryExport`)
- `responsiveness`: input/long-task telemetry (`ResponsivenessExport`)
- `jit`: JIT telemetry snapshot (currently a placeholder in most builds, but always present)
- optional: `benchmarks` (attached by `web/src/aero.ts` when bench suites are enabled)

The JSON schema used by CI and tooling lives at:

- `bench/schema/perf-output.schema.json`

CI perf workflows validate `perf_export.json` artifacts against this schema (using `tools/perf/validate_perf_export.mjs`) to catch breaking changes early.

---

## Trace mode (timeline capture)

Traces answer questions like:

- “What exactly was the main thread doing during a stutter?”
- “Are we blocked on GPU submission, I/O, or cross-worker synchronization?”
- “Is JIT compilation happening on the hot path?”

Aero traces should be emitted in **Chrome Trace Event** JSON format so they can be opened in:

- **Perfetto UI:** https://ui.perfetto.dev/
- **Chrome tracing:** `chrome://tracing`

### Capturing a trace

In the DevTools console:

```js
window.aero.perf.traceStart();
// Reproduce the problem for ~5-15 seconds.
window.aero.perf.traceStop();
const trace = await window.aero.perf.exportTrace();
```

To download the trace as a file:

```js
const json = await window.aero.perf.exportTrace({ asString: true });
const blob = new Blob([typeof json === "string" ? json : JSON.stringify(json)], { type: "application/json" });
const a = document.createElement("a");
a.href = URL.createObjectURL(blob);
a.download = `aero-trace-${new Date().toISOString()}.json`;
a.click();
```

In builds that include the Perf HUD, the **Trace JSON** button performs this same export asynchronously via `exportTrace({ asString: true })`.
Large traces can take a moment to serialize (events are gathered from multiple workers), so the HUD disables the button while the export is in-flight.

Notes:

- Keep traces short (seconds, not minutes). Long traces are hard to analyze and expensive to record.
- Prefer capturing a trace immediately after a cold start if you’re investigating startup costs (shader compilation, caches).
- Trace recording uses bounded ring buffers; if you capture too long, you may see dropped events under `trace.otherData.aero.droppedRecordsByThread`.

For more details on trace instrumentation, worker lanes, and dropped-event accounting, see:

- [`docs/16-perf-tracing.md`](./16-perf-tracing.md)

### Opening a trace in Perfetto

1. Save the trace JSON returned by `exportTrace()` to a file.
2. Open https://ui.perfetto.dev/
3. Drag-and-drop the JSON file into the UI.

### Opening a trace in `chrome://tracing`

1. Open `chrome://tracing` in a Chromium-based browser.
2. Click **Load**.
3. Select the trace JSON file you saved from `exportTrace()`.

### Capturing a trace from startup

Some builds support starting tracing automatically via URL param:

- `http://127.0.0.1:4173/?trace`

This is useful when investigating early startup costs (shader compilation, worker startup, cache warmup).

### Interpreting lanes / tracks

Contributors should expect traces to include separate tracks for:

- **Main thread:** UI, input routing, canvas presentation, coordination
- **CPU worker:** guest instruction execution, translation/JIT, interrupts
- **GPU worker:** command encoding, texture/shader management
- **I/O worker:** storage reads/writes, network packets, audio buffers

Common patterns:

- **Wide slices on the main thread** often indicate rendering/UI work or accidental synchronous waits.
- **Periodic “sawtooth” patterns** in memory/GC regions often indicate allocation churn.
- **Gaps on the CPU worker** with activity elsewhere suggest the emulator is waiting (sync, I/O, frame pacing).

---

## Running benchmarks locally

Benchmarks are the **repeatable** way to validate performance changes. They should:

- Use a production-like build
- Run multiple iterations
- Warn when results are noisy

Related references:

- CI browser perf runner: [`tools/perf/README.md`](../tools/perf/README.md)
- Bench harness + GPU scenarios: [`bench/README.md`](../bench/README.md)

### Build + serve (production-like)

If you want to benchmark the *app* (not just synthetic microbenches), run against a built preview server.

```bash
npm ci

# Production bundle + preview server
npm run build
npm run preview
```

Note: the default preview server (`web/index.html` → `web/src/main.ts`) installs the Perf HUD and exposes
`window.aero.perf.captureStart/captureStop/export()` (and trace APIs). CI's Playwright perf runner
(`tools/perf/run.mjs`) relies on this API to write `perf_export.json`.

For perf-sensitive work (SharedArrayBuffer / threads / COOP+COEP), prefer the COI preview helper:

```bash
npm run serve:coi
```

### Run the Playwright benchmark runner (CI parity)

The CI browser perf harness lives under `tools/perf/` and can be run locally.

One-time setup:

```bash
npm ci
node scripts/playwright_install.mjs chromium
```

On minimal Linux environments, you may need system dependencies:

```bash
node scripts/playwright_install.mjs chromium --with-deps
```

CI-parity run (build + `vite preview` + perf harness, matching `.github/workflows/perf*.yml`):

```bash
node scripts/ci/run_browser_perf.mjs --preview --out-dir perf-results/local --iterations 7
```

Quick run (no build; uses the perf harness' internal `data:` URL):

```bash
npm run bench:browser -- --iterations 7 --out-dir perf-results/local
```

Capture an Aero trace alongside the run (best-effort; requires the page to expose `window.aero.perf.traceStart/traceStop/exportTrace`):

```bash
node tools/perf/run.mjs --trace --out-dir perf-results/local --iterations 7 --url http://127.0.0.1:4173/
```

Capture a fixed-duration trace window (useful when you want a longer trace than a single benchmark run):

```bash
node tools/perf/run.mjs --trace-duration-ms 5000 --out-dir perf-results/local --iterations 7 --url http://127.0.0.1:4173/
```

Include app-provided microbenches (best-effort; requires `window.aero.bench.runMicrobenchSuite`):

```bash
node tools/perf/run.mjs --include-aero-bench --out-dir perf-results/local --iterations 7 --url http://127.0.0.1:4173/
```

What to expect:

- Results are written to `perf-results/` (raw samples + `summary.json`).
- Summaries include **median-of-N** numbers and a **CV** (coefficient of variation) to help spot noise.

To benchmark a specific URL (e.g. your local preview server), pass `--url`:

```bash
npm run bench:browser -- --iterations 7 --out-dir perf-results/local --url http://127.0.0.1:4173/
```

### Run the GPU benchmark scenarios (Playwright + WebGPU/WebGL2)

The GPU bench runner executes scenario scripts in a real browser and emits a JSON report:

```bash
npm run bench:gpu -- --scenarios vga_text_scroll,vbe_lfb_blit --iterations 7 --headless false --output gpu_bench.json
```

Compare two GPU bench reports (baseline vs candidate) and emit a Markdown report (plus machine-readable JSON):

```bash
node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs scripts/compare_gpu_benchmarks.ts \
  --baseline gpu_bench_base.json \
  --candidate gpu_bench_head.json \
  --out-dir gpu_bench_compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

### Run + compare the storage I/O benchmark suite (OPFS + IndexedDB)

The storage bench is a Playwright-driven macrobench (`bench/runner.ts storage_io`) that writes a raw `storage_bench.json`.

Note: this suite measures host-side OPFS/IndexedDB throughput. IndexedDB is async-only and does not
currently back the synchronous Rust disk/controller path (`aero_storage::{StorageBackend, VirtualDisk}`).
See: [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and
[`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

To reproduce the CI compare locally:

```bash
npm run bench:storage -- --out-dir storage-perf-results/base
npm run bench:storage -- --out-dir storage-perf-results/head

npm run compare:storage -- \
  --baseline storage-perf-results/base/storage_bench.json \
  --candidate storage-perf-results/head/storage_bench.json \
  --out-dir storage-perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

Outputs:
- `storage-perf-results/compare/compare.md`
- `storage-perf-results/compare/summary.json` (machine-readable)

### Run + compare the gateway benchmark suite (backend networking)

The gateway bench (`backend/aero-gateway/bench/run.mjs`) runs loopback-only benchmarks for Aero's backend networking paths:

- TCP proxy RTT (p50/p90/p99)
- TCP proxy throughput (MiB/s)
- DoH QPS + cache hit ratio

It does **not** require Playwright (pure Node + local sockets), but it does require the gateway build artifacts in `backend/aero-gateway/dist/`:

```bash
npm -w backend/aero-gateway run build

node backend/aero-gateway/bench/run.mjs --mode smoke --json gateway-perf-results/base/raw.json
node backend/aero-gateway/bench/run.mjs --mode smoke --json gateway-perf-results/head/raw.json

node --experimental-strip-types --import ./scripts/register-ts-strip-loader.mjs scripts/compare_gateway_benchmarks.ts \
  --baseline gateway-perf-results/base/raw.json \
  --candidate gateway-perf-results/head/raw.json \
  --out-dir gateway-perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

Outputs:
- `gateway-perf-results/compare/compare.md`
- `gateway-perf-results/compare/summary.json` (machine-readable)

### Interpreting summary output and variance warnings

When the summary shows a high **CV** (coefficient of variation):

- **Re-run** with more iterations (e.g. 15+).
- Ensure your machine is not thermally throttling and nothing heavy is running in the background.
- Prefer the same browser version you use for baselines.

Treat small single-digit % changes as suspicious unless variance is low and the change reproduces across runs.

To reproduce the CI comparison locally (baseline vs candidate):

```bash
node tools/perf/run.mjs --out-dir perf-results/base --iterations 7
node tools/perf/run.mjs --out-dir perf-results/head --iterations 7

node tools/perf/compare.mjs \
  --baseline perf-results/base/summary.json \
  --candidate perf-results/head/summary.json \
  --out-dir perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

---

## Updating baselines and thresholds

Baselines are used to detect regressions in CI. Update them when:

- You intentionally improve performance (so CI stops failing once the improvement lands)
- A known, unavoidable regression is accepted (with explicit acknowledgement)
- The baseline environment changes (e.g. major browser version change)

In CI:

- The **browser CI perf** workflow (`tools/perf`) compares **PR vs base commit** (no committed “golden baseline” file).
- Thresholds live in [`bench/perf_thresholds.json`](../bench/perf_thresholds.json) (versioned, shared across CI perf tooling: browser/GPU/storage/gateway/Node microbench/Criterion).
  - PR gating uses profile `pr-smoke`
  - Nightly runs should use profile `nightly`

Separately, PF-009 adds a **checked-in baseline** file (`bench/baseline.json`) and a Node microbench compare tool (`node bench/compare`).
It uses the same threshold policy file (`bench/perf_thresholds.json`) under the `node` suite.

### Updating the PF-009 baseline (`bench/baseline.json`)

Use the baseline updater script:

```bash
npm run bench:update-baseline -- --scenario all --iterations 15
```

This re-runs the lightweight Node microbench (`bench/run.js`) and updates `bench/baseline.json` in-place, printing a
before/after summary table to make PR review easier.

Sanity check the updated baseline:

```bash
npm run bench:node
node bench/compare --fail-on-regression --json
```

Guidelines:

- Update baselines on a **quiet, stable machine**.
- Include the **before/after summary** in the PR description.
- Avoid “baseline churn”: if results are noisy, fix the source of noise first.

---

## CI perf jobs

CI performance jobs are split by intent:

- **PR perf (gating):** [`/.github/workflows/perf.yml`](../.github/workflows/perf.yml) runs a small browser-only suite and compares PR vs base commit.
- **Nightly perf (non-gating + data collection):** [`/.github/workflows/perf-nightly.yml`](../.github/workflows/perf-nightly.yml) runs more iterations and publishes history/dashboard artifacts.
- **PR GPU perf (gating):** [`/.github/workflows/gpu-perf.yml`](../.github/workflows/gpu-perf.yml) runs a small GPU scenario set and compares PR vs base commit.
- **PR storage perf (gating):** [`/.github/workflows/storage-perf.yml`](../.github/workflows/storage-perf.yml) runs the storage bench and compares PR vs base commit.
- **PR gateway perf (gating):** [`/.github/workflows/gateway-perf.yml`](../.github/workflows/gateway-perf.yml) runs the Aero Gateway bench and compares PR vs base commit.
- **PR CPU microbenches (gating):** [`/.github/workflows/bench.yml`](../.github/workflows/bench.yml) runs Criterion benchmarks and compares PR vs base commit.

Where to find artifacts:

- In GitHub Actions, open the workflow run and download artifacts such as:
  - JSON exports (`*.json`)
  - Trace captures (`trace.json`)
  - Benchmark summaries (`summary.json`, `compare.md`, history dashboards)

What CI currently uploads (subject to change):

- **PR perf:** `perf-smoke-<run_id>` (includes baseline + candidate summaries and `compare.md`)
- **Nightly perf:** `perf-nightly-<run_id>` (browser perf run output) and `perf-history-dashboard` (time-series history + static dashboard bundle)

For the canonical repo, the nightly workflow also publishes the dashboard to `gh-pages` (GitHub Pages) so trends are visible without downloading artifacts.

---

## Pitfalls / best practices

### Noisy machines

Performance measurements are extremely sensitive to background load.

Best practices:

- Close CPU-heavy apps (video calls, IDE indexing, games).
- Plug laptops into power and disable battery-saver modes.
- Avoid running benchmarks right after boot (let background tasks settle).

### Browser updates

Browser engine updates can cause large swings in WASM and WebGPU performance.

Best practices:

- Record the browser version (exports/traces should include it).
- Re-run baselines after major browser upgrades.

### Headless vs headed differences

Headless runs can differ due to:

- Different compositor behavior
- Different GPU selection / SwiftShader fallback
- Different timer granularity and scheduling

Best practices:

- Use **headed Chromium** for local investigations unless you are specifically debugging CI.
- Note that CI perf intentionally runs **headless Chromium with GPU/WebGPU disabled** for stability; expect different numbers locally.
- Compare “headless vs headed” only against baselines collected the same way.

---

## Quick-start checklist (copy/paste friendly)

Capture a perf export:

1. Run Aero locally and reproduce the workload.
2. Start a capture (HUD **Start**, or run `window.aero.perf.captureStart()`).
3. Reproduce the issue for ~5–15 seconds, then stop capture (HUD **Stop**, or `window.aero.perf.captureStop()`).
4. Open DevTools → Console and run: `window.aero.perf.export()`
5. Save/download the JSON and attach it to your issue/PR.

Run the microbench benchmark locally:

1. `npm ci && node scripts/playwright_install.mjs chromium` (one-time; use `--with-deps` on minimal Linux hosts)
2. `npm run bench:run -- --scenario microbench --iterations 7`

Alternative (run the underlying runner directly, without the wrapper):

- `node tools/perf/run.mjs --out-dir perf-results/local --iterations 7`

To benchmark your locally served build instead of the built-in `data:` page:

1. `npm ci && npm run serve:coi`
2. `node bench/run --scenario microbench --iterations 7 --url http://127.0.0.1:4173/`
