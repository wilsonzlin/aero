#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

// Win7-era D3D UMD DDI headers (Win7 / WDDM 1.1).
#include <d3d10umddi.h>
#include <d3d10_1umddi.h>
#include <d3d11.h>
#include <d3d11umddi.h>
#include <d3dumddi.h>
#include <d3dkmthk.h>

static void PrintSeparator() {
  printf("------------------------------------------------------------\n");
}

#define PRINT_SIZE(T) printf("%-48s sizeof=%lu\n", #T, (unsigned long)sizeof(T))
#define PRINT_OFF(T, F) printf("  %-46s offsetof=%lu\n", #F, (unsigned long)offsetof(T, F))

#if defined(_MSC_VER)
  #define PRINT_OFF_OPT(T, F)                                                     \
    __if_exists(T::F) { PRINT_OFF(T, F); }                                        \
    __if_not_exists(T::F) {                                                       \
      printf("  %-46s offsetof=<n/a>\n", #F);                                      \
    }
#else
  // This probe is intended for MSVC/WDK builds; keep a simple fallback.
  #define PRINT_OFF_OPT(T, F) PRINT_OFF(T, F)
#endif

int main() {
  printf("AeroGPU Win7 WDK header/layout probe (arch=%s)\n", (sizeof(void*) == 8) ? "x64" : "x86");
  PrintSeparator();

  PRINT_SIZE(void*);
  PRINT_SIZE(D3DKMT_HANDLE);
  PrintSeparator();

  // Runtime callback table (function pointers) that the UMD uses for submission/sync.
  PRINT_SIZE(D3DDDI_DEVICECALLBACKS);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnCreateDeviceCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnDestroyDeviceCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb2);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnDestroyContextCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnDestroySynchronizationObjectCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnAllocateCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnDeallocateCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnGetCommandBufferCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnRenderCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnPresentCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnWaitForSynchronizationObjectCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnLockCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnUnlockCb);
  PRINT_OFF_OPT(D3DDDI_DEVICECALLBACKS, pfnSetErrorCb);
  PrintSeparator();

  // D3D10/11-specific callback wrappers (contain at least pfnSetErrorCb and may embed D3DDDI_DEVICECALLBACKS).
  PRINT_SIZE(D3D10DDI_DEVICECALLBACKS);
  PRINT_OFF_OPT(D3D10DDI_DEVICECALLBACKS, pfnLockCb);
  PRINT_OFF_OPT(D3D10DDI_DEVICECALLBACKS, pfnUnlockCb);
  PRINT_OFF_OPT(D3D10DDI_DEVICECALLBACKS, pfnSetErrorCb);
  PrintSeparator();

  PRINT_SIZE(D3D11DDI_DEVICECALLBACKS);
  PRINT_OFF_OPT(D3D11DDI_DEVICECALLBACKS, pfnLockCb);
  PRINT_OFF_OPT(D3D11DDI_DEVICECALLBACKS, pfnUnlockCb);
  PRINT_OFF_OPT(D3D11DDI_DEVICECALLBACKS, pfnSetErrorCb);
  PrintSeparator();

  // CreateDevice arg structs: where the runtime provides pCallbacks/pUMCallbacks.
#if defined(_MSC_VER)
  __if_exists(D3D10DDIARG_CREATEDEVICE) {
    PRINT_SIZE(D3D10DDIARG_CREATEDEVICE);
    PRINT_OFF_OPT(D3D10DDIARG_CREATEDEVICE, hRTDevice);
    PRINT_OFF_OPT(D3D10DDIARG_CREATEDEVICE, pCallbacks);
    PRINT_OFF_OPT(D3D10DDIARG_CREATEDEVICE, pUMCallbacks);
    PRINT_OFF_OPT(D3D10DDIARG_CREATEDEVICE, pDeviceFuncs);
    PrintSeparator();
  }
  __if_not_exists(D3D10DDIARG_CREATEDEVICE) {
    printf("%-48s <n/a>\n", "D3D10DDIARG_CREATEDEVICE");
    PrintSeparator();
  }

  __if_exists(D3D11DDIARG_CREATEDEVICE) {
    PRINT_SIZE(D3D11DDIARG_CREATEDEVICE);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, hRTDevice);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pCallbacks);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pDeviceCallbacks);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pUMCallbacks);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, hImmediateContext);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, hDeviceContext);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pDeviceFuncs);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pDeviceContextFuncs);
    PRINT_OFF_OPT(D3D11DDIARG_CREATEDEVICE, pImmediateContextFuncs);
    PrintSeparator();
  }
  __if_not_exists(D3D11DDIARG_CREATEDEVICE) {
    printf("%-48s <n/a>\n", "D3D11DDIARG_CREATEDEVICE");
    PrintSeparator();
  }
