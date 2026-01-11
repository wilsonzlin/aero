#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

// Win7-era D3D UMD DDI headers (Win7 / WDDM 1.1).
#include <d3d10umddi.h>
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
  PRINT_OFF_OPT(D3DDDICB_RENDER, pCommandBuffer);
  PRINT_OFF_OPT(D3DDDICB_RENDER, CommandLength);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pAllocationList);
  PRINT_OFF_OPT(D3DDDICB_RENDER, AllocationListSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pPatchLocationList);
  PRINT_OFF_OPT(D3DDDICB_RENDER, PatchLocationListSize);
  PRINT_OFF_OPT(D3DDDICB_RENDER, pDmaBufferPrivateData);
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_RENDER::NewFenceValue) { PRINT_OFF(D3DDDICB_RENDER, NewFenceValue); }
  __if_exists(D3DDDICB_RENDER::NewCommandBufferSize) { PRINT_OFF(D3DDDICB_RENDER, NewCommandBufferSize); }
  __if_exists(D3DDDICB_RENDER::NewAllocationListSize) { PRINT_OFF(D3DDDICB_RENDER, NewAllocationListSize); }
  __if_exists(D3DDDICB_RENDER::NewPatchLocationListSize) { PRINT_OFF(D3DDDICB_RENDER, NewPatchLocationListSize); }
#endif
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_PRESENT);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, hContext);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pCommandBuffer);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, CommandLength);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pAllocationList);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, AllocationListSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pPatchLocationList);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, PatchLocationListSize);
  PRINT_OFF_OPT(D3DDDICB_PRESENT, pDmaBufferPrivateData);
#if defined(_MSC_VER)
  __if_exists(D3DDDICB_PRESENT::NewFenceValue) { PRINT_OFF(D3DDDICB_PRESENT, NewFenceValue); }
#endif
  PrintSeparator();

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
