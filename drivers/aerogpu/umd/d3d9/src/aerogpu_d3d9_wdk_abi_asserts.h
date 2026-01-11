// Optional compile-time ABI assertions for Win7 D3D9 UMD builds against the WDK headers.
//
// This header is intentionally a no-op unless you are building the UMD against
// the *real* WDK D3D headers (d3dumddi.h / d3d9umddi.h). The repository build
// uses a small "compat" DDI surface and does not ship the WDK headers.
//
// Usage (WDK build only)
// ----------------------
// 1) Define `AEROGPU_D3D9_USE_WDK_DDI` in your WDK build.
// 2) Include this header in a translation unit after the WDK headers are
//    available on the include path.
// 3) Optionally define one or more `AEROGPU_D3D9_WDK_ABI_EXPECT_*` macros (see
//    below) using values captured from the probe tool:
//      drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/
//
// The intent is to "freeze" ABI-critical sizes/offsets/entrypoint decorations so
// future header/toolchain drift is caught at compile time.

#pragma once

#if !defined(AEROGPU_D3D9_USE_WDK_DDI)
// Repo-local builds do not have the WDK headers; keep this header inert.
#else

#include <stddef.h>

#include <d3dkmthk.h>
#include <d3dumddi.h>
#include <d3d9umddi.h>

// Pull in AeroGPU's portable D3D9UMDDI surface so we can assert that the
// `AEROGPU_D3D9DDIARG_*` structs (used by the translation layer) remain prefix
// ABI compatible with the WDK structs when we forward pointers across the DDI.
#include "../include/aerogpu_d3d9_umd.h"

// -----------------------------------------------------------------------------
// Compile-time assertion (C/C++, C++03-safe)
// -----------------------------------------------------------------------------
// Some Win7-targeted toolchains may be older than C++11; avoid relying on `static_assert`.

#if defined(__cplusplus)
#define AEROGPU_ABI_STATIC_ASSERT(expr, msg) \
  typedef char aerogpu_abi_static_assert_##__LINE__[(expr) ? 1 : -1]
#else
  #ifndef C_ASSERT
    #define C_ASSERT(expr) typedef char aerogpu_c_assert_##__LINE__[(expr) ? 1 : -1]
  #endif
  #define AEROGPU_ABI_STATIC_ASSERT(expr, msg) C_ASSERT(expr)
#endif

// -----------------------------------------------------------------------------
// x86 stdcall stack byte computation for function pointer typedefs
// -----------------------------------------------------------------------------
// This is useful for validating that x86 exports match their `.def` stack sizes
// (e.g. `_OpenAdapter@4` vs `_OpenAdapter@8`).

#define AEROGPU_ABI_STACK_ROUND4(x) (((x) + 3) & ~((size_t)3))

#if defined(__cplusplus)

template <typename T>
struct aerogpu_abi_stdcall_stack_bytes;

template <typename R>
struct aerogpu_abi_stdcall_stack_bytes<R(__stdcall*)(void)> {
  static const size_t value = 0;
};

template <typename R, typename A1>
struct aerogpu_abi_stdcall_stack_bytes<R(__stdcall*)(A1)> {
  static const size_t value = AEROGPU_ABI_STACK_ROUND4(sizeof(A1));
};

template <typename R, typename A1, typename A2>
struct aerogpu_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2)> {
  static const size_t value = AEROGPU_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_ABI_STACK_ROUND4(sizeof(A2));
};

template <typename R, typename A1, typename A2, typename A3>
struct aerogpu_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3)> {
  static const size_t value =
      AEROGPU_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_ABI_STACK_ROUND4(sizeof(A2)) + AEROGPU_ABI_STACK_ROUND4(sizeof(A3));
};

template <typename R, typename A1, typename A2, typename A3, typename A4>
struct aerogpu_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3, A4)> {
  static const size_t value = AEROGPU_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_ABI_STACK_ROUND4(sizeof(A2)) +
                              AEROGPU_ABI_STACK_ROUND4(sizeof(A3)) + AEROGPU_ABI_STACK_ROUND4(sizeof(A4));
};

