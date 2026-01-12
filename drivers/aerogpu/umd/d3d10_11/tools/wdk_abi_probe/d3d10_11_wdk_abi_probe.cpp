// Win7 D3D10/11 UMD ABI probe (WDK headers)
//
// Purpose
// -------
// This program is intended to be built in an environment that can compile against
// the Win7 D3D10/D3D11 UMD DDI headers to verify ABI-critical structure layouts and
// exported entrypoint decorations for the D3D10/11 user-mode driver.
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
//   - MSVC / WDK headers
//   - C++03+ (avoid C++17 so this can be built in older WDK environments)

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <stddef.h>
#include <stdio.h>

#include <d3dkmthk.h>
#include <d3dumddi.h>
#include <d3d10umddi.h>
#include <d3d10_1umddi.h>
#include <d3d11umddi.h>

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

#define PRINT_SIZE(T) print_sizeof(#T, sizeof(T))
#define PRINT_OFF(T, F) print_offsetof(#T, #F, offsetof(T, F))

#if defined(_MSC_VER)
  #define PRINT_OFF_OPT(T, F)                                                                                         \
    __if_exists(T::F) { PRINT_OFF(T, F); }                                                                             \
    __if_not_exists(T::F) { print_offsetof_na(#T, #F); }
#else
  // This probe is intended for MSVC/WDK builds; keep a simple fallback.
  #define PRINT_OFF_OPT(T, F) PRINT_OFF(T, F)
#endif

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

int main() {
  printf("== Win7 D3D10/11 UMD WDK ABI probe ==\n");

#if defined(_M_IX86)
  printf("arch: x86\n");
#elif defined(_M_X64) || defined(_M_AMD64)
  printf("arch: x64\n");
#else
  printf("arch: unknown\n");
#endif

#if defined(_MSC_VER)
  printf("_MSC_VER = %d\n", _MSC_VER);
#endif

  PRINT_SIZE(void*);
  printf("\n");

  print_header("D3D10DDIARG_OPENADAPTER");
  PRINT_SIZE(D3D10DDIARG_OPENADAPTER);
  PRINT_OFF(D3D10DDIARG_OPENADAPTER, Interface);
  PRINT_OFF(D3D10DDIARG_OPENADAPTER, Version);
  PRINT_OFF_OPT(D3D10DDIARG_OPENADAPTER, hRTAdapter);
  PRINT_OFF(D3D10DDIARG_OPENADAPTER, hAdapter);
  PRINT_OFF_OPT(D3D10DDIARG_OPENADAPTER, pAdapterCallbacks);
  PRINT_OFF(D3D10DDIARG_OPENADAPTER, pAdapterFuncs);

  print_header("D3D10DDI_ADAPTERFUNCS");
  PRINT_SIZE(D3D10DDI_ADAPTERFUNCS);
  PRINT_OFF(D3D10DDI_ADAPTERFUNCS, pfnGetCaps);
  PRINT_OFF(D3D10DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceSize);
  PRINT_OFF(D3D10DDI_ADAPTERFUNCS, pfnCreateDevice);
  PRINT_OFF(D3D10DDI_ADAPTERFUNCS, pfnCloseAdapter);

  print_header("D3D10_1DDI_ADAPTERFUNCS");
  PRINT_SIZE(D3D10_1DDI_ADAPTERFUNCS);
  PRINT_OFF(D3D10_1DDI_ADAPTERFUNCS, pfnGetCaps);
  PRINT_OFF(D3D10_1DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceSize);
  PRINT_OFF(D3D10_1DDI_ADAPTERFUNCS, pfnCreateDevice);
  PRINT_OFF(D3D10_1DDI_ADAPTERFUNCS, pfnCloseAdapter);

  print_header("D3D11DDI_ADAPTERFUNCS");
  PRINT_SIZE(D3D11DDI_ADAPTERFUNCS);
  PRINT_OFF(D3D11DDI_ADAPTERFUNCS, pfnGetCaps);
  PRINT_OFF(D3D11DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceSize);
  PRINT_OFF_OPT(D3D11DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceContextSize);
  PRINT_OFF(D3D11DDI_ADAPTERFUNCS, pfnCreateDevice);
  PRINT_OFF(D3D11DDI_ADAPTERFUNCS, pfnCloseAdapter);

  print_header("D3D10DDI_DEVICEFUNCS");
  PRINT_SIZE(D3D10DDI_DEVICEFUNCS);
  PRINT_OFF(D3D10DDI_DEVICEFUNCS, pfnDestroyDevice);
  PRINT_OFF(D3D10DDI_DEVICEFUNCS, pfnCreateResource);
  PRINT_OFF_OPT(D3D10DDI_DEVICEFUNCS, pfnPresent);
  PRINT_OFF_OPT(D3D10DDI_DEVICEFUNCS, pfnFlush);
  PRINT_OFF_OPT(D3D10DDI_DEVICEFUNCS, pfnRotateResourceIdentities);

  print_header("D3D10_1DDI_DEVICEFUNCS");
  PRINT_SIZE(D3D10_1DDI_DEVICEFUNCS);
  PRINT_OFF(D3D10_1DDI_DEVICEFUNCS, pfnDestroyDevice);
  PRINT_OFF(D3D10_1DDI_DEVICEFUNCS, pfnCreateResource);
  PRINT_OFF_OPT(D3D10_1DDI_DEVICEFUNCS, pfnPresent);
  PRINT_OFF_OPT(D3D10_1DDI_DEVICEFUNCS, pfnFlush);
  PRINT_OFF_OPT(D3D10_1DDI_DEVICEFUNCS, pfnRotateResourceIdentities);

  print_header("D3D11DDI_DEVICEFUNCS");
  PRINT_SIZE(D3D11DDI_DEVICEFUNCS);
  PRINT_OFF(D3D11DDI_DEVICEFUNCS, pfnDestroyDevice);
  PRINT_OFF(D3D11DDI_DEVICEFUNCS, pfnCreateResource);
  PRINT_OFF_OPT(D3D11DDI_DEVICEFUNCS, pfnPresent);
  PRINT_OFF_OPT(D3D11DDI_DEVICEFUNCS, pfnRotateResourceIdentities);

  print_header("D3D11DDI_DEVICECONTEXTFUNCS");
  PRINT_SIZE(D3D11DDI_DEVICECONTEXTFUNCS);
  PRINT_OFF(D3D11DDI_DEVICECONTEXTFUNCS, pfnVsSetShader);
  PRINT_OFF_OPT(D3D11DDI_DEVICECONTEXTFUNCS, pfnDraw);
  PRINT_OFF_OPT(D3D11DDI_DEVICECONTEXTFUNCS, pfnFlush);
  PRINT_OFF_OPT(D3D11DDI_DEVICECONTEXTFUNCS, pfnPresent);
  PRINT_OFF_OPT(D3D11DDI_DEVICECONTEXTFUNCS, pfnRotateResourceIdentities);

  print_header("Interface constants");
  printf("D3D10DDI_INTERFACE_VERSION   = 0x%08X\n", (unsigned)D3D10DDI_INTERFACE_VERSION);
  printf("D3D10DDI_SUPPORTED           = 0x%08X\n", (unsigned)D3D10DDI_SUPPORTED);
  printf("D3D10_1DDI_INTERFACE_VERSION = 0x%08X\n", (unsigned)D3D10_1DDI_INTERFACE_VERSION);
  printf("D3D10_1DDI_SUPPORTED         = 0x%08X\n", (unsigned)D3D10_1DDI_SUPPORTED);
  printf("D3D11DDI_INTERFACE_VERSION   = 0x%08X\n", (unsigned)D3D11DDI_INTERFACE_VERSION);
#ifdef D3D11DDI_INTERFACE
  printf("D3D11DDI_INTERFACE           = 0x%08X\n", (unsigned)D3D11DDI_INTERFACE);
#endif
#ifdef D3D11DDI_SUPPORTED
  printf("D3D11DDI_SUPPORTED           = 0x%08X\n", (unsigned)D3D11DDI_SUPPORTED);
#endif

  print_header("Exported entrypoints");
  printf("runtime expects: OpenAdapter10, OpenAdapter10_2, OpenAdapter11\n");
#if defined(_M_IX86)
  printf("x86 stdcall decoration:\n");
  __if_exists(PFND3D10DDI_OPENADAPTER) {
    const unsigned stack_bytes = (unsigned)aerogpu_stdcall_stack_bytes<PFND3D10DDI_OPENADAPTER>::value;
    printf("OpenAdapter10   => _OpenAdapter10@%u\n", stack_bytes);
    printf("OpenAdapter10_2 => _OpenAdapter10_2@%u\n", stack_bytes);
  }
  __if_not_exists(PFND3D10DDI_OPENADAPTER) {
    printf("OpenAdapter10   => <typedef PFND3D10DDI_OPENADAPTER not found>\n");
    printf("OpenAdapter10_2 => <typedef PFND3D10DDI_OPENADAPTER not found>\n");
  }

  __if_exists(PFND3D11DDI_OPENADAPTER) {
    const unsigned stack_bytes = (unsigned)aerogpu_stdcall_stack_bytes<PFND3D11DDI_OPENADAPTER>::value;
    printf("OpenAdapter11   => _OpenAdapter11@%u\n", stack_bytes);
  }
  __if_not_exists(PFND3D11DDI_OPENADAPTER) {
    printf("OpenAdapter11   => <typedef PFND3D11DDI_OPENADAPTER not found>\n");
  }
#else
  printf("x64: no stdcall decoration\n");
#endif

  return 0;
}
