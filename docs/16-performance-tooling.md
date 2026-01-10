# 16 - Performance Tooling (Profiling, HUD, Traces, Benchmarks)

## Overview

Aero ships with **first-party performance tooling** that contributors should use for:

- **Interactive profiling** while developing locally (Perf HUD)
- **Bug reports / PR evidence** (JSON exports + traces)
- **Regression detection** (benchmarks + baselines in CI)

This doc describes the expected workflows and how to interpret the outputs.

---

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
| **MIPS (avg)** | Estimated guest throughput (million instructions/s) | Useful for CPU/JIT regressions independent of rendering |
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

Trace capture is an optional/advanced feature; not every build exposes it yet. When available, the API is expected to be:

In the DevTools console:

```js
window.aero.perf.traceStart();
// Reproduce the problem for ~5-15 seconds.
const trace = await window.aero.perf.traceStop();
```

Notes:

- Keep traces short (seconds, not minutes). Long traces are hard to analyze and expensive to record.
- Prefer capturing a trace immediately after a cold start if you’re investigating startup costs (shader compilation, caches).

### Opening a trace in Perfetto

1. Save the trace JSON returned by `traceStop()` to a file (or use your build’s download button if present).
2. Open https://ui.perfetto.dev/
3. Drag-and-drop the JSON file into the UI.

### Opening a trace in `chrome://tracing`

1. Open `chrome://tracing` in a Chromium-based browser.
2. Click **Load**.
3. Select the trace JSON file you saved from `traceStop()`.

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

For perf-sensitive work (SharedArrayBuffer / threads / COOP+COEP), prefer the COI preview helper:

```bash
npm run serve:coi
```

### Run the Playwright benchmark runner (CI parity)

The CI browser perf harness lives under `tools/perf/` and can be run locally.

One-time setup:

```bash
cd tools/perf
npm ci
npx --yes playwright@$(node -p "require('./package.json').dependencies['playwright-core']") install chromium
cd ../..
```

```bash
node bench/run --scenario microbench --iterations 7
```

What to expect:

- Results are written to `perf-results/` (raw samples + `summary.json`).
- Summaries include **median-of-N** numbers and a **CV** (coefficient of variation) to help spot noise.

To benchmark a specific URL (e.g. your local preview server), pass `--url`:

```bash
node bench/run --scenario microbench --iterations 7 --url http://127.0.0.1:4173/
```

### Run the GPU benchmark scenarios (Playwright + WebGPU/WebGL2)

The GPU bench runner executes scenario scripts in a real browser and emits a JSON report:

```bash
npm run bench:gpu -- --scenarios vga_text_scroll,vbe_lfb_blit --headless false --output gpu_bench.json
```

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
  --regression-threshold-pct 15 \
  --extreme-cv-threshold 0.5
```

---

## Updating baselines and thresholds

Baselines are used to detect regressions in CI. Update them when:

- You intentionally improve performance (so CI stops failing once the improvement lands)
- A known, unavoidable regression is accepted (with explicit acknowledgement)
- The baseline environment changes (e.g. major browser version change)

In CI:

- The PR perf workflow compares **PR vs base commit** (no committed “golden baseline” file).
- Thresholds live in `.github/workflows/perf.yml` as environment variables (and can also be passed to `tools/perf/compare.mjs`):
  - `PERF_REGRESSION_THRESHOLD_PCT`
  - `PERF_EXTREME_CV_THRESHOLD`

Use the baseline updater script: [`bench/update-baseline`](../bench/update-baseline).

Guidelines:

- Update baselines on a **quiet, stable machine**.
- Include the **before/after summary** in the PR description.
- Avoid “baseline churn”: if results are noisy, fix the source of noise first.

---

## CI perf jobs

CI performance jobs are split by intent:

- **PR perf (gating):** `.github/workflows/perf.yml` runs a small browser-only suite and compares PR vs base commit.
- **Nightly perf (non-gating + data collection):** `.github/workflows/perf-nightly.yml` runs more iterations and publishes history/dashboard artifacts.

Where to find artifacts:

- In GitHub Actions, open the workflow run and download artifacts such as:
  - JSON exports (`*.json`)
  - Trace captures (`trace*.json`)
  - Benchmark summaries (`summary.json`, `compare.md`, history dashboards)

What CI currently uploads (subject to change):

- **PR perf:** `perf-smoke-<run_id>` (includes baseline + candidate summaries and `compare.md`)
- **Nightly perf:** `perf-nightly-<run_id>` (browser perf run output) and `perf-history-dashboard` (microbench history + dashboard bundle)

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
2. Open DevTools → Console.
3. Run: `window.aero.perf.export()`
4. Save/download the JSON and attach it to your issue/PR.

Run the microbench benchmark locally:

1. `cd tools/perf && npm ci && npx playwright install chromium && cd ../..` (one-time)
2. `node bench/run --scenario microbench --iterations 7`

To benchmark your locally served build instead of the built-in `data:` page:

1. `npm ci && npm run serve:coi`
2. `node bench/run --scenario microbench --iterations 7 --url http://127.0.0.1:4173/`
