// Optional compile-time ABI assertions for Win7 D3D10/11 UMD builds against the
// Windows WDK headers.
//
// This header is intentionally a no-op unless you are building the UMD against
// the *real* WDK D3D headers (`d3dumddi.h` / `d3d10umddi.h` / `d3d11umddi.h`).
// The repository build uses a small "compat" DDI surface and does not ship the
// WDK headers.
//
// The intent is to "freeze" ABI-critical sizes/offsets/entrypoint decorations so
// future header/toolchain drift is caught at compile time (instead of causing a
// Win7 loader/runtime crash due to table-size overruns or x86 stdcall mismatch).

#pragma once

#if !(defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
// Repo-local builds do not have the WDK headers; keep this header inert.
#else

#include <stddef.h> // offsetof, size_t

// Pull in the canonical WDK DDI types we want to validate.
#include <d3dkmthk.h>
#include <d3dumddi.h>
#include <d3d10umddi.h>
#include <d3d10_1umddi.h>
#include <d3d11umddi.h>

// -----------------------------------------------------------------------------
// Compile-time assertion (C/C++, C++03-safe)
// -----------------------------------------------------------------------------
// Some Win7-targeted toolchains may be older than C++11; prefer `static_assert`
// when available, but keep a fallback that works with older compilers.

#if defined(__cplusplus)
  #if (__cplusplus >= 201103L) || (defined(_MSC_VER) && _MSC_VER >= 1600)
    #define AEROGPU_D3D10_11_ABI_STATIC_ASSERT(expr, msg) static_assert((expr), msg)
  #else
    #define AEROGPU_D3D10_11_ABI_STATIC_ASSERT(expr, msg) \
      typedef char aerogpu_d3d10_11_abi_static_assert_##__LINE__[(expr) ? 1 : -1]
  #endif
#else
  #ifndef C_ASSERT
    #define C_ASSERT(expr) typedef char aerogpu_d3d10_11_c_assert_##__LINE__[(expr) ? 1 : -1]
  #endif
  #define AEROGPU_D3D10_11_ABI_STATIC_ASSERT(expr, msg) C_ASSERT(expr)
#endif

// -----------------------------------------------------------------------------
// x86 stdcall stack byte computation for function pointer typedefs
// -----------------------------------------------------------------------------
// Useful for validating that x86 exports match their `.def` stack sizes
// (e.g. `_OpenAdapter10@4` vs `_OpenAdapter10@8`).

#define AEROGPU_D3D10_11_ABI_STACK_ROUND4(x) (((x) + 3) & ~((size_t)3))

#if defined(__cplusplus)

template <typename T>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes;

template <typename R>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes<R(__stdcall*)(void)> {
  static const size_t value = 0;
};

template <typename R, typename A1>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes<R(__stdcall*)(A1)> {
  static const size_t value = AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A1));
};

template <typename R, typename A1, typename A2>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2)> {
  static const size_t value = AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A2));
};

template <typename R, typename A1, typename A2, typename A3>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3)> {
  static const size_t value = AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A2)) +
                              AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A3));
};

template <typename R, typename A1, typename A2, typename A3, typename A4>
struct aerogpu_d3d10_11_abi_stdcall_stack_bytes<R(__stdcall*)(A1, A2, A3, A4)> {
  static const size_t value = AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A1)) + AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A2)) +
                              AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A3)) + AEROGPU_D3D10_11_ABI_STACK_ROUND4(sizeof(A4));
};

#endif // __cplusplus

// -----------------------------------------------------------------------------
// Optional expected-value checks
// -----------------------------------------------------------------------------
//
// The canonical Win7 driver build (MSBuild + WDK) should treat ABI drift as a
// hard failure. The build can opt-in to using the checked-in expected values by
// defining:
//   AEROGPU_D3D10_11_WDK_ABI_ENFORCE_EXPECTED
//
// This keeps repo-local/non-WDK builds unaffected.

#if defined(AEROGPU_D3D10_11_WDK_ABI_ENFORCE_EXPECTED)
  #include "aerogpu_d3d10_11_wdk_abi_expected.h"
#endif

// -----------------------------------------------------------------------------
// Assert helpers
// -----------------------------------------------------------------------------

