# Perf tooling (CI)

This directory contains small, self-contained performance tooling used by GitHub Actions:

- PR workflow: `.github/workflows/perf.yml` (smoke, fast)
- Nightly workflow: `.github/workflows/perf-nightly.yml` (more iterations)

## Benchmarks

The current suite is intentionally minimal and browser-only:

- `chromium_startup_ms`: time to launch Chromium, create a page, and load the target URL (defaults to an internal `data:` URL)
- `microbench_ms`: a deterministic JavaScript microbenchmark executed in Chromium
- `aero_microbench_suite_ms` (opt-in): calls `window.aero.bench.runMicrobenchSuite()` if available and measures wall time (`--include-aero-bench`)

This keeps CI signal stable and avoids GPU/WebGPU dependencies.

In CI, the workflows build the app and run against a Vite `preview` server (`http://127.0.0.1:4173/`) via `--url` so "startup" includes a realistic page load.

CI uses a small wrapper script, [`scripts/ci/run_browser_perf.mjs`](../../scripts/ci/run_browser_perf.mjs), to:

- detect the workspace (via `.github/actions/setup-node-workspace`)
- build the app (when running in `--preview` mode)
- start/stop a Vite preview server safely
- run the perf harness (`tools/perf/run.mjs`) and collect artifacts in a consistent layout

The runner also writes `perf_export.json` and `trace.json` alongside `raw.json`/`summary.json`:

- If the page exposes `window.aero.perf` with the capture/export API, it contains a short capture export.
- Otherwise it contains `null` (so CI artifacts always have a consistent file layout).

Trace capture is opt-in:

- Pass `--trace` (or `--trace-duration-ms <n>`) and, when the page exposes `window.aero.perf.traceStart/traceStop/exportTrace`, the runner writes a Chrome Trace Event JSON file to `trace.json`.
- Otherwise `trace.json` contains `null` (same “always present” layout as `perf_export.json`).

## Perf export metadata (PF-006)

When the perf export includes a `jit` section, `run.mjs` also embeds it under `meta.aeroPerf.jit` in `raw.json` and `summary.json`.

This is meant to make perf regressions easier to attribute (e.g. throughput drop because JIT compile time spiked), without affecting timed samples (perf export capture happens after the timed microbench loop).

`compare.mjs` will include these JIT metrics in `compare.md` when present so the GitHub Actions job summary shows them alongside benchmark deltas.

## Threshold policy

All perf regression thresholds live in a single file:

- [`bench/perf_thresholds.json`](../../bench/perf_thresholds.json)

The browser compare tool uses the `browser` suite section with either:

- `--profile pr-smoke` (PR gating), or
- `--profile nightly` (tighter thresholds for long-running builds / nightly jobs).

## Usage

Run locally (requires a Playwright Chromium install):

```bash
# Install deps once from the repo root (npm workspaces).
npm ci

node tools/perf/run.mjs --out-dir perf-results/local --iterations 7
```

Run locally against a built/previewed app (closer to CI behavior):

```bash
# One-time setup (per machine): install Playwright browsers.
# (Install deps first so `npx playwright` uses the repo-pinned version.)
npm ci
node scripts/playwright_install.mjs chromium

# Build + start `vite preview`, wait for readiness, run tools/perf/run.mjs, and clean up.
node scripts/ci/run_browser_perf.mjs --preview --out-dir perf-results/local --iterations 7
```

Capture an Aero trace (best-effort; requires the target page to expose the trace API):

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7 --trace
```

Capture a fixed-duration trace window (useful when you want more than a single benchmark run in the trace):

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7 --trace-duration-ms 5000
```

Include app-provided microbenches (best-effort; requires `window.aero.bench.runMicrobenchSuite`):

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7 --include-aero-bench
```

To benchmark a specific URL instead of the built-in `data:` page:

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7 --url http://127.0.0.1:4173/
```

Compare two runs (baseline vs candidate):

```bash
node tools/perf/compare.mjs \
  --baseline perf-results/base/summary.json \
  --candidate perf-results/head/summary.json \
  --out-dir perf-results/compare \
  --thresholds-file bench/perf_thresholds.json \
  --profile pr-smoke
```

## Environment consistency

CI pins:

- Node via `actions/setup-node` using the repo root [`.nvmrc`](../../.nvmrc) (`node-version-file`)
- Playwright via the `playwright-core` dependency, pinned by the repo root `package-lock.json`
- Chromium only (installed via `.github/actions/setup-playwright`, cached in `~/.cache/ms-playwright`)

Chromium is launched with a fixed set of flags to reduce variance:

- `--disable-dev-shm-usage`
- `--disable-gpu`
- `--disable-features=WebGPU`
- `--no-first-run`
- `--no-default-browser-check`
- `--disable-background-networking`
- `--disable-background-timer-throttling`
- `--disable-renderer-backgrounding`
- `--disable-backgrounding-occluded-windows`
- `--disable-extensions`
- `--disable-sync`

The runner retries browser launch and navigation a small number of times to reduce flake.
