# @aero/aero-stats

Shared, mergeable streaming statistics primitives used for Aero performance reporting.

## Metric definitions (standard across HUD/export/bench/compare)

Given a sequence of frame times `t_i` in milliseconds:

- `frames = n`
- `total_time_ms = Σ t_i`
- `mean_frame_time_ms = Σ t_i / n`

### FPS

All FPS metrics are derived from frame-time metrics (never from averaging instantaneous FPS samples):

- `avg_fps = frames / (total_time_ms / 1000) = 1000 / mean_frame_time_ms`
- `median_fps = 1000 / p50_frame_time_ms`
- `p95_fps = 1000 / p95_frame_time_ms`

### 1% / 0.1% low FPS

Computed from slow-frame percentiles:

- `1% low FPS = 1000 / p99_frame_time_ms`
- `0.1% low FPS = 1000 / p99.9_frame_time_ms`

### Quantiles

Percentiles are computed over the **frame time distribution** (higher frame time = worse).

### Variance / CoV

Variance and standard deviation are computed over frame times (milliseconds) using Welford’s numerically stable algorithm:

- `variance_ms2` is the **population variance** (`M2 / n`)
- `stdev_ms = sqrt(variance_ms2)`
- `cov = stdev_ms / mean_frame_time_ms`

### MIPS (million instructions per second)

If `instructions` is a count over an interval of `elapsed_ms`:

- `mips = instructions / (elapsed_ms / 1000) / 1e6`

