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
- Playwright via `PLAYWRIGHT_VERSION` in workflow + `playwright-core` dependency
- Chromium only (`npx playwright@<version> install chromium`)

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
