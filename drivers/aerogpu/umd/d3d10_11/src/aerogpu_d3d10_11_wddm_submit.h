#pragma once

#include <cstddef>
#include <cstdint>

#include "../include/aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_wddm_submit_alloc.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dkmthk.h>
#endif

namespace aerogpu::d3d10_11 {

// Shared Win7/WDDM 1.1 submission helper for the D3D10 and D3D11 UMDs.
//
// This module is compiled only in WDK builds (`AEROGPU_UMD_USE_WDK_HEADERS=1`).
// Repository builds do not have access to the WDK DDI headers, so the class is
// intentionally a stub when those headers are unavailable.

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

class WddmSubmit {
 public:
  WddmSubmit() = default;
  ~WddmSubmit() noexcept;

  WddmSubmit(const WddmSubmit&) = delete;
  WddmSubmit& operator=(const WddmSubmit&) = delete;

  // Initializes the WDDM submission state:
  // - Creates the kernel device (`hDevice`) via `pfnCreateDeviceCb`.
  // - Creates the kernel context (`hContext`) + monitored-fence sync object
  //   (`hSyncObject`) via `pfnCreateContextCb2`/`pfnCreateContextCb`.
  //
  // `adapter_handle` should match the handle passed to the runtime at OpenAdapter
  // time (typically the `.pDrvPrivate` pointer behind `D3D10DDI_HADAPTER`).
  //
  // `runtime_device_private` is `hRTDevice.pDrvPrivate` from CreateDevice.
  HRESULT Init(const D3DDDI_DEVICECALLBACKS* callbacks,
               void* adapter_handle,
               void* runtime_device_private,
               D3DKMT_HANDLE kmt_adapter_for_debug = 0);

  void Shutdown();

  D3DKMT_HANDLE hDevice() const {
    return hDevice_;
  }
  D3DKMT_HANDLE hContext() const {
    return hContext_;
  }
  D3DKMT_HANDLE hSyncObject() const {
    return hSyncObject_;
  }

  // Submits a finalized AeroGPU command stream to the kernel, chunking at
  // AeroGPU packet boundaries if the runtime provides a smaller-than-requested
  // DMA buffer. When `want_present` is true, the last chunk is routed through
  // the Present callback when available so the KMD hits DxgkDdiPresent.
  //
  // `allocation_handles` provides the WDDM allocations that should be included
  // in the runtime's allocation list for this submission. The AeroGPU Win7 KMD
  // uses that list to build a sideband allocation table so the host can resolve
  // `backing_alloc_id` values in the AeroGPU command stream.
  //
  // On success, returns S_OK and writes the per-submission fence value to
  // `out_fence` (0 when no submission occurs).
  HRESULT SubmitAeroCmdStream(const uint8_t* stream_bytes,
                              size_t stream_size,
                              bool want_present,
                              const WddmSubmitAllocation* allocations,
                              uint32_t allocation_count,
                              uint64_t* out_fence);

  // Waits for a fence value on the monitored-fence sync object returned by
  // CreateContext. `timeout_ms == 0` performs a non-blocking poll.
  //
  // On timeout/poll miss, returns `DXGI_ERROR_WAS_STILL_DRAWING` (0x887A000A).
  HRESULT WaitForFenceWithTimeout(uint64_t fence, uint32_t timeout_ms);

  // Convenience wrapper for an infinite wait.
  HRESULT WaitForFence(uint64_t fence);

  // Best-effort query of the completed fence value. If a monitored fence CPU VA
  // is available this returns that value; otherwise this returns a conservative
  // cached value, optionally refreshed via a poll or debug escape.
  uint64_t QueryCompletedFence();

 private:
  const D3DDDI_DEVICECALLBACKS* callbacks_ = nullptr;
  void* adapter_handle_ = nullptr;
  void* runtime_device_private_ = nullptr;

  D3DKMT_HANDLE kmt_adapter_for_debug_ = 0;

  D3DKMT_HANDLE hDevice_ = 0;
  D3DKMT_HANDLE hContext_ = 0;
  D3DKMT_HANDLE hSyncObject_ = 0;

  volatile uint64_t* monitored_fence_value_ = nullptr;

  // Runtime-provided per-DMA-buffer private data for the current command buffer.
  //
  // The Win7 AeroGPU KMD requires this blob to be non-null on every Render/Present
  // submission. Header/interface revisions vary on where the pointer is exposed
  // (CreateContext vs Allocate/GetCommandBuffer vs in/out submit structs), so we
  // stash the latest observed value here as a fallback.
  void* dma_private_data_ = nullptr;
  UINT dma_private_data_bytes_ = 0;

  uint64_t last_submitted_fence_ = 0;
  uint64_t last_completed_fence_ = 0;
};

#else

class WddmSubmit {
 public:
  HRESULT Init(const void*, void*, void*, uint32_t = 0) {
    return E_NOTIMPL;
  }
  void Shutdown() {}
  uint32_t hDevice() const {
    return 0;
  }
  uint32_t hContext() const {
    return 0;
  }
  uint32_t hSyncObject() const {
    return 0;
  }
  HRESULT SubmitAeroCmdStream(const uint8_t*, size_t, bool, const WddmSubmitAllocation*, uint32_t, uint64_t*) {
    return E_NOTIMPL;
  }
  HRESULT WaitForFenceWithTimeout(uint64_t, uint32_t) {
    return E_NOTIMPL;
  }
  HRESULT WaitForFence(uint64_t) {
    return E_NOTIMPL;
  }
  uint64_t QueryCompletedFence() {
    return 0;
  }
};

#endif

} // namespace aerogpu::d3d10_11
