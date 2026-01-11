// Win7 D3D10/11 UMD ABI probe (WDK headers).
//
// This program is intended to be compiled in a Win7-era WDK build environment so
// that `d3d10umddi.h`, `d3d10_1umddi.h`, and `d3d11umddi.h` are available on the
// include path. It prints:
// - key struct sizes/offsets
// - expected x86 stdcall export decoration for OpenAdapter10/10_2/11

#include <windows.h>

#include <d3d10umddi.h>
#include <d3d10_1umddi.h>
#include <d3d11umddi.h>

#include <cstdio>
#include <cstddef>
#include <type_traits>
#include <utility>

template <typename T, typename = void>
struct has_member_pAdapterCallbacks : std::false_type {};
template <typename T>
struct has_member_pAdapterCallbacks<T, std::void_t<decltype(std::declval<T>().pAdapterCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hRTAdapter : std::false_type {};
template <typename T>
struct has_member_hRTAdapter<T, std::void_t<decltype(std::declval<T>().hRTAdapter)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnCalcPrivateDeviceContextSize : std::false_type {};
template <typename T>
struct has_member_pfnCalcPrivateDeviceContextSize<T, std::void_t<decltype(std::declval<T>().pfnCalcPrivateDeviceContextSize)>>
    : std::true_type {};

int main() {
  std::printf("== Win7 D3D10/11 UMD WDK ABI probe ==\n");

#if defined(_M_IX86)
  std::printf("arch: x86\n");
#elif defined(_M_X64)
  std::printf("arch: x64\n");
#else
  std::printf("arch: unknown\n");
#endif

  std::printf("sizeof(void*) = %zu\n", sizeof(void*));
  std::printf("\n");

  std::printf("== D3D10DDIARG_OPENADAPTER ==\n");
  std::printf("  sizeof(D3D10DDIARG_OPENADAPTER) = %zu\n", sizeof(D3D10DDIARG_OPENADAPTER));
  std::printf("  offsetof(Interface) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, Interface));
  std::printf("  offsetof(Version) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, Version));
  std::printf("  offsetof(hAdapter) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, hAdapter));
  if constexpr (has_member_hRTAdapter<D3D10DDIARG_OPENADAPTER>::value) {
    std::printf("  offsetof(hRTAdapter) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, hRTAdapter));
  } else {
    std::printf("  offsetof(hRTAdapter) = <absent>\n");
  }
  if constexpr (has_member_pAdapterCallbacks<D3D10DDIARG_OPENADAPTER>::value) {
    std::printf("  offsetof(pAdapterCallbacks) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, pAdapterCallbacks));
  } else {
    std::printf("  offsetof(pAdapterCallbacks) = <absent>\n");
  }
  std::printf("  offsetof(pAdapterFuncs) = %zu\n", offsetof(D3D10DDIARG_OPENADAPTER, pAdapterFuncs));
  std::printf("\n");

  std::printf("== Interface constants ==\n");
  std::printf("  D3D10DDI_INTERFACE_VERSION  = 0x%08X\n", static_cast<unsigned>(D3D10DDI_INTERFACE_VERSION));
  std::printf("  D3D10DDI_SUPPORTED          = 0x%08X\n", static_cast<unsigned>(D3D10DDI_SUPPORTED));
  std::printf("  D3D10_1DDI_INTERFACE_VERSION= 0x%08X\n", static_cast<unsigned>(D3D10_1DDI_INTERFACE_VERSION));
  std::printf("  D3D10_1DDI_SUPPORTED         = 0x%08X\n", static_cast<unsigned>(D3D10_1DDI_SUPPORTED));
  std::printf("  D3D11DDI_INTERFACE_VERSION  = 0x%08X\n", static_cast<unsigned>(D3D11DDI_INTERFACE_VERSION));
#ifdef D3D11DDI_INTERFACE
  std::printf("  D3D11DDI_INTERFACE          = 0x%08X\n", static_cast<unsigned>(D3D11DDI_INTERFACE));
#endif
#ifdef D3D11DDI_SUPPORTED
  std::printf("  D3D11DDI_SUPPORTED          = 0x%08X\n", static_cast<unsigned>(D3D11DDI_SUPPORTED));
#endif
  std::printf("\n");

  std::printf("== Win7 caps enum values (for tracing) ==\n");
  std::printf("  D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL          = %u\n",
              static_cast<unsigned>(D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL));
  std::printf("  D3D10DDICAPS_TYPE_FORMAT_SUPPORT               = %u\n",
              static_cast<unsigned>(D3D10DDICAPS_TYPE_FORMAT_SUPPORT));
  std::printf("  D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS   = %u\n",
              static_cast<unsigned>(D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS));
  __if_exists(D3D10DDICAPS_TYPE_SHADER) {
    std::printf("  D3D10DDICAPS_TYPE_SHADER                       = %u\n",
                static_cast<unsigned>(D3D10DDICAPS_TYPE_SHADER));
  }

  std::printf("  D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL        = %u\n",
              static_cast<unsigned>(D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL));
  std::printf("  D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT             = %u\n",
              static_cast<unsigned>(D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT));
  std::printf("  D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS = %u\n",
              static_cast<unsigned>(D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS));
  __if_exists(D3D10_1DDICAPS_TYPE_SHADER) {
    std::printf("  D3D10_1DDICAPS_TYPE_SHADER                     = %u\n",
                static_cast<unsigned>(D3D10_1DDICAPS_TYPE_SHADER));
  }

  std::printf("  D3D11DDICAPS_TYPE_THREADING                    = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_THREADING));
  std::printf("  D3D11DDICAPS_TYPE_DOUBLES                      = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_DOUBLES));
  std::printf("  D3D11DDICAPS_TYPE_FORMAT                       = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_FORMAT));
  // Some WDKs don't expose a named FORMAT_SUPPORT2 enum member. The runtime
  // still uses it (commonly value 3) for D3D11_FEATURE_FORMAT_SUPPORT2.
  std::printf("  D3D11DDICAPS_TYPE_FORMAT_SUPPORT2              = %u (if present)\n", 3u);
  std::printf("  D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS     = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS));
  std::printf("  D3D11DDICAPS_TYPE_D3D11_OPTIONS                = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_D3D11_OPTIONS));
  std::printf("  D3D11DDICAPS_TYPE_ARCHITECTURE_INFO            = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_ARCHITECTURE_INFO));
  std::printf("  D3D11DDICAPS_TYPE_D3D9_OPTIONS                 = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_D3D9_OPTIONS));
  std::printf("  D3D11DDICAPS_TYPE_FEATURE_LEVELS               = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_FEATURE_LEVELS));
  std::printf("  D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS   = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS));
  std::printf("  D3D11DDICAPS_TYPE_SHADER                       = %u\n",
              static_cast<unsigned>(D3D11DDICAPS_TYPE_SHADER));
  std::printf("\n");

  std::printf("== D3D11DDI_ADAPTERFUNCS ==\n");
  std::printf("  sizeof(D3D11DDI_ADAPTERFUNCS) = %zu\n", sizeof(D3D11DDI_ADAPTERFUNCS));
  std::printf("  offsetof(pfnGetCaps) = %zu\n", offsetof(D3D11DDI_ADAPTERFUNCS, pfnGetCaps));
  std::printf("  offsetof(pfnCalcPrivateDeviceSize) = %zu\n", offsetof(D3D11DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceSize));
  if constexpr (has_member_pfnCalcPrivateDeviceContextSize<D3D11DDI_ADAPTERFUNCS>::value) {
    std::printf("  offsetof(pfnCalcPrivateDeviceContextSize) = %zu\n",
                offsetof(D3D11DDI_ADAPTERFUNCS, pfnCalcPrivateDeviceContextSize));
  } else {
    std::printf("  offsetof(pfnCalcPrivateDeviceContextSize) = <absent>\n");
  }
  std::printf("  offsetof(pfnCreateDevice) = %zu\n", offsetof(D3D11DDI_ADAPTERFUNCS, pfnCreateDevice));
  std::printf("  offsetof(pfnCloseAdapter) = %zu\n", offsetof(D3D11DDI_ADAPTERFUNCS, pfnCloseAdapter));
  std::printf("\n");

  std::printf("== Exported entrypoints ==\n");
  std::printf("  runtime expects: OpenAdapter10, OpenAdapter10_2, OpenAdapter11\n");
  if (sizeof(void*) == 4) {
    const unsigned stack_bytes = static_cast<unsigned>(sizeof(void*));
    std::printf("  x86 stdcall decoration:\n");
    std::printf("    OpenAdapter10   => _OpenAdapter10@%u\n", stack_bytes);
    std::printf("    OpenAdapter10_2 => _OpenAdapter10_2@%u\n", stack_bytes);
    std::printf("    OpenAdapter11   => _OpenAdapter11@%u\n", stack_bytes);
  } else {
    std::printf("  x64: no stdcall decoration\n");
  }

  return 0;
}
