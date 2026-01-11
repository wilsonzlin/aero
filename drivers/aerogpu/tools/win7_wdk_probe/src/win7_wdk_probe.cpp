#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

// WDK 7.1 headers (Win7 / WDDM 1.1).
#include <d3d10umddi.h>
#include <d3d11umddi.h>
#include <d3dumddi.h>
#include <d3dkmthk.h>

static void PrintSeparator() {
  printf("------------------------------------------------------------\n");
}

#define PRINT_SIZE(T) printf("%-48s sizeof=%lu\n", #T, (unsigned long)sizeof(T))
#define PRINT_OFF(T, F) printf("  %-46s offsetof=%lu\n", #F, (unsigned long)offsetof(T, F))

int main() {
  printf("AeroGPU Win7 WDK 7.1 probe (arch=%s)\n", (sizeof(void*) == 8) ? "x64" : "x86");
  PrintSeparator();

  PRINT_SIZE(void*);
  PRINT_SIZE(D3DKMT_HANDLE);
  PrintSeparator();

  // Core submission/wait CB structs used by D3D10/D3D11 UMDs on WDDM 1.1.
  PRINT_SIZE(D3DDDICB_GETCOMMANDINFO);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, hContext);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, pCommandBuffer);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, CommandBufferSize);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, pAllocationList);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, AllocationListSize);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, pPatchLocationList);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, PatchLocationListSize);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, pDmaBufferPrivateData);
  PRINT_OFF(D3DDDICB_GETCOMMANDINFO, DmaBufferPrivateDataSize);
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_RENDER);
  PRINT_OFF(D3DDDICB_RENDER, hContext);
  PRINT_OFF(D3DDDICB_RENDER, pCommandBuffer);
  PRINT_OFF(D3DDDICB_RENDER, CommandLength);
  PRINT_OFF(D3DDDICB_RENDER, pAllocationList);
  PRINT_OFF(D3DDDICB_RENDER, AllocationListSize);
  PRINT_OFF(D3DDDICB_RENDER, pPatchLocationList);
  PRINT_OFF(D3DDDICB_RENDER, PatchLocationListSize);
  PRINT_OFF(D3DDDICB_RENDER, pDmaBufferPrivateData);
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_PRESENT);
  PRINT_OFF(D3DDDICB_PRESENT, hContext);
  PRINT_OFF(D3DDDICB_PRESENT, pCommandBuffer);
  PRINT_OFF(D3DDDICB_PRESENT, CommandLength);
  PRINT_OFF(D3DDDICB_PRESENT, pAllocationList);
  PRINT_OFF(D3DDDICB_PRESENT, AllocationListSize);
  PRINT_OFF(D3DDDICB_PRESENT, pPatchLocationList);
  PRINT_OFF(D3DDDICB_PRESENT, PatchLocationListSize);
  PRINT_OFF(D3DDDICB_PRESENT, pDmaBufferPrivateData);
  PrintSeparator();

  PRINT_SIZE(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT);
  PRINT_OFF(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, hContext);
  PRINT_OFF(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, ObjectCount);
  PRINT_OFF(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, hSyncObjects);
  PRINT_OFF(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, FenceValue);
  PRINT_OFF(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT, Timeout);
  PrintSeparator();

  PRINT_SIZE(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT);
  PRINT_OFF(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, hContext);
  PRINT_OFF(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, ObjectCount);
  PRINT_OFF(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, hSyncObjects);
  PRINT_OFF(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, FenceValue);
  PRINT_OFF(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT, Timeout);
  PrintSeparator();

  printf("Done.\n");
  return 0;
}

