# Perf tooling (CI)

This directory contains small, self-contained performance tooling used by GitHub Actions:

- PR workflow: `.github/workflows/perf.yml` (smoke, fast)
- Nightly workflow: `.github/workflows/perf-nightly.yml` (more iterations)

## Benchmarks

The current suite is intentionally minimal and browser-only:

- `chromium_startup_ms`: time to launch Chromium, create a page, and load the target URL (defaults to an internal `data:` URL)
- `microbench_ms`: a deterministic JavaScript microbenchmark executed in Chromium

This keeps CI signal stable and avoids GPU/WebGPU dependencies.

In CI, the workflows build the app and run against a Vite `preview` server (`http://127.0.0.1:4173/`) via `--url` so "startup" includes a realistic page load.

The runner also writes `perf_export.json` alongside `raw.json`/`summary.json`:

- If the page exposes `window.aero.perf` with the capture/export API, it contains a short capture export.
- Otherwise it contains `null` (so CI artifacts always have a consistent file layout).

## Perf export metadata (PF-006)

When the perf export includes a `jit` section, `run.mjs` also embeds it under `meta.aeroPerf.jit` in `raw.json` and `summary.json`.

This is meant to make perf regressions easier to attribute (e.g. throughput drop because JIT compile time spiked), without affecting timed samples (perf export capture happens after the timed microbench loop).

`compare.mjs` will include these JIT metrics in `compare.md` when present so the GitHub Actions job summary shows them alongside benchmark deltas.

## Usage

Run locally (requires a Playwright Chromium install):

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7
```

To benchmark a specific URL instead of the built-in `data:` page:

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7 --url http://127.0.0.1:4173/
```

## Environment consistency

CI pins:

- Node via `actions/setup-node` (`NODE_VERSION` in workflow)
- Playwright via the `playwright-core` dependency, pinned by `tools/perf/package-lock.json`
- Chromium only (installed via `node node_modules/playwright-core/cli.js install chromium`)

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
