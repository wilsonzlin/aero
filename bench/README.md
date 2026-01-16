# Benchmarks

This directory contains performance/telemetry tooling used for CI regression tracking and local profiling.

## Quick-start (canonical commands)

- `npm run bench:browser` — **browser perf** run (wrapper around `tools/perf/run.mjs` via `bench/run`; pass `--url http://127.0.0.1:4173/` to include a real page load).
- `node scripts/ci/run_browser_perf.mjs --preview ...` — **CI-parity browser perf** run (build + `vite preview` + perf harness in one step).
- `npm run bench:node` — **lightweight Node** microbench for PF-009 (`bench/run.js` → `bench/results.json`).
- `npm run bench:compare` — compare `bench/results.json` against the checked-in `bench/baseline.json` (PF-009).
- `npm run bench:update-baseline` — re-record `bench/baseline.json` (PF-009).
- `npm run bench:gpu` — GPU benchmark suite (`bench/gpu_bench.ts`).
- `npm run bench:storage` — storage macrobench scenario (`bench/runner.ts storage_io`).
- `npm run bench:gateway` — backend networking benchmarks (TCP proxy RTT/throughput, DoH QPS/cache).

Note: `bench/run` also has a **legacy macro mode** (triggered by `--output`/`--results-dir`), but it overlaps with
`tools/perf` and is considered deprecated for contributor workflows (see “Legacy browser macrobench harness” below).

## Capturing traces / app microbenches (browser perf)

For quick local runs (no preview server), `bench/run` can forward trace + app microbench flags directly to `tools/perf/run.mjs`:

```bash
node bench/run --scenario all --iterations 7 --include-aero-bench --trace-duration-ms 5000
```

For CI-parity runs (build + `vite preview`), use the wrapper script instead:

```bash
PERF_TRACE_DURATION_MS=5000 node scripts/ci/run_browser_perf.mjs --preview --iterations 7 --out-dir perf-results/local
```

## Nightly perf history (dashboard)

Files:

- `bench/history.js` — appends benchmark results into a versioned `bench/history.json` time series and can generate `bench/history.md`.
- `bench/history.schema.json` — JSON schema for the history file.
- `bench/dashboard/` — static dashboard that loads `history.json` and renders trend graphs.
- `bench/run.js` — optional lightweight Node microbench that writes `bench/results.json` (useful for quick local smoke tests).

Local usage:

Append a CI-parity browser perf run output (recommended; matches the nightly workflow):

```bash
node scripts/ci/run_browser_perf.mjs --preview --iterations 7 --out-dir perf-results/local
node bench/history.js append \
  --history bench/history.json \
  --input perf-results/local/raw.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

Or run the lightweight Node microbench:

```bash
npm run bench:node
node bench/history.js append \
  --history bench/history.json \
  --input bench/results.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

You can also append a macrobench scenario runner output (`bench/runner.ts` writes `report.json`) — metrics are treated as single-sample values (n=1):

```bash
node bench/history.js append \
  --history bench/history.json \
  --input perf-results/local/report.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

For storage I/O, prefer appending the raw `storage_bench.json` output instead; it contains per-run samples so the history can compute `n` + CoV (the dashboard uses the per-run mean as the primary `value`):

```bash
node bench/history.js append \
  --history bench/history.json \
  --input storage-perf-results/head/storage_bench.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

Generate a lightweight markdown report:

```bash
node bench/history.js render-md --history bench/history.json --out bench/history.md
```

### Baseline regression compare (PF-009)

PF-009 adds a baseline/threshold compare tool that turns the microbench JSON into an actionable
regression report (Markdown for CI artifacts/PR comments + optional JSON details).

Files:

- `bench/baseline.json` — checked-in baseline results (same schema as `bench/results.json`, plus optional metadata/variance expectations).
- `bench/perf_thresholds.json` — threshold policy (shared across browser/GPU/storage/Node microbench tooling; the PF-009 compare uses the `node` suite).
- `bench/compare` — compares baseline vs current and writes:
  - `bench/compare.md`
  - `bench/compare.json` (optional; machine-readable)

Example:

```bash
npm run bench:node
node bench/compare --fail-on-regression --json
```

To re-record the checked-in baseline (`bench/baseline.json`):

```bash
npm run bench:update-baseline -- --scenario all --iterations 15
```

Comparisons use **median-of-N** (`samples`) per metric. If the current run has high variance, the
per-metric CV is included in the report and can trigger an `unstable` result. Metrics can also be
marked `informational` in thresholds so they never fail CI.

## GPU benchmark suite

