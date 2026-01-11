# Aero Gateway Benchmarks

This directory contains **repeatable local benchmarks** for Aero's backend networking features:

- **TCP proxy** (WebSocket → TCP relay)
- **DoH** (DNS-over-HTTPS endpoint + in-memory cache)

The benchmark harness is designed to be:

- **offline** (loopback-only; no external network)
- **repeatable** (starts/stops its own local servers)
- **CI-friendly** (a short smoke mode that finishes in < 60s)

## Quickstart

From the repo root:

```bash
npm ci
npm -w backend/aero-gateway run bench
```

`npm run bench` automatically builds the gateway (`npm run build`) so the benchmark exercises the same code paths as production.

## Benchmark modes

### Local (default)

Runs a slightly longer benchmark to establish local baselines:

```bash
npm -w backend/aero-gateway run bench
```

This writes a JSON report to `backend/aero-gateway/bench/results.json` and prints a human-readable summary to stdout.

The report includes `tool`, `startedAt`, and `finishedAt` fields so other tooling can identify and timestamp runs.

### Nightly (CI trending)

Runs a configuration intended for nightly perf history dashboards (more stable than the smoke mode, but still designed to finish quickly on CI runners):

```bash
npm -w backend/aero-gateway run build
node backend/aero-gateway/bench/run.mjs --mode nightly --json backend/aero-gateway/bench/results.json
```

### CI smoke

Runs a short, conservative benchmark and **asserts minimum thresholds**:

```bash
npm -w backend/aero-gateway run bench:smoke
```

This is intended for GitHub Actions / perf regression smoke tests.

## What is measured?

### TCP proxy

- **RTT**: median/p90/p99 round-trip latency for a small payload sent through the WebSocket TCP proxy to a local echo server.
- **Throughput**: time to upload a fixed-size payload (5–10 MiB depending on mode) through the proxy to a local sink server.

The JSON report also includes lightweight variance summaries:

- RTT reports include `stdev` and `cv` (coefficient of variation) computed over the RTT sample set.
- Throughput is run multiple times and reports `throughput.stats` (`n/min/max/mean/stdev/cv`) over MiB/s, plus per-run samples in `throughput.runs`.

### DoH

- **QPS**: HTTP requests per second against `/dns-query` for a fixed `A` query (loopback-resolved).
- **Cache hit ratio**: computed from gateway metrics (cache hits / (hits + misses)).

For DoH, the JSON report includes `qpsStats` (derived from autocannon's summary stats) and additional latency variance fields when available.

To keep the benchmark **offline**, the runner starts a local UDP DNS server and configures the gateway's `DNS_UPSTREAMS` to point to it. The upstream returns a deterministic `A` record so the DoH cache hit path is exercised without contacting real DNS resolvers.

## Interpreting results

These numbers are **sensitive to local machine load**. Use the results as:

- a baseline for your machine (run multiple times and compare)
- a smoke-test for catastrophic regressions in CI (thresholds are intentionally conservative)
