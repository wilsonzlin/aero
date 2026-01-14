#pragma once

#include <cstdint>

#include "../include/aerogpu_d3d9_umd.h"

#include "aerogpu_wddm_alloc_list.h"
#include "aerogpu_wddm_context.h"

// Re-export the shared WDDM private driver data contract used by both the UMD
// and KMD.
#include "../../../protocol/aerogpu_wddm_alloc.h"

namespace aerogpu {

// -----------------------------------------------------------------------------
// WDDM allocation helpers (Win7 / WDDM 1.1)
// -----------------------------------------------------------------------------
//
// These helpers wrap the runtime-provided D3DDDI device callbacks that create
// allocations and map/unmap them for CPU access. Repository builds do not ship
// with the Win7 WDK headers, so the real implementations are only compiled when
// `AEROGPU_D3D9_USE_WDK_DDI` is defined and truthy (set to 1).

HRESULT wddm_create_allocation(const WddmDeviceCallbacks& callbacks,
                               WddmHandle hDevice,
                               uint64_t size_bytes,
                               const aerogpu_wddm_alloc_priv* priv,
                               uint32_t priv_size,
                               WddmAllocationHandle* hAllocationOut,
                               WddmHandle hContext = 0) noexcept;

HRESULT wddm_destroy_allocation(const WddmDeviceCallbacks& callbacks,
                                  WddmHandle hDevice,
                                  WddmAllocationHandle hAllocation,
                                  WddmHandle hContext = 0) noexcept;

HRESULT wddm_lock_allocation(const WddmDeviceCallbacks& callbacks,
                               WddmHandle hDevice,
                               WddmAllocationHandle hAllocation,
                               uint64_t offset_bytes,
                               uint64_t size_bytes,
                               uint32_t lock_flags,
                               void** out_ptr,
                               WddmHandle hContext = 0) noexcept;

inline HRESULT wddm_lock_allocation(const WddmDeviceCallbacks& callbacks,
                                     WddmHandle hDevice,
                                     WddmAllocationHandle hAllocation,
                                     uint64_t offset_bytes,
                                     uint64_t size_bytes,
                                     void** out_ptr,
                                     WddmHandle hContext = 0) noexcept {
  return wddm_lock_allocation(callbacks,
                              hDevice,
                              hAllocation,
                              offset_bytes,
                              size_bytes,
                              /*lock_flags=*/0,
                              out_ptr,
                              hContext);
}

HRESULT wddm_unlock_allocation(const WddmDeviceCallbacks& callbacks,
                                 WddmHandle hDevice,
                                 WddmAllocationHandle hAllocation,
                                 WddmHandle hContext = 0) noexcept;

} // namespace aerogpu
