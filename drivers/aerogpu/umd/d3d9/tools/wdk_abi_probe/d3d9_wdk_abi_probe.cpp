// AeroGPU D3D9 UMD - Win7 D3D9 ABI probe (WDK headers)
//
// Purpose
// -------
// This program is intended to be built in an environment that can compile against
// the Win7 D3D9 UMD DDI headers (typically from the Windows 7 WDK / 7600-era kit) to verify
// ABI-critical structure layouts and exported entrypoint decorations for the
// D3D9 user-mode driver.
//
// It is deliberately standalone and does not depend on any AeroGPU code.
//
// Output is a simple, copy-pastable table of:
//   - sizeof(type)
//   - offsetof(type, member) for a handful of high-value members
//   - x86 stdcall stack byte counts for exported entrypoints (=> @_N decoration)
//
// Note: This file is *not* built as part of the repo's normal toolchain.
//       See README.md in this directory for build steps.
//
// Build assumptions:
//   - MSVC (printf uses %Iu for size_t)
//   - C++03+ (no dependency on C++11)
//
// Headers:
//   - d3dumddi.h   (core UMD DDI + OpenAdapter arg structs)
//   - d3d9umddi.h  (D3D9-specific function table types)
//   - d3dkmthk.h   (D3DKMT handles used by some structs/callbacks)

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stddef.h>
#include <stdio.h>

#include <d3dkmthk.h>
#include <d3dumddi.h>
#include <d3d9umddi.h>

// MSVC-compatible printf format for size_t.
#if defined(_MSC_VER)
#define AEROGPU_PRIuSIZE "%Iu"
#else
#define AEROGPU_PRIuSIZE "%zu"
#endif

static void print_header(const char* title) {
  printf("\n== %s ==\n", title);
}

static void print_sizeof(const char* type_name, size_t size) {
  printf("sizeof(%s) = " AEROGPU_PRIuSIZE "\n", type_name, size);
}

static void print_offsetof(const char* type_name, const char* member_name, size_t off) {
  printf("  offsetof(%s, %s) = " AEROGPU_PRIuSIZE "\n", type_name, member_name, off);
}

static void print_offsetof_na(const char* type_name, const char* member_name) {
  printf("  offsetof(%s, %s) = <n/a>\n", type_name, member_name);
}

// -----------------------------------------------------------------------------
// Member presence detection (C++03 SFINAE)
// -----------------------------------------------------------------------------
// We want this probe to compile across minor header diffs (e.g. member renamed
// between WDDM 1.0/1.1). For each member we care about, we:
//   1) detect if the member exists, and
//   2) only compute offsetof if it does.
//
// This avoids hard compile failures and lets the output show "<n/a>" instead.

#define AEROGPU_DEFINE_HAS_MEMBER(member)                                                     \
  template <typename T>                                                                      \
  class aerogpu_has_member_##member {                                                        \
    typedef char yes[1];                                                                     \
    typedef char no[2];                                                                      \
                                                                                             \
    template <typename U>                                                                    \
    static yes& test(char (*)[sizeof(&U::member)]);                                          \
                                                                                             \
    template <typename U>                                                                    \
    static no& test(...);                                                                    \
                                                                                             \
   public:                                                                                   \
    enum { value = (sizeof(test<T>(0)) == sizeof(yes)) };                                    \
  };                                                                                         \
                                                                                             \
  template <typename T, bool Has>                                                            \
  struct aerogpu_print_offset_##member;                                                      \
                                                                                             \
  template <typename T>                                                                      \
  struct aerogpu_print_offset_##member<T, true> {                                            \
    static void run(const char* type_name) {                                                  \
      print_offsetof(type_name, #member, offsetof(T, member));                               \
    }                                                                                        \
  };                                                                                         \
                                                                                             \
  template <typename T>                                                                      \
  struct aerogpu_print_offset_##member<T, false> {                                           \
    static void run(const char* type_name) {                                                  \
      print_offsetof_na(type_name, #member);                                                  \
    }                                                                                        \
  };

#define AEROGPU_PRINT_MEMBER_OFFSET(Type, member)                                             \
  aerogpu_print_offset_##member<Type, aerogpu_has_member_##member<Type>::value>::run(#Type);

// Define the subset of members we want to probe across several structs.
// (Add more as the UMD implementation grows.)
AEROGPU_DEFINE_HAS_MEMBER(Interface)
AEROGPU_DEFINE_HAS_MEMBER(InterfaceVersion)
AEROGPU_DEFINE_HAS_MEMBER(Version)
AEROGPU_DEFINE_HAS_MEMBER(hAdapter)
AEROGPU_DEFINE_HAS_MEMBER(pAdapterCallbacks)
AEROGPU_DEFINE_HAS_MEMBER(pAdapterFuncs)