#endif // __cplusplus

// -----------------------------------------------------------------------------
// Optional expected-value checks (define macros to enable)
// -----------------------------------------------------------------------------

// Examples (x86):
//   /DAEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER_STDCALL_BYTES=4
//   /DAEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER2_STDCALL_BYTES=4
//
// Examples (both arches):
//   /DAEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER=...
//   /DAEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterFuncs=...

#if defined(_M_IX86) && defined(__cplusplus)
  #if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER_STDCALL_BYTES)
AEROGPU_ABI_STATIC_ASSERT(
    aerogpu_abi_stdcall_stack_bytes<PFND3DDDI_OPENADAPTER>::value == AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapter do not match expected value");
  #endif

  #if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER2_STDCALL_BYTES)
AEROGPU_ABI_STATIC_ASSERT(
    aerogpu_abi_stdcall_stack_bytes<PFND3DDDI_OPENADAPTER2>::value == AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER2_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapter2 do not match expected value");
  #endif

  #if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMHDC_STDCALL_BYTES)
AEROGPU_ABI_STATIC_ASSERT(
    aerogpu_abi_stdcall_stack_bytes<PFND3DDDI_OPENADAPTERFROMHDC>::value ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMHDC_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapterFromHdc do not match expected value");
  #endif

  #if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMLUID_STDCALL_BYTES)
AEROGPU_ABI_STATIC_ASSERT(
    aerogpu_abi_stdcall_stack_bytes<PFND3DDDI_OPENADAPTERFROMLUID>::value ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMLUID_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapterFromLuid do not match expected value");
  #endif
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDIARG_OPENADAPTER) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER,
    "sizeof(D3DDDIARG_OPENADAPTER) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterFuncs)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_OPENADAPTER, pAdapterFuncs) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterFuncs,
    "offsetof(D3DDDIARG_OPENADAPTER, pAdapterFuncs) does not match expected value");
#endif

// -----------------------------------------------------------------------------
// D3D9UMDDI device arg structs (Win7 D3D9 runtime -> UMD)
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATEDEVICE)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_CREATEDEVICE) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATEDEVICE,
    "sizeof(D3D9DDIARG_CREATEDEVICE) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_pCallbacks)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATEDEVICE, pCallbacks) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_pCallbacks,
    "offsetof(D3D9DDIARG_CREATEDEVICE, pCallbacks) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATERESOURCE)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_CREATERESOURCE) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATERESOURCE,
    "sizeof(D3D9DDIARG_CREATERESOURCE) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_CREATERESOURCE) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATERESOURCE,
    "AEROGPU_D3D9DDIARG_CREATERESOURCE must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Type)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Type,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Type) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Type,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, type) does not match expected WDK Type offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Format)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Format,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Format) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Format,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, format) does not match expected WDK Format offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Width)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Width,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Width) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Width,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, width) does not match expected WDK Width offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Height)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Height,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Height) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Height,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, height) does not match expected WDK Height offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Depth)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Depth,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Depth) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Depth,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, depth) does not match expected WDK Depth offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_MipLevels)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, MipLevels) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_MipLevels,
    "offsetof(D3D9DDIARG_CREATERESOURCE, MipLevels) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, mip_levels) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_MipLevels,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, mip_levels) does not match expected WDK MipLevels offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Usage)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Usage,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Usage) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Usage,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, usage) does not match expected WDK Usage offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Pool)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Pool) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Pool,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Pool) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pool) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Pool,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pool) does not match expected WDK Pool offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Size)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Size,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Size) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Size,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, size) does not match expected WDK Size offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hResource,
    "offsetof(D3D9DDIARG_CREATERESOURCE, hResource) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hResource,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, hResource) does not match expected WDK hResource offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pSharedHandle)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, pSharedHandle) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pSharedHandle,
    "offsetof(D3D9DDIARG_CREATERESOURCE, pSharedHandle) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pSharedHandle) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pSharedHandle,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pSharedHandle) does not match expected WDK pSharedHandle offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pPrivateDriverData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pPrivateDriverData,
    "offsetof(D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pPrivateDriverData,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) does not match expected WDK pPrivateDriverData offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_PrivateDriverDataSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_PrivateDriverDataSize,
    "offsetof(D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_PrivateDriverDataSize,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) does not match expected WDK PrivateDriverDataSize offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hAllocation)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hAllocation,
    "offsetof(D3D9DDIARG_CREATERESOURCE, hAllocation) does not match expected value");