#define AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(Type, Expected) \
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(sizeof(Type) == (Expected), "sizeof(" #Type ") does not match expected value")

#define AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(Type, Member, Expected)                                   \
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(offsetof(Type, Member) == (Expected), "offsetof(" #Type ", " #Member \
                                                                   ") does not match expected value")

// -----------------------------------------------------------------------------
// x86 export decoration checks
// -----------------------------------------------------------------------------

#if defined(_M_IX86) && defined(__cplusplus)
  #if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_STDCALL_BYTES)
typedef HRESULT(__stdcall* aerogpu_d3d10_11_openadapter10_fn)(D3D10DDIARG_OPENADAPTER*);
static const aerogpu_d3d10_11_openadapter10_fn aerogpu_d3d10_11_openadapter10_sigcheck = &OpenAdapter10;
AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
    aerogpu_d3d10_11_abi_stdcall_stack_bytes<aerogpu_d3d10_11_openadapter10_fn>::value ==
        AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapter10 do not match expected value");
__if_exists(PFND3D10DDI_OPENADAPTER) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D10DDI_OPENADAPTER>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D10DDI_OPENADAPTER do not match expected value");
}
  #endif

  #if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES)
typedef HRESULT(__stdcall* aerogpu_d3d10_11_openadapter10_2_fn)(D3D10DDIARG_OPENADAPTER*);
static const aerogpu_d3d10_11_openadapter10_2_fn aerogpu_d3d10_11_openadapter10_2_sigcheck = &OpenAdapter10_2;
AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
    aerogpu_d3d10_11_abi_stdcall_stack_bytes<aerogpu_d3d10_11_openadapter10_2_fn>::value ==
        AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapter10_2 do not match expected value");
__if_exists(PFND3D10DDI_OPENADAPTER) {
  // Some WDKs do not expose a distinct typedef for the 10.1 OpenAdapter export.
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D10DDI_OPENADAPTER>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D10DDI_OPENADAPTER (OpenAdapter10_2) do not match expected value");
}
__if_exists(PFND3D10DDI_OPENADAPTER2) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D10DDI_OPENADAPTER2>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D10DDI_OPENADAPTER2 (OpenAdapter10_2) do not match expected value");
}
__if_exists(PFND3D10DDI_OPENADAPTER10_2) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D10DDI_OPENADAPTER10_2>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D10DDI_OPENADAPTER10_2 (OpenAdapter10_2) do not match expected value");
}
__if_exists(PFND3D10_1DDI_OPENADAPTER) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D10_1DDI_OPENADAPTER>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D10_1DDI_OPENADAPTER (OpenAdapter10_2) do not match expected value");
}
  #endif

  #if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER11_STDCALL_BYTES)
typedef HRESULT(__stdcall* aerogpu_d3d10_11_openadapter11_fn)(D3D10DDIARG_OPENADAPTER*);
static const aerogpu_d3d10_11_openadapter11_fn aerogpu_d3d10_11_openadapter11_sigcheck = &OpenAdapter11;
AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
    aerogpu_d3d10_11_abi_stdcall_stack_bytes<aerogpu_d3d10_11_openadapter11_fn>::value ==
        AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER11_STDCALL_BYTES,
    "x86 stdcall stack bytes for OpenAdapter11 do not match expected value");
__if_exists(PFND3D11DDI_OPENADAPTER) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D11DDI_OPENADAPTER>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER11_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D11DDI_OPENADAPTER do not match expected value");
}
__if_exists(PFND3D11DDI_OPENADAPTER11) {
  AEROGPU_D3D10_11_ABI_STATIC_ASSERT(
      aerogpu_d3d10_11_abi_stdcall_stack_bytes<PFND3D11DDI_OPENADAPTER11>::value ==
          AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER11_STDCALL_BYTES,
      "WDK x86 stdcall stack bytes for PFND3D11DDI_OPENADAPTER11 do not match expected value");
}
  #endif
#endif // _M_IX86 && __cplusplus

// -----------------------------------------------------------------------------
// WDK struct size/offset checks
// -----------------------------------------------------------------------------

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDIARG_OPENADAPTER)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D10DDIARG_OPENADAPTER, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDIARG_OPENADAPTER);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Interface)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                     Interface,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Interface);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Version)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                     Version,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Version);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hRTAdapter)
  #if defined(__cplusplus)
__if_exists(D3D10DDIARG_OPENADAPTER::hRTAdapter) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                       hRTAdapter,
                                       AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hRTAdapter);
}
  #endif
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hAdapter)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                      hAdapter,
                                      AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hAdapter);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterCallbacks)
  #if defined(__cplusplus)
__if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                       pAdapterCallbacks,
                                       AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterCallbacks);
}
  #endif
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterFuncs)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDIARG_OPENADAPTER,
                                     pAdapterFuncs,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterFuncs);
#endif