AEROGPU_DEFINE_HAS_MEMBER(pfnCloseAdapter)
AEROGPU_DEFINE_HAS_MEMBER(pfnGetCaps)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateDevice)
AEROGPU_DEFINE_HAS_MEMBER(pfnQueryAdapterInfo)

AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyDevice)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateResource)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyResource)
AEROGPU_DEFINE_HAS_MEMBER(pfnLock)
AEROGPU_DEFINE_HAS_MEMBER(pfnUnlock)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateSwapChain)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroySwapChain)
AEROGPU_DEFINE_HAS_MEMBER(pfnCheckDeviceState)
AEROGPU_DEFINE_HAS_MEMBER(pfnWaitForVBlank)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetGPUThreadPriority)
AEROGPU_DEFINE_HAS_MEMBER(pfnGetGPUThreadPriority)
AEROGPU_DEFINE_HAS_MEMBER(pfnCheckResourceResidency)
AEROGPU_DEFINE_HAS_MEMBER(pfnQueryResourceResidency)
AEROGPU_DEFINE_HAS_MEMBER(pfnGetDisplayModeEx)
AEROGPU_DEFINE_HAS_MEMBER(pfnComposeRects)
AEROGPU_DEFINE_HAS_MEMBER(pfnPresent)
AEROGPU_DEFINE_HAS_MEMBER(pfnPresentEx)
AEROGPU_DEFINE_HAS_MEMBER(pfnFlush)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateQuery)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyQuery)
AEROGPU_DEFINE_HAS_MEMBER(pfnIssueQuery)
AEROGPU_DEFINE_HAS_MEMBER(pfnGetQueryData)
AEROGPU_DEFINE_HAS_MEMBER(pfnWaitForIdle)
AEROGPU_DEFINE_HAS_MEMBER(pfnBlt)
AEROGPU_DEFINE_HAS_MEMBER(pfnColorFill)
AEROGPU_DEFINE_HAS_MEMBER(pfnUpdateSurface)
AEROGPU_DEFINE_HAS_MEMBER(pfnUpdateTexture)
AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource)
AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource2)

// Commonly hit by DWM and simple D3D9 apps (fixed function / legacy paths).
AEROGPU_DEFINE_HAS_MEMBER(pfnBeginScene)
AEROGPU_DEFINE_HAS_MEMBER(pfnEndScene)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetFVF)
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawPrimitive2)
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawIndexedPrimitive2)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTextureStageState)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTransform)
AEROGPU_DEFINE_HAS_MEMBER(pfnMultiplyTransform)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetClipPlane)

AEROGPU_DEFINE_HAS_MEMBER(pfnAllocateCb)
AEROGPU_DEFINE_HAS_MEMBER(pfnDeallocateCb)
AEROGPU_DEFINE_HAS_MEMBER(pfnSubmitCommandCb)
AEROGPU_DEFINE_HAS_MEMBER(pfnRenderCb)
AEROGPU_DEFINE_HAS_MEMBER(pfnPresentCb)

// Resource-related structs use different naming conventions across header sets.
// Probe both common spellings (Type/type, Width/width, etc.) so the output can
// tell us which one is present.
AEROGPU_DEFINE_HAS_MEMBER(Type)
AEROGPU_DEFINE_HAS_MEMBER(type)
AEROGPU_DEFINE_HAS_MEMBER(Format)
AEROGPU_DEFINE_HAS_MEMBER(format)
AEROGPU_DEFINE_HAS_MEMBER(Width)
AEROGPU_DEFINE_HAS_MEMBER(width)
AEROGPU_DEFINE_HAS_MEMBER(Height)
AEROGPU_DEFINE_HAS_MEMBER(height)
AEROGPU_DEFINE_HAS_MEMBER(Depth)
AEROGPU_DEFINE_HAS_MEMBER(depth)
AEROGPU_DEFINE_HAS_MEMBER(MipLevels)
AEROGPU_DEFINE_HAS_MEMBER(mip_levels)
AEROGPU_DEFINE_HAS_MEMBER(Usage)
AEROGPU_DEFINE_HAS_MEMBER(usage)
AEROGPU_DEFINE_HAS_MEMBER(Pool)
AEROGPU_DEFINE_HAS_MEMBER(pool)
AEROGPU_DEFINE_HAS_MEMBER(Size)
AEROGPU_DEFINE_HAS_MEMBER(size)
AEROGPU_DEFINE_HAS_MEMBER(hResource)
AEROGPU_DEFINE_HAS_MEMBER(pSharedHandle)
AEROGPU_DEFINE_HAS_MEMBER(hAllocation)
AEROGPU_DEFINE_HAS_MEMBER(hAllocations)
AEROGPU_DEFINE_HAS_MEMBER(pAllocations)

