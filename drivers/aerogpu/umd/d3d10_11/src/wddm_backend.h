// WDDM backend for AeroGPU D3D10/11 UMD.
//
// This backend is responsible for getting AeroGPU DMA buffers submitted through
// the real Win7 WDDM path (dxgkrnl -> AeroGPU KMD) rather than completing fences
// in-process.
//
// The repository can be built without the WDK headers; in that configuration we
// fall back to a lightweight in-process fence.
//
// NOTE: The Win7 WDK surface has multiple callback tables:
// - D3D10/11 runtime callbacks (error reporting, etc.)
// - D3DDDI callbacks (DMA buffer allocation + Render/Present submission)
// This module only depends on the D3DDDI callbacks for submission.
//
// Logging:
//   Define `AEROGPU_D3D10_11_UMD_LOG=1` to enable OutputDebugStringA logging.

#pragma once

#include <condition_variable>
#include <cstdint>
#include <mutex>
#include <vector>

#include "../include/aerogpu_d3d10_11_umd.h"

#if defined(_WIN32)
  #include <windows.h>
#endif

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dkmthk.h>
  // Forward declared here to avoid including <d3dumddi.h> in non-UMD code.
  struct D3DDDI_DEVICECALLBACKS;
#endif

#ifndef AEROGPU_D3D10_11_UMD_LOG
  #define AEROGPU_D3D10_11_UMD_LOG 0
#endif

namespace aerogpu::wddm {

// WDDM handles are 32-bit values (D3DKMT_HANDLE). Keep the public surface of the
// backend WOW64-safe by representing them as uint32_t even in x64 builds.
using AllocationHandle = uint32_t;

// WDDM kernel object handles (device/context/sync object) are also D3DKMT_HANDLE,
// i.e. 32-bit. Represent them as uint32_t for WOW64 correctness.
using KernelHandle = uint32_t;

struct SubmissionAlloc {
  AllocationHandle hAllocation = 0;
  bool write = false;
};

struct LockedRange {
  void* data = nullptr;
  uint32_t row_pitch = 0;
  uint32_t depth_pitch = 0;
};

class Backend {
 public:
  Backend() = default;
  ~Backend();

  Backend(const Backend&) = delete;
  Backend& operator=(const Backend&) = delete;

  void reset();

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  HRESULT InitFromD3D10CreateDevice(D3D10DDI_HADAPTER hAdapter,
                                    const D3D10DDIARG_CREATEDEVICE& args);
  HRESULT InitFromD3D11CreateDevice(D3D10DDI_HADAPTER hAdapter,
                                    const D3D11DDIARG_CREATEDEVICE& args);
#endif

  // Submission helpers.
  HRESULT SubmitRender(const void* cmd, size_t cmd_size,
                       const SubmissionAlloc* allocs, size_t alloc_count,
                       uint64_t* fence_out);
  HRESULT SubmitPresent(const void* cmd, size_t cmd_size,
                        const SubmissionAlloc* allocs, size_t alloc_count,
                        uint64_t* fence_out);

  // Fence wait helper. A timeout of INFINITE is allowed on Windows.
  HRESULT WaitForFence(uint64_t fence_value, uint32_t timeout_ms);

  struct AllocationDesc {
    uint64_t size_bytes = 0;
    bool cpu_visible = true;
    bool primary = false;
    bool render_target = false;
    bool shared = false;
  };

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Resource allocation helpers (CreateResource / DestroyResource).
  //
  // The runtime associates allocations with an `hRTResource` handle. The UMD is
  // responsible for providing an AeroGPU allocation-private-data blob that
  // defines a stable `alloc_id` for host-visible guest-backed resources.
  HRESULT CreateAllocation(D3D10DDI_HRTRESOURCE hrt_resource,
                           const AllocationDesc& desc,
                           AllocationHandle* out_handle,
                           KernelHandle* out_km_resource,
                           uint32_t* out_alloc_id,
                           uint64_t* out_share_token,
                           HANDLE* out_shared_handle);

  HRESULT DestroyAllocation(D3D10DDI_HRTRESOURCE hrt_resource,
                            KernelHandle km_resource,
                            AllocationHandle handle);
#else
  // Portable build stubs.
  HRESULT CreateAllocation(uint64_t size_bytes, AllocationHandle* out_handle);
  HRESULT DestroyAllocation(AllocationHandle handle);
#endif

  HRESULT LockAllocation(AllocationHandle handle,
                         uint64_t offset_bytes,
                         uint64_t size_bytes,
                         bool read_only,
                         bool do_not_wait,
                         bool discard,
                         bool no_overwrite,
                         LockedRange* out);
  HRESULT UnlockAllocation(AllocationHandle handle);

  uint64_t last_submitted_fence() const { return last_submitted_fence_; }
  uint64_t last_completed_fence() const { return last_completed_fence_; }
  KernelHandle hContext() const { return km_context_; }
  KernelHandle hSyncObject() const { return km_sync_object_; }

 private:
  HRESULT SubmitInternal(bool present,
                         const void* cmd, size_t cmd_size,
                         const SubmissionAlloc* allocs, size_t alloc_count,
                         uint64_t* fence_out);

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Runtime device handles and WDDM submission callbacks captured during
  // CreateDevice.
  void* adapter_handle_ = nullptr; // passed to CreateDeviceCb
  D3D11DDI_HRTDEVICE hrt_device11_{};
  D3D10DDI_HRTDEVICE hrt_device10_{};
  const D3DDDI_DEVICECALLBACKS* ddi_callbacks_ = nullptr;

  // Kernel submission objects.
  D3DKMT_HANDLE km_device_ = 0;
  D3DKMT_HANDLE km_context_ = 0;
  D3DKMT_HANDLE km_sync_object_ = 0;
#endif

  uint64_t last_submitted_fence_ = 0;
  uint64_t last_completed_fence_ = 0;

  // Stub build synchronization.
  std::mutex* stub_mutex_ = nullptr;
  std::condition_variable* stub_cv_ = nullptr;
};

}  // namespace aerogpu::wddm
