#include <assert.h>
#include <stdint.h>

#include "drivers/aerogpu/tools/win7_dbgctl/src/aerogpu_fence_watch_math.h"

int main(void) {
  /* Basic increment: deltas and rate. */
  {
    const aerogpu_fence_delta_stats s = aerogpu_fence_compute_delta(10, 5, 15, 8, 0.5);
    assert(s.reset == 0);
    assert(s.delta_submitted == 5);
    assert(s.delta_completed == 3);
    /* 3 / 0.5 == 6 exactly. */
    assert(s.completed_per_s == 6.0);
  }

  /* No change: delta=0, rate=0. */
  {
    const aerogpu_fence_delta_stats s = aerogpu_fence_compute_delta(10, 5, 10, 5, 1.0);
    assert(s.reset == 0);
    assert(s.delta_submitted == 0);
    assert(s.delta_completed == 0);
    assert(s.completed_per_s == 0.0);
  }

  /* dt=0: rate should be 0 (avoid divide-by-zero). */
  {
    const aerogpu_fence_delta_stats s = aerogpu_fence_compute_delta(10, 5, 12, 7, 0.0);
    assert(s.reset == 0);
    assert(s.delta_submitted == 2);
    assert(s.delta_completed == 2);
    assert(s.completed_per_s == 0.0);
  }

  /* Counter reset: mark reset and zero deltas/rate. */
  {
    const aerogpu_fence_delta_stats s = aerogpu_fence_compute_delta(10, 5, 1, 2, 1.0);
    assert(s.reset != 0);
    assert(s.delta_submitted == 0);
    assert(s.delta_completed == 0);
    assert(s.completed_per_s == 0.0);
  }

  return 0;
}