AEROGPU_DEFINE_HAS_MEMBER(hDevice)
AEROGPU_DEFINE_HAS_MEMBER(hContext)
AEROGPU_DEFINE_HAS_MEMBER(hSyncObject)
AEROGPU_DEFINE_HAS_MEMBER(NodeOrdinal)
AEROGPU_DEFINE_HAS_MEMBER(EngineAffinity)
AEROGPU_DEFINE_HAS_MEMBER(Flags)
AEROGPU_DEFINE_HAS_MEMBER(pPrivateDriverData)
AEROGPU_DEFINE_HAS_MEMBER(PrivateDriverDataSize)
AEROGPU_DEFINE_HAS_MEMBER(phAllocation)
AEROGPU_DEFINE_HAS_MEMBER(pOpenAllocationInfo)

AEROGPU_DEFINE_HAS_MEMBER(pCommandBuffer)
AEROGPU_DEFINE_HAS_MEMBER(CommandLength)
AEROGPU_DEFINE_HAS_MEMBER(CommandBufferSize)
AEROGPU_DEFINE_HAS_MEMBER(pAllocationList)
AEROGPU_DEFINE_HAS_MEMBER(AllocationListSize)
AEROGPU_DEFINE_HAS_MEMBER(pPatchLocationList)
AEROGPU_DEFINE_HAS_MEMBER(PatchLocationListSize)
AEROGPU_DEFINE_HAS_MEMBER(NumAllocations)
AEROGPU_DEFINE_HAS_MEMBER(NumPatchLocations)
AEROGPU_DEFINE_HAS_MEMBER(SubmissionFenceId)
AEROGPU_DEFINE_HAS_MEMBER(pSubmissionFenceId)
AEROGPU_DEFINE_HAS_MEMBER(pDmaBufferPrivateData)
AEROGPU_DEFINE_HAS_MEMBER(DmaBufferPrivateDataSize)
AEROGPU_DEFINE_HAS_MEMBER(pNewCommandBuffer)
AEROGPU_DEFINE_HAS_MEMBER(NewCommandBufferSize)
AEROGPU_DEFINE_HAS_MEMBER(pNewAllocationList)
AEROGPU_DEFINE_HAS_MEMBER(NewAllocationListSize)
AEROGPU_DEFINE_HAS_MEMBER(pNewPatchLocationList)
AEROGPU_DEFINE_HAS_MEMBER(NewPatchLocationListSize)

// -----------------------------------------------------------------------------
// x86 stdcall stack size computation for function pointer typedefs
// -----------------------------------------------------------------------------

#define AEROGPU_STACK_ROUND4(x) (((x) + 3) & ~static_cast<size_t>(3))

template <typename T>
struct aerogpu_stdcall_stack_bytes;

template <typename R>
struct aerogpu_stdcall_stack_bytes<R(__stdcall*)(void)> {
  static const size_t value = 0;
};

template <typename R, typename A1>
struct aerogpu_stdcall_stack_bytes<R(__stdcall*)(A1)> {
  static const size_t value = AEROGPU_STACK_ROUND4(sizeof(A1));
};

template <typename R, typename A1, typename A2>
struct aerogpu_stdcall_stack_bytes<R(__stdcall*)(A1, A2)> {
  static const size_t value = AEROGPU_STACK_ROUND4(sizeof(A1)) + AEROGPU_STACK_ROUND4(sizeof(A2));
};

template <typename R, typename A1, typename A2, typename A3>
struct aerogpu_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3)> {
  static const size_t value =
      AEROGPU_STACK_ROUND4(sizeof(A1)) + AEROGPU_STACK_ROUND4(sizeof(A2)) + AEROGPU_STACK_ROUND4(sizeof(A3));
};

template <typename R, typename A1, typename A2, typename A3, typename A4>
struct aerogpu_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3, A4)> {
  static const size_t value = AEROGPU_STACK_ROUND4(sizeof(A1)) + AEROGPU_STACK_ROUND4(sizeof(A2)) +
                              AEROGPU_STACK_ROUND4(sizeof(A3)) + AEROGPU_STACK_ROUND4(sizeof(A4));
};