#endif

  // D3D11 adapter GetCaps / CheckFeatureSupport surfaces (helps keep pfnGetCaps implementations correct).
  PRINT_SIZE(D3D11DDIARG_GETCAPS);
  PRINT_OFF_OPT(D3D11DDIARG_GETCAPS, Type);
  PRINT_OFF_OPT(D3D11DDIARG_GETCAPS, pData);
  PRINT_OFF_OPT(D3D11DDIARG_GETCAPS, DataSize);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_THREADING", (unsigned)D3D11DDICAPS_TYPE_THREADING);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_DOUBLES", (unsigned)D3D11DDICAPS_TYPE_DOUBLES);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_FORMAT", (unsigned)D3D11DDICAPS_TYPE_FORMAT);
  printf("  %-46s value=%u\n", "D3D11_FEATURE_FORMAT_SUPPORT2", (unsigned)D3D11_FEATURE_FORMAT_SUPPORT2);
  printf("  %-46s value=%u\n",
         "D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS",
         (unsigned)D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_D3D11_OPTIONS", (unsigned)D3D11DDICAPS_TYPE_D3D11_OPTIONS);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_ARCHITECTURE_INFO", (unsigned)D3D11DDICAPS_TYPE_ARCHITECTURE_INFO);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_D3D9_OPTIONS", (unsigned)D3D11DDICAPS_TYPE_D3D9_OPTIONS);
  printf("  %-46s value=%u\n", "D3D11DDICAPS_TYPE_FEATURE_LEVELS", (unsigned)D3D11DDICAPS_TYPE_FEATURE_LEVELS);
  printf("  %-46s value=%u\n",
         "D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS",
         (unsigned)D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS);
  PrintSeparator();

  // API-facing CheckFeatureSupport structs (runtime forwards these to the UMD via GetCaps).
  PRINT_SIZE(D3D11_FEATURE_DATA_THREADING);
  PRINT_SIZE(D3D11_FEATURE_DATA_DOUBLES);
  PRINT_SIZE(D3D11_FEATURE_DATA_FORMAT_SUPPORT);
  PRINT_SIZE(D3D11_FEATURE_DATA_FORMAT_SUPPORT2);
  PRINT_SIZE(D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS);
  PRINT_SIZE(D3D11_FEATURE_DATA_D3D11_OPTIONS);
  PRINT_SIZE(D3D11_FEATURE_DATA_ARCHITECTURE_INFO);
  PRINT_SIZE(D3D11_FEATURE_DATA_D3D9_OPTIONS);
  PRINT_SIZE(D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS);
  PrintSeparator();

  // D3D10 / D3D10.1 adapter GetCaps surfaces (FORMAT_SUPPORT is in/out).
  PRINT_SIZE(D3D10DDIARG_GETCAPS);
  PRINT_OFF_OPT(D3D10DDIARG_GETCAPS, Type);
  PRINT_OFF_OPT(D3D10DDIARG_GETCAPS, pData);
  PRINT_OFF_OPT(D3D10DDIARG_GETCAPS, DataSize);
  printf("  %-46s value=%u\n",
         "D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL",
         (unsigned)D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL);
  printf("  %-46s value=%u\n", "D3D10DDICAPS_TYPE_FORMAT_SUPPORT", (unsigned)D3D10DDICAPS_TYPE_FORMAT_SUPPORT);
  PRINT_SIZE(D3D10DDIARG_FORMAT_SUPPORT);
  PRINT_OFF_OPT(D3D10DDIARG_FORMAT_SUPPORT, Format);
  PRINT_OFF_OPT(D3D10DDIARG_FORMAT_SUPPORT, FormatSupport);
  PRINT_OFF_OPT(D3D10DDIARG_FORMAT_SUPPORT, FormatSupport2);
  PrintSeparator();

  PRINT_SIZE(D3D10_1DDIARG_GETCAPS);
  PRINT_OFF_OPT(D3D10_1DDIARG_GETCAPS, Type);
  PRINT_OFF_OPT(D3D10_1DDIARG_GETCAPS, pData);
  PRINT_OFF_OPT(D3D10_1DDIARG_GETCAPS, DataSize);
  printf("  %-46s value=%u\n",
         "D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL",
         (unsigned)D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL);
  printf("  %-46s value=%u\n",
         "D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT",
         (unsigned)D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT);
  PRINT_SIZE(D3D10_1DDIARG_FORMAT_SUPPORT);
  PRINT_OFF_OPT(D3D10_1DDIARG_FORMAT_SUPPORT, Format);
  PRINT_OFF_OPT(D3D10_1DDIARG_FORMAT_SUPPORT, FormatSupport);
  PRINT_OFF_OPT(D3D10_1DDIARG_FORMAT_SUPPORT, FormatSupport2);
  PrintSeparator();

  // Device/context creation structs (hContext + hSyncObject + initial DMA buffers).
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_CREATEDEVICE) {
    PRINT_SIZE(D3DDDICB_CREATEDEVICE);
    PRINT_OFF_OPT(D3DDDICB_CREATEDEVICE, hAdapter);
    PRINT_OFF_OPT(D3DDDICB_CREATEDEVICE, hDevice);
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_CREATEDEVICE) {
    printf("%-48s <n/a>\n", "D3DDDICB_CREATEDEVICE");
    PrintSeparator();
  }

  __if_exists(D3DDDICB_CREATECONTEXT) {
    PRINT_SIZE(D3DDDICB_CREATECONTEXT);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, hDevice);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, NodeOrdinal);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, EngineAffinity);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, Flags);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, pPrivateDriverData);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, PrivateDriverDataSize);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, hContext);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, hSyncObject);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, pCommandBuffer);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, CommandBufferSize);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, pAllocationList);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, AllocationListSize);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, pPatchLocationList);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, PatchLocationListSize);
    // Some header revisions also expose per-DMA-buffer private data here.
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, pDmaBufferPrivateData);
    PRINT_OFF_OPT(D3DDDICB_CREATECONTEXT, DmaBufferPrivateDataSize);
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_CREATECONTEXT) {
    printf("%-48s <n/a>\n", "D3DDDICB_CREATECONTEXT");
    PrintSeparator();
  }
