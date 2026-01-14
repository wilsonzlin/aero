// Optional compile-time ABI assertions for Win7 D3D9 UMD builds against the WDK headers.
//
// This header is intentionally a no-op unless you are building the UMD against
// the *real* WDK D3D headers (d3dumddi.h / d3d9umddi.h). The repository build
// uses a small "compat" DDI surface and does not ship the WDK headers.
//
// Usage (WDK build only)
// ----------------------
// 1) Define `AEROGPU_D3D9_USE_WDK_DDI=1` in your WDK build.
// 2) Include this header in a translation unit after the WDK headers are
//    available on the include path.
// 3) Optionally define one or more `AEROGPU_D3D9_WDK_ABI_EXPECT_*` macros (see
//    below) using values captured from the probe tool:
//      drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/
//
// The intent is to "freeze" ABI-critical sizes/offsets/entrypoint decorations so
// future header/toolchain drift is caught at compile time.

#pragma once

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
// Repo-local builds do not have the WDK headers; keep this header inert.
#else

#include <stddef.h>

#include <d3dkmthk.h>
#include <d3dumddi.h>
#include <d3d9umddi.h>

// The D3D9 UMD builds directly against the canonical WDK D3D9UMDDI structs.
// These checks are strictly about freezing the WDK-facing ABI (sizes, offsets,
// and x86 stdcall decorations).

// -----------------------------------------------------------------------------
// Compile-time assertion (C/C++, C++03-safe)
// -----------------------------------------------------------------------------
// Some Win7-targeted toolchains may be older than C++11; prefer `static_assert`
// when available, but keep a fallback that works with older compilers.

#if defined(__cplusplus)
  #if (__cplusplus >= 201103L) || (defined(_MSC_VER) && _MSC_VER >= 1600)
    #define AEROGPU_ABI_STATIC_ASSERT(expr, msg) static_assert((expr), msg)
  #else
    #define AEROGPU_ABI_STATIC_ASSERT(expr, msg) \
      typedef char aerogpu_abi_static_assert_##__LINE__[(expr) ? 1 : -1]
  #endif
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
//
// In the canonical Win7 driver build (MSBuild + WDK), ABI drift should be a hard
// failure. The build can opt-in to using the checked-in expected values by
// defining:
//   AEROGPU_D3D9_WDK_ABI_ENFORCE_EXPECTED
//
// This keeps repo-local/non-WDK builds unaffected.

#if defined(AEROGPU_D3D9_WDK_ABI_ENFORCE_EXPECTED)
  #include "aerogpu_d3d9_wdk_abi_expected.h"
#endif

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

#define AEROGPU_D3D9_WDK_ASSERT_SIZEOF(Type, Expected) \
  AEROGPU_ABI_STATIC_ASSERT(sizeof(Type) == (Expected), "sizeof(" #Type ") does not match expected value")

#define AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(Type, Member, Expected)                                   \
  AEROGPU_ABI_STATIC_ASSERT(offsetof(Type, Member) == (Expected), "offsetof(" #Type ", " #Member \
                                                                  ") does not match expected value")

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER)
AEROGPU_D3D9_WDK_ASSERT_SIZEOF(D3DDDIARG_OPENADAPTER, AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterFuncs)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTER,
                                 pAdapterFuncs,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterFuncs);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterCallbacks2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTER,
                                 pAdapterCallbacks2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterCallbacks2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER2)
AEROGPU_D3D9_WDK_ASSERT_SIZEOF(D3DDDIARG_OPENADAPTER2, AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterFuncs)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTER2,
                                 pAdapterFuncs,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterFuncs);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterCallbacks2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTER2,
                                 pAdapterCallbacks2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterCallbacks2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTERFROMHDC)
AEROGPU_D3D9_WDK_ASSERT_SIZEOF(D3DDDIARG_OPENADAPTERFROMHDC,
                               AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTERFROMHDC);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_pAdapterFuncs)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMHDC,
                                 pAdapterFuncs,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_pAdapterFuncs);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_pAdapterCallbacks2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMHDC,
                                 pAdapterCallbacks2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_pAdapterCallbacks2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_AdapterLuid)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMHDC,
                                 AdapterLuid,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMHDC_AdapterLuid);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTERFROMLUID)
AEROGPU_D3D9_WDK_ASSERT_SIZEOF(D3DDDIARG_OPENADAPTERFROMLUID,
                               AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTERFROMLUID);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_pAdapterFuncs)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMLUID,
                                 pAdapterFuncs,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_pAdapterFuncs);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_pAdapterCallbacks2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMLUID,
                                 pAdapterCallbacks2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_pAdapterCallbacks2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_AdapterLuid)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3DDDIARG_OPENADAPTERFROMLUID,
                                 AdapterLuid,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTERFROMLUID_AdapterLuid);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_DEVICEFUNCS)