// -----------------------------------------------------------------------------
// Probes
// -----------------------------------------------------------------------------

static void probe_openadapter_structs() {
  print_header("OpenAdapter arg structs");

  print_sizeof("D3DDDIARG_OPENADAPTER", sizeof(D3DDDIARG_OPENADAPTER));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, Interface)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, InterfaceVersion)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, Version)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, hAdapter)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, pAdapterCallbacks)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER, pAdapterFuncs)

  // Not all WDKs expose OpenAdapter2; if the type is missing this file will not
  // compile. For the Win7 D3D9 UMD header set, it is expected to exist.
  print_sizeof("D3DDDIARG_OPENADAPTER2", sizeof(D3DDDIARG_OPENADAPTER2));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, Interface)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, InterfaceVersion)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, Version)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, hAdapter)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, pAdapterCallbacks)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_OPENADAPTER2, pAdapterFuncs)
}

static void probe_function_tables() {
  print_header("Function tables");

  print_sizeof("D3D9DDI_ADAPTERFUNCS", sizeof(D3D9DDI_ADAPTERFUNCS));
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_ADAPTERFUNCS, pfnCloseAdapter)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_ADAPTERFUNCS, pfnGetCaps)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_ADAPTERFUNCS, pfnCreateDevice)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_ADAPTERFUNCS, pfnQueryAdapterInfo)

  print_sizeof("D3D9DDI_DEVICEFUNCS", sizeof(D3D9DDI_DEVICEFUNCS));
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDestroyDevice)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnCreateResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDestroyResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnLock)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnUnlock)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnCreateSwapChain)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDestroySwapChain)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnCheckDeviceState)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnWaitForVBlank)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnSetGPUThreadPriority)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnGetGPUThreadPriority)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnCheckResourceResidency)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnQueryResourceResidency)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnGetDisplayModeEx)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnComposeRects)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnPresent)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnPresentEx)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnFlush)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnCreateQuery)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDestroyQuery)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnIssueQuery)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnGetQueryData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnWaitForIdle)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnBlt)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnColorFill)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnUpdateSurface)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnUpdateTexture)

  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnBeginScene)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnEndScene)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnSetFVF)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDrawPrimitive2)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnDrawIndexedPrimitive2)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnSetTextureStageState)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnSetTransform)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnMultiplyTransform)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnSetClipPlane)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnOpenResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDI_DEVICEFUNCS, pfnOpenResource2)
}

static void probe_openresource_structs() {
  print_header("OpenResource arg structs");

  print_sizeof("D3D9DDIARG_OPENRESOURCE", sizeof(D3D9DDIARG_OPENRESOURCE));
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hAllocation)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, phAllocation)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, pOpenAllocationInfo)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, NumAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize)
}

static void probe_device_callbacks() {
  print_header("Runtime callback tables");

  print_sizeof("D3DDDI_DEVICECALLBACKS", sizeof(D3DDDI_DEVICECALLBACKS));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDI_DEVICECALLBACKS, pfnAllocateCb)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDI_DEVICECALLBACKS, pfnDeallocateCb)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDI_DEVICECALLBACKS, pfnSubmitCommandCb)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDI_DEVICECALLBACKS, pfnRenderCb)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDI_DEVICECALLBACKS, pfnPresentCb)
}

static void probe_submit_structs() {
  print_header("Submission-related structs");

  print_sizeof("D3DDDIARG_CREATECONTEXT", sizeof(D3DDDIARG_CREATECONTEXT));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, hDevice)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, NodeOrdinal)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, EngineAffinity)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, Flags)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, hContext)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, hSyncObject)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, pPrivateDriverData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, PrivateDriverDataSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, pCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, CommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, pAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, AllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, pPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, PatchLocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, pDmaBufferPrivateData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_CREATECONTEXT, DmaBufferPrivateDataSize)

  print_sizeof("D3DDDIARG_SUBMITCOMMAND", sizeof(D3DDDIARG_SUBMITCOMMAND));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, hContext)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, CommandLength)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, pAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, AllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, pDmaBufferPrivateData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, DmaBufferPrivateDataSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDIARG_SUBMITCOMMAND, SubmissionFenceId)
}