`bench/gpu_bench.ts` runs graphics-focused benchmarks (WebGPU with WebGL2 fallback) and emits a JSON
report suitable for artifact upload and regression comparison.

### Running locally

```bash
npm run bench:gpu -- --iterations 7 --output gpu_bench.json
```

Common options:

- `--scenarios vga_text_scroll,vbe_lfb_blit` (comma-separated)
- `--scenario-params path/to/params.json` (per-scenario overrides)
- `--iterations 7` (run each scenario N times and aggregate using median-of-N)
- `--headless false` (run headful)
- `--swiftshader true` (force software GL for more stable CI; may disable WebGPU on some platforms)

### Comparing results in CI

```bash
node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
  --baseline baseline.json \
  --candidate gpu_bench.json \
  --out-dir gpu-perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

The compare script writes `compare.md` + `summary.json` to `--out-dir` and exits non-zero if any
metric regresses by more than the configured threshold (exit code 2 indicates extreme variance).

You can also override thresholds directly (useful for local debugging or when tuning CI):

```bash
node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
  --baseline baseline.json \
  --current gpu_bench.json \
  --outDir gpu-perf-results/compare \
  --thresholdPct 15 \
  --cvThreshold 0.5
```

Or via environment variables:

```bash
GPU_PERF_REGRESSION_THRESHOLD_PCT=15 \
GPU_PERF_EXTREME_CV_THRESHOLD=0.5 \
  node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
    --baseline baseline.json \
    --candidate gpu_bench.json \
    --out-dir gpu-perf-results/compare
```

## Storage I/O benchmark suite (OPFS + IndexedDB)

The `storage_io` scenario loads `web/storage_bench.html` in Chromium via Playwright and records:

- `storage_bench.json` — raw storage benchmark report (OPFS preferred; falls back to IndexedDB)
- `report.json` — scenario runner report containing a small set of key metrics (seq read/write MB/s, random read p50/p95)

Note: IndexedDB is async-only and does not currently back the synchronous Rust disk/controller path
(`aero_storage::{StorageBackend, VirtualDisk}`); see:

- [`docs/19-indexeddb-storage-story.md`](../docs/19-indexeddb-storage-story.md)
- [`docs/20-storage-trait-consolidation.md`](../docs/20-storage-trait-consolidation.md)

The benchmark uses a fixed `random_seed` so random I/O patterns are repeatable across runs.

### Running locally (CI parity)

```bash
npm ci
node scripts/playwright_install.mjs chromium --with-deps

# Write artifacts to storage-perf-results/head/
npm run bench:storage -- --out-dir storage-perf-results/head
```

### Comparing two runs

```bash
npm run compare:storage -- \
  --baseline storage-perf-results/base/storage_bench.json \
  --candidate storage-perf-results/head/storage_bench.json \
  --out-dir storage-perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke

cat storage-perf-results/compare/compare.md
```

The compare tool writes `compare.md` + `summary.json` to `--out-dir` and gates on:

- capability regressions (e.g. OPFS → IndexedDB fallback, or OPFS `sync_access_handle` → `async`)
- benchmark config mismatches between baseline/current runs (to avoid apples-to-oranges comparisons)
- extreme variance (CV threshold per metric)

It also includes any `warnings[]` from the benchmark output in the Markdown report.
The exit code matches other perf suites (`0` pass, `1` regression, `2` unstable).

Optional environment overrides (useful for local debugging or CI tuning):

```bash
STORAGE_PERF_REGRESSION_THRESHOLD_PCT=15 \
STORAGE_PERF_EXTREME_CV_THRESHOLD=0.5 \
  npm run compare:storage -- \
    --baseline storage-perf-results/base/storage_bench.json \
    --candidate storage-perf-results/head/storage_bench.json \
    --out-dir storage-perf-results/compare
```

`scripts/compare_storage_benchmarks.ts` remains as a compatibility wrapper for older invocations
(`--current`, `--outDir`, `--thresholdPct`, `--json`) and can write `compare.json` for legacy tooling:

```bash
npm run compare:storage:legacy -- --help
```

## Gateway benchmark suite (backend networking)

`backend/aero-gateway/bench/run.mjs` runs local loopback-only benchmarks for:

- TCP proxy RTT (p50/p90/p99)
- TCP proxy throughput (MiB/s)
- DoH QPS + cache hit ratio

### Running locally

```bash
npm ci
npm -w backend/aero-gateway run bench
```

### Comparing two runs (PR smoke style)

```bash
node --experimental-strip-types scripts/compare_gateway_benchmarks.ts \
  --baseline baseline.json \
  --candidate candidate.json \
  --out-dir gateway-perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