AEROGPU_D3D9_WDK_ASSERT_SIZEOF(D3D9DDI_DEVICEFUNCS, AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_DEVICEFUNCS);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateResource)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCreateResource,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateResource);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnOpenResource)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnOpenResource,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnOpenResource);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnOpenResource2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnOpenResource2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnOpenResource2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyDevice)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDestroyDevice,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyDevice);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyResource)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDestroyResource,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyResource);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnLock)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnLock,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnLock);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUnlock)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnUnlock,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUnlock);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetRenderTarget)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetRenderTarget,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetRenderTarget);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetDepthStencil)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetDepthStencil,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetDepthStencil);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateVertexDecl)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCreateVertexDecl,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateVertexDecl);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetVertexDecl)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetVertexDecl,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetVertexDecl);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyVertexDecl)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDestroyVertexDecl,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyVertexDecl);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateShader)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCreateShader,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateShader);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetShader)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(
    D3D9DDI_DEVICEFUNCS, pfnSetShader, AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetShader);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyShader)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDestroyShader,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyShader);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetShaderConstF)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetShaderConstF,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetShaderConstF);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateVertexShader)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCreateVertexShader,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateVertexShader);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBeginScene)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnBeginScene,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBeginScene);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnEndScene)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnEndScene,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnEndScene);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetSwapChain)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetSwapChain,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetSwapChain);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetSwapChain)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetSwapChain,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetSwapChain);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnReset)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS, pfnReset, AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnReset);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnResetEx)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnResetEx,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnResetEx);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetFVF)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetFVF,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetFVF);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawPrimitive2)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDrawPrimitive2,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawPrimitive2);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetViewport)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetViewport,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetViewport);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetScissorRect)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetScissorRect,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetScissorRect);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetTexture)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetTexture,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetTexture);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetSamplerState)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetSamplerState,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetSamplerState);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetRenderState)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetRenderState,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetRenderState);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetStreamSource)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetStreamSource,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetStreamSource);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetIndices)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetIndices,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetIndices);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnClear)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnClear,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnClear);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawPrimitive)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDrawPrimitive,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawPrimitive);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawIndexedPrimitive)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnDrawIndexedPrimitive,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDrawIndexedPrimitive);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnRotateResourceIdentities)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnRotateResourceIdentities,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnRotateResourceIdentities);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresent)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnPresent,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresent);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresentEx)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnPresentEx,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresentEx);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnFlush)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnFlush,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnFlush);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetMaximumFrameLatency)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnSetMaximumFrameLatency,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetMaximumFrameLatency);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetMaximumFrameLatency)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetMaximumFrameLatency,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetMaximumFrameLatency);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetPresentStats)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetPresentStats,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetPresentStats);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetLastPresentCount)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetLastPresentCount,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetLastPresentCount);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateQuery)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCreateQuery,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateQuery);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnIssueQuery)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnIssueQuery,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnIssueQuery);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetQueryData)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetQueryData,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetQueryData);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetRenderTargetData)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnGetRenderTargetData,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetRenderTargetData);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCopyRects)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnCopyRects,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCopyRects);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBlt)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnBlt,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBlt);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnColorFill)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnColorFill,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnColorFill);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateSurface)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnUpdateSurface,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateSurface);
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateTexture)
AEROGPU_D3D9_WDK_ASSERT_OFFSETOF(D3D9DDI_DEVICEFUNCS,
                                 pfnUpdateTexture,
                                 AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateTexture);
#endif

#undef AEROGPU_D3D9_WDK_ASSERT_SIZEOF
#undef AEROGPU_D3D9_WDK_ASSERT_OFFSETOF