#endif

  // DMA buffer allocation/release structs (common Win7 D3D10/11 submission pattern).
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_ALLOCATE) {
    PRINT_SIZE(D3DDDICB_ALLOCATE);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, hContext);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, DmaBufferSize);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, CommandBufferSize);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, pDmaBuffer);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, pCommandBuffer);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, pAllocationList);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, AllocationListSize);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, pPatchLocationList);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, PatchLocationListSize);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, pDmaBufferPrivateData);
    PRINT_OFF_OPT(D3DDDICB_ALLOCATE, DmaBufferPrivateDataSize);
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_ALLOCATE) {
    printf("%-48s <n/a>\n", "D3DDDICB_ALLOCATE");
    PrintSeparator();
  }

  __if_exists(D3DDDICB_DEALLOCATE) {
    PRINT_SIZE(D3DDDICB_DEALLOCATE);
    PRINT_OFF_OPT(D3DDDICB_DEALLOCATE, pDmaBuffer);
    PRINT_OFF_OPT(D3DDDICB_DEALLOCATE, pCommandBuffer);
    PRINT_OFF_OPT(D3DDDICB_DEALLOCATE, pAllocationList);
    PRINT_OFF_OPT(D3DDDICB_DEALLOCATE, pPatchLocationList);
    PRINT_OFF_OPT(D3DDDICB_DEALLOCATE, pDmaBufferPrivateData);
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_DEALLOCATE) {
    printf("%-48s <n/a>\n", "D3DDDICB_DEALLOCATE");
    PrintSeparator();
  }
#endif

  // Core submission/wait CB structs used by D3D10/D3D11 UMDs on WDDM 1.1.
  PRINT_SIZE(D3DDDICB_GETCOMMANDINFO);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, hContext);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, pCommandBuffer);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, CommandBufferSize);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, pAllocationList);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, AllocationListSize);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, pPatchLocationList);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, PatchLocationListSize);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, pDmaBufferPrivateData);
  PRINT_OFF_OPT(D3DDDICB_GETCOMMANDINFO, DmaBufferPrivateDataSize);
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_RENDER);
  PRINT_OFF_OPT(D3DDDICB_RENDER, hContext);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pDmaBuffer);
  PRINT_OFF_OPT(D3DDDICB_RENDER, DmaBufferSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pCommandBuffer);
  PRINT_OFF_OPT(D3DDDICB_RENDER, CommandLength);
  PRINT_OFF_OPT(D3DDDICB_RENDER, CommandBufferSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pAllocationList);
  PRINT_OFF_OPT(D3DDDICB_RENDER, AllocationListSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pPatchLocationList);
  PRINT_OFF_OPT(D3DDDICB_RENDER, PatchLocationListSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pDmaBufferPrivateData);
  PRINT_OFF_OPT(D3DDDICB_RENDER, DmaBufferPrivateDataSize);
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_RENDER::SubmissionFenceId) { PRINT_OFF(D3DDDICB_RENDER, SubmissionFenceId); }
  __if_exists(D3DDDICB_RENDER::NewFenceValue) { PRINT_OFF(D3DDDICB_RENDER, NewFenceValue); }
  __if_exists(D3DDDICB_RENDER::NewCommandBufferSize) { PRINT_OFF(D3DDDICB_RENDER, NewCommandBufferSize); }
  __if_exists(D3DDDICB_RENDER::NewAllocationListSize) { PRINT_OFF(D3DDDICB_RENDER, NewAllocationListSize); }
  __if_exists(D3DDDICB_RENDER::NewPatchLocationListSize) { PRINT_OFF(D3DDDICB_RENDER, NewPatchLocationListSize); }