static void probe_submit_callbacks() {
  print_header("Submission callback structs");

  print_sizeof("D3DDDICB_RENDER", sizeof(D3DDDICB_RENDER));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, hContext)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, CommandLength)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, CommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, AllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, NumAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, PatchLocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, NumPatchLocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pDmaBufferPrivateData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, DmaBufferPrivateDataSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, SubmissionFenceId)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pSubmissionFenceId)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pNewCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, NewCommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pNewAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, NewAllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, pNewPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_RENDER, NewPatchLocationListSize)

  print_sizeof("D3DDDICB_PRESENT", sizeof(D3DDDICB_PRESENT));
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, hContext)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, CommandLength)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, CommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, AllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, NumAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, PatchLocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, NumPatchLocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pDmaBufferPrivateData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, DmaBufferPrivateDataSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, SubmissionFenceId)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pSubmissionFenceId)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pNewCommandBuffer)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, NewCommandBufferSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pNewAllocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, NewAllocationListSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, pNewPatchLocationList)
  AEROGPU_PRINT_MEMBER_OFFSET(D3DDDICB_PRESENT, NewPatchLocationListSize)
}

static void probe_resource_structs() {
  print_header("Resource-related structs");

  print_sizeof("D3D9DDIARG_CREATERESOURCE", sizeof(D3D9DDIARG_CREATERESOURCE));
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Type)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, type)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Format)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, format)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Width)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, width)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Height)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, height)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Depth)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, depth)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, MipLevels)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, mip_levels)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Usage)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, usage)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Pool)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, pool)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, Size)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, size)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, hResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, pSharedHandle)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, NumAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, hAllocation)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, hAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, pAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, pPrivateDriverData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize)

  print_sizeof("D3D9DDIARG_OPENRESOURCE", sizeof(D3D9DDIARG_OPENRESOURCE));
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hResource)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, NumAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hAllocation)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, hAllocations)
  AEROGPU_PRINT_MEMBER_OFFSET(D3D9DDIARG_OPENRESOURCE, pAllocations)
}

static void probe_export_decorations() {
  print_header("Exported entrypoints (x86 stdcall decoration)");

#if defined(_M_IX86)
  const size_t openadapter_stack = aerogpu_stdcall_stack_bytes<PFND3DDDI_OPENADAPTER>::value;
  const size_t openadapter2_stack = aerogpu_stdcall_stack_bytes<PFND3DDDI_OPENADAPTER2>::value;
  const size_t openadapter_from_hdc_stack = aerogpu_stdcall_stack_bytes<PFND3DDDI_OPENADAPTERFROMHDC>::value;
  const size_t openadapter_from_luid_stack = aerogpu_stdcall_stack_bytes<PFND3DDDI_OPENADAPTERFROMLUID>::value;

  printf("PFND3DDDI_OPENADAPTER  => _OpenAdapter@" AEROGPU_PRIuSIZE "\n", openadapter_stack);
  printf("PFND3DDDI_OPENADAPTER2 => _OpenAdapter2@" AEROGPU_PRIuSIZE "\n", openadapter2_stack);
  printf("PFND3DDDI_OPENADAPTERFROMHDC  => _OpenAdapterFromHdc@" AEROGPU_PRIuSIZE "\n", openadapter_from_hdc_stack);
  printf("PFND3DDDI_OPENADAPTERFROMLUID => _OpenAdapterFromLuid@" AEROGPU_PRIuSIZE "\n", openadapter_from_luid_stack);
#else
  printf("(x64 build: Win64 has no stdcall @N decoration; use dumpbin to verify exports)\n");
#endif
}

int main() {
  printf("AeroGPU D3D9 WDK ABI probe\n");

#if defined(_MSC_VER)
  printf("_MSC_VER = %d\n", _MSC_VER);
#endif

#if defined(_M_IX86)
  printf("arch = x86\n");
#elif defined(_M_X64)
  printf("arch = x64\n");
#elif defined(_M_ARM64)
  printf("arch = arm64 (unsupported for Win7)\n");
#else
  printf("arch = (unknown)\n");
#endif

  printf("sizeof(void*) = " AEROGPU_PRIuSIZE "\n", sizeof(void*));

#ifdef D3D_UMD_INTERFACE_VERSION
  printf("D3D_UMD_INTERFACE_VERSION = %u\n", static_cast<unsigned>(D3D_UMD_INTERFACE_VERSION));
#endif

  probe_export_decorations();
  probe_openadapter_structs();
  probe_function_tables();
  probe_openresource_structs();
  probe_device_callbacks();
  probe_submit_structs();
  probe_submit_callbacks();
  probe_resource_structs();

  return 0;
}