// -----------------------------------------------------------------------------
// D3D9UMDDI device arg structs (Win7 D3D9 runtime -> UMD)
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATEDEVICE)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_CREATEDEVICE) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATEDEVICE,
    "sizeof(D3D9DDIARG_CREATEDEVICE) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_hAdapter)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATEDEVICE, hAdapter) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_hAdapter,
    "offsetof(D3D9DDIARG_CREATEDEVICE, hAdapter) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_hDevice)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATEDEVICE, hDevice) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_hDevice,
    "offsetof(D3D9DDIARG_CREATEDEVICE, hDevice) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATEDEVICE, Flags) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATEDEVICE_Flags,
    "offsetof(D3D9DDIARG_CREATEDEVICE, Flags) does not match expected value");
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
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Type)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Type,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Type) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Format)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Format,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Format) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Width)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Width,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Width) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Height)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Height,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Height) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Depth)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Depth,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Depth) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_MipLevels)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, MipLevels) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_MipLevels,
    "offsetof(D3D9DDIARG_CREATERESOURCE, MipLevels) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Usage)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Usage,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Usage) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Pool)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Pool) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Pool,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Pool) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Size)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, Size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_Size,
    "offsetof(D3D9DDIARG_CREATERESOURCE, Size) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hResource,
    "offsetof(D3D9DDIARG_CREATERESOURCE, hResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pSharedHandle)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, pSharedHandle) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pSharedHandle,
    "offsetof(D3D9DDIARG_CREATERESOURCE, pSharedHandle) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pPrivateDriverData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_pPrivateDriverData,
    "offsetof(D3D9DDIARG_CREATERESOURCE, pPrivateDriverData) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_PrivateDriverDataSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_PrivateDriverDataSize,
    "offsetof(D3D9DDIARG_CREATERESOURCE, PrivateDriverDataSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hAllocation)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_CREATERESOURCE, hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_CREATERESOURCE_hAllocation,
    "offsetof(D3D9DDIARG_CREATERESOURCE, hAllocation) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_OPENRESOURCE) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE,
    "sizeof(D3D9DDIARG_OPENRESOURCE) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_pPrivateDriverData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_pPrivateDriverData,
    "offsetof(D3D9DDIARG_OPENRESOURCE, pPrivateDriverData) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_PrivateDriverDataSize)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_PrivateDriverDataSize,
    "offsetof(D3D9DDIARG_OPENRESOURCE, PrivateDriverDataSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hAllocation)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, hAllocation) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hAllocation,
    "offsetof(D3D9DDIARG_OPENRESOURCE, hAllocation) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Type)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Type) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Type,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Type) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Format)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Format) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Format,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Format) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Width)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Width) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Width,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Width) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Height)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Height) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Height,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Height) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Depth)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Depth) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Depth,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Depth) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_MipLevels)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, MipLevels) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_MipLevels,
    "offsetof(D3D9DDIARG_OPENRESOURCE, MipLevels) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Usage)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Usage) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Usage,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Usage) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Size)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, Size) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_Size,
    "offsetof(D3D9DDIARG_OPENRESOURCE, Size) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_OPENRESOURCE, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_OPENRESOURCE_hResource,
    "offsetof(D3D9DDIARG_OPENRESOURCE, hResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_LOCK) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK,
    "sizeof(D3D9DDIARG_LOCK) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_hResource,
    "offsetof(D3D9DDIARG_LOCK, hResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_OffsetToLock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, OffsetToLock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_OffsetToLock,
    "offsetof(D3D9DDIARG_LOCK, OffsetToLock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_SizeToLock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, SizeToLock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_SizeToLock,
    "offsetof(D3D9DDIARG_LOCK, SizeToLock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_LOCK, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_LOCK_Flags,
    "offsetof(D3D9DDIARG_LOCK, Flags) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_UNLOCK) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK,
    "sizeof(D3D9DDIARG_UNLOCK) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_hResource)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, hResource) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_hResource,
    "offsetof(D3D9DDIARG_UNLOCK, hResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_OffsetToUnlock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, OffsetToUnlock) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_OffsetToUnlock,
    "offsetof(D3D9DDIARG_UNLOCK, OffsetToUnlock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_SizeToUnlock)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_UNLOCK, SizeToUnlock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_UNLOCK_SizeToUnlock,
    "offsetof(D3D9DDIARG_UNLOCK, SizeToUnlock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDI_LOCKED_BOX) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX,
    "sizeof(D3D9DDI_LOCKED_BOX) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_pData)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, pData) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_pData,
    "offsetof(D3D9DDI_LOCKED_BOX, pData) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_rowPitch)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, rowPitch) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_rowPitch,
    "offsetof(D3D9DDI_LOCKED_BOX, rowPitch) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_slicePitch)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_LOCKED_BOX, slicePitch) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_LOCKED_BOX_slicePitch,
    "offsetof(D3D9DDI_LOCKED_BOX, slicePitch) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_PRESENT) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT,
    "sizeof(D3D9DDIARG_PRESENT) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSrc)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSrc,
    "offsetof(D3D9DDIARG_PRESENT, hSrc) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSwapChain)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hSwapChain) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hSwapChain,
    "offsetof(D3D9DDIARG_PRESENT, hSwapChain) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hWnd)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_hWnd,
    "offsetof(D3D9DDIARG_PRESENT, hWnd) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_SyncInterval)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, SyncInterval) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_SyncInterval,
    "offsetof(D3D9DDIARG_PRESENT, SyncInterval) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENT, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENT_Flags,
    "offsetof(D3D9DDIARG_PRESENT, Flags) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX)