// AeroGPU uses `wddm_hAllocation` to mirror the WDDM allocation handle for the
// backing store.
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, wddm_hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hAllocation,
    "offsetof(AEROGPU_D3D9DDIARG_CREATERESOURCE, wddm_hAllocation) does not match expected WDK hAllocation offset");
// Ensure the member types are size-compatible so prefix memcpy-based forwarding
// stays well-defined on both x86 and x64.
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(((D3D9DDIARG_CREATERESOURCE*)0)->hAllocation) ==
        sizeof(((AEROGPU_D3D9DDIARG_CREATERESOURCE*)0)->wddm_hAllocation),
    "D3D9DDIARG_CREATERESOURCE::hAllocation type size does not match AeroGPU wddm_hAllocation");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_OPENRESOURCE) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE,
    "sizeof(D3D9DDIARG_OPENRESOURCE) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_OPENRESOURCE) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE,
    "AEROGPU_D3D9DDIARG_OPENRESOURCE must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_pPrivateDriverData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_pPrivateDriverData,
    "offsetof(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_pPrivateDriverData,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) does not match expected WDK pPrivateDriverData offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_PrivateDriverDataSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_PrivateDriverDataSize,
    "offsetof(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, private_driver_data_size) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_PrivateDriverDataSize,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, private_driver_data_size) does not match expected WDK PrivateDriverDataSize offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hAllocation)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hAllocation,
    "offsetof(D3D9DDIARG_OPENRESOURCE, hAllocation) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, wddm_hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hAllocation,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, wddm_hAllocation) does not match expected WDK hAllocation offset");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(((D3D9DDIARG_OPENRESOURCE*)0)->hAllocation) == sizeof(((AEROGPU_D3D9DDIARG_OPENRESOURCE*)0)->wddm_hAllocation),
    "D3D9DDIARG_OPENRESOURCE::hAllocation type size does not match AeroGPU wddm_hAllocation");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Type)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Type,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Type) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Type,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, type) does not match expected WDK Type offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Format)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Format,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Format) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Format,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, format) does not match expected WDK Format offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Width)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Width,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Width) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Width,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, width) does not match expected WDK Width offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Height)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Height,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Height) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Height,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, height) does not match expected WDK Height offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Depth)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Depth,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Depth) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Depth,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, depth) does not match expected WDK Depth offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_MipLevels)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, MipLevels) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_MipLevels,
    "offsetof(D3D9DDIARG_OPENRESOURCE, MipLevels) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, mip_levels) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_MipLevels,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, mip_levels) does not match expected WDK MipLevels offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Usage)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Usage,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Usage) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Usage,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, usage) does not match expected WDK Usage offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Size)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Size,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Size) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Size,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, size) does not match expected WDK Size offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hResource,
    "offsetof(D3D9DDIARG_OPENRESOURCE, hResource) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hResource,
    "offsetof(AEROGPU_D3D9DDIARG_OPENRESOURCE, hResource) does not match expected WDK hResource offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_LOCK) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK,
    "sizeof(D3D9DDIARG_LOCK) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_LOCK) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK,
    "AEROGPU_D3D9DDIARG_LOCK must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_hResource,
    "offsetof(D3D9DDIARG_LOCK, hResource) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_LOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_hResource,
    "offsetof(AEROGPU_D3D9DDIARG_LOCK, hResource) does not match expected WDK hResource offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_OffsetToLock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, OffsetToLock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_OffsetToLock,
    "offsetof(D3D9DDIARG_LOCK, OffsetToLock) does not match expected value");
