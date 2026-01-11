#pragma once

#include <cstdint>

#include "../include/aerogpu_d3d9_umd.h"

#include "aerogpu_cmd.h"

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  #include <d3dumddi.h>
#endif

namespace aerogpu {

// WDDM plumbing is only available when building against the Win7 WDK DDI headers.
// Repository/CI builds do not have access to those headers, so we provide a tiny
// stub surface that keeps the UMD self-contained.
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
using WddmHandle = D3DKMT_HANDLE;
using WddmDeviceCallbacks = D3DDDI_DEVICECALLBACKS;
using WddmAllocationList = D3DDDI_ALLOCATIONLIST;
using WddmPatchLocationList = D3DDDI_PATCHLOCATIONLIST;
#else
using WddmHandle = uint32_t;

struct WddmAllocationList {};
struct WddmPatchLocationList {};

// Compat-only placeholder. The real WDK type contains many more callbacks.
struct WddmDeviceCallbacks {};
#endif

struct WddmContext {
  WddmHandle hContext = 0;
  WddmHandle hSyncObject = 0;

  uint8_t* pCommandBuffer = nullptr;
  uint32_t CommandBufferSize = 0;

  WddmAllocationList* pAllocationList = nullptr;
  uint32_t AllocationListSize = 0; // entries

  WddmPatchLocationList* pPatchLocationList = nullptr;
  uint32_t PatchLocationListSize = 0; // entries

  uint32_t command_buffer_bytes_used = 0;
  uint32_t allocation_list_entries_used = 0;
  uint32_t patch_location_entries_used = 0;

  void reset_submission_buffers();
  void destroy(const WddmDeviceCallbacks& callbacks);
};

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
HRESULT wddm_create_device(const WddmDeviceCallbacks& callbacks, void* hAdapter, WddmHandle* hDeviceOut);
void wddm_destroy_device(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice);

HRESULT wddm_create_context(const WddmDeviceCallbacks& callbacks, WddmHandle hDevice, WddmContext* ctxOut);
#endif

} // namespace aerogpu