AEROGPU_ABI_STATIC_ASSERT(
    sizeof(D3D9DDIARG_PRESENTEX) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX,
    "sizeof(D3D9DDIARG_PRESENTEX) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hSrc)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, hSrc) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hSrc,
    "offsetof(D3D9DDIARG_PRESENTEX, hSrc) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hWnd)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, hWnd) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_hWnd,
    "offsetof(D3D9DDIARG_PRESENTEX, hWnd) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_SyncInterval)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, SyncInterval) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_SyncInterval,
    "offsetof(D3D9DDIARG_PRESENTEX, SyncInterval) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_Flags)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDIARG_PRESENTEX, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDIARG_PRESENTEX_Flags,
    "offsetof(D3D9DDIARG_PRESENTEX, Flags) does not match expected value");
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

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterCallbacks)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_OPENADAPTER, pAdapterCallbacks) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_pAdapterCallbacks,
    "offsetof(D3DDDIARG_OPENADAPTER, pAdapterCallbacks) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_hAdapter)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_OPENADAPTER, hAdapter) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER_hAdapter,
                          "offsetof(D3DDDIARG_OPENADAPTER, hAdapter) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER2)
AEROGPU_ABI_STATIC_ASSERT(sizeof(D3DDDIARG_OPENADAPTER2) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER2,
                          "sizeof(D3DDDIARG_OPENADAPTER2) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterFuncs)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_OPENADAPTER2, pAdapterFuncs) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterFuncs,
    "offsetof(D3DDDIARG_OPENADAPTER2, pAdapterFuncs) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterCallbacks)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3DDDIARG_OPENADAPTER2, pAdapterCallbacks) ==
        AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_pAdapterCallbacks,
    "offsetof(D3DDDIARG_OPENADAPTER2, pAdapterCallbacks) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_hAdapter)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_OPENADAPTER2, hAdapter) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_OPENADAPTER2_hAdapter,
                          "offsetof(D3DDDIARG_OPENADAPTER2, hAdapter) does not match expected value");
#endif

// -----------------------------------------------------------------------------
// Function tables
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_ADAPTERFUNCS)
AEROGPU_ABI_STATIC_ASSERT(sizeof(D3D9DDI_ADAPTERFUNCS) == AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_ADAPTERFUNCS,
                          "sizeof(D3D9DDI_ADAPTERFUNCS) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnCloseAdapter)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_ADAPTERFUNCS, pfnCloseAdapter) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnCloseAdapter,
                          "offsetof(D3D9DDI_ADAPTERFUNCS, pfnCloseAdapter) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnGetCaps)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_ADAPTERFUNCS, pfnGetCaps) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnGetCaps,
                          "offsetof(D3D9DDI_ADAPTERFUNCS, pfnGetCaps) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnCreateDevice)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_ADAPTERFUNCS, pfnCreateDevice) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnCreateDevice,
                          "offsetof(D3D9DDI_ADAPTERFUNCS, pfnCreateDevice) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnQueryAdapterInfo)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_ADAPTERFUNCS, pfnQueryAdapterInfo) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_ADAPTERFUNCS_pfnQueryAdapterInfo,
                          "offsetof(D3D9DDI_ADAPTERFUNCS, pfnQueryAdapterInfo) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyDevice)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyDevice) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyDevice,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyDevice) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateResource)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateResource) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateResource,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyResource)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyResource) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyResource,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyResource) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnLock)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnLock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnLock,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnLock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUnlock)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnUnlock) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUnlock,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnUnlock) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateSwapChain)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateSwapChain) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateSwapChain,
    "offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateSwapChain) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroySwapChain)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroySwapChain) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroySwapChain,
    "offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroySwapChain) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCheckDeviceState)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckDeviceState) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCheckDeviceState,
    "offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckDeviceState) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnWaitForVBlank)
AEROGPU_ABI_STATIC_ASSERT(
    offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForVBlank) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnWaitForVBlank,
    "offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForVBlank) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetGPUThreadPriority)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnSetGPUThreadPriority) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnSetGPUThreadPriority,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnSetGPUThreadPriority) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetGPUThreadPriority)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetGPUThreadPriority) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetGPUThreadPriority,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnGetGPUThreadPriority) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCheckResourceResidency)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckResourceResidency) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCheckResourceResidency,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnCheckResourceResidency) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnQueryResourceResidency)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnQueryResourceResidency) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnQueryResourceResidency,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnQueryResourceResidency) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetDisplayModeEx)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetDisplayModeEx) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetDisplayModeEx,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnGetDisplayModeEx) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnComposeRects)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnComposeRects) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnComposeRects,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnComposeRects) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresent)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnPresent) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnPresent,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnPresent) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnFlush)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnFlush) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnFlush,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnFlush) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateQuery)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateQuery) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnCreateQuery,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnCreateQuery) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyQuery)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyQuery) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnDestroyQuery,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnDestroyQuery) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnIssueQuery)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnIssueQuery) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnIssueQuery,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnIssueQuery) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetQueryData)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnGetQueryData) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnGetQueryData,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnGetQueryData) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnWaitForIdle)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForIdle) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnWaitForIdle,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnWaitForIdle) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBlt)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnBlt) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnBlt,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnBlt) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnColorFill)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnColorFill) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnColorFill,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnColorFill) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateSurface)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateSurface) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateSurface,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateSurface) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateTexture)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateTexture) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3D9DDI_DEVICEFUNCS_pfnUpdateTexture,
                          "offsetof(D3D9DDI_DEVICEFUNCS, pfnUpdateTexture) does not match expected value");