#endif
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_PRESENT);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, hContext);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pDmaBuffer);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, DmaBufferSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pCommandBuffer);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, CommandLength);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, CommandBufferSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pAllocationList);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, AllocationListSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pPatchLocationList);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, PatchLocationListSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pDmaBufferPrivateData);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, DmaBufferPrivateDataSize);
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_PRESENT::SubmissionFenceId) { PRINT_OFF(D3DDDICB_PRESENT, SubmissionFenceId); }
  __if_exists(D3DDDICB_PRESENT::NewFenceValue) { PRINT_OFF(D3DDDICB_PRESENT, NewFenceValue); }
#endif
  PrintSeparator();

  // Lock/unlock structs used by Map/Unmap paths (`pfnLockCb` / `pfnUnlockCb`).
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_LOCK) {
    PRINT_SIZE(D3DDDICB_LOCK);
    PRINT_OFF_OPT(D3DDDICB_LOCK, hAllocation);
    // Header revisions vary; probe common subresource field spellings.
    PRINT_OFF_OPT(D3DDDICB_LOCK, SubResourceIndex);
    PRINT_OFF_OPT(D3DDDICB_LOCK, SubresourceIndex);
    PRINT_OFF_OPT(D3DDDICB_LOCK, Flags);
    PRINT_OFF_OPT(D3DDDICB_LOCK, pData);
    PRINT_OFF_OPT(D3DDDICB_LOCK, Pitch);
    PRINT_OFF_OPT(D3DDDICB_LOCK, SlicePitch);
    __if_exists(D3DDDICB_LOCKFLAGS) {
      PRINT_SIZE(D3DDDICB_LOCKFLAGS);
      printf("  D3DDDICB_LOCKFLAGS members (header-dependent):\n");
      __if_exists(D3DDDICB_LOCKFLAGS::ReadOnly) { printf("    ReadOnly\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::ReadOnly) { printf("    ReadOnly <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) { printf("    WriteOnly\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::WriteOnly) { printf("    WriteOnly <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::Write) { printf("    Write\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::Write) { printf("    Write <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::Discard) { printf("    Discard\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::Discard) { printf("    Discard <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::NoOverwrite) { printf("    NoOverwrite\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::NoOverwrite) { printf("    NoOverwrite <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::DoNotWait) { printf("    DoNotWait\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::DoNotWait) { printf("    DoNotWait <n/a>\n"); }
      __if_exists(D3DDDICB_LOCKFLAGS::DonotWait) { printf("    DonotWait\n"); }
      __if_not_exists(D3DDDICB_LOCKFLAGS::DonotWait) { printf("    DonotWait <n/a>\n"); }
    }
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_LOCK) {
    printf("%-48s <n/a>\n", "D3DDDICB_LOCK");
    PrintSeparator();
  }

  __if_exists(D3DDDICB_UNLOCK) {
    PRINT_SIZE(D3DDDICB_UNLOCK);
    PRINT_OFF_OPT(D3DDDICB_UNLOCK, hAllocation);
    PRINT_OFF_OPT(D3DDDICB_UNLOCK, SubResourceIndex);
    PRINT_OFF_OPT(D3DDDICB_UNLOCK, SubresourceIndex);
    PrintSeparator();
  }
  __if_not_exists(D3DDDICB_UNLOCK) {
    printf("%-48s <n/a>\n", "D3DDDICB_UNLOCK");
    PrintSeparator();
  }
#endif

  PRINT_SIZE(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, hContext);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, ObjectCount);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, ObjectHandleArray);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, hSyncObjects);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, FenceValueArray);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, FenceValue);
  PRINT_OFF_OPT(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, Timeout);
  PrintSeparator();

  PRINT_SIZE(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, hAdapter);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, hContext);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, ObjectCount);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, ObjectHandleArray);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, hSyncObjects);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, FenceValueArray);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, FenceValue);
  PRINT_OFF_OPT(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, Timeout);
  PrintSeparator();

  printf("Done.\n");
  return 0;
}