The compare script writes `compare.md` + `summary.json` to `--out-dir` and exits non-zero on regression
(exit code 2 indicates extreme variance).

Optional environment overrides:

```bash
GATEWAY_PERF_REGRESSION_THRESHOLD_PCT=15 \
GATEWAY_PERF_EXTREME_CV_THRESHOLD=0.5 \
  node --experimental-strip-types scripts/compare_gateway_benchmarks.ts \
    --baseline baseline.json \
    --candidate candidate.json \
    --out-dir gateway-perf-results/compare
```

## Scenario runner (benchmark suite infrastructure)

The scenario runner provides an extensible plugin interface for running benchmark scenarios (both `micro` and `macro`).

PF-008 uses this runner for guest CPU throughput benchmarks (the `guest_cpu` scenario) and for future full-system macrobenchmarks (boot time, desktop FPS, app launch time) once the emulator can boot OS images.

### Milestones and readiness signals

Macrobench scenarios should wait on host-visible signals instead of guessing:

- `window.aero.status.phase` ∈ `booting` | `installing` | `desktop` | `idle`
- `window.aero.waitForEvent(name)` / `window.aero.events`
  - phase transitions: `phase:<phase>` (e.g. `phase:desktop`)
  - convenience milestones: `desktop_ready`, `idle_ready`

As a last resort, scenarios can fall back to screenshot stability detection (hash-equal frames over N intervals) via `milestones.waitForStableScreen()`.

### Running locally

List scenarios:

```bash
node --experimental-strip-types bench/runner.ts --list
```

Run a scenario:

```bash
node --experimental-strip-types bench/runner.ts guest_cpu
node --experimental-strip-types bench/runner.ts noop
```

Common scenario IDs:

- `guest_cpu`: guest CPU throughput benchmark (PF-008; headless browser scenario)
- `storage_io`: storage I/O benchmark suite (see “Storage I/O benchmark suite” above)
- `system_boot`: boots a full OS image (requires `--disk-image`)
- `noop`: minimal smoke test / harness validation

### Guest CPU throughput benchmark (`guest_cpu`)

The `guest_cpu` scenario runs the guest CPU throughput benchmark in a headless Chromium session and executes:

`window.aero.bench.runGuestCpuBench({ variant, mode, seconds })`

Artifacts written to the output directory:

- `guest_cpu_bench.json` (raw benchmark report)
- `perf_export.json` (from `window.aero.perf.export()`, if available)
- `report.json` (scenario runner summary + key metrics)

Run locally:

```bash
node --experimental-strip-types bench/runner.ts guest_cpu
```

### Disk images (local-only)

Macrobench scenarios can optionally require a user-provided disk image.

Provide it via flag or env var:

```bash
# Flag
node --experimental-strip-types bench/runner.ts system_boot --disk-image /path/to/win7.img

# Env var
AERO_DISK_IMAGE_PATH=/path/to/win7.img node --experimental-strip-types bench/runner.ts system_boot
```

CI should only select scenarios that do not require proprietary images (or will see a clean `skipped` report).

### Output

Each run writes a `report.json` to the selected output directory and, when supported by the emulator driver:

- `perf_export.json`
- `screenshots/*.png`
- `trace.bin`

By default, `bench/runner.ts` writes results under `bench/results/` (ignored by git).

### Macrobench metrics (standard IDs)

Macrobench scenarios should report consistent metric IDs/units:

- `boot_time_ms` (`ms`): start → `desktop_ready`
- `desktop_fps` (`fps`): steady-state FPS over an interval
- `app_launch_time_ms` (`ms`): trigger → first stable frame
- `input_latency_ms` (`ms`): representative latency while desktop is active

## Legacy browser macrobench harness (Playwright)

Playwright-driven runner that loads the app in Chromium, executes scenarios, and captures `window.aero.perf.export()` after each iteration.

**Deprecated:** this predates `tools/perf` and overlaps with it. Prefer `npm run bench:browser` unless you are actively
migrating these scenarios into the newer tooling.

```bash
node bench/run --scenario microbench --iterations 5 --output out.json
```

Outputs:

- `bench/results/<run-id>.json` — raw per-iteration results and perf exports
- `bench/results/<run-id>.summary.json` — aggregated stats (median/stdev/CoV)
- `out.json` — optional copy of the summary (`--output`)

### Scenarios

- `startup`: navigation → `window.aero.isReady()` time + wasm timing hints from perf export (if present)
- `microbench`: runs `window.aero.bench.runMicrobenchSuite()`
- `idle_raf`: idle requestAnimationFrame loop for `--idle-seconds` and reports FPS + frame-time percentiles
