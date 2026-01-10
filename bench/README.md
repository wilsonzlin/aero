# Benchmarks

This directory contains performance/telemetry tooling used for CI regression tracking and local profiling.

## Nightly perf history (dashboard)

Files:

- `bench/history.js` — appends benchmark results into a versioned `bench/history.json` time series and can generate `bench/history.md`.
- `bench/history.schema.json` — JSON schema for the history file.
- `bench/dashboard/` — static dashboard that loads `history.json` and renders trend graphs.
- `bench/run.js` — optional lightweight Node microbench that writes `bench/results.json` (useful for quick local smoke tests).

Local usage:

Append `tools/perf/run.mjs` output (recommended; matches the nightly workflow):

```bash
node tools/perf/run.mjs --out-dir perf-results/local --iterations 7
node bench/history.js append \
  --history bench/history.json \
  --input perf-results/local/raw.json \
  --timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --commit "$(git rev-parse HEAD)" \
  --repository "wilsonzlin/aero"
```

Or run the lightweight Node microbench:

```bash
node bench/run.js --out bench/results.json
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

`bench/gpu_bench.ts` runs graphics-focused benchmarks (WebGPU with WebGL2 fallback) and emits a JSON report suitable for artifact upload and regression comparison.

### Running locally

```bash
node --experimental-strip-types bench/gpu_bench.ts --output gpu_bench.json
```

Common options:

- `--scenarios vga_text_scroll,vbe_lfb_blit` (comma-separated)
- `--scenario-params path/to/params.json` (per-scenario overrides)
- `--headless false` (run headful)
- `--swiftshader true` (force software GL for more stable CI; may disable WebGPU on some platforms)

### Comparing results in CI

```bash
node --experimental-strip-types scripts/compare_gpu_benchmarks.ts \
  --baseline baseline.json \
  --current gpu_bench.json \
  --thresholdPct 5
```

The compare script exits non-zero if any primary metric regresses by more than the configured threshold.

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
