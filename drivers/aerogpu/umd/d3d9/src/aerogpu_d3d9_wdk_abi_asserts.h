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

#endif // AEROGPU_D3D9_USE_WDK_DDI
