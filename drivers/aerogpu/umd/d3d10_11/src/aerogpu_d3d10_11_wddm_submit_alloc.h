#pragma once

#include <cstdint>

namespace aerogpu::d3d10_11 {

// WDDM allocation reference carried alongside each Win7/WDDM 1.1 submission.
//
// The WDDM runtime consumes a per-submission allocation list; each entry includes
// a `WriteOperation` flag indicating whether the GPU is expected to write to the
// allocation during this submission.
//
// AeroGPU tracks read vs write usage for referenced allocations so it can avoid
// pessimistically marking all allocations as written (which can cause unnecessary
// residency/mapping churn on Win7).
struct WddmSubmitAllocation {
  // WDDM allocation handle (D3DKMT_HANDLE) stored as a u32 so this type stays
  // WDK-independent.
  uint32_t allocation_handle = 0;

  // 0 = read-only, 1 = written by GPU in this submission.
  uint8_t write = 0;
};

} // namespace aerogpu::d3d10_11

