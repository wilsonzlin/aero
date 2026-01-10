# Benchmarks

This directory contains performance/telemetry tooling used for CI regression
tracking and local profiling.

## Microbench harness (nightly perf workflow)

Files:

- `bench/run.js` — runs a small set of microbenchmarks and writes `bench/results.json`.
- `bench/history.js` — appends benchmark results into a versioned `bench/history.json` time series and can generate `bench/history.md`.
- `bench/history.schema.json` — JSON schema for the history file.
- `bench/dashboard/` — static dashboard that loads `history.json` and renders trend graphs.

Local usage:

Run the benchmarks:

```bash
node bench/run.js --out bench/results.json
```

Append into history:

```bash
node bench/history.js append \
  --history bench/history.json \
  --input bench/results.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

Generate a lightweight markdown report:

```bash
node bench/history.js render-md --history bench/history.json --out bench/history.md
```

## GPU benchmark suite

`bench/gpu_bench.ts` runs graphics-focused benchmarks (WebGPU with WebGL2
fallback) and emits a JSON report suitable for artifact upload and regression
comparison.

### Running locally

```bash
node --experimental-strip-types bench/gpu_bench.ts --output gpu_bench.json
```

Common options:

- `--scenarios vga_text_scroll,vbe_lfb_blit` (comma-separated)
- `--headless false` (run headful)
- `--swiftshader true` (force software GL for more stable CI; may disable WebGPU on some platforms)

### Comparing results in CI

```bash
node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
  --baseline baseline.json \
  --current gpu_bench.json \
  --thresholdPct 5
```

The compare script exits non-zero if any primary metric regresses by more than
the configured threshold.