// Adapter function tables.
#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_ADAPTERFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D10DDI_ADAPTERFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_ADAPTERFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnGetCaps)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_ADAPTERFUNCS,
                                     pfnGetCaps,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnGetCaps);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_ADAPTERFUNCS,
                                     pfnCalcPrivateDeviceSize,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCreateDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_ADAPTERFUNCS,
                                     pfnCreateDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCreateDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCloseAdapter)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_ADAPTERFUNCS,
                                     pfnCloseAdapter,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCloseAdapter);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_ADAPTERFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D10_1DDI_ADAPTERFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_ADAPTERFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnGetCaps)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_ADAPTERFUNCS,
                                     pfnGetCaps,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnGetCaps);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_ADAPTERFUNCS,
                                     pfnCalcPrivateDeviceSize,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCreateDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_ADAPTERFUNCS,
                                     pfnCreateDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCreateDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCloseAdapter)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_ADAPTERFUNCS,
                                     pfnCloseAdapter,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCloseAdapter);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_ADAPTERFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D11DDI_ADAPTERFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_ADAPTERFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnGetCaps)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_ADAPTERFUNCS,
                                     pfnGetCaps,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnGetCaps);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_ADAPTERFUNCS,
                                     pfnCalcPrivateDeviceSize,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceContextSize)
  #if defined(__cplusplus)
__if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_ADAPTERFUNCS,
                                       pfnCalcPrivateDeviceContextSize,
                                       AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceContextSize);
}
  #endif
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCreateDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_ADAPTERFUNCS,
                                     pfnCreateDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCreateDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCloseAdapter)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_ADAPTERFUNCS,
                                     pfnCloseAdapter,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCloseAdapter);
#endif

// Device function tables.
#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_DEVICEFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D10DDI_DEVICEFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_DEVICEFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnDestroyDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_DEVICEFUNCS,
                                     pfnDestroyDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnDestroyDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnCreateResource)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_DEVICEFUNCS,
                                     pfnCreateResource,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnCreateResource);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnPresent)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_DEVICEFUNCS,
                                     pfnPresent,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnPresent);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnFlush)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_DEVICEFUNCS,
                                     pfnFlush,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnFlush);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnRotateResourceIdentities)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10DDI_DEVICEFUNCS,
                                     pfnRotateResourceIdentities,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnRotateResourceIdentities);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_DEVICEFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D10_1DDI_DEVICEFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_DEVICEFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnDestroyDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_DEVICEFUNCS,
                                     pfnDestroyDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnDestroyDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnCreateResource)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_DEVICEFUNCS,
                                     pfnCreateResource,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnCreateResource);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnPresent)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_DEVICEFUNCS,
                                     pfnPresent,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnPresent);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnFlush)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_DEVICEFUNCS,
                                     pfnFlush,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnFlush);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnRotateResourceIdentities)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D10_1DDI_DEVICEFUNCS,
                                     pfnRotateResourceIdentities,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnRotateResourceIdentities);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICEFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D11DDI_DEVICEFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICEFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnDestroyDevice)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICEFUNCS,
                                     pfnDestroyDevice,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnDestroyDevice);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnCreateResource)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICEFUNCS,
                                     pfnCreateResource,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnCreateResource);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnPresent)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICEFUNCS,
                                     pfnPresent,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnPresent);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnRotateResourceIdentities)
  #if defined(__cplusplus)
__if_exists(D3D11DDI_DEVICEFUNCS::pfnRotateResourceIdentities) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICEFUNCS,
                                       pfnRotateResourceIdentities,
                                       AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnRotateResourceIdentities);
}
  #endif
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICECONTEXTFUNCS)
AEROGPU_D3D10_11_WDK_ASSERT_SIZEOF(D3D11DDI_DEVICECONTEXTFUNCS, AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICECONTEXTFUNCS);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnVsSetShader)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICECONTEXTFUNCS,
                                     pfnVsSetShader,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnVsSetShader);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnDraw)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICECONTEXTFUNCS,
                                     pfnDraw,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnDraw);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnFlush)
AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICECONTEXTFUNCS,
                                     pfnFlush,
                                     AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnFlush);
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnPresent)
  #if defined(__cplusplus)
__if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnPresent) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(D3D11DDI_DEVICECONTEXTFUNCS,
                                       pfnPresent,
                                       AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnPresent);
}
  #endif
#endif

#if defined(AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnRotateResourceIdentities)
  #if defined(__cplusplus)
__if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnRotateResourceIdentities) {
  AEROGPU_D3D10_11_WDK_ASSERT_OFFSETOF(
      D3D11DDI_DEVICECONTEXTFUNCS,
      pfnRotateResourceIdentities,
      AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnRotateResourceIdentities);
}
  #endif
#endif

#endif // AEROGPU_UMD_USE_WDK_HEADERS