// AeroGPU's portable struct uses `offset_bytes` for the same field.
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_LOCK, offset_bytes) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_OffsetToLock,
    "offsetof(AEROGPU_D3D9DDIARG_LOCK, offset_bytes) does not match expected WDK OffsetToLock offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_SizeToLock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, SizeToLock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_SizeToLock,
    "offsetof(D3D9DDIARG_LOCK, SizeToLock) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_LOCK, size_bytes) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_SizeToLock,
    "offsetof(AEROGPU_D3D9DDIARG_LOCK, size_bytes) does not match expected WDK SizeToLock offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_Flags,
    "offsetof(D3D9DDIARG_LOCK, Flags) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_LOCK, flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_Flags,
    "offsetof(AEROGPU_D3D9DDIARG_LOCK, flags) does not match expected WDK Flags offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_UNLOCK) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK,
    "sizeof(D3D9DDIARG_UNLOCK) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_UNLOCK) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK,
    "AEROGPU_D3D9DDIARG_UNLOCK must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_hResource,
    "offsetof(D3D9DDIARG_UNLOCK, hResource) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_UNLOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_hResource,
    "offsetof(AEROGPU_D3D9DDIARG_UNLOCK, hResource) does not match expected WDK hResource offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_OffsetToUnlock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, OffsetToUnlock) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_OffsetToUnlock,
    "offsetof(D3D9DDIARG_UNLOCK, OffsetToUnlock) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_UNLOCK, offset_bytes) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_OffsetToUnlock,
    "offsetof(AEROGPU_D3D9DDIARG_UNLOCK, offset_bytes) does not match expected WDK OffsetToUnlock offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_SizeToUnlock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, SizeToUnlock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_SizeToUnlock,
    "offsetof(D3D9DDIARG_UNLOCK, SizeToUnlock) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_UNLOCK, size_bytes) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_SizeToUnlock,
    "offsetof(AEROGPU_D3D9DDIARG_UNLOCK, size_bytes) does not match expected WDK SizeToUnlock offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDI_LOCKED_BOX) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX,
    "sizeof(D3D9DDI_LOCKED_BOX) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDI_LOCKED_BOX) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX,
    "sizeof(AEROGPU_D3D9DDI_LOCKED_BOX) does not match expected WDK locked box size");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_pData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, pData) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_pData,
    "offsetof(D3D9DDI_LOCKED_BOX, pData) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, pData) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_pData,
    "offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, pData) does not match expected WDK pData offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_rowPitch)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, rowPitch) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_rowPitch,
    "offsetof(D3D9DDI_LOCKED_BOX, rowPitch) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, rowPitch) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_rowPitch,
    "offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, rowPitch) does not match expected WDK rowPitch offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_slicePitch)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, slicePitch) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_slicePitch,
    "offsetof(D3D9DDI_LOCKED_BOX, slicePitch) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, slicePitch) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_slicePitch,
    "offsetof(AEROGPU_D3D9DDI_LOCKED_BOX, slicePitch) does not match expected WDK slicePitch offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_PRESENT) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT,
    "sizeof(D3D9DDIARG_PRESENT) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_PRESENT) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT,
    "AEROGPU_D3D9DDIARG_PRESENT must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSrc)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSrc,
    "offsetof(D3D9DDIARG_PRESENT, hSrc) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENT, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSrc,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENT, hSrc) does not match expected WDK hSrc offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSwapChain)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hSwapChain) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSwapChain,
    "offsetof(D3D9DDIARG_PRESENT, hSwapChain) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENT, hSwapChain) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSwapChain,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENT, hSwapChain) does not match expected WDK hSwapChain offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hWnd)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hWnd,
    "offsetof(D3D9DDIARG_PRESENT, hWnd) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENT, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hWnd,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENT, hWnd) does not match expected WDK hWnd offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_SyncInterval)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, SyncInterval) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_SyncInterval,
    "offsetof(D3D9DDIARG_PRESENT, SyncInterval) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENT, sync_interval) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_SyncInterval,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENT, sync_interval) does not match expected WDK SyncInterval offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_Flags,
    "offsetof(D3D9DDIARG_PRESENT, Flags) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENT, flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_Flags,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENT, flags) does not match expected WDK Flags offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_PRESENTEX) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX,
    "sizeof(D3D9DDIARG_PRESENTEX) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(AEROGPU_D3D9DDIARG_PRESENTEX) <= AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX,
    "AEROGPU_D3D9DDIARG_PRESENTEX must be prefix-compatible (smaller than WDK struct)");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hSrc)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hSrc,
    "offsetof(D3D9DDIARG_PRESENTEX, hSrc) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hSrc,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, hSrc) does not match expected WDK hSrc offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hWnd)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hWnd,
    "offsetof(D3D9DDIARG_PRESENTEX, hWnd) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hWnd,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, hWnd) does not match expected WDK hWnd offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_SyncInterval)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, SyncInterval) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_SyncInterval,
    "offsetof(D3D9DDIARG_PRESENTEX, SyncInterval) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, sync_interval) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_SyncInterval,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, sync_interval) does not match expected WDK SyncInterval offset");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_Flags,
    "offsetof(D3D9DDIARG_PRESENTEX, Flags) does not match expected value");
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, d3d9_present_flags) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_Flags,
    "offsetof(AEROGPU_D3D9DDIARG_PRESENTEX, d3d9_present_flags) does not match expected WDK Flags offset");
