# Benchmarks

This directory contains performance/telemetry tooling used for CI regression tracking and local profiling.

## Quick-start (canonical commands)

- `npm run bench:browser` — **browser CI-parity** perf run (wrapper around `tools/perf/run.mjs` via `bench/run`; requires `npm ci` + `npx playwright install chromium` once).
- `npm run bench:node` — **lightweight Node** microbench for PF-009 (`bench/run.js` → `bench/results.json`).
- `npm run bench:compare` — compare `bench/results.json` against the checked-in `bench/baseline.json` (PF-009).
- `npm run bench:update-baseline` — re-record `bench/baseline.json` (PF-009).
- `npm run bench:gpu` — GPU benchmark suite (`bench/gpu_bench.ts`).
- `npm run bench:storage` — storage macrobench scenario (`bench/runner.ts storage_io`).

Note: `bench/run` also has a **legacy macro mode** (triggered by `--output`/`--results-dir`), but it overlaps with
`tools/perf` and is considered deprecated for contributor workflows (see “Legacy browser macrobench harness” below).

## Nightly perf history (dashboard)

Files:

- `bench/history.js` — appends benchmark results into a versioned `bench/history.json` time series and can generate `bench/history.md`.
- `bench/history.schema.json` — JSON schema for the history file.
- `bench/dashboard/` — static dashboard that loads `history.json` and renders trend graphs.
- `bench/run.js` — optional lightweight Node microbench that writes `bench/results.json` (useful for quick local smoke tests).

Local usage:

Append a CI-parity browser perf run output (recommended; matches the nightly workflow):

```bash
npm run bench:browser -- --iterations 7 --out-dir perf-results/local
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

## Storage I/O benchmark suite (OPFS + IndexedDB)

The `storage_io` scenario loads `web/storage_bench.html` in Chromium via Playwright and records:

- `storage_bench.json` — raw storage benchmark report (OPFS preferred; falls back to IndexedDB)
- `report.json` — scenario runner report containing a small set of key metrics (seq read/write MB/s, random read p50/p95)

The benchmark uses a fixed `random_seed` so random I/O patterns are repeatable across runs.

### Running locally (CI parity)

```bash
npm ci
npx playwright install --with-deps chromium

# Write artifacts to storage-perf-results/head/
npm run bench:storage -- --out-dir storage-perf-results/head
```

### Comparing two runs

```bash
node --experimental-strip-types scripts/compare_storage_benchmarks.ts \
  --baseline storage-perf-results/base/storage_bench.json \
  --current storage-perf-results/head/storage_bench.json \
  --outDir storage-perf-results \
  --thresholdPct 15 \
  --json

cat storage-perf-results/compare.md
```

The compare script writes `compare.md` + `summary.json` to `--out-dir` and exits non-zero if any metric
regresses by more than the configured threshold (exit code 2 indicates extreme variance / missing data).
`scripts/compare_storage_benchmarks.ts --json` also writes `compare.json` (a copy of `summary.json`) for legacy tooling.
The per-metric table includes the baseline/current coefficient-of-variation (CV) computed from the
per-run samples for quick noise inspection.

## Scenario runner (PF-008 macrobench framework)

The scenario runner provides an extensible plugin interface so we can evolve from microbenchmarks to full-system macrobenchmarks (boot time, desktop FPS, app launch time) once the emulator can boot OS images.

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
node --experimental-strip-types bench/runner.ts noop
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
