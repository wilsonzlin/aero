# Benchmarks

This directory contains a small, dependency-free benchmark harness used by the nightly perf workflow.

## Files

- `bench/run.js` — runs a small set of microbenchmarks and writes `bench/results.json`.
- `bench/history.js` — appends benchmark results into a versioned `bench/history.json` time series and can generate `bench/history.md`.
- `bench/history.schema.json` — JSON schema for the history file.
- `bench/dashboard/` — static dashboard that loads `history.json` and renders trend graphs.

## Local usage

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