#endif

// -----------------------------------------------------------------------------
// Runtime callback table + submit args (UMD -> dxgkrnl)
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDI_DEVICECALLBACKS)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDI_DEVICECALLBACKS) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDI_DEVICECALLBACKS,
    "sizeof(D3DDDI_DEVICECALLBACKS) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnPresentCb)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDI_DEVICECALLBACKS, pfnPresentCb) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnPresentCb,
    "offsetof(D3DDDI_DEVICECALLBACKS, pfnPresentCb) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnCreateContextCb2)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb2) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnCreateContextCb2,
    "offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb2) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnCreateContextCb)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnCreateContextCb,
    "offsetof(D3DDDI_DEVICECALLBACKS, pfnCreateContextCb) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_CREATECONTEXT)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDIARG_CREATECONTEXT) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_CREATECONTEXT,
    "sizeof(D3DDDIARG_CREATECONTEXT) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hSyncObject)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, hSyncObject) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hSyncObject,
    "offsetof(D3DDDIARG_CREATECONTEXT, hSyncObject) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, pCommandBuffer) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pCommandBuffer,
    "offsetof(D3DDDIARG_CREATECONTEXT, pCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_CommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, CommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_CommandBufferSize,
    "offsetof(D3DDDIARG_CREATECONTEXT, CommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, pAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pAllocationList,
    "offsetof(D3DDDIARG_CREATECONTEXT, pAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_AllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, AllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_AllocationListSize,
    "offsetof(D3DDDIARG_CREATECONTEXT, AllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, pPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pPatchLocationList,
    "offsetof(D3DDDIARG_CREATECONTEXT, pPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_PatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_CREATECONTEXT, PatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_PatchLocationListSize,
    "offsetof(D3DDDIARG_CREATECONTEXT, PatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_SUBMITCOMMAND)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDIARG_SUBMITCOMMAND) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_SUBMITCOMMAND,
    "sizeof(D3DDDIARG_SUBMITCOMMAND) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_hContext)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, hContext) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_hContext,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, hContext) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pCommandBuffer,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandLength)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandLength,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandBufferSize,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pAllocationList,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_AllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_AllocationListSize,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pPatchLocationList,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_PatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_PatchLocationListSize,
    "offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_RENDER)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDIARG_RENDER) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_RENDER,
    "sizeof(D3DDDIARG_RENDER) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_hContext)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, hContext) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_hContext,
    "offsetof(D3DDDIARG_RENDER, hContext) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pCommandBuffer) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pCommandBuffer,
    "offsetof(D3DDDIARG_RENDER, pCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_CommandLength)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, CommandLength) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_CommandLength,
    "offsetof(D3DDDIARG_RENDER, CommandLength) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_CommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, CommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_CommandBufferSize,
    "offsetof(D3DDDIARG_RENDER, CommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pAllocationList,
    "offsetof(D3DDDIARG_RENDER, pAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_AllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, AllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_AllocationListSize,
    "offsetof(D3DDDIARG_RENDER, AllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pPatchLocationList,
    "offsetof(D3DDDIARG_RENDER, pPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_PatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, PatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_PatchLocationListSize,
    "offsetof(D3DDDIARG_RENDER, PatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pNewCommandBuffer) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewCommandBuffer,
    "offsetof(D3DDDIARG_RENDER, pNewCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewCommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, NewCommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewCommandBufferSize,
    "offsetof(D3DDDIARG_RENDER, NewCommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pNewAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewAllocationList,
    "offsetof(D3DDDIARG_RENDER, pNewAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewAllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, NewAllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewAllocationListSize,
    "offsetof(D3DDDIARG_RENDER, NewAllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, pNewPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_pNewPatchLocationList,
    "offsetof(D3DDDIARG_RENDER, pNewPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewPatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, NewPatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_NewPatchLocationListSize,
    "offsetof(D3DDDIARG_RENDER, NewPatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_SubmissionFenceId)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_RENDER, SubmissionFenceId) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_RENDER_SubmissionFenceId,
    "offsetof(D3DDDIARG_RENDER, SubmissionFenceId) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_PRESENT)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3DDDIARG_PRESENT) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_PRESENT,
    "sizeof(D3DDDIARG_PRESENT) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_hContext)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, hContext) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_hContext,
    "offsetof(D3DDDIARG_PRESENT, hContext) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pCommandBuffer) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pCommandBuffer,
    "offsetof(D3DDDIARG_PRESENT, pCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_CommandLength)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, CommandLength) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_CommandLength,
    "offsetof(D3DDDIARG_PRESENT, CommandLength) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_CommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, CommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_CommandBufferSize,
    "offsetof(D3DDDIARG_PRESENT, CommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pAllocationList,
    "offsetof(D3DDDIARG_PRESENT, pAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_AllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, AllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_AllocationListSize,
    "offsetof(D3DDDIARG_PRESENT, AllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pPatchLocationList,
    "offsetof(D3DDDIARG_PRESENT, pPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_PatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, PatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_PatchLocationListSize,
    "offsetof(D3DDDIARG_PRESENT, PatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pNewCommandBuffer) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewCommandBuffer,
    "offsetof(D3DDDIARG_PRESENT, pNewCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewCommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, NewCommandBufferSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewCommandBufferSize,
    "offsetof(D3DDDIARG_PRESENT, NewCommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewAllocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pNewAllocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewAllocationList,
    "offsetof(D3DDDIARG_PRESENT, pNewAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewAllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, NewAllocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewAllocationListSize,
    "offsetof(D3DDDIARG_PRESENT, NewAllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, pNewPatchLocationList) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_pNewPatchLocationList,
    "offsetof(D3DDDIARG_PRESENT, pNewPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewPatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, NewPatchLocationListSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_NewPatchLocationListSize,
    "offsetof(D3DDDIARG_PRESENT, NewPatchLocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_SubmissionFenceId)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_PRESENT, SubmissionFenceId) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_PRESENT_SubmissionFenceId,
    "offsetof(D3DDDIARG_PRESENT, SubmissionFenceId) does not match expected value");
#endif

#endif // AEROGPU_D3D9_USE_WDK_DDI