#endif

// -----------------------------------------------------------------------------
// Runtime callback tables
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnAllocateCb)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDI_DEVICECALLBACKS, pfnAllocateCb) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnAllocateCb,
                          "offsetof(D3DDDI_DEVICECALLBACKS, pfnAllocateCb) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnDeallocateCb)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDI_DEVICECALLBACKS, pfnDeallocateCb) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnDeallocateCb,
                          "offsetof(D3DDDI_DEVICECALLBACKS, pfnDeallocateCb) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnSubmitCommandCb)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDI_DEVICECALLBACKS, pfnSubmitCommandCb) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnSubmitCommandCb,
                          "offsetof(D3DDDI_DEVICECALLBACKS, pfnSubmitCommandCb) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnRenderCb)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDI_DEVICECALLBACKS, pfnRenderCb) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDI_DEVICECALLBACKS_pfnRenderCb,
                          "offsetof(D3DDDI_DEVICECALLBACKS, pfnRenderCb) does not match expected value");
#endif

// -----------------------------------------------------------------------------
// Submission-related structs
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hDevice)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, hDevice) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hDevice,
                          "offsetof(D3DDDIARG_CREATECONTEXT, hDevice) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_NodeOrdinal)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, NodeOrdinal) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_NodeOrdinal,
                          "offsetof(D3DDDIARG_CREATECONTEXT, NodeOrdinal) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_EngineAffinity)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, EngineAffinity) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_EngineAffinity,
                          "offsetof(D3DDDIARG_CREATECONTEXT, EngineAffinity) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_Flags)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, Flags) == AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_Flags,
                          "offsetof(D3DDDIARG_CREATECONTEXT, Flags) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hContext)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, hContext) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_hContext,
                          "offsetof(D3DDDIARG_CREATECONTEXT, hContext) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pPrivateDriverData)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, pPrivateDriverData) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_pPrivateDriverData,
                          "offsetof(D3DDDIARG_CREATECONTEXT, pPrivateDriverData) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_PrivateDriverDataSize)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_CREATECONTEXT, PrivateDriverDataSize) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_CREATECONTEXT_PrivateDriverDataSize,
                          "offsetof(D3DDDIARG_CREATECONTEXT, PrivateDriverDataSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_hContext)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, hContext) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_hContext,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, hContext) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pCommandBuffer)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pCommandBuffer,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, pCommandBuffer) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandLength)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandLength,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, CommandLength) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandBufferSize)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_CommandBufferSize,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, CommandBufferSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pAllocationList)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pAllocationList,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, pAllocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_AllocationListSize)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_AllocationListSize,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, AllocationListSize) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pPatchLocationList)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_pPatchLocationList,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, pPatchLocationList) does not match expected value");
#endif

#if defined(AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_PatchLocationListSize)
AEROGPU_ABI_STATIC_ASSERT(offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) ==
                              AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_D3DDDIARG_SUBMITCOMMAND_PatchLocationListSize,
                          "offsetof(D3DDDIARG_SUBMITCOMMAND, PatchLocationListSize) does not match expected value");
#endif

#endif // AEROGPU_D3D9_USE_WDK_DDI
