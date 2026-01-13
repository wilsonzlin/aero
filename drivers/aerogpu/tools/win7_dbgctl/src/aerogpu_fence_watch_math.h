/*
 * Fence watcher delta/rate math for `aerogpu_dbgctl --watch-fence`.
 *
 * Kept in a standalone header (no Windows dependencies) so we can unit test the
 * computation in `emulator/protocol/tests` with a C compiler.
 */
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct aerogpu_fence_delta_stats {
  uint64_t delta_submitted;
  uint64_t delta_completed;
  double completed_per_s;
  uint32_t reset; /* non-zero if counters moved backwards */
} aerogpu_fence_delta_stats;

static inline aerogpu_fence_delta_stats aerogpu_fence_compute_delta(uint64_t prev_submitted,
                                                                    uint64_t prev_completed,
                                                                    uint64_t now_submitted,
                                                                    uint64_t now_completed,
                                                                    double dt_seconds) {
  aerogpu_fence_delta_stats s;
  s.delta_submitted = 0;
  s.delta_completed = 0;
  s.completed_per_s = 0.0;
  s.reset = 0;

  if (now_submitted >= prev_submitted) {
    s.delta_submitted = now_submitted - prev_submitted;
  } else {
    s.reset = 1;
  }

  if (now_completed >= prev_completed) {
    s.delta_completed = now_completed - prev_completed;
  } else {
    s.reset = 1;
  }

  if (!s.reset && dt_seconds > 0.0) {
    s.completed_per_s = (double)s.delta_completed / dt_seconds;
  }

  return s;
}

#ifdef __cplusplus
} /* extern "C" */
#endif

