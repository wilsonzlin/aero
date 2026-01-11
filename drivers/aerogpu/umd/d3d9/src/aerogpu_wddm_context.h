#pragma once

#include <cstdint>

#include "../include/aerogpu_d3d9_umd.h"

#include "aerogpu_cmd.h"

namespace aerogpu {

// Win7/WDDM submission ABI surface.
//
// In portable builds we use clean-room definitions from
// `include/aerogpu_d3d9_umd.h`. In WDK builds, the real WDK types are used.
#if defined(_WIN32)
using WddmHandle = D3DKMT_HANDLE;
using WddmDeviceCallbacks = D3DDDI_DEVICECALLBACKS;
using WddmAllocationList = D3DDDI_ALLOCATIONLIST;
using WddmPatchLocationList = D3DDDI_PATCHLOCATIONLIST;
#else
using WddmHandle = uint32_t;
struct WddmAllocationList {};
struct WddmPatchLocationList {};
struct WddmDeviceCallbacks {};
#endif

struct WddmContext {
  WddmHandle hContext = 0;
  WddmHandle hSyncObject = 0;

  uint8_t* pCommandBuffer = nullptr;
  uint32_t CommandBufferSize = 0;

  WddmAllocationList* pAllocationList = nullptr;
  uint32_t AllocationListSize = 0; // entries (capacity)

  WddmPatchLocationList* pPatchLocationList = nullptr;
  uint32_t PatchLocationListSize = 0; // entries (capacity)

  // Runtime-provided per-DMA-buffer private data (WDDM).
  //
  // This memory is passed through the submission callbacks and is visible to the
  // KMD at DxgkDdiRender/DxgkDdiPresent time via `pDmaBufferPrivateData`. The
  // AeroGPU Win7 KMD uses it to tag submissions and associate per-submit metadata
  // (allocation tables) with the eventual DxgkDdiSubmitCommand call.
  void* pDmaBufferPrivateData = nullptr;
  uint32_t DmaBufferPrivateDataSize = 0; // bytes

  uint32_t command_buffer_bytes_used = 0;
  uint32_t allocation_list_entries_used = 0;
  uint32_t patch_location_entries_used = 0;

  void reset_submission_buffers();
  void destroy(const WddmDeviceCallbacks& callbacks);
};

#if defined(_WIN32)
HRESULT wddm_create_device(const WddmDeviceCallbacks& callbacks, void* hAdapter, WddmHandle* hDeviceOut);
void wddm_destroy_device(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice);

HRESULT wddm_create_context(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice, WddmContext* ctxOut);
#endif

} // namespace aerogpu
