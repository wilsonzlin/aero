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

- **In-app UI:** Use the developer menu and toggle **Performance HUD**.
- **Keyboard shortcut:** `Ctrl+Shift+H` (dev builds).

If neither is available (e.g. when embedding Aero), you can toggle it programmatically:

```js
window.aero.perf.hudToggle();
```

### Update frequency / sampling window

To avoid the HUD itself becoming a source of overhead:

- The HUD **renders at a fixed rate** (default: **4 Hz / every 250ms**).
- Most values are computed over a **rolling window** (typically the last **1s** or last **N frames**).

### Metrics (what they mean)

The exact fields may evolve, but contributors should expect the HUD to include:

| Metric | Meaning | How to use it |
| --- | --- | --- |
| **FPS** | Frames per second presented to the canvas | Drops usually indicate a pacing problem (CPU, GPU, or sync) |
| **Frame time (ms)** | Wall time per frame (lower is better) | Compare to 16.7ms (60Hz) or 33.3ms (30Hz) |
| **Emulation speed** (e.g. MIPS / instructions/s) | Guest execution throughput | Use to spot CPU/JIT regressions independent of rendering |
| **CPU worker time** | Time spent running the CPU emulation worker | High values usually mean the emulator is compute-bound |
| **GPU time** | Time spent encoding/submitting GPU work | Spikes often correlate with large state changes or shader work |
| **I/O time** | Time spent in storage/network/audio queues | Correlate with stutters during heavy disk activity |
| **WASM memory / JS heap** | Memory footprint | Watch for growth (leaks) and sudden GC-triggering spikes |
| **Dropped / long frames** | Count of frames above a threshold (e.g. >33ms) | Use when investigating “feels janky” reports |

Interpretation tip: **one bad frame is not a regression**. Look for sustained changes in the rolling averages and for increases in long-frame counts.

---

## Capturing and downloading JSON exports

JSON exports are meant to be attached to issues/PRs. They should include:

- Build metadata (commit SHA, build mode, feature flags)
- Environment metadata (browser version, OS, device info)
- Aggregated counters and histograms for the current run

### Export from the DevTools console

Run this in the page console:

```js
await window.aero.perf.export();
```

Expected behavior:

- `export()` returns a plain JSON-serializable object.
- In dev builds, it may also trigger a file download automatically (depending on configuration).

### Manual download (if your build doesn’t auto-download)

```js
const data = await window.aero.perf.export();
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

### Build + serve (production-like)

Run benchmarks against a built bundle rather than a dev server:

```bash
# Build a production bundle
pnpm build

# Serve it locally (any static server is fine)
pnpm serve
```

If your project uses a different package manager, substitute `npm run build` / `npm run serve`.

### Run the Playwright benchmark runner

The runner launches the app in a browser and executes a named scenario:

```bash
node bench/run --scenario microbench --iterations 7
```

What to expect:

- The runner prints a per-metric summary (mean/median/p95).
- It emits a warning when run-to-run variance is high (indicating noise).

### Interpreting summary output and variance warnings

When the runner warns about variance:

- **Re-run** with more iterations (e.g. 15+).
- Ensure your machine is not thermally throttling and nothing heavy is running in the background.
- Prefer the same browser version you use for baselines.

Treat small single-digit % changes as suspicious unless variance is low and the change reproduces across runs.

---

## Updating baselines and thresholds

Baselines are used to detect regressions in CI. Update them when:

- You intentionally improve performance (so CI stops failing once the improvement lands)
- A known, unavoidable regression is accepted (with explicit acknowledgement)
- The baseline environment changes (e.g. major browser version change)

Use the baseline updater script: [`bench/update-baseline`](../bench/update-baseline).

Guidelines:

- Update baselines on a **quiet, stable machine**.
- Include the **before/after summary** in the PR description.
- Avoid “baseline churn”: if results are noisy, fix the source of noise first.

---

## CI perf jobs

CI performance jobs are split by intent:

- **PR perf (gating):** a small set of quick scenarios to catch obvious regressions early.
- **Nightly perf (non-gating + data collection):** longer runs to track trends and reduce variance.

Where to find artifacts:

- In GitHub Actions, open the workflow run and download artifacts such as:
  - JSON exports (`*.json`)
  - Trace captures (`trace*.json`)
  - Benchmark summaries (`summary*.txt` / `summary*.md`)

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
- Compare “headless vs headed” only against baselines collected the same way.

---

## Quick-start checklist (copy/paste friendly)

Capture a perf export:

1. Run Aero locally and reproduce the workload.
2. Open DevTools → Console.
3. Run: `await window.aero.perf.export()`
4. Save/download the JSON and attach it to your issue/PR.

Run the microbench benchmark locally:

1. `pnpm build && pnpm serve`
2. `node bench/run --scenario microbench --iterations 7`
