#include "../include/aerogpu_d3d9_umd.h"
#include "aerogpu_d3d9_wdk_abi_asserts.h"

#include <array>
#include <algorithm>
#include <bitset>
#include <chrono>
#include <cctype>
#include <cstddef>
#include <cstring>
#include <cstdlib>
#include <cwchar>
#include <initializer_list>
#include <limits>
#include <memory>
#include <mutex>
#include <new>
#include <thread>
#include <type_traits>
#include <unordered_map>
#include <utility>

#if defined(_WIN32)
  #include <d3d9types.h>
#endif

#ifndef D3DVS_VERSION
  #define D3DVS_VERSION(major, minor) (0xFFFE0000u | ((major) << 8) | (minor))
#endif

#ifndef D3DPS_VERSION
  #define D3DPS_VERSION(major, minor) (0xFFFF0000u | ((major) << 8) | (minor))
#endif

#include "aerogpu_d3d9_caps.h"
#include "aerogpu_d3d9_blit.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_submit.h"
#include "aerogpu_d3d9_dma_priv.h"
#include "aerogpu_wddm_submit_buffer_utils.h"
#include "aerogpu_win7_abi.h"
#include "aerogpu_log.h"
#include "aerogpu_alloc.h"
#include "aerogpu_trace.h"
#include "aerogpu_wddm_alloc.h"
#include "../../common/aerogpu_win32_security.h"

namespace {

template <typename T, typename = void>
struct has_interface_version_member : std::false_type {};

template <typename T>
struct has_interface_version_member<T, std::void_t<decltype(std::declval<T>().InterfaceVersion)>> : std::true_type {};

template <typename T>
UINT get_interface_version(const T* open) {
  if (!open) {
    return 0;
  }
  if constexpr (has_interface_version_member<T>::value) {
    return open->InterfaceVersion;
  }
  return open->Interface;
}

template <typename T, typename = void>
struct has_adapter_callbacks2_member : std::false_type {};

template <typename T>
struct has_adapter_callbacks2_member<T, std::void_t<decltype(std::declval<T>().pAdapterCallbacks2)>> : std::true_type {};

template <typename T>
D3DDDI_ADAPTERCALLBACKS2* get_adapter_callbacks2(T* open) {
  if (!open) {
    return nullptr;
  }
  if constexpr (has_adapter_callbacks2_member<T>::value) {
    return open->pAdapterCallbacks2;
  }
  return nullptr;
}

template <typename T, typename = void>
struct has_vid_pn_source_id_member : std::false_type {};

template <typename T>
struct has_vid_pn_source_id_member<T, std::void_t<decltype(std::declval<T>().VidPnSourceId)>> : std::true_type {};

template <typename T>
void set_vid_pn_source_id(T* open, UINT vid_pn_source_id) {
  if (!open) {
    return;
  }
  if constexpr (has_vid_pn_source_id_member<T>::value) {
    open->VidPnSourceId = vid_pn_source_id;
  } else {
    (void)vid_pn_source_id;
  }
}

} // namespace

namespace aerogpu {

// D3D9 StateBlock (BeginStateBlock/EndStateBlock + Create/Capture/Apply).
//
// This is a minimal state capture model that records the subset of device state
// the current AeroGPU D3D9 UMD already understands/emits:
// - render states
// - sampler states
// - texture bindings
// - render target + depth/stencil bindings
// - viewport + scissor
// - VB/IB bindings
// - vertex decl / FVF hint
// - shader bindings + float constants
//
// State blocks are runtime-managed objects; the runtime owns their lifetime and
// invokes DeleteStateBlock when released.
struct StateBlock {
  // Render state (D3DRS_*). Only the 0..255 range is cached by the UMD today.
  std::bitset<256> render_state_mask{};
  std::array<uint32_t, 256> render_state_values{};

  // Sampler state (D3DSAMP_*). Cached as [stage][state], with both ranges 0..15.
  std::bitset<16 * 16> sampler_state_mask{}; // stage * 16 + state
  std::array<uint32_t, 16 * 16> sampler_state_values{};

  // Texture bindings (pixel shader stages only; 0..15).
  std::bitset<16> texture_mask{};
  std::array<Resource*, 16> textures{};

  // Render target bindings (0..3) + depth/stencil.
  std::bitset<4> render_target_mask{};
  std::array<Resource*, 4> render_targets{};
  bool depth_stencil_set = false;
  Resource* depth_stencil = nullptr;

  // Viewport + scissor.
  bool viewport_set = false;
  D3DDDIVIEWPORTINFO viewport = {0, 0, 0, 0, 0.0f, 1.0f};
  bool scissor_set = false;
  RECT scissor_rect = {0, 0, 0, 0};
  BOOL scissor_enabled = FALSE;

  // VB/IB bindings.
  std::bitset<16> stream_mask{};
  std::array<DeviceStateStream, 16> streams{};
  bool index_buffer_set = false;
  Resource* index_buffer = nullptr;
  D3DDDIFORMAT index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  uint32_t index_offset_bytes = 0;

  // Input layout state.
  bool vertex_decl_set = false;
  VertexDecl* vertex_decl = nullptr;
  bool fvf_set = false;
  uint32_t fvf = 0;

  // Shader bindings (D3D9 stages: VS/PS) + float constants.
  bool user_vs_set = false;
  Shader* user_vs = nullptr;
  bool user_ps_set = false;
  Shader* user_ps = nullptr;

  std::bitset<256> vs_const_mask{};
  std::array<float, 256 * 4> vs_consts{};
  std::bitset<256> ps_const_mask{};
  std::array<float, 256 * 4> ps_consts{};
};

namespace {

#define AEROGPU_D3D9_STUB_LOG_ONCE()                 \
  do {                                               \
    static std::once_flag aerogpu_once;              \
    const char* fn = __func__;                       \
    std::call_once(aerogpu_once, [fn] {              \
      aerogpu::logf("aerogpu-d3d9: stub %s\n", fn);  \
    });                                              \
  } while (0)

template <typename FuncTable>
const char* d3d9_vtable_member_name(size_t index);

template <typename FuncTable>
bool d3d9_validate_nonnull_vtable(const FuncTable* table, const char* table_name) {
  if (!table || !table_name) {
    return false;
  }

  static_assert(sizeof(FuncTable) % sizeof(void*) == 0, "D3D9 DDI function tables must be pointer arrays");
  const uint8_t* bytes = reinterpret_cast<const uint8_t*>(table);
  constexpr size_t kPtrBytes = sizeof(void*);
  const std::array<uint8_t, kPtrBytes> zero{};
  const size_t count = sizeof(FuncTable) / kPtrBytes;

  for (size_t i = 0; i < count; ++i) {
    const uint8_t* slot = bytes + i * kPtrBytes;
    if (std::memcmp(slot, zero.data(), kPtrBytes) == 0) {
      const char* member_name = d3d9_vtable_member_name<FuncTable>(i);
      if (member_name) {
        aerogpu::logf("aerogpu-d3d9: %s missing entry index=%llu (bytes=%llu) member=%s\n",
                      table_name,
                      static_cast<unsigned long long>(i),
                      static_cast<unsigned long long>(i * sizeof(void*)),
                      member_name);
      } else {
        aerogpu::logf("aerogpu-d3d9: %s missing entry index=%llu (bytes=%llu)\n",
                      table_name,
                      static_cast<unsigned long long>(i),
                      static_cast<unsigned long long>(i * sizeof(void*)));
      }
      return false;
    }
  }
  return true;
}

constexpr int32_t kMinGpuThreadPriority = -7;
constexpr int32_t kMaxGpuThreadPriority = 7;

// D3DERR_INVALIDCALL (0x8876086C) is returned by the UMD for invalid arguments.
constexpr HRESULT kD3DErrInvalidCall = static_cast<HRESULT>(0x8876086CL);

// S_PRESENT_OCCLUDED (0x08760868) is returned by CheckDeviceState/PresentEx when
// the target window is occluded/minimized. Prefer the SDK macro when available
// but provide a fallback so repo builds don't need d3d9.h.
#if defined(S_PRESENT_OCCLUDED)
constexpr HRESULT kSPresentOccluded = S_PRESENT_OCCLUDED;
#else
constexpr HRESULT kSPresentOccluded = 0x08760868L;
#endif

// D3D9 API/UMD query constants (numeric values from d3d9types.h).
constexpr uint32_t kD3DQueryTypeEvent = 8u;
constexpr uint32_t kD3DIssueEnd = 0x1u;
// Some D3D9 runtimes/WDK header vintages appear to use 0x2 to signal END at the
// DDI boundary (even though the public IDirect3DQuery9::Issue API uses 0x2 for
// BEGIN). Be permissive and accept both encodings for EVENT queries.
constexpr uint32_t kD3DIssueEndAlt = 0x2u;
constexpr uint32_t kD3DGetDataFlush = 0x1u;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// D3DPRESENT_* flags (numeric values from d3d9.h). We only need DONOTWAIT for
// max-frame-latency throttling.
constexpr uint32_t kD3dPresentDoNotWait = 0x00000001u; // D3DPRESENT_DONOTWAIT
constexpr uint32_t kD3dPresentIntervalImmediate = 0x80000000u; // D3DPRESENT_INTERVAL_IMMEDIATE

// D3DERR_WASSTILLDRAWING (0x8876021C). Returned by PresentEx when DONOTWAIT is
// specified and the present is throttled.
constexpr HRESULT kD3dErrWasStillDrawing = static_cast<HRESULT>(-2005532132);

constexpr uint32_t kMaxFrameLatencyMin = 1;
constexpr uint32_t kMaxFrameLatencyMax = 16;

// Bounded wait for PresentEx throttling. This must be finite to avoid hangs in
// DWM/PresentEx call sites if the GPU stops making forward progress.
constexpr uint32_t kPresentThrottleMaxWaitMs = 100;

// Some WDDM/D3D9 callback structs may not expose `SubmissionFenceId`/`NewFenceValue`
// depending on the WDK header vintage. When the runtime does not provide a
// per-submission fence value via the callback out-params, we fall back to
// querying the AeroGPU KMD fence counters via D3DKMTEscape so we still return a
// real fence value for the submission.

std::once_flag g_submit_log_once;
bool g_submit_log_enabled = false;
#if defined(_WIN32)
std::once_flag g_dma_priv_invalid_once;
std::once_flag g_dma_priv_size_mismatch_once;
#endif

bool submit_log_enabled() {
  std::call_once(g_submit_log_once, [] {
#if defined(_WIN32)
    char buf[32] = {};
    const DWORD n = GetEnvironmentVariableA("AEROGPU_D3D9_LOG_SUBMITS", buf, static_cast<DWORD>(sizeof(buf)));
    if (n == 0 || n >= sizeof(buf)) {
      g_submit_log_enabled = false;
      return;
    }
    for (char& c : buf) {
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    g_submit_log_enabled = (std::strcmp(buf, "1") == 0 || std::strcmp(buf, "true") == 0 || std::strcmp(buf, "yes") == 0 ||
                            std::strcmp(buf, "on") == 0);
#else
    const char* v = std::getenv("AEROGPU_D3D9_LOG_SUBMITS");
    if (!v || !*v) {
      g_submit_log_enabled = false;
      return;
    }
    char buf[32] = {};
    std::strncpy(buf, v, sizeof(buf) - 1);
    buf[sizeof(buf) - 1] = 0;
    for (char& c : buf) {
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    g_submit_log_enabled = (std::strcmp(buf, "1") == 0 || std::strcmp(buf, "true") == 0 || std::strcmp(buf, "yes") == 0 ||
                            std::strcmp(buf, "on") == 0);
#endif
  });
  return g_submit_log_enabled;
}

// Some D3D9 UMD DDI members vary across WDK header vintages. Use compile-time
// detection (SFINAE) so the UMD can populate as many entrypoints as possible
// without hard-failing compilation when a member is absent.
//
// This mirrors the approach in `tools/wdk_abi_probe/`.
#define AEROGPU_DEFINE_HAS_MEMBER(member)                                                      \
  template <typename T, typename = void>                                                       \
  struct aerogpu_has_member_##member : std::false_type {};                                      \
  template <typename T>                                                                        \
  struct aerogpu_has_member_##member<T, std::void_t<decltype(&T::member)>> : std::true_type {}

AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource);
AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource2);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetFVF);
AEROGPU_DEFINE_HAS_MEMBER(pfnBeginScene);
AEROGPU_DEFINE_HAS_MEMBER(pfnEndScene);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawPrimitive2);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawIndexedPrimitive2);
AEROGPU_DEFINE_HAS_MEMBER(pfnWaitForVBlank);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetGPUThreadPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetGPUThreadPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnCheckResourceResidency);
AEROGPU_DEFINE_HAS_MEMBER(pfnQueryResourceResidency);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetPriority);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetDisplayModeEx);
AEROGPU_DEFINE_HAS_MEMBER(pfnComposeRects);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetConvolutionMonoKernel);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetAutoGenFilterType);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetAutoGenFilterType);
AEROGPU_DEFINE_HAS_MEMBER(pfnGenerateMipSubLevels);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawPrimitiveUP);

// Fixed function / legacy state paths (commonly hit by DWM + simple D3D9 apps).
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTextureStageState);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTransform);
AEROGPU_DEFINE_HAS_MEMBER(pfnMultiplyTransform);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetClipPlane);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetShaderConstI);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetShaderConstB);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetMaterial);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetLight);
AEROGPU_DEFINE_HAS_MEMBER(pfnLightEnable);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetNPatchMode);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetStreamSourceFreq);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetGammaRamp);
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnDeleteStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnCaptureStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnApplyStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnValidateDevice);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetSoftwareVertexProcessing);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetCursorProperties);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetCursorPosition);
AEROGPU_DEFINE_HAS_MEMBER(pfnShowCursor);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetPaletteEntries);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetCurrentTexturePalette);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetClipStatus);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetClipStatus);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetGammaRamp);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawRectPatch);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawTriPatch);
AEROGPU_DEFINE_HAS_MEMBER(pfnDeletePatch);
AEROGPU_DEFINE_HAS_MEMBER(pfnProcessVertices);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetRasterStatus);
AEROGPU_DEFINE_HAS_MEMBER(pfnSetDialogBoxMode);
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawIndexedPrimitiveUP);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetSoftwareVertexProcessing);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetTransform);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetClipPlane);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetViewport);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetScissorRect);
AEROGPU_DEFINE_HAS_MEMBER(pfnBeginStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnEndStateBlock);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetMaterial);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetLight);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetLightEnable);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetRenderTarget);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetDepthStencil);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetTexture);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetTextureStageState);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetSamplerState);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetRenderState);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetPaletteEntries);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetCurrentTexturePalette);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetNPatchMode);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetFVF);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetVertexDecl);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetStreamSource);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetStreamSourceFreq);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetIndices);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetShader);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetShaderConstF);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetShaderConstI);
AEROGPU_DEFINE_HAS_MEMBER(pfnGetShaderConstB);

// OpenResource arg fields (vary across WDK versions).
AEROGPU_DEFINE_HAS_MEMBER(hAllocation);
AEROGPU_DEFINE_HAS_MEMBER(hAllocations);
AEROGPU_DEFINE_HAS_MEMBER(phAllocation);
AEROGPU_DEFINE_HAS_MEMBER(pOpenAllocationInfo);
AEROGPU_DEFINE_HAS_MEMBER(NumAllocations);

#undef AEROGPU_DEFINE_HAS_MEMBER

template <typename FuncTable>
const char* d3d9_vtable_member_name(size_t index) {
  constexpr size_t kPtrBytes = sizeof(void*);
  if constexpr (std::is_same_v<FuncTable, D3D9DDI_ADAPTERFUNCS>) {
    if (index == offsetof(FuncTable, pfnCloseAdapter) / kPtrBytes) {
      return "pfnCloseAdapter";
    }
    if (index == offsetof(FuncTable, pfnGetCaps) / kPtrBytes) {
      return "pfnGetCaps";
    }
    if (index == offsetof(FuncTable, pfnCreateDevice) / kPtrBytes) {
      return "pfnCreateDevice";
    }
    if (index == offsetof(FuncTable, pfnQueryAdapterInfo) / kPtrBytes) {
      return "pfnQueryAdapterInfo";
    }
    return nullptr;
  }

  if constexpr (std::is_same_v<FuncTable, D3D9DDI_DEVICEFUNCS>) {
    if (index == offsetof(FuncTable, pfnDestroyDevice) / kPtrBytes) {
      return "pfnDestroyDevice";
    }
    if (index == offsetof(FuncTable, pfnCreateResource) / kPtrBytes) {
      return "pfnCreateResource";
    }
    if constexpr (aerogpu_has_member_pfnOpenResource<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnOpenResource) / kPtrBytes) {
        return "pfnOpenResource";
      }
    }
    if constexpr (aerogpu_has_member_pfnOpenResource2<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnOpenResource2) / kPtrBytes) {
        return "pfnOpenResource2";
      }
    }
    if (index == offsetof(FuncTable, pfnDestroyResource) / kPtrBytes) {
      return "pfnDestroyResource";
    }
    if (index == offsetof(FuncTable, pfnLock) / kPtrBytes) {
      return "pfnLock";
    }
    if (index == offsetof(FuncTable, pfnUnlock) / kPtrBytes) {
      return "pfnUnlock";
    }
    if (index == offsetof(FuncTable, pfnSetRenderTarget) / kPtrBytes) {
      return "pfnSetRenderTarget";
    }
    if (index == offsetof(FuncTable, pfnSetDepthStencil) / kPtrBytes) {
      return "pfnSetDepthStencil";
    }
    if (index == offsetof(FuncTable, pfnSetViewport) / kPtrBytes) {
      return "pfnSetViewport";
    }
    if (index == offsetof(FuncTable, pfnSetScissorRect) / kPtrBytes) {
      return "pfnSetScissorRect";
    }
    if (index == offsetof(FuncTable, pfnSetTexture) / kPtrBytes) {
      return "pfnSetTexture";
    }
    if constexpr (aerogpu_has_member_pfnSetTextureStageState<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetTextureStageState) / kPtrBytes) {
        return "pfnSetTextureStageState";
      }
    }
    if (index == offsetof(FuncTable, pfnSetSamplerState) / kPtrBytes) {
      return "pfnSetSamplerState";
    }
    if (index == offsetof(FuncTable, pfnSetRenderState) / kPtrBytes) {
      return "pfnSetRenderState";
    }
    if constexpr (aerogpu_has_member_pfnSetMaterial<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetMaterial) / kPtrBytes) {
        return "pfnSetMaterial";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetLight<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetLight) / kPtrBytes) {
        return "pfnSetLight";
      }
    }
    if constexpr (aerogpu_has_member_pfnLightEnable<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnLightEnable) / kPtrBytes) {
        return "pfnLightEnable";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetNPatchMode<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetNPatchMode) / kPtrBytes) {
        return "pfnSetNPatchMode";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetGammaRamp<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetGammaRamp) / kPtrBytes) {
        return "pfnSetGammaRamp";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetTransform<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetTransform) / kPtrBytes) {
        return "pfnSetTransform";
      }
    }
    if constexpr (aerogpu_has_member_pfnMultiplyTransform<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnMultiplyTransform) / kPtrBytes) {
        return "pfnMultiplyTransform";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetClipPlane<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetClipPlane) / kPtrBytes) {
        return "pfnSetClipPlane";
      }
    }
    if (index == offsetof(FuncTable, pfnCreateVertexDecl) / kPtrBytes) {
      return "pfnCreateVertexDecl";
    }
    if (index == offsetof(FuncTable, pfnSetVertexDecl) / kPtrBytes) {
      return "pfnSetVertexDecl";
    }
    if (index == offsetof(FuncTable, pfnDestroyVertexDecl) / kPtrBytes) {
      return "pfnDestroyVertexDecl";
    }
    if constexpr (aerogpu_has_member_pfnSetFVF<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetFVF) / kPtrBytes) {
        return "pfnSetFVF";
      }
    }
    if (index == offsetof(FuncTable, pfnCreateShader) / kPtrBytes) {
      return "pfnCreateShader";
    }
    if (index == offsetof(FuncTable, pfnSetShader) / kPtrBytes) {
      return "pfnSetShader";
    }
    if (index == offsetof(FuncTable, pfnDestroyShader) / kPtrBytes) {
      return "pfnDestroyShader";
    }
    if (index == offsetof(FuncTable, pfnSetShaderConstF) / kPtrBytes) {
      return "pfnSetShaderConstF";
    }
    if constexpr (aerogpu_has_member_pfnSetShaderConstI<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetShaderConstI) / kPtrBytes) {
        return "pfnSetShaderConstI";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetShaderConstB<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetShaderConstB) / kPtrBytes) {
        return "pfnSetShaderConstB";
      }
    }
    if constexpr (aerogpu_has_member_pfnCreateStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnCreateStateBlock) / kPtrBytes) {
        return "pfnCreateStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnDeleteStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDeleteStateBlock) / kPtrBytes) {
        return "pfnDeleteStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnCaptureStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnCaptureStateBlock) / kPtrBytes) {
        return "pfnCaptureStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnApplyStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnApplyStateBlock) / kPtrBytes) {
        return "pfnApplyStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnValidateDevice<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnValidateDevice) / kPtrBytes) {
        return "pfnValidateDevice";
      }
    }
    if (index == offsetof(FuncTable, pfnSetStreamSource) / kPtrBytes) {
      return "pfnSetStreamSource";
    }
    if constexpr (aerogpu_has_member_pfnSetStreamSourceFreq<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetStreamSourceFreq) / kPtrBytes) {
        return "pfnSetStreamSourceFreq";
      }
    }
    if (index == offsetof(FuncTable, pfnSetIndices) / kPtrBytes) {
      return "pfnSetIndices";
    }
    if constexpr (aerogpu_has_member_pfnSetSoftwareVertexProcessing<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetSoftwareVertexProcessing) / kPtrBytes) {
        return "pfnSetSoftwareVertexProcessing";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetCursorProperties<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetCursorProperties) / kPtrBytes) {
        return "pfnSetCursorProperties";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetCursorPosition<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetCursorPosition) / kPtrBytes) {
        return "pfnSetCursorPosition";
      }
    }
    if constexpr (aerogpu_has_member_pfnShowCursor<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnShowCursor) / kPtrBytes) {
        return "pfnShowCursor";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetPaletteEntries<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetPaletteEntries) / kPtrBytes) {
        return "pfnSetPaletteEntries";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetCurrentTexturePalette<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetCurrentTexturePalette) / kPtrBytes) {
        return "pfnSetCurrentTexturePalette";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetClipStatus<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetClipStatus) / kPtrBytes) {
        return "pfnSetClipStatus";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetClipStatus<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetClipStatus) / kPtrBytes) {
        return "pfnGetClipStatus";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetGammaRamp<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetGammaRamp) / kPtrBytes) {
        return "pfnGetGammaRamp";
      }
    }
    if constexpr (aerogpu_has_member_pfnBeginScene<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnBeginScene) / kPtrBytes) {
        return "pfnBeginScene";
      }
    }
    if constexpr (aerogpu_has_member_pfnEndScene<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnEndScene) / kPtrBytes) {
        return "pfnEndScene";
      }
    }
    if (index == offsetof(FuncTable, pfnClear) / kPtrBytes) {
      return "pfnClear";
    }
    if (index == offsetof(FuncTable, pfnDrawPrimitive) / kPtrBytes) {
      return "pfnDrawPrimitive";
    }
    if constexpr (aerogpu_has_member_pfnDrawPrimitiveUP<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawPrimitiveUP) / kPtrBytes) {
        return "pfnDrawPrimitiveUP";
      }
    }
    if constexpr (aerogpu_has_member_pfnDrawIndexedPrimitiveUP<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawIndexedPrimitiveUP) / kPtrBytes) {
        return "pfnDrawIndexedPrimitiveUP";
      }
    }
    if (index == offsetof(FuncTable, pfnDrawIndexedPrimitive) / kPtrBytes) {
      return "pfnDrawIndexedPrimitive";
    }
    if constexpr (aerogpu_has_member_pfnDrawRectPatch<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawRectPatch) / kPtrBytes) {
        return "pfnDrawRectPatch";
      }
    }
    if constexpr (aerogpu_has_member_pfnDrawTriPatch<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawTriPatch) / kPtrBytes) {
        return "pfnDrawTriPatch";
      }
    }
    if constexpr (aerogpu_has_member_pfnDeletePatch<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDeletePatch) / kPtrBytes) {
        return "pfnDeletePatch";
      }
    }
    if constexpr (aerogpu_has_member_pfnProcessVertices<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnProcessVertices) / kPtrBytes) {
        return "pfnProcessVertices";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetRasterStatus<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetRasterStatus) / kPtrBytes) {
        return "pfnGetRasterStatus";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetDialogBoxMode<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetDialogBoxMode) / kPtrBytes) {
        return "pfnSetDialogBoxMode";
      }
    }
    if constexpr (aerogpu_has_member_pfnDrawPrimitive2<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawPrimitive2) / kPtrBytes) {
        return "pfnDrawPrimitive2";
      }
    }
    if constexpr (aerogpu_has_member_pfnDrawIndexedPrimitive2<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnDrawIndexedPrimitive2) / kPtrBytes) {
        return "pfnDrawIndexedPrimitive2";
      }
    }
    if (index == offsetof(FuncTable, pfnCreateSwapChain) / kPtrBytes) {
      return "pfnCreateSwapChain";
    }
    if (index == offsetof(FuncTable, pfnDestroySwapChain) / kPtrBytes) {
      return "pfnDestroySwapChain";
    }
    if (index == offsetof(FuncTable, pfnGetSwapChain) / kPtrBytes) {
      return "pfnGetSwapChain";
    }
    if (index == offsetof(FuncTable, pfnSetSwapChain) / kPtrBytes) {
      return "pfnSetSwapChain";
    }
    if (index == offsetof(FuncTable, pfnReset) / kPtrBytes) {
      return "pfnReset";
    }
    if (index == offsetof(FuncTable, pfnResetEx) / kPtrBytes) {
      return "pfnResetEx";
    }
    if (index == offsetof(FuncTable, pfnCheckDeviceState) / kPtrBytes) {
      return "pfnCheckDeviceState";
    }
    if constexpr (aerogpu_has_member_pfnWaitForVBlank<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnWaitForVBlank) / kPtrBytes) {
        return "pfnWaitForVBlank";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetGPUThreadPriority<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetGPUThreadPriority) / kPtrBytes) {
        return "pfnSetGPUThreadPriority";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetGPUThreadPriority<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetGPUThreadPriority) / kPtrBytes) {
        return "pfnGetGPUThreadPriority";
      }
    }
    if constexpr (aerogpu_has_member_pfnCheckResourceResidency<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnCheckResourceResidency) / kPtrBytes) {
        return "pfnCheckResourceResidency";
      }
    }
    if constexpr (aerogpu_has_member_pfnQueryResourceResidency<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnQueryResourceResidency) / kPtrBytes) {
        return "pfnQueryResourceResidency";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetPriority<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetPriority) / kPtrBytes) {
        return "pfnSetPriority";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetPriority<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetPriority) / kPtrBytes) {
        return "pfnGetPriority";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetDisplayModeEx<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetDisplayModeEx) / kPtrBytes) {
        return "pfnGetDisplayModeEx";
      }
    }
    if constexpr (aerogpu_has_member_pfnComposeRects<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnComposeRects) / kPtrBytes) {
        return "pfnComposeRects";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetConvolutionMonoKernel<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetConvolutionMonoKernel) / kPtrBytes) {
        return "pfnSetConvolutionMonoKernel";
      }
    }
    if constexpr (aerogpu_has_member_pfnSetAutoGenFilterType<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnSetAutoGenFilterType) / kPtrBytes) {
        return "pfnSetAutoGenFilterType";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetAutoGenFilterType<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetAutoGenFilterType) / kPtrBytes) {
        return "pfnGetAutoGenFilterType";
      }
    }
    if constexpr (aerogpu_has_member_pfnGenerateMipSubLevels<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGenerateMipSubLevels) / kPtrBytes) {
        return "pfnGenerateMipSubLevels";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetSoftwareVertexProcessing<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetSoftwareVertexProcessing) / kPtrBytes) {
        return "pfnGetSoftwareVertexProcessing";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetTransform<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetTransform) / kPtrBytes) {
        return "pfnGetTransform";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetClipPlane<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetClipPlane) / kPtrBytes) {
        return "pfnGetClipPlane";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetViewport<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetViewport) / kPtrBytes) {
        return "pfnGetViewport";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetScissorRect<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetScissorRect) / kPtrBytes) {
        return "pfnGetScissorRect";
      }
    }
    if constexpr (aerogpu_has_member_pfnBeginStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnBeginStateBlock) / kPtrBytes) {
        return "pfnBeginStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnEndStateBlock<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnEndStateBlock) / kPtrBytes) {
        return "pfnEndStateBlock";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetMaterial<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetMaterial) / kPtrBytes) {
        return "pfnGetMaterial";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetLight<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetLight) / kPtrBytes) {
        return "pfnGetLight";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetLightEnable<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetLightEnable) / kPtrBytes) {
        return "pfnGetLightEnable";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetRenderTarget<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetRenderTarget) / kPtrBytes) {
        return "pfnGetRenderTarget";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetDepthStencil<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetDepthStencil) / kPtrBytes) {
        return "pfnGetDepthStencil";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetTexture<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetTexture) / kPtrBytes) {
        return "pfnGetTexture";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetTextureStageState<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetTextureStageState) / kPtrBytes) {
        return "pfnGetTextureStageState";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetSamplerState<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetSamplerState) / kPtrBytes) {
        return "pfnGetSamplerState";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetRenderState<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetRenderState) / kPtrBytes) {
        return "pfnGetRenderState";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetPaletteEntries<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetPaletteEntries) / kPtrBytes) {
        return "pfnGetPaletteEntries";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetCurrentTexturePalette<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetCurrentTexturePalette) / kPtrBytes) {
        return "pfnGetCurrentTexturePalette";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetNPatchMode<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetNPatchMode) / kPtrBytes) {
        return "pfnGetNPatchMode";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetFVF<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetFVF) / kPtrBytes) {
        return "pfnGetFVF";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetVertexDecl<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetVertexDecl) / kPtrBytes) {
        return "pfnGetVertexDecl";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetStreamSource<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetStreamSource) / kPtrBytes) {
        return "pfnGetStreamSource";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetStreamSourceFreq<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetStreamSourceFreq) / kPtrBytes) {
        return "pfnGetStreamSourceFreq";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetIndices<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetIndices) / kPtrBytes) {
        return "pfnGetIndices";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetShader<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetShader) / kPtrBytes) {
        return "pfnGetShader";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetShaderConstF<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetShaderConstF) / kPtrBytes) {
        return "pfnGetShaderConstF";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetShaderConstI<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetShaderConstI) / kPtrBytes) {
        return "pfnGetShaderConstI";
      }
    }
    if constexpr (aerogpu_has_member_pfnGetShaderConstB<FuncTable>::value) {
      if (index == offsetof(FuncTable, pfnGetShaderConstB) / kPtrBytes) {
        return "pfnGetShaderConstB";
      }
    }
    if (index == offsetof(FuncTable, pfnRotateResourceIdentities) / kPtrBytes) {
      return "pfnRotateResourceIdentities";
    }
    if (index == offsetof(FuncTable, pfnPresent) / kPtrBytes) {
      return "pfnPresent";
    }
    if (index == offsetof(FuncTable, pfnPresentEx) / kPtrBytes) {
      return "pfnPresentEx";
    }
    if (index == offsetof(FuncTable, pfnFlush) / kPtrBytes) {
      return "pfnFlush";
    }
    if (index == offsetof(FuncTable, pfnSetMaximumFrameLatency) / kPtrBytes) {
      return "pfnSetMaximumFrameLatency";
    }
    if (index == offsetof(FuncTable, pfnGetMaximumFrameLatency) / kPtrBytes) {
      return "pfnGetMaximumFrameLatency";
    }
    if (index == offsetof(FuncTable, pfnGetPresentStats) / kPtrBytes) {
      return "pfnGetPresentStats";
    }
    if (index == offsetof(FuncTable, pfnGetLastPresentCount) / kPtrBytes) {
      return "pfnGetLastPresentCount";
    }
    if (index == offsetof(FuncTable, pfnCreateQuery) / kPtrBytes) {
      return "pfnCreateQuery";
    }
    if (index == offsetof(FuncTable, pfnDestroyQuery) / kPtrBytes) {
      return "pfnDestroyQuery";
    }
    if (index == offsetof(FuncTable, pfnIssueQuery) / kPtrBytes) {
      return "pfnIssueQuery";
    }
    if (index == offsetof(FuncTable, pfnGetQueryData) / kPtrBytes) {
      return "pfnGetQueryData";
    }
    if (index == offsetof(FuncTable, pfnGetRenderTargetData) / kPtrBytes) {
      return "pfnGetRenderTargetData";
    }
    if (index == offsetof(FuncTable, pfnCopyRects) / kPtrBytes) {
      return "pfnCopyRects";
    }
    if (index == offsetof(FuncTable, pfnWaitForIdle) / kPtrBytes) {
      return "pfnWaitForIdle";
    }
    if (index == offsetof(FuncTable, pfnBlt) / kPtrBytes) {
      return "pfnBlt";
    }
    if (index == offsetof(FuncTable, pfnColorFill) / kPtrBytes) {
      return "pfnColorFill";
    }
    if (index == offsetof(FuncTable, pfnUpdateSurface) / kPtrBytes) {
      return "pfnUpdateSurface";
    }
    if (index == offsetof(FuncTable, pfnUpdateTexture) / kPtrBytes) {
      return "pfnUpdateTexture";
    }
  }
  return nullptr;
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
template <typename T, typename = void>
struct aerogpu_has_member_pDrvPrivate : std::false_type {};

template <typename T>
struct aerogpu_has_member_pDrvPrivate<T, std::void_t<decltype(std::declval<T>().pDrvPrivate)>> : std::true_type {};

template <typename T>
uint64_t d3d9_stub_trace_arg(const T& v) {
  if constexpr (aerogpu_has_member_pDrvPrivate<T>::value) {
    return d3d9_trace_arg_ptr(v.pDrvPrivate);
  } else if constexpr (std::is_pointer_v<T>) {
    return d3d9_trace_arg_ptr(v);
  } else if constexpr (std::is_enum_v<T>) {
    using Under = std::underlying_type_t<T>;
    return static_cast<uint64_t>(static_cast<Under>(v));
  } else if constexpr (std::is_integral_v<T>) {
    return static_cast<uint64_t>(v);
  } else {
    return 0;
  }
}

template <typename... Args>
std::array<uint64_t, 4> d3d9_stub_trace_args(const Args&... args) {
  std::array<uint64_t, 4> out{};
  size_t i = 0;
  (void)std::initializer_list<int>{
      (i < out.size() ? (out[i++] = d3d9_stub_trace_arg(args), 0) : 0)...};
  return out;
}

#define AEROGPU_D3D9_DEFINE_DDI_STUB(member, trace_func, stub_hr)                                \
  template <typename Fn>                                                                         \
  struct aerogpu_d3d9_stub_##member;                                                             \
  template <typename Ret, typename... Args>                                                      \
  struct aerogpu_d3d9_stub_##member<Ret(__stdcall*)(Args...)> {                                  \
    static Ret __stdcall member(Args... args) {                                                   \
      AEROGPU_D3D9_STUB_LOG_ONCE();                                                              \
      const auto packed = d3d9_stub_trace_args(args...);                                         \
      D3d9TraceCall trace(trace_func, packed[0], packed[1], packed[2], packed[3]);               \
      if constexpr (std::is_same_v<Ret, void>) {                                                  \
        (void)trace.ret(stub_hr);                                                                 \
        return;                                                                                   \
      }                                                                                           \
      if constexpr (std::is_same_v<Ret, HRESULT>) {                                               \
        return trace.ret(stub_hr);                                                                \
      }                                                                                           \
      (void)trace.ret(stub_hr);                                                                   \
      return Ret{};                                                                               \
    }                                                                                             \
  };                                                                                              \
  template <typename Ret, typename... Args>                                                      \
  struct aerogpu_d3d9_stub_##member<Ret(*)(Args...)> {                                            \
    static Ret member(Args... args) {                                                             \
      AEROGPU_D3D9_STUB_LOG_ONCE();                                                              \
      const auto packed = d3d9_stub_trace_args(args...);                                         \
      D3d9TraceCall trace(trace_func, packed[0], packed[1], packed[2], packed[3]);               \
      if constexpr (std::is_same_v<Ret, void>) {                                                  \
        (void)trace.ret(stub_hr);                                                                 \
        return;                                                                                   \
      }                                                                                           \
      if constexpr (std::is_same_v<Ret, HRESULT>) {                                               \
        return trace.ret(stub_hr);                                                                \
      }                                                                                           \
      (void)trace.ret(stub_hr);                                                                   \
      return Ret{};                                                                               \
    }                                                                                             \
  }

// Stubbed entrypoints: keep these non-NULL so the Win7 runtime can call into the
// UMD without crashing. See `drivers/aerogpu/umd/d3d9/README.md`.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetTextureStageState, D3d9TraceFunc::DeviceSetTextureStageState, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetTransform, D3d9TraceFunc::DeviceSetTransform, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnMultiplyTransform, D3d9TraceFunc::DeviceMultiplyTransform, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetClipPlane, D3d9TraceFunc::DeviceSetClipPlane, S_OK);

// Shader constant paths (int/bool) are not implemented yet; treat as a no-op to
// keep DWM alive while we bring up shader translation.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetShaderConstI, D3d9TraceFunc::DeviceSetShaderConstI, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetShaderConstB, D3d9TraceFunc::DeviceSetShaderConstB, S_OK);

// Fixed-function lighting/material, N-Patch, instancing, and gamma ramp are not
// supported yet. Treat these as no-ops to avoid Win7 runtime crashes when apps
// use legacy state paths.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetMaterial, D3d9TraceFunc::DeviceSetMaterial, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetLight, D3d9TraceFunc::DeviceSetLight, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnLightEnable, D3d9TraceFunc::DeviceLightEnable, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetNPatchMode, D3d9TraceFunc::DeviceSetNPatchMode, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetStreamSourceFreq, D3d9TraceFunc::DeviceSetStreamSourceFreq, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetGammaRamp, D3d9TraceFunc::DeviceSetGammaRamp, S_OK);

// D3D9Ex image processing API. Treat as a no-op until the fixed-function path is
// fully implemented (DWM should not rely on it).
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetConvolutionMonoKernel, D3d9TraceFunc::DeviceSetConvolutionMonoKernel, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetAutoGenFilterType, D3d9TraceFunc::DeviceSetAutoGenFilterType, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetAutoGenFilterType, D3d9TraceFunc::DeviceGetAutoGenFilterType, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGenerateMipSubLevels, D3d9TraceFunc::DeviceGenerateMipSubLevels, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetPriority, D3d9TraceFunc::DeviceSetPriority, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetPriority, D3d9TraceFunc::DeviceGetPriority, D3DERR_NOTAVAILABLE);

// Cursor, palette, and clip-status management is not implemented yet, but these
// can be treated as benign no-ops for bring-up.
AEROGPU_D3D9_DEFINE_DDI_STUB(
    pfnSetSoftwareVertexProcessing, D3d9TraceFunc::DeviceSetSoftwareVertexProcessing, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetCursorProperties, D3d9TraceFunc::DeviceSetCursorProperties, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetCursorPosition, D3d9TraceFunc::DeviceSetCursorPosition, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnShowCursor, D3d9TraceFunc::DeviceShowCursor, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetPaletteEntries, D3d9TraceFunc::DeviceSetPaletteEntries, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetCurrentTexturePalette, D3d9TraceFunc::DeviceSetCurrentTexturePalette, S_OK);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetClipStatus, D3d9TraceFunc::DeviceSetClipStatus, S_OK);

// "Get" style queries have output parameters; return an explicit failure so the
// runtime does not consume uninitialized output data.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetClipStatus, D3d9TraceFunc::DeviceGetClipStatus, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetGammaRamp, D3d9TraceFunc::DeviceGetGammaRamp, D3DERR_NOTAVAILABLE);

// Patch rendering (N-Patch/patches) and ProcessVertices are not supported yet.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnDrawRectPatch, D3d9TraceFunc::DeviceDrawRectPatch, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnDrawTriPatch, D3d9TraceFunc::DeviceDrawTriPatch, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnDeletePatch, D3d9TraceFunc::DeviceDeletePatch, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnProcessVertices, D3d9TraceFunc::DeviceProcessVertices, D3DERR_NOTAVAILABLE);

// Dialog-box mode impacts present/occlusion semantics; treat as a no-op for bring-up.
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnSetDialogBoxMode, D3d9TraceFunc::DeviceSetDialogBoxMode, S_OK);

// Legacy user-pointer draw path (indexed). Implemented (see device_draw_indexed_primitive_up).

// Various state "getters" (largely used by legacy apps). These have output
// parameters; return a clean failure so callers don't consume uninitialized
// memory.
AEROGPU_D3D9_DEFINE_DDI_STUB(
    pfnGetSoftwareVertexProcessing, D3d9TraceFunc::DeviceGetSoftwareVertexProcessing, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetTransform, D3d9TraceFunc::DeviceGetTransform, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetClipPlane, D3d9TraceFunc::DeviceGetClipPlane, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetViewport, D3d9TraceFunc::DeviceGetViewport, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetScissorRect, D3d9TraceFunc::DeviceGetScissorRect, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetMaterial, D3d9TraceFunc::DeviceGetMaterial, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetLight, D3d9TraceFunc::DeviceGetLight, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetLightEnable, D3d9TraceFunc::DeviceGetLightEnable, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetRenderTarget, D3d9TraceFunc::DeviceGetRenderTarget, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetDepthStencil, D3d9TraceFunc::DeviceGetDepthStencil, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetTexture, D3d9TraceFunc::DeviceGetTexture, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetTextureStageState, D3d9TraceFunc::DeviceGetTextureStageState, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetSamplerState, D3d9TraceFunc::DeviceGetSamplerState, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetRenderState, D3d9TraceFunc::DeviceGetRenderState, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetPaletteEntries, D3d9TraceFunc::DeviceGetPaletteEntries, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(
    pfnGetCurrentTexturePalette, D3d9TraceFunc::DeviceGetCurrentTexturePalette, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetNPatchMode, D3d9TraceFunc::DeviceGetNPatchMode, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetFVF, D3d9TraceFunc::DeviceGetFVF, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetVertexDecl, D3d9TraceFunc::DeviceGetVertexDecl, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetStreamSource, D3d9TraceFunc::DeviceGetStreamSource, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetStreamSourceFreq, D3d9TraceFunc::DeviceGetStreamSourceFreq, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetIndices, D3d9TraceFunc::DeviceGetIndices, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetShader, D3d9TraceFunc::DeviceGetShader, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetShaderConstF, D3d9TraceFunc::DeviceGetShaderConstF, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetShaderConstI, D3d9TraceFunc::DeviceGetShaderConstI, D3DERR_NOTAVAILABLE);
AEROGPU_D3D9_DEFINE_DDI_STUB(pfnGetShaderConstB, D3d9TraceFunc::DeviceGetShaderConstB, D3DERR_NOTAVAILABLE);

#undef AEROGPU_D3D9_DEFINE_DDI_STUB

// -----------------------------------------------------------------------------
// Type-safe D3D9 DDI thunks (WDK builds)
// -----------------------------------------------------------------------------
// The Win7 D3D9 runtime loads the UMD purely by ABI contract. When wiring the
// D3D9DDI_*FUNCS vtables, avoid function-pointer casts that can mask real
// signature mismatches with the WDK headers (calling convention / argument
// types / return types).
//
// Instead, assign pointers to compiler-checked thunks. If an implementation
// signature drifts from what the WDK declares, the build should fail.
template <typename Fn, auto Impl>
struct aerogpu_d3d9_ddi_thunk;

template <typename Ret, typename... Args, auto Impl>
struct aerogpu_d3d9_ddi_thunk<Ret(__stdcall*)(Args...), Impl> {
  using impl_return_t = decltype(Impl(std::declval<Args>()...));
  static_assert(std::is_same_v<impl_return_t, Ret>,
                "D3D9 DDI entrypoint return type mismatch (write an explicit adapter instead of casting)");

  static Ret __stdcall thunk(Args... args) {
    if constexpr (std::is_same_v<Ret, void>) {
      Impl(args...);
      return;
    } else {
      return Impl(args...);
    }
  }
};

template <typename Ret, typename... Args, auto Impl>
struct aerogpu_d3d9_ddi_thunk<Ret(*)(Args...), Impl> {
  using impl_return_t = decltype(Impl(std::declval<Args>()...));
  static_assert(std::is_same_v<impl_return_t, Ret>,
                "D3D9 DDI entrypoint return type mismatch (write an explicit adapter instead of casting)");

  static Ret thunk(Args... args) {
    if constexpr (std::is_same_v<Ret, void>) {
      Impl(args...);
      return;
    } else {
      return Impl(args...);
    }
  }
};
#endif

uint64_t monotonic_ms() {
#if defined(_WIN32)
  return static_cast<uint64_t>(GetTickCount64());
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

namespace {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
template <typename T, typename = void>
struct has_member_hAllocation : std::false_type {};
template <typename T>
struct has_member_hAllocation<T, std::void_t<decltype(std::declval<T&>().hAllocation)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NumAllocations_res : std::false_type {};
template <typename T>
struct has_member_NumAllocations_res<T, std::void_t<decltype(std::declval<T&>().NumAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hAllocations : std::false_type {};
template <typename T>
struct has_member_hAllocations<T, std::void_t<decltype(std::declval<T&>().hAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocations : std::false_type {};
template <typename T>
struct has_member_pAllocations<T, std::void_t<decltype(std::declval<T&>().pAllocations)>> : std::true_type {};

template <typename ArgsT>
WddmAllocationHandle extract_primary_wddm_allocation_handle(const ArgsT& args) {
  if constexpr (has_member_hAllocation<ArgsT>::value &&
                std::is_convertible_v<decltype(std::declval<ArgsT&>().hAllocation), WddmAllocationHandle>) {
    const auto h = static_cast<WddmAllocationHandle>(args.hAllocation);
    if (h != 0) {
      return h;
    }
  }

  if constexpr (has_member_hAllocations<ArgsT>::value) {
    const UINT count = [&]() -> UINT {
      if constexpr (has_member_NumAllocations_res<ArgsT>::value) {
        return static_cast<UINT>(args.NumAllocations);
      }
      return 0;
    }();

    if (count != 0) {
      // `hAllocations` is typically a pointer/array of allocation handles.
      const auto* handles = args.hAllocations;
      if (handles) {
        if constexpr (std::is_convertible_v<std::remove_reference_t<decltype(handles[0])>, WddmAllocationHandle>) {
          const auto h = static_cast<WddmAllocationHandle>(handles[0]);
          if (h != 0) {
            return h;
          }
        }
      }
    }
  }

  if constexpr (has_member_pAllocations<ArgsT>::value) {
    const UINT count = [&]() -> UINT {
      if constexpr (has_member_NumAllocations_res<ArgsT>::value) {
        return static_cast<UINT>(args.NumAllocations);
      }
      // Some structs may omit a count; assume at least 1 when a pointer is present.
      return 1;
    }();

    if (count != 0) {
      const auto* allocs = args.pAllocations;
      if (allocs) {
        using Elem = std::remove_pointer_t<decltype(allocs)>;
        if constexpr (std::is_class_v<Elem> && has_member_hAllocation<Elem>::value &&
                      std::is_convertible_v<decltype(std::declval<Elem&>().hAllocation), WddmAllocationHandle>) {
          const auto h = static_cast<WddmAllocationHandle>(allocs[0].hAllocation);
          if (h != 0) {
            return h;
          }
        } else if constexpr (!std::is_class_v<Elem> && std::is_convertible_v<Elem, WddmAllocationHandle>) {
          const auto h = static_cast<WddmAllocationHandle>(allocs[0]);
          if (h != 0) {
            return h;
          }
        }
      }
    }
  }

  return 0;
}
#endif

WddmAllocationHandle get_wddm_allocation_from_create_resource(const D3D9DDIARG_CREATERESOURCE* args) {
  if (!args) {
    return 0;
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  return extract_primary_wddm_allocation_handle(*args);
#else
  return static_cast<WddmAllocationHandle>(args->wddm_hAllocation);
#endif
}

} // namespace

// -----------------------------------------------------------------------------
// D3D9 DDI struct member accessors
// -----------------------------------------------------------------------------
// The portable ABI subset in `include/aerogpu_d3d9_umd.h` intentionally models
// only the fields exercised by the current translation layer. When building
// against real WDK headers, the same structs may use different member spellings
// (e.g. `Type` vs `type`, `OffsetToLock` vs `offset_bytes`). Use compile-time
// member detection to keep the driver buildable across header vintages.

template <typename...>
struct aerogpu_d3d9_always_false : std::false_type {};

#define AEROGPU_D3D9_DEFINE_HAS_MEMBER(member)                                                    \
  template <typename T, typename = void>                                                          \
  struct aerogpu_d3d9_has_member_##member : std::false_type {};                                   \
  template <typename T>                                                                           \
  struct aerogpu_d3d9_has_member_##member<T, std::void_t<decltype(std::declval<T&>().member)>>    \
      : std::true_type {}

// Resource description fields.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Type);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(type);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Format);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(format);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Width);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(width);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Height);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(height);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Depth);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(depth);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(MipLevels);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(mip_levels);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Usage);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(usage);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Pool);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pool);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Size);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(size);

// Present parameters fields.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(BackBufferWidth);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(backbuffer_width);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(BackBufferHeight);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(backbuffer_height);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(BackBufferFormat);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(backbuffer_format);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(BackBufferCount);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(backbuffer_count);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(SwapEffect);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(swap_effect);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(PresentationInterval);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(presentation_interval);

// State-block args (CreateStateBlock/ValidateDevice).
AEROGPU_D3D9_DEFINE_HAS_MEMBER(StateBlockType);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hStateBlock);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pNumPasses);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(NumPasses);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Windowed);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(windowed);

// Common flag-style fields (appear in many arg structs).
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Flags);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(flags);
// Some D3D9UMDDI structs (e.g. PresentEx in the repo-local ABI subset) use a more
// explicit name for the same flag field.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(d3d9_present_flags);

// Lock/unlock arg fields.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(OffsetToLock);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(SizeToLock);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(OffsetToUnlock);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(SizeToUnlock);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(offset_bytes);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(size_bytes);

// Present arg fields (member names vary between `hWnd` and `hWindow`, `hSrc` and
// `hSrcResource`, etc).
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hWnd);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hWindow);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(SyncInterval);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(sync_interval);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hSrc);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hSrcResource);
// Query arg fields (member names vary across header sets).
AEROGPU_D3D9_DEFINE_HAS_MEMBER(QueryType);

// Locked box output fields.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pData);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pBits);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(RowPitch);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(rowPitch);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(SlicePitch);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(slicePitch);

// Swap chain / reset arg fields: some header sets embed a present-parameters
// struct, others store a pointer.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pPresentParameters);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pPresentationParameters);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(PresentParameters);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(PresentationParameters);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(present_params);

// Misc arg fields used by the translation layer.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(rect_count);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(RectCount);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(NumRects);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pSrcRects);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pRects);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pRectList);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(resource_count);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(NumResources);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(data_size);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(DataSize);
// Per-allocation private driver data blob fields (shared resources/OpenResource).
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pPrivateDriverData);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pKmdAllocPrivateData);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(PrivateDriverDataSize);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(private_driver_data_size);

// Blt/ColorFill/Update* fields.
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hDst);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hDstResource);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(hDestResource);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(filter);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Filter);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(color_argb);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(Color);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pDstRect);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pDestRect);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pDstPoint);
AEROGPU_D3D9_DEFINE_HAS_MEMBER(pDestPoint);

#undef AEROGPU_D3D9_DEFINE_HAS_MEMBER

template <typename ArgsT>
uint32_t d3d9_resource_type(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Type<ArgsT>::value) {
    return static_cast<uint32_t>(args.Type);
  } else if constexpr (aerogpu_d3d9_has_member_type<ArgsT>::value) {
    return static_cast<uint32_t>(args.type);
  } else {
    static_assert(aerogpu_d3d9_always_false<ArgsT>::value, "D3D9 resource args missing Type/type member");
  }
}

template <typename ArgsT>
uint32_t d3d9_optional_resource_type(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Type<ArgsT>::value) {
    return static_cast<uint32_t>(args.Type);
  } else if constexpr (aerogpu_d3d9_has_member_type<ArgsT>::value) {
    return static_cast<uint32_t>(args.type);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_format(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Format<ArgsT>::value) {
    return static_cast<uint32_t>(args.Format);
  } else if constexpr (aerogpu_d3d9_has_member_format<ArgsT>::value) {
    return static_cast<uint32_t>(args.format);
  } else {
    static_assert(aerogpu_d3d9_always_false<ArgsT>::value, "D3D9 resource args missing Format/format member");
  }
}

template <typename ArgsT>
uint32_t d3d9_optional_resource_format(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Format<ArgsT>::value) {
    return static_cast<uint32_t>(args.Format);
  } else if constexpr (aerogpu_d3d9_has_member_format<ArgsT>::value) {
    return static_cast<uint32_t>(args.format);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_width(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Width<ArgsT>::value) {
    return static_cast<uint32_t>(args.Width);
  } else if constexpr (aerogpu_d3d9_has_member_width<ArgsT>::value) {
    return static_cast<uint32_t>(args.width);
  } else {
    static_assert(aerogpu_d3d9_always_false<ArgsT>::value, "D3D9 resource args missing Width/width member");
  }
}

template <typename ArgsT>
uint32_t d3d9_optional_resource_width(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Width<ArgsT>::value) {
    return static_cast<uint32_t>(args.Width);
  } else if constexpr (aerogpu_d3d9_has_member_width<ArgsT>::value) {
    return static_cast<uint32_t>(args.width);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_height(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Height<ArgsT>::value) {
    return static_cast<uint32_t>(args.Height);
  } else if constexpr (aerogpu_d3d9_has_member_height<ArgsT>::value) {
    return static_cast<uint32_t>(args.height);
  } else {
    static_assert(aerogpu_d3d9_always_false<ArgsT>::value, "D3D9 resource args missing Height/height member");
  }
}

template <typename ArgsT>
uint32_t d3d9_optional_resource_height(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Height<ArgsT>::value) {
    return static_cast<uint32_t>(args.Height);
  } else if constexpr (aerogpu_d3d9_has_member_height<ArgsT>::value) {
    return static_cast<uint32_t>(args.height);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_depth(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Depth<ArgsT>::value) {
    return static_cast<uint32_t>(args.Depth);
  } else if constexpr (aerogpu_d3d9_has_member_depth<ArgsT>::value) {
    return static_cast<uint32_t>(args.depth);
  } else {
    return 1u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_mip_levels(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_MipLevels<ArgsT>::value) {
    return static_cast<uint32_t>(args.MipLevels);
  } else if constexpr (aerogpu_d3d9_has_member_mip_levels<ArgsT>::value) {
    return static_cast<uint32_t>(args.mip_levels);
  } else {
    return 1u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_usage(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Usage<ArgsT>::value) {
    return static_cast<uint32_t>(args.Usage);
  } else if constexpr (aerogpu_d3d9_has_member_usage<ArgsT>::value) {
    return static_cast<uint32_t>(args.usage);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_pool(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Pool<ArgsT>::value) {
    return static_cast<uint32_t>(args.Pool);
  } else if constexpr (aerogpu_d3d9_has_member_pool<ArgsT>::value) {
    return static_cast<uint32_t>(args.pool);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
uint32_t d3d9_resource_size(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_Size<ArgsT>::value) {
    return static_cast<uint32_t>(args.Size);
  } else if constexpr (aerogpu_d3d9_has_member_size<ArgsT>::value) {
    return static_cast<uint32_t>(args.size);
  } else {
    return 0u;
  }
}

template <typename LockT>
uint32_t d3d9_lock_offset(const LockT& lock) {
  if constexpr (aerogpu_d3d9_has_member_OffsetToLock<LockT>::value) {
    return static_cast<uint32_t>(lock.OffsetToLock);
  } else if constexpr (aerogpu_d3d9_has_member_offset_bytes<LockT>::value) {
    return static_cast<uint32_t>(lock.offset_bytes);
  } else {
    return 0u;
  }
}

template <typename LockT>
uint32_t d3d9_lock_size(const LockT& lock) {
  if constexpr (aerogpu_d3d9_has_member_SizeToLock<LockT>::value) {
    return static_cast<uint32_t>(lock.SizeToLock);
  } else if constexpr (aerogpu_d3d9_has_member_size_bytes<LockT>::value) {
    return static_cast<uint32_t>(lock.size_bytes);
  } else {
    return 0u;
  }
}

template <typename LockT>
uint32_t d3d9_lock_flags(const LockT& lock) {
  if constexpr (aerogpu_d3d9_has_member_Flags<LockT>::value) {
    return static_cast<uint32_t>(lock.Flags);
  } else if constexpr (aerogpu_d3d9_has_member_flags<LockT>::value) {
    return static_cast<uint32_t>(lock.flags);
  } else {
    return 0u;
  }
}

template <typename UnlockT>
uint32_t d3d9_unlock_offset(const UnlockT& unlock) {
  if constexpr (aerogpu_d3d9_has_member_OffsetToUnlock<UnlockT>::value) {
    return static_cast<uint32_t>(unlock.OffsetToUnlock);
  } else if constexpr (aerogpu_d3d9_has_member_offset_bytes<UnlockT>::value) {
    return static_cast<uint32_t>(unlock.offset_bytes);
  } else {
    return 0u;
  }
}

template <typename UnlockT>
uint32_t d3d9_unlock_size(const UnlockT& unlock) {
  if constexpr (aerogpu_d3d9_has_member_SizeToUnlock<UnlockT>::value) {
    return static_cast<uint32_t>(unlock.SizeToUnlock);
  } else if constexpr (aerogpu_d3d9_has_member_size_bytes<UnlockT>::value) {
    return static_cast<uint32_t>(unlock.size_bytes);
  } else {
    return 0u;
  }
}

template <typename PresentT>
HWND d3d9_present_hwnd(const PresentT& present) {
  if constexpr (aerogpu_d3d9_has_member_hWnd<PresentT>::value) {
    return present.hWnd;
  } else if constexpr (aerogpu_d3d9_has_member_hWindow<PresentT>::value) {
    return present.hWindow;
  } else {
    return nullptr;
  }
}

template <typename PresentT>
D3DDDI_HRESOURCE d3d9_present_src(const PresentT& present) {
  if constexpr (aerogpu_d3d9_has_member_hSrc<PresentT>::value) {
    return present.hSrc;
  } else if constexpr (aerogpu_d3d9_has_member_hSrcResource<PresentT>::value) {
    return present.hSrcResource;
  } else {
    return {};
  }
}

template <typename PresentT>
uint32_t d3d9_present_sync_interval(const PresentT& present) {
  if constexpr (aerogpu_d3d9_has_member_SyncInterval<PresentT>::value) {
    return static_cast<uint32_t>(present.SyncInterval);
  } else if constexpr (aerogpu_d3d9_has_member_sync_interval<PresentT>::value) {
    return static_cast<uint32_t>(present.sync_interval);
  } else {
    return 0u;
  }
}

template <typename PresentT>
uint32_t d3d9_present_flags(const PresentT& present) {
  if constexpr (aerogpu_d3d9_has_member_Flags<PresentT>::value) {
    return static_cast<uint32_t>(present.Flags);
  } else if constexpr (aerogpu_d3d9_has_member_flags<PresentT>::value) {
    return static_cast<uint32_t>(present.flags);
  } else if constexpr (aerogpu_d3d9_has_member_d3d9_present_flags<PresentT>::value) {
    return static_cast<uint32_t>(present.d3d9_present_flags);
  } else {
    return 0u;
  }
}

template <typename QueryT>
uint32_t d3d9_query_type(const QueryT& query) {
  if constexpr (aerogpu_d3d9_has_member_QueryType<QueryT>::value) {
    return static_cast<uint32_t>(query.QueryType);
  } else if constexpr (aerogpu_d3d9_has_member_Type<QueryT>::value) {
    return static_cast<uint32_t>(query.Type);
  } else if constexpr (aerogpu_d3d9_has_member_type<QueryT>::value) {
    return static_cast<uint32_t>(query.type);
  } else {
    static_assert(aerogpu_d3d9_always_false<QueryT>::value, "D3D9 query args missing QueryType/Type/type member");
  }
}

template <typename LockedBoxT>
void d3d9_locked_box_set_ptr(LockedBoxT* box, void* ptr) {
  if (!box) {
    return;
  }
  if constexpr (aerogpu_d3d9_has_member_pData<LockedBoxT>::value) {
    box->pData = ptr;
  } else if constexpr (aerogpu_d3d9_has_member_pBits<LockedBoxT>::value) {
    box->pBits = ptr;
  } else {
    static_assert(aerogpu_d3d9_always_false<LockedBoxT>::value, "LockedBox missing pData/pBits member");
  }
}

template <typename LockedBoxT>
void d3d9_locked_box_set_row_pitch(LockedBoxT* box, uint32_t pitch) {
  if (!box) {
    return;
  }
  if constexpr (aerogpu_d3d9_has_member_RowPitch<LockedBoxT>::value) {
    box->RowPitch = pitch;
  } else if constexpr (aerogpu_d3d9_has_member_rowPitch<LockedBoxT>::value) {
    box->rowPitch = pitch;
  } else {
    static_assert(aerogpu_d3d9_always_false<LockedBoxT>::value, "LockedBox missing RowPitch/rowPitch member");
  }
}

template <typename LockedBoxT>
void d3d9_locked_box_set_slice_pitch(LockedBoxT* box, uint32_t pitch) {
  if (!box) {
    return;
  }
  if constexpr (aerogpu_d3d9_has_member_SlicePitch<LockedBoxT>::value) {
    box->SlicePitch = pitch;
  } else if constexpr (aerogpu_d3d9_has_member_slicePitch<LockedBoxT>::value) {
    box->slicePitch = pitch;
  } else {
    static_assert(aerogpu_d3d9_always_false<LockedBoxT>::value, "LockedBox missing SlicePitch/slicePitch member");
  }
}

template <typename SwapArgsT>
const D3D9DDI_PRESENT_PARAMETERS* d3d9_get_present_params(const SwapArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_pPresentParameters<SwapArgsT>::value) {
    return args.pPresentParameters;
  } else if constexpr (aerogpu_d3d9_has_member_pPresentationParameters<SwapArgsT>::value) {
    return args.pPresentationParameters;
  } else if constexpr (aerogpu_d3d9_has_member_PresentParameters<SwapArgsT>::value) {
    return &args.PresentParameters;
  } else if constexpr (aerogpu_d3d9_has_member_PresentationParameters<SwapArgsT>::value) {
    return &args.PresentationParameters;
  } else if constexpr (aerogpu_d3d9_has_member_present_params<SwapArgsT>::value) {
    return &args.present_params;
  } else {
    return nullptr;
  }
}

template <typename PpT>
uint32_t d3d9_pp_backbuffer_width(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_BackBufferWidth<PpT>::value) {
    return static_cast<uint32_t>(pp.BackBufferWidth);
  } else if constexpr (aerogpu_d3d9_has_member_backbuffer_width<PpT>::value) {
    return static_cast<uint32_t>(pp.backbuffer_width);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_backbuffer_height(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_BackBufferHeight<PpT>::value) {
    return static_cast<uint32_t>(pp.BackBufferHeight);
  } else if constexpr (aerogpu_d3d9_has_member_backbuffer_height<PpT>::value) {
    return static_cast<uint32_t>(pp.backbuffer_height);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_backbuffer_format(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_BackBufferFormat<PpT>::value) {
    return static_cast<uint32_t>(pp.BackBufferFormat);
  } else if constexpr (aerogpu_d3d9_has_member_backbuffer_format<PpT>::value) {
    return static_cast<uint32_t>(pp.backbuffer_format);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_backbuffer_count(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_BackBufferCount<PpT>::value) {
    return static_cast<uint32_t>(pp.BackBufferCount);
  } else if constexpr (aerogpu_d3d9_has_member_backbuffer_count<PpT>::value) {
    return static_cast<uint32_t>(pp.backbuffer_count);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_swap_effect(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_SwapEffect<PpT>::value) {
    return static_cast<uint32_t>(pp.SwapEffect);
  } else if constexpr (aerogpu_d3d9_has_member_swap_effect<PpT>::value) {
    return static_cast<uint32_t>(pp.swap_effect);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_flags(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_Flags<PpT>::value) {
    return static_cast<uint32_t>(pp.Flags);
  } else if constexpr (aerogpu_d3d9_has_member_flags<PpT>::value) {
    return static_cast<uint32_t>(pp.flags);
  } else {
    return 0u;
  }
}

template <typename PpT>
uint32_t d3d9_pp_presentation_interval(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_PresentationInterval<PpT>::value) {
    return static_cast<uint32_t>(pp.PresentationInterval);
  } else if constexpr (aerogpu_d3d9_has_member_presentation_interval<PpT>::value) {
    return static_cast<uint32_t>(pp.presentation_interval);
  } else {
    return 0u;
  }
}

template <typename PpT>
BOOL d3d9_pp_windowed(const PpT& pp) {
  if constexpr (aerogpu_d3d9_has_member_Windowed<PpT>::value) {
    return pp.Windowed;
  } else if constexpr (aerogpu_d3d9_has_member_windowed<PpT>::value) {
    return pp.windowed;
  } else {
    return TRUE;
  }
}

template <typename CopyRectsT>
uint32_t d3d9_copy_rects_count(const CopyRectsT& args) {
  if constexpr (aerogpu_d3d9_has_member_NumRects<CopyRectsT>::value) {
    return static_cast<uint32_t>(args.NumRects);
  } else if constexpr (aerogpu_d3d9_has_member_RectCount<CopyRectsT>::value) {
    return static_cast<uint32_t>(args.RectCount);
  } else if constexpr (aerogpu_d3d9_has_member_rect_count<CopyRectsT>::value) {
    return static_cast<uint32_t>(args.rect_count);
  } else {
    return 0u;
  }
}

template <typename CopyRectsT>
const RECT* d3d9_copy_rects_rects(const CopyRectsT& args) {
  if constexpr (aerogpu_d3d9_has_member_pRects<CopyRectsT>::value) {
    return args.pRects;
  } else if constexpr (aerogpu_d3d9_has_member_pRectList<CopyRectsT>::value) {
    return args.pRectList;
  } else if constexpr (aerogpu_d3d9_has_member_pSrcRects<CopyRectsT>::value) {
    return args.pSrcRects;
  } else {
    return nullptr;
  }
}

template <typename QueryResT>
uint32_t d3d9_query_resource_residency_count(const QueryResT& args) {
  if constexpr (aerogpu_d3d9_has_member_NumResources<QueryResT>::value) {
    return static_cast<uint32_t>(args.NumResources);
  } else if constexpr (aerogpu_d3d9_has_member_resource_count<QueryResT>::value) {
    return static_cast<uint32_t>(args.resource_count);
  } else {
    return 0u;
  }
}

template <typename QueryDataT>
uint32_t d3d9_query_data_size(const QueryDataT& args) {
  if constexpr (aerogpu_d3d9_has_member_DataSize<QueryDataT>::value) {
    return static_cast<uint32_t>(args.DataSize);
  } else if constexpr (aerogpu_d3d9_has_member_data_size<QueryDataT>::value) {
    return static_cast<uint32_t>(args.data_size);
  } else {
    return 0u;
  }
}

template <typename ArgsT>
const void* d3d9_private_driver_data_ptr(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_pPrivateDriverData<ArgsT>::value) {
    return args.pPrivateDriverData;
  } else if constexpr (aerogpu_d3d9_has_member_pKmdAllocPrivateData<ArgsT>::value) {
    return args.pKmdAllocPrivateData;
  } else {
    return nullptr;
  }
}

template <typename ArgsT>
uint32_t d3d9_private_driver_data_size(const ArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_PrivateDriverDataSize<ArgsT>::value) {
    return static_cast<uint32_t>(args.PrivateDriverDataSize);
  } else if constexpr (aerogpu_d3d9_has_member_private_driver_data_size<ArgsT>::value) {
    return static_cast<uint32_t>(args.private_driver_data_size);
  } else {
    return 0u;
  }
}

template <typename HResArgsT>
D3DDDI_HRESOURCE d3d9_arg_src_resource(const HResArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_hSrc<HResArgsT>::value) {
    return args.hSrc;
  } else if constexpr (aerogpu_d3d9_has_member_hSrcResource<HResArgsT>::value) {
    return args.hSrcResource;
  } else {
    return {};
  }
}

template <typename HResArgsT>
D3DDDI_HRESOURCE d3d9_arg_dst_resource(const HResArgsT& args) {
  if constexpr (aerogpu_d3d9_has_member_hDst<HResArgsT>::value) {
    return args.hDst;
  } else if constexpr (aerogpu_d3d9_has_member_hDstResource<HResArgsT>::value) {
    return args.hDstResource;
  } else if constexpr (aerogpu_d3d9_has_member_hDestResource<HResArgsT>::value) {
    return args.hDestResource;
  } else {
    return {};
  }
}

template <typename BltT>
uint32_t d3d9_blt_filter(const BltT& args) {
  if constexpr (aerogpu_d3d9_has_member_Filter<BltT>::value) {
    return static_cast<uint32_t>(args.Filter);
  } else if constexpr (aerogpu_d3d9_has_member_filter<BltT>::value) {
    return static_cast<uint32_t>(args.filter);
  } else {
    return 0u;
  }
}

template <typename ColorFillT>
uint32_t d3d9_color_fill_color(const ColorFillT& args) {
  if constexpr (aerogpu_d3d9_has_member_Color<ColorFillT>::value) {
    return static_cast<uint32_t>(args.Color);
  } else if constexpr (aerogpu_d3d9_has_member_color_argb<ColorFillT>::value) {
    return static_cast<uint32_t>(args.color_argb);
  } else {
    return 0u;
  }
}

template <typename UpdateSurfT>
const POINT* d3d9_update_surface_dst_point(const UpdateSurfT& args) {
  if constexpr (aerogpu_d3d9_has_member_pDstPoint<UpdateSurfT>::value) {
    return args.pDstPoint;
  } else if constexpr (aerogpu_d3d9_has_member_pDestPoint<UpdateSurfT>::value) {
    return args.pDestPoint;
  } else {
    return nullptr;
  }
}

template <typename UpdateSurfT>
const RECT* d3d9_update_surface_dst_rect(const UpdateSurfT& args) {
  if constexpr (aerogpu_d3d9_has_member_pDstRect<UpdateSurfT>::value) {
    return args.pDstRect;
  } else if constexpr (aerogpu_d3d9_has_member_pDestRect<UpdateSurfT>::value) {
    return args.pDestRect;
  } else {
    return nullptr;
  }
}

uint64_t qpc_now() {
#if defined(_WIN32)
  LARGE_INTEGER li;
  QueryPerformanceCounter(&li);
  return static_cast<uint64_t>(li.QuadPart);
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

void sleep_ms(uint32_t ms) {
#if defined(_WIN32)
  Sleep(ms);
#else
  std::this_thread::sleep_for(std::chrono::milliseconds(ms));
#endif
}

struct FenceSnapshot {
  uint64_t last_submitted = 0;
  uint64_t last_completed = 0;
};

#if defined(_WIN32)

// Best-effort HDC -> adapter LUID translation.
//
// Win7's D3D9 runtime and DWM may open the same adapter using both the HDC and
// LUID paths. Returning a stable LUID from OpenAdapterFromHdc is critical so our
// adapter cache (keyed by LUID) maps both opens to the same Adapter instance.
using NTSTATUS = LONG;

constexpr bool nt_success(NTSTATUS st) {
  return st >= 0;
}

struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  UINT hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
};

struct D3DKMT_CLOSEADAPTER {
  UINT hAdapter;
};

using PFND3DKMTOpenAdapterFromHdc = NTSTATUS(__stdcall*)(D3DKMT_OPENADAPTERFROMHDC* pData);
using PFND3DKMTCloseAdapter = NTSTATUS(__stdcall*)(D3DKMT_CLOSEADAPTER* pData);

bool get_luid_from_hdc(HDC hdc, LUID* luid_out) {
  if (!hdc || !luid_out) {
    return false;
  }

  HMODULE gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!gdi32) {
    return false;
  }

  auto* open_adapter_from_hdc =
      reinterpret_cast<PFND3DKMTOpenAdapterFromHdc>(GetProcAddress(gdi32, "D3DKMTOpenAdapterFromHdc"));
  auto* close_adapter =
      reinterpret_cast<PFND3DKMTCloseAdapter>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
  if (!open_adapter_from_hdc || !close_adapter) {
    FreeLibrary(gdi32);
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC open{};
  open.hDc = hdc;
  open.hAdapter = 0;
  std::memset(&open.AdapterLuid, 0, sizeof(open.AdapterLuid));
  open.VidPnSourceId = 0;

  const NTSTATUS st = open_adapter_from_hdc(&open);
  if (!nt_success(st) || open.hAdapter == 0) {
    FreeLibrary(gdi32);
    return false;
  }

  *luid_out = open.AdapterLuid;

  D3DKMT_CLOSEADAPTER close{};
  close.hAdapter = open.hAdapter;
  close_adapter(&close);

  FreeLibrary(gdi32);
  return true;
}

#endif

FenceSnapshot refresh_fence_snapshot(Adapter* adapter) {
  FenceSnapshot snap{};
  if (!adapter) {
    return snap;
  }

#if defined(_WIN32)
  // DWM and many D3D9Ex clients poll EVENT queries in tight loops. Querying the
  // KMD fence counter (last completed) requires a D3DKMTEscape call, so throttle
  // it to a small interval to avoid burning CPU in the kernel.
  //
  // Note: we intentionally do *not* use the escape's \"last submitted\" fence as
  // a per-submission fence ID when polling. Under multi-process workloads (DWM +
  // apps) it is global and can be dominated by another process's submissions.
  // Per-submission fence IDs must come from the runtime callbacks (e.g.
  // SubmissionFenceId / NewFenceValue).
  constexpr uint64_t kMinFenceQueryIntervalMs = 4;
  const uint64_t now_ms = monotonic_ms();
  bool should_query_kmd = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (now_ms >= adapter->last_kmd_fence_query_ms &&
        (now_ms - adapter->last_kmd_fence_query_ms) >= kMinFenceQueryIntervalMs) {
      adapter->last_kmd_fence_query_ms = now_ms;
      should_query_kmd = true;
    }
  }

  if (should_query_kmd && adapter->kmd_query_available.load(std::memory_order_acquire)) {
    uint64_t completed = 0;
    if (adapter->kmd_query.QueryFence(/*last_submitted=*/nullptr, &completed)) {
      bool updated = false;
      {
        std::lock_guard<std::mutex> lock(adapter->fence_mutex);
        const uint64_t prev_completed = adapter->completed_fence;
        adapter->completed_fence = std::max<uint64_t>(adapter->completed_fence, completed);
        updated = (adapter->completed_fence != prev_completed);
      }
      if (updated) {
        adapter->fence_cv.notify_all();
      }
    } else {
      adapter->kmd_query_available.store(false, std::memory_order_release);
    }
  }
#endif

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    snap.last_submitted = adapter->last_submitted_fence;
    snap.last_completed = adapter->completed_fence;
  }
  return snap;
}

void retire_completed_presents_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return;
  }

  const uint64_t completed = refresh_fence_snapshot(dev->adapter).last_completed;
  while (!dev->inflight_present_fences.empty() && dev->inflight_present_fences.front() <= completed) {
    dev->inflight_present_fences.pop_front();
  }
}

enum class FenceWaitResult {
  Complete,
  NotReady,
  Failed,
};

#if defined(_WIN32)
using AerogpuNtStatus = LONG;

constexpr AerogpuNtStatus kStatusSuccess = 0x00000000L;
constexpr AerogpuNtStatus kStatusTimeout = 0x00000102L;
constexpr AerogpuNtStatus kStatusNotSupported = static_cast<AerogpuNtStatus>(0xC00000BBL);
#endif

FenceWaitResult wait_for_fence(Device* dev, uint64_t fence_value, uint32_t timeout_ms) {
  if (!dev || !dev->adapter) {
    return FenceWaitResult::Failed;
  }
  if (fence_value == 0) {
    return FenceWaitResult::Complete;
  }

  Adapter* adapter = dev->adapter;

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->completed_fence >= fence_value) {
      return FenceWaitResult::Complete;
    }
  }

#if defined(_WIN32)
  // For bounded waits, prefer letting the kernel wait on the WDDM sync object.
  // This avoids user-mode polling loops (Sleep(1) + repeated fence queries).
  if (timeout_ms != 0) {
    const WddmHandle sync_object = dev->wddm_context.hSyncObject;
    if (sync_object != 0) {
      const AerogpuNtStatus st = static_cast<AerogpuNtStatus>(
          adapter->kmd_query.WaitForSyncObject(static_cast<uint32_t>(sync_object), fence_value, timeout_ms));
      {
        static std::once_flag once;
        std::call_once(once, [st, timeout_ms] {
          aerogpu::logf("aerogpu-d3d9: wait_for_fence using syncobj wait (timeout_ms=%u) NTSTATUS=0x%08lx\n",
                        static_cast<unsigned>(timeout_ms),
                        static_cast<unsigned long>(st));
        });
      }
      if (st == kStatusSuccess) {
        {
          std::lock_guard<std::mutex> lock(adapter->fence_mutex);
          adapter->completed_fence = std::max(adapter->completed_fence, fence_value);
        }
        adapter->fence_cv.notify_all();
        return FenceWaitResult::Complete;
      }
      if (st == kStatusTimeout) {
        return FenceWaitResult::NotReady;
      }
    }
  }
#endif

  // Fast path: for polling callers (GetData), avoid per-call kernel waits. We
  // prefer querying the KMD fence counters (throttled inside
  // refresh_fence_snapshot) so tight polling loops don't spam syscalls.
  if (timeout_ms == 0) {
    if (refresh_fence_snapshot(adapter).last_completed >= fence_value) {
      return FenceWaitResult::Complete;
    }

#if defined(_WIN32)
    // If the KMD fence query path is unavailable, fall back to polling the WDDM
    // sync object once. This keeps EVENT queries functional even if the escape
    // path is missing.
    if (!adapter->kmd_query_available.load(std::memory_order_acquire)) {
      const WddmHandle sync_object = dev->wddm_context.hSyncObject;
      if (sync_object != 0) {
        const AerogpuNtStatus st = static_cast<AerogpuNtStatus>(
            adapter->kmd_query.WaitForSyncObject(static_cast<uint32_t>(sync_object), fence_value, /*timeout_ms=*/0));
        {
          static std::once_flag once;
          std::call_once(once, [st] {
            aerogpu::logf("aerogpu-d3d9: wait_for_fence using syncobj poll NTSTATUS=0x%08lx\n",
                          static_cast<unsigned long>(st));
          });
        }
        if (st == kStatusSuccess) {
          {
            std::lock_guard<std::mutex> lock(adapter->fence_mutex);
            adapter->completed_fence = std::max(adapter->completed_fence, fence_value);
          }
          adapter->fence_cv.notify_all();
          return FenceWaitResult::Complete;
        }
      }
    }
#endif

    return FenceWaitResult::NotReady;
  }

  const uint64_t deadline = monotonic_ms() + timeout_ms;
#if defined(_WIN32)
  {
    static std::once_flag once;
    std::call_once(once, [timeout_ms] {
      aerogpu::logf("aerogpu-d3d9: wait_for_fence falling back to polling (timeout_ms=%u)\n",
                    static_cast<unsigned>(timeout_ms));
    });
  }
#endif
  while (monotonic_ms() < deadline) {
    if (refresh_fence_snapshot(adapter).last_completed >= fence_value) {
      return FenceWaitResult::Complete;
    }

    sleep_ms(1);
  }

  return (refresh_fence_snapshot(adapter).last_completed >= fence_value) ? FenceWaitResult::Complete
                                                                        : FenceWaitResult::NotReady;
}

HRESULT throttle_presents_locked(Device* dev, uint32_t d3d9_present_flags) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (!dev->adapter) {
    return E_FAIL;
  }

  // Clamp in case callers pass unexpected values.
  if (dev->max_frame_latency < kMaxFrameLatencyMin) {
    dev->max_frame_latency = kMaxFrameLatencyMin;
  }
  if (dev->max_frame_latency > kMaxFrameLatencyMax) {
    dev->max_frame_latency = kMaxFrameLatencyMax;
  }

  retire_completed_presents_locked(dev);

  if (dev->inflight_present_fences.size() < dev->max_frame_latency) {
    return S_OK;
  }

  const bool dont_wait = (d3d9_present_flags & kD3dPresentDoNotWait) != 0;
  if (dont_wait) {
    return kD3dErrWasStillDrawing;
  }

  // Wait for at least one present fence to retire, but never indefinitely.
  const uint64_t deadline = monotonic_ms() + kPresentThrottleMaxWaitMs;
  while (dev->inflight_present_fences.size() >= dev->max_frame_latency) {
    const uint64_t now = monotonic_ms();
    if (now >= deadline) {
      // Forward progress failed; drop the oldest fence to ensure PresentEx
      // returns quickly. This preserves overall system responsiveness at the
      // expense of perfect throttling accuracy under GPU hangs.
      dev->inflight_present_fences.pop_front();
      break;
    }

    const uint64_t oldest = dev->inflight_present_fences.front();
    const uint32_t time_left = static_cast<uint32_t>(std::min<uint64_t>(deadline - now, kPresentThrottleMaxWaitMs));
    (void)wait_for_fence(dev, oldest, time_left);
    retire_completed_presents_locked(dev);
  }

  return S_OK;
}

uint32_t d3d9_format_to_aerogpu(uint32_t d3d9_format) {
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8
    case 21u:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case 22u:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    // D3DFMT_A8B8G8R8
    case 32u:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    // D3DFMT_D24S8
    case 75u:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    // D3DFMT_DXT1/DXT2/DXT3/DXT4/DXT5 (FOURCC codes; see d3d9_make_fourcc in aerogpu_d3d9_objects.h)
    case static_cast<uint32_t>(kD3dFmtDxt1):
      return AEROGPU_FORMAT_BC1_RGBA_UNORM;
    // DXT2 is the premultiplied-alpha variant of DXT3. AeroGPU does not encode
    // alpha-premultiplication at the format level, so treat it as BC2.
    case static_cast<uint32_t>(kD3dFmtDxt2):
    case static_cast<uint32_t>(kD3dFmtDxt3):
      return AEROGPU_FORMAT_BC2_RGBA_UNORM;
    // DXT4 is the premultiplied-alpha variant of DXT5. AeroGPU does not encode
    // alpha-premultiplication at the format level, so treat it as BC3.
    case static_cast<uint32_t>(kD3dFmtDxt4):
    case static_cast<uint32_t>(kD3dFmtDxt5):
      return AEROGPU_FORMAT_BC3_RGBA_UNORM;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

static bool SupportsBcFormats(const Device* dev) {
  if (!dev || !dev->adapter) {
    return false;
  }

#if defined(_WIN32)
  // On Windows we can usually query the active device ABI version via the
  // UMDRIVERPRIVATE blob. Be conservative: if we cannot query it, assume BC
  // formats are unsupported so we don't emit commands the host cannot parse.
  if (!dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2u);
#else
  // Portable builds don't have a real device to query; assume the matching host
  // supports the formats compiled into the protocol headers.
  (void)dev;
  return true;
#endif
}

// D3DLOCK_* flags (numeric values from d3d9.h). Only the bits we care about are
// defined here to keep the UMD self-contained.
constexpr uint32_t kD3DLOCK_READONLY = 0x00000010u;
constexpr uint32_t kD3DLOCK_DISCARD = 0x00002000u;
constexpr uint32_t kD3DLOCK_NOOVERWRITE = 0x00001000u;

// D3DPOOL_* (numeric values from d3d9.h).
constexpr uint32_t kD3DPOOL_DEFAULT = 0u;
constexpr uint32_t kD3DPOOL_SYSTEMMEM = 2u;

constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

constexpr D3DDDIFORMAT kD3dFmtIndex16 = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
constexpr D3DDDIFORMAT kD3dFmtIndex32 = static_cast<D3DDDIFORMAT>(102); // D3DFMT_INDEX32

uint32_t d3d9_stage_to_aerogpu_stage(uint32_t stage) {
  return (stage == kD3d9ShaderStageVs) ? AEROGPU_SHADER_STAGE_VERTEX : AEROGPU_SHADER_STAGE_PIXEL;
}

uint32_t d3d9_index_format_to_aerogpu(D3DDDIFORMAT fmt) {
  return (fmt == kD3dFmtIndex32) ? AEROGPU_INDEX_FORMAT_UINT32 : AEROGPU_INDEX_FORMAT_UINT16;
}

// D3DUSAGE_* subset (numeric values from d3d9types.h).
constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;

uint32_t d3d9_usage_to_aerogpu_usage_flags(uint32_t usage) {
  uint32_t flags = AEROGPU_RESOURCE_USAGE_TEXTURE;
  if (usage & kD3DUsageRenderTarget) {
    flags |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (usage & kD3DUsageDepthStencil) {
    flags |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return flags;
}

uint32_t d3d9_prim_to_topology(D3DDDIPRIMITIVETYPE prim) {
  switch (prim) {
    case D3DDDIPT_POINTLIST:
      return AEROGPU_TOPOLOGY_POINTLIST;
    case D3DDDIPT_LINELIST:
      return AEROGPU_TOPOLOGY_LINELIST;
    case D3DDDIPT_LINESTRIP:
      return AEROGPU_TOPOLOGY_LINESTRIP;
    case D3DDDIPT_TRIANGLESTRIP:
      return AEROGPU_TOPOLOGY_TRIANGLESTRIP;
    case D3DDDIPT_TRIANGLEFAN:
      return AEROGPU_TOPOLOGY_TRIANGLEFAN;
    case D3DDDIPT_TRIANGLELIST:
    default:
      return AEROGPU_TOPOLOGY_TRIANGLELIST;
  }
}

uint32_t vertex_count_from_primitive(D3DDDIPRIMITIVETYPE prim, uint32_t primitive_count) {
  switch (prim) {
    case D3DDDIPT_POINTLIST:
      return primitive_count;
    case D3DDDIPT_LINELIST:
      return primitive_count * 2;
    case D3DDDIPT_LINESTRIP:
      return primitive_count + 1;
    case D3DDDIPT_TRIANGLELIST:
      return primitive_count * 3;
    case D3DDDIPT_TRIANGLESTRIP:
    case D3DDDIPT_TRIANGLEFAN:
      return primitive_count + 2;
    default:
      return primitive_count * 3;
  }
}

uint32_t index_count_from_primitive(D3DDDIPRIMITIVETYPE prim, uint32_t primitive_count) {
  // Indexed draws follow the same primitive->index expansion rules.
  return vertex_count_from_primitive(prim, primitive_count);
}

bool clamp_rect(const RECT* in, uint32_t width, uint32_t height, RECT* out) {
  if (!out || width == 0 || height == 0) {
    return false;
  }

  RECT r{};
  if (in) {
    r = *in;
  } else {
    r.left = 0;
    r.top = 0;
    r.right = static_cast<long>(width);
    r.bottom = static_cast<long>(height);
  }

  const long max_x = static_cast<long>(width);
  const long max_y = static_cast<long>(height);

  r.left = std::clamp(r.left, 0l, max_x);
  r.right = std::clamp(r.right, 0l, max_x);
  r.top = std::clamp(r.top, 0l, max_y);
  r.bottom = std::clamp(r.bottom, 0l, max_y);

  if (r.right <= r.left || r.bottom <= r.top) {
    return false;
  }

  *out = r;
  return true;
}

// -----------------------------------------------------------------------------
// Minimal fixed-function (FVF) support (bring-up)
// -----------------------------------------------------------------------------

constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;

constexpr uint32_t kSupportedFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;

#pragma pack(push, 1)
struct D3DVERTEXELEMENT9_COMPAT {
  uint16_t Stream;
  uint16_t Offset;
  uint8_t Type;
  uint8_t Method;
  uint8_t Usage;
  uint8_t UsageIndex;
};
#pragma pack(pop)

static_assert(sizeof(D3DVERTEXELEMENT9_COMPAT) == 8, "D3DVERTEXELEMENT9 must be 8 bytes");

constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

// -----------------------------------------------------------------------------
// Handle helpers
// -----------------------------------------------------------------------------

Adapter* as_adapter(D3DDDI_HADAPTER hAdapter) {
  return reinterpret_cast<Adapter*>(hAdapter.pDrvPrivate);
}

Device* as_device(D3DDDI_HDEVICE hDevice) {
  return reinterpret_cast<Device*>(hDevice.pDrvPrivate);
}

Resource* as_resource(D3DDDI_HRESOURCE hRes) {
  return reinterpret_cast<Resource*>(hRes.pDrvPrivate);
}

SwapChain* as_swapchain(D3D9DDI_HSWAPCHAIN hSwapChain) {
  return reinterpret_cast<SwapChain*>(hSwapChain.pDrvPrivate);
}

Shader* as_shader(D3D9DDI_HSHADER hShader) {
  return reinterpret_cast<Shader*>(hShader.pDrvPrivate);
}

VertexDecl* as_vertex_decl(D3D9DDI_HVERTEXDECL hDecl) {
  return reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
}

Query* as_query(D3D9DDI_HQUERY hQuery) {
  return reinterpret_cast<Query*>(hQuery.pDrvPrivate);
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
StateBlock* as_state_block(D3D9DDI_HSTATEBLOCK hStateBlock) {
  return reinterpret_cast<StateBlock*>(hStateBlock.pDrvPrivate);
}
#endif

// -----------------------------------------------------------------------------
// State-block recording helpers
// -----------------------------------------------------------------------------
// Callers must hold `Device::mutex`.
inline void stateblock_record_render_state_locked(Device* dev, uint32_t state, uint32_t value) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  if (state >= 256) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->render_state_mask.set(state);
  sb->render_state_values[state] = value;
}

inline void stateblock_record_sampler_state_locked(Device* dev, uint32_t stage, uint32_t state, uint32_t value) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  if (stage >= 16 || state >= 16) {
    return;
  }
  const uint32_t idx = stage * 16u + state;
  StateBlock* sb = dev->recording_state_block;
  sb->sampler_state_mask.set(idx);
  sb->sampler_state_values[idx] = value;
}

inline void stateblock_record_texture_locked(Device* dev, uint32_t stage, Resource* tex) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  if (stage >= 16) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->texture_mask.set(stage);
  sb->textures[stage] = tex;
}

inline void stateblock_record_render_target_locked(Device* dev, uint32_t slot, Resource* rt) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  if (slot >= 4) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->render_target_mask.set(slot);
  sb->render_targets[slot] = rt;
}

inline void stateblock_record_depth_stencil_locked(Device* dev, Resource* ds) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->depth_stencil_set = true;
  sb->depth_stencil = ds;
}

inline void stateblock_record_viewport_locked(Device* dev, const D3DDDIVIEWPORTINFO& vp) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->viewport_set = true;
  sb->viewport = vp;
}

inline void stateblock_record_scissor_locked(Device* dev, const RECT& rect, BOOL enabled) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->scissor_set = true;
  sb->scissor_rect = rect;
  sb->scissor_enabled = enabled;
}

inline void stateblock_record_stream_source_locked(Device* dev, uint32_t stream, const DeviceStateStream& ss) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  if (stream >= 16) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->stream_mask.set(stream);
  sb->streams[stream] = ss;
}

inline void stateblock_record_index_buffer_locked(Device* dev, Resource* ib, D3DDDIFORMAT fmt, uint32_t offset_bytes) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->index_buffer_set = true;
  sb->index_buffer = ib;
  sb->index_format = fmt;
  sb->index_offset_bytes = offset_bytes;
}

inline void stateblock_record_vertex_decl_locked(Device* dev, VertexDecl* decl, uint32_t fvf) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  sb->vertex_decl_set = true;
  sb->vertex_decl = decl;
  sb->fvf_set = true;
  sb->fvf = fvf;
}

inline void stateblock_record_shader_locked(Device* dev, uint32_t stage, Shader* sh) {
  if (!dev || !dev->recording_state_block) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  // Be permissive: some D3D9 header/runtime combinations may not use the exact
  // {0,1} encoding at the DDI boundary. Match the main shader binding path
  // (`device_set_shader`), which treats any non-VS stage as PS.
  if (stage == kD3d9ShaderStageVs) {
    sb->user_vs_set = true;
    sb->user_vs = sh;
  } else {
    sb->user_ps_set = true;
    sb->user_ps = sh;
  }
}

inline void stateblock_record_shader_const_f_locked(
    Device* dev,
    uint32_t stage,
    uint32_t start_reg,
    const float* pData,
    uint32_t vec4_count) {
  if (!dev || !dev->recording_state_block || !pData || vec4_count == 0) {
    return;
  }
  StateBlock* sb = dev->recording_state_block;
  std::bitset<256>* mask = nullptr;
  float* dst = nullptr;
  if (stage == kD3d9ShaderStageVs) {
    mask = &sb->vs_const_mask;
    dst = sb->vs_consts.data();
  } else {
    mask = &sb->ps_const_mask;
    dst = sb->ps_consts.data();
  }

  if (start_reg >= 256) {
    return;
  }
  const uint32_t write_regs = std::min(vec4_count, 256u - start_reg);
  for (uint32_t i = 0; i < write_regs; ++i) {
    mask->set(start_reg + i);
    std::memcpy(dst + static_cast<size_t>(start_reg + i) * 4,
                pData + static_cast<size_t>(i) * 4,
                4 * sizeof(float));
  }
}

// Forward-declared so helpers can opportunistically split submissions when the
// runtime-provided DMA buffer / allocation list is full.
uint64_t submit(Device* dev, bool is_present = false);
#if defined(_WIN32)
// Ensures the device has a valid runtime-provided command buffer + allocation
// list bound for recording (CreateContext persistent buffers, or Allocate/
// GetCommandBuffer fallback).
// Callers must hold `Device::mutex`.
bool wddm_ensure_recording_buffers(Device* dev, size_t bytes_needed);
#endif

// -----------------------------------------------------------------------------
// WDDM allocation-list tracking helpers (Win7 / WDDM 1.1)
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Command emission helpers (protocol: drivers/aerogpu/protocol/aerogpu_cmd.h)
// -----------------------------------------------------------------------------
bool ensure_cmd_space(Device* dev, size_t bytes_needed) {
  if (!dev) {
    return false;
  }
  if (!dev->adapter) {
    return false;
  }

#if defined(_WIN32)
  if (dev->wddm_context.hContext != 0) {
    // In WDDM builds, never allow command emission to fall back to the
    // vector-backed writer: submissions must be built in runtime-provided DMA
    // buffers so allocation-list tracking and DMA-private-data handoff to the
    // KMD are correct.
    if (!wddm_ensure_recording_buffers(dev, bytes_needed)) {
      return false;
    }
  }
#endif

  if (dev->cmd.bytes_remaining() >= bytes_needed) {
    return true;
  }

  // If the current submission is non-empty, flush it and retry.
  if (!dev->cmd.empty()) {
    (void)submit(dev);
  }

#if defined(_WIN32)
  if (dev->wddm_context.hContext != 0) {
    if (!wddm_ensure_recording_buffers(dev, bytes_needed)) {
      return false;
    }
  }
#endif

  return dev->cmd.bytes_remaining() >= bytes_needed;
}

template <typename T>
T* append_fixed_locked(Device* dev, uint32_t opcode) {
  const size_t needed = align_up(sizeof(T), 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendFixed<T>(opcode);
}

template <typename HeaderT>
HeaderT* append_with_payload_locked(Device* dev, uint32_t opcode, const void* payload, size_t payload_size) {
  const size_t needed = align_up(sizeof(HeaderT) + payload_size, 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendWithPayload<HeaderT>(opcode, payload, payload_size);
}

HRESULT track_resource_allocation_locked(Device* dev, Resource* res, bool write) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  // Only track allocations when running on the WDDM path. Repo/compat builds
  // don't have WDDM allocation handles or runtime-provided allocation lists.
  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

#if defined(_WIN32)
  // Ensure the allocation list backing store is available before we attempt to
  // write D3DDDI_ALLOCATIONLIST entries.
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  if (!wddm_ensure_recording_buffers(dev, min_packet)) {
    return E_FAIL;
  }
#endif

  // Allocation tracking requires a bound allocation-list buffer. In portable
  // builds/tests we may toggle `hContext` without wiring a list; treat that as
  // "tracking disabled" so unit tests focused on other behavior keep working.
  if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
    return E_FAIL;
#else
    return S_OK;
#endif
  }

  if (res->backing_alloc_id == 0) {
    // backing_alloc_id==0 denotes a host-allocated resource (no guest allocation
    // table entry required).
    return S_OK;
  }

  if (res->wddm_hAllocation == 0) {
    logf("aerogpu-d3d9: missing WDDM hAllocation for resource handle=%u alloc_id=%u\n",
         res->handle,
         res->backing_alloc_id);
    return E_FAIL;
  }

  AllocRef ref{};
  if (write) {
    ref = dev->alloc_list_tracker.track_render_target_write(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  } else if (res->kind == ResourceKind::Buffer) {
    ref = dev->alloc_list_tracker.track_buffer_read(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  } else {
    ref = dev->alloc_list_tracker.track_texture_read(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  }

  if (ref.status == AllocRefStatus::kNeedFlush) {
    // Split the submission and retry.
    (void)submit(dev);

    if (write) {
      ref = dev->alloc_list_tracker.track_render_target_write(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    } else if (res->kind == ResourceKind::Buffer) {
      ref = dev->alloc_list_tracker.track_buffer_read(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    } else {
      ref = dev->alloc_list_tracker.track_texture_read(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    }
  }

  if (ref.status != AllocRefStatus::kOk) {
    logf("aerogpu-d3d9: failed to track allocation (handle=%u alloc_id=%u status=%u)\n",
         res->handle,
         res->backing_alloc_id,
         static_cast<uint32_t>(ref.status));
    return E_FAIL;
  }

  return S_OK;
}

HRESULT track_draw_state_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

#if defined(_WIN32)
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  if (!wddm_ensure_recording_buffers(dev, min_packet)) {
    return E_FAIL;
  }
#endif

  if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
    return E_FAIL;
#else
    return S_OK;
#endif
  }

  // The allocation list is keyed by the stable `alloc_id` (backing_alloc_id) and
  // can legally alias multiple per-process WDDM allocation handles to the same
  // alloc_id for shared resources. Count unique alloc_ids rather than WDDM
  // handles so we don't incorrectly reject valid draws on small allocation lists
  // (e.g. shared resources opened multiple times).
  std::array<UINT, 4 + 1 + 16 + 16 + 1> unique_allocs{};
  size_t unique_alloc_len = 0;
  auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
    if (!res) {
      return;
    }
    if (res->backing_alloc_id == 0) {
      return;
    }
    if (res->wddm_hAllocation == 0) {
      return;
    }
    const UINT alloc_id = res->backing_alloc_id;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (unique_allocs[i] == alloc_id) {
        return;
      }
    }
    unique_allocs[unique_alloc_len++] = alloc_id;
  };

  for (uint32_t i = 0; i < 4; i++) {
    add_alloc(dev->render_targets[i]);
  }
  add_alloc(dev->depth_stencil);
  for (uint32_t i = 0; i < 16; i++) {
    add_alloc(dev->textures[i]);
  }
  for (uint32_t i = 0; i < 16; i++) {
    add_alloc(dev->streams[i].vb);
  }
  add_alloc(dev->index_buffer);

  const UINT needed_total = static_cast<UINT>(unique_alloc_len);
  if (needed_total != 0) {
    const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
    if (needed_total > cap) {
      logf("aerogpu-d3d9: draw requires %u allocations but allocation list capacity is %u\n",
           static_cast<unsigned>(needed_total),
           static_cast<unsigned>(cap));
      return E_FAIL;
    }

    UINT needed_new = 0;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
        needed_new++;
      }
    }
    const UINT existing = dev->alloc_list_tracker.list_len();
    if (existing > cap || needed_new > cap - existing) {
      (void)submit(dev);
    }
  }

  for (uint32_t i = 0; i < 4; i++) {
    if (dev->render_targets[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (hr < 0) {
      return hr;
    }
  }

  for (uint32_t i = 0; i < 16; i++) {
    if (dev->textures[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->textures[i], /*write=*/false);
      if (hr < 0) {
        return hr;
      }
    }
  }

  for (uint32_t i = 0; i < 16; i++) {
    if (dev->streams[i].vb) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->streams[i].vb, /*write=*/false);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->index_buffer) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->index_buffer, /*write=*/false);
    if (hr < 0) {
      return hr;
    }
  }

  return S_OK;
}

HRESULT track_render_targets_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

#if defined(_WIN32)
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  if (!wddm_ensure_recording_buffers(dev, min_packet)) {
    return E_FAIL;
  }
#endif

  if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
    return E_FAIL;
#else
    return S_OK;
#endif
  }

  std::array<UINT, 4 + 1> unique_allocs{};
  size_t unique_alloc_len = 0;
  auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
    if (!res) {
      return;
    }
    if (res->backing_alloc_id == 0) {
      return;
    }
    if (res->wddm_hAllocation == 0) {
      return;
    }
    const UINT alloc_id = res->backing_alloc_id;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (unique_allocs[i] == alloc_id) {
        return;
      }
    }
    unique_allocs[unique_alloc_len++] = alloc_id;
  };

  for (uint32_t i = 0; i < 4; ++i) {
    add_alloc(dev->render_targets[i]);
  }
  add_alloc(dev->depth_stencil);

  const UINT needed_total = static_cast<UINT>(unique_alloc_len);
  if (needed_total != 0) {
    const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
    if (needed_total > cap) {
      logf("aerogpu-d3d9: render target bindings require %u allocations but allocation list capacity is %u\n",
           static_cast<unsigned>(needed_total),
           static_cast<unsigned>(cap));
      return E_FAIL;
    }

    UINT needed_new = 0;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
        needed_new++;
      }
    }
    const UINT existing = dev->alloc_list_tracker.list_len();
    if (existing > cap || needed_new > cap - existing) {
      (void)submit(dev);
    }
  }

  for (uint32_t i = 0; i < 4; i++) {
    if (dev->render_targets[i]) {
      HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
      if (hr < 0) {
        return hr;
      }
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (hr < 0) {
      return hr;
    }
  }

  return S_OK;
}

bool emit_set_render_targets_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_targets>(dev, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }

  // The host executor rejects gapped render-target bindings (a null RT followed
  // by a non-null RT). Clamp to the contiguous prefix to avoid emitting a packet
  // that would abort command-stream execution.
  uint32_t color_count = 0;
  while (color_count < 4 && dev->render_targets[color_count]) {
    color_count++;
  }
  for (uint32_t i = color_count; i < 4; ++i) {
    dev->render_targets[i] = nullptr;
  }

  cmd->color_count = color_count;
  cmd->depth_stencil = dev->depth_stencil ? dev->depth_stencil->handle : 0;

  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  for (uint32_t i = 0; i < color_count; i++) {
    cmd->colors[i] = dev->render_targets[i] ? dev->render_targets[i]->handle : 0;
  }
  return true;
}

bool emit_bind_shaders_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_bind_shaders>(dev, AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    return false;
  }
  cmd->vs = dev->vs ? dev->vs->handle : 0;
  cmd->ps = dev->ps ? dev->ps->handle : 0;
  cmd->cs = 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_topology_locked(Device* dev, uint32_t topology) {
  if (dev->topology == topology) {
    return true;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_primitive_topology>(dev, AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    return false;
  }
  dev->topology = topology;
  cmd->topology = topology;
  cmd->reserved0 = 0;
  return true;
}

bool emit_create_resource_locked(Device* dev, Resource* res) {
  if (!dev || !res) {
    return false;
  }

  if (res->kind == ResourceKind::Buffer) {
    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_create_buffer), 4))) {
      return false;
    }
    if (track_resource_allocation_locked(dev, res, /*write=*/false) < 0) {
      return false;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_create_buffer>(dev, AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      return false;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER | AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;
    return true;
  }

  if (res->kind == ResourceKind::Surface || res->kind == ResourceKind::Texture2D) {
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_create_texture2d), 4))) {
      return false;
    }
    if (track_resource_allocation_locked(dev, res, /*write=*/false) < 0) {
      return false;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_create_texture2d>(dev, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      return false;
    }
    cmd->texture_handle = res->handle;
    cmd->usage_flags = d3d9_usage_to_aerogpu_usage_flags(res->usage);
    cmd->format = d3d9_format_to_aerogpu(res->format);
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;
    return true;
  }
  return false;
}

bool emit_destroy_resource_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_resource>(dev, AEROGPU_CMD_DESTROY_RESOURCE);
  if (!cmd) {
    return false;
  }
  cmd->resource_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_export_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_export_shared_surface>(dev, AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  if (!cmd) {
    return false;
  }
  logf("aerogpu-d3d9: export shared surface handle=%u share_token=0x%llx\n",
       static_cast<unsigned>(res->handle),
       static_cast<unsigned long long>(res->share_token));
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
  return true;
}

bool emit_import_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_import_shared_surface>(dev, AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!cmd) {
    return false;
  }
  logf("aerogpu-d3d9: import shared surface out_handle=%u share_token=0x%llx\n",
       static_cast<unsigned>(res->handle),
       static_cast<unsigned long long>(res->share_token));
  cmd->out_resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
  return true;
}

bool emit_create_shader_locked(Device* dev, Shader* sh) {
  if (!dev || !sh) {
    return false;
  }

  auto* cmd = append_with_payload_locked<aerogpu_cmd_create_shader_dxbc>(
      dev,
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->bytecode.data(), sh->bytecode.size());
  if (!cmd) {
    return false;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = d3d9_stage_to_aerogpu_stage(sh->stage);
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->bytecode.size());
  cmd->reserved0 = 0;
  return true;
}

bool emit_destroy_shader_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_shader>(dev, AEROGPU_CMD_DESTROY_SHADER);
  if (!cmd) {
    return false;
  }
  cmd->shader_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_create_input_layout_locked(Device* dev, VertexDecl* decl) {
  if (!dev || !decl) {
    return false;
  }

  auto* cmd = append_with_payload_locked<aerogpu_cmd_create_input_layout>(
      dev,
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, decl->blob.data(), decl->blob.size());
  if (!cmd) {
    return false;
  }
  cmd->input_layout_handle = decl->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(decl->blob.size());
  cmd->reserved0 = 0;
  return true;
}

bool emit_destroy_input_layout_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return false;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_input_layout>(dev, AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  if (!cmd) {
    return false;
  }
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_input_layout_locked(Device* dev, VertexDecl* decl) {
  if (!dev) {
    return false;
  }
  if (dev->vertex_decl == decl) {
    return true;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_input_layout>(dev, AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    return false;
  }

  dev->vertex_decl = decl;
  cmd->input_layout_handle = decl ? decl->handle : 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_stream_source_locked(
    Device* dev,
    uint32_t stream,
    Resource* vb,
    uint32_t offset_bytes,
    uint32_t stride_bytes) {
  if (!dev || stream >= 16) {
    return false;
  }

  DeviceStateStream& ss = dev->streams[stream];
  if (ss.vb == vb && ss.offset_bytes == offset_bytes && ss.stride_bytes == stride_bytes) {
    return true;
  }

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = vb ? vb->handle : 0;
  binding.stride_bytes = stride_bytes;
  binding.offset_bytes = offset_bytes;
  binding.reserved0 = 0;

  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
      dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
  if (!cmd) {
    return false;
  }
  cmd->start_slot = stream;
  cmd->buffer_count = 1;

  ss.vb = vb;
  ss.offset_bytes = offset_bytes;
  ss.stride_bytes = stride_bytes;
  return true;
}

Shader* create_internal_shader_locked(
    Device* dev,
    uint32_t stage,
    const void* bytecode,
    uint32_t bytecode_size) {
  if (!dev || !dev->adapter || !bytecode || bytecode_size == 0) {
    return nullptr;
  }

  auto sh = std::make_unique<Shader>();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = stage;
  try {
    sh->bytecode.resize(bytecode_size);
  } catch (...) {
    return nullptr;
  }
  std::memcpy(sh->bytecode.data(), bytecode, bytecode_size);

  if (!emit_create_shader_locked(dev, sh.get())) {
    return nullptr;
  }
  return sh.release();
}

VertexDecl* create_internal_vertex_decl_locked(Device* dev, const void* pDecl, uint32_t decl_size) {
  if (!dev || !dev->adapter || !pDecl || decl_size == 0) {
    return nullptr;
  }

  auto decl = std::make_unique<VertexDecl>();
  decl->handle = allocate_global_handle(dev->adapter);
  try {
    decl->blob.resize(decl_size);
  } catch (...) {
    return nullptr;
  }
  std::memcpy(decl->blob.data(), pDecl, decl_size);

  if (!emit_create_input_layout_locked(dev, decl.get())) {
    return nullptr;
  }
  return decl.release();
}

HRESULT ensure_fixedfunc_pipeline_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  if (dev->fvf != kSupportedFvfXyzrhwDiffuse) {
    return D3DERR_INVALIDCALL;
  }

  if (!dev->fixedfunc_vs) {
    const void* vs_bytes = fixedfunc::kVsPassthroughPosColor;
    const uint32_t vs_size = static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor));
    dev->fixedfunc_vs = create_internal_shader_locked(dev, kD3d9ShaderStageVs, vs_bytes, vs_size);
    if (!dev->fixedfunc_vs) {
      return E_OUTOFMEMORY;
    }
  }
  if (!dev->fixedfunc_ps) {
    const void* ps_bytes = fixedfunc::kPsPassthroughColor;
    const uint32_t ps_size = static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor));
    dev->fixedfunc_ps = create_internal_shader_locked(dev, kD3d9ShaderStagePs, ps_bytes, ps_size);
    if (!dev->fixedfunc_ps) {
      return E_OUTOFMEMORY;
    }
  }

  // Ensure the FVF-derived declaration is bound.
  if (dev->fvf_vertex_decl) {
    if (!emit_set_input_layout_locked(dev, dev->fvf_vertex_decl)) {
      return E_OUTOFMEMORY;
    }
  }

  // Bind the fixed-function shaders iff the app did not set explicit shaders.
  if (!dev->user_vs && !dev->user_ps) {
    if (dev->vs != dev->fixedfunc_vs || dev->ps != dev->fixedfunc_ps) {
      Shader* prev_vs = dev->vs;
      Shader* prev_ps = dev->ps;
      dev->vs = dev->fixedfunc_vs;
      dev->ps = dev->fixedfunc_ps;
      if (!emit_bind_shaders_locked(dev)) {
        dev->vs = prev_vs;
        dev->ps = prev_ps;
        return E_OUTOFMEMORY;
      }
    }
  }

  return S_OK;
}

HRESULT ensure_up_vertex_buffer_locked(Device* dev, uint32_t required_size) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  if (required_size == 0) {
    return E_INVALIDARG;
  }

  const uint32_t current_size = dev->up_vertex_buffer ? dev->up_vertex_buffer->size_bytes : 0;
  if (dev->up_vertex_buffer && current_size >= required_size) {
    return S_OK;
  }

  // Grow to the next power-of-two-ish size to avoid reallocating every draw.
  uint32_t new_size = current_size ? current_size : 4096u;
  while (new_size < required_size) {
    new_size = (new_size > (0x7FFFFFFFu / 2)) ? required_size : (new_size * 2);
  }

  auto vb = std::make_unique<Resource>();
  vb->handle = allocate_global_handle(dev->adapter);
  vb->kind = ResourceKind::Buffer;
  vb->size_bytes = new_size;
  try {
    vb->storage.resize(new_size);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_create_resource_locked(dev, vb.get())) {
    return E_OUTOFMEMORY;
  }

  Resource* old = dev->up_vertex_buffer;
  dev->up_vertex_buffer = vb.release();
  if (old) {
    (void)emit_destroy_resource_locked(dev, old->handle);
    delete old;
  }
  return S_OK;
}

HRESULT ensure_up_index_buffer_locked(Device* dev, uint32_t required_size) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  if (required_size == 0) {
    return E_INVALIDARG;
  }

  const uint32_t current_size = dev->up_index_buffer ? dev->up_index_buffer->size_bytes : 0;
  if (dev->up_index_buffer && current_size >= required_size) {
    return S_OK;
  }

  uint32_t new_size = current_size ? current_size : 2048u;
  while (new_size < required_size) {
    new_size = (new_size > (0x7FFFFFFFu / 2)) ? required_size : (new_size * 2);
  }

  auto ib = std::make_unique<Resource>();
  ib->handle = allocate_global_handle(dev->adapter);
  ib->kind = ResourceKind::Buffer;
  ib->size_bytes = new_size;
  try {
    ib->storage.resize(new_size);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_create_resource_locked(dev, ib.get())) {
    return E_OUTOFMEMORY;
  }

  Resource* old = dev->up_index_buffer;
  dev->up_index_buffer = ib.release();
  if (old) {
    (void)emit_destroy_resource_locked(dev, old->handle);
    delete old;
  }
  return S_OK;
}

HRESULT emit_upload_buffer_locked(Device* dev, Resource* res, const void* data, uint32_t size_bytes) {
  if (!dev || !res || !data || size_bytes == 0) {
    return E_INVALIDARG;
  }
  const bool is_buffer = (res->kind == ResourceKind::Buffer);

  if (res->backing_alloc_id != 0) {
    // Host-side validation rejects UPLOAD_RESOURCE for guest-backed resources.
    // Callers must update guest-backed buffers via Lock/Unlock + RESOURCE_DIRTY_RANGE.
    logf("aerogpu-d3d9: emit_upload_buffer_locked called on guest-backed resource handle=%u alloc_id=%u\n",
         static_cast<unsigned>(res->handle),
         static_cast<unsigned>(res->backing_alloc_id));
    return E_INVALIDARG;
  }

  // WebGPU buffer copies require 4-byte alignment. Pad uploads for buffer resources so
  // callers can upload D3D9-sized data (e.g. 3x u16 indices = 6 bytes) without
  // tripping host validation.
  const uint32_t aligned_size_bytes =
      is_buffer ? static_cast<uint32_t>(align_up(static_cast<size_t>(size_bytes), 4)) : size_bytes;

  if (aligned_size_bytes > res->size_bytes) {
    return E_INVALIDARG;
  }

  // Keep a CPU copy for debug/validation and for fixed-function emulation that
  // reads from buffers.
  if (res->storage.size() < aligned_size_bytes) {
    try {
      res->storage.resize(aligned_size_bytes);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
  }
  // Use memmove because some call sites may upload from memory already backed by
  // `res->storage` (overlapping ranges).
  std::memmove(res->storage.data(), data, size_bytes);
  if (aligned_size_bytes > size_bytes) {
    std::memset(res->storage.data() + size_bytes, 0, aligned_size_bytes - size_bytes);
  }

  const uint8_t* src = res->storage.data();
  uint32_t remaining = aligned_size_bytes;
  uint32_t cur_offset = 0;

  while (remaining) {
    // Ensure we can fit at least a minimal upload packet (header + N bytes).
    const size_t min_payload = is_buffer ? 4 : 1;
    const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + min_payload, 4);
    if (!ensure_cmd_space(dev, min_needed)) {
      return E_OUTOFMEMORY;
    }

    // Uploads write into the destination buffer. Track its backing allocation
    // so the KMD alloc table contains the mapping for guest-backed resources.
    // (For internal host-only buffers backing_alloc_id==0, this is a no-op.)
    HRESULT track_hr = track_resource_allocation_locked(dev, res, /*write=*/true);
    if (FAILED(track_hr)) {
      return track_hr;
    }

    // Allocation tracking may have split/flushed the submission; ensure we
    // still have room for at least a minimal upload packet before sizing the
    // next chunk.
    if (!ensure_cmd_space(dev, min_needed)) {
      return E_OUTOFMEMORY;
    }

    const size_t avail = dev->cmd.bytes_remaining();
    size_t chunk = 0;
    if (avail > sizeof(aerogpu_cmd_upload_resource)) {
      chunk = std::min<size_t>(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
    }
    if (is_buffer) {
      chunk &= ~static_cast<size_t>(3);
      // If we can't fit a 4-byte-aligned chunk, force a split and retry.
      if (chunk == 0) {
        submit(dev);
        continue;
      }
    } else {
      while (chunk && align_up(sizeof(aerogpu_cmd_upload_resource) + chunk, 4) > avail) {
        chunk--;
      }
    }
    if (!chunk) {
      // Should only happen if the command buffer is extremely small; try a forced
      // submit and retry.
      submit(dev);
      continue;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_upload_resource>(
        dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }

    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = cur_offset;
    cmd->size_bytes = chunk;

    src += chunk;
    cur_offset += static_cast<uint32_t>(chunk);
    remaining -= static_cast<uint32_t>(chunk);
  }
  return S_OK;
}

float read_f32_unaligned(const uint8_t* p) {
  float v = 0.0f;
  std::memcpy(&v, p, sizeof(v));
  return v;
}

void write_f32_unaligned(uint8_t* p, float v) {
  std::memcpy(p, &v, sizeof(v));
}

void get_viewport_dims_locked(Device* dev, float* out_x, float* out_y, float* out_w, float* out_h) {
  float x = dev->viewport.X;
  float y = dev->viewport.Y;
  float w = dev->viewport.Width;
  float h = dev->viewport.Height;

  if (w <= 0.0f || h <= 0.0f) {
    // Some apps rely on the default viewport. Use the current render target as a
    // conservative fallback.
    if (dev->render_targets[0]) {
      w = static_cast<float>(std::max(1u, dev->render_targets[0]->width));
      h = static_cast<float>(std::max(1u, dev->render_targets[0]->height));
      x = 0.0f;
      y = 0.0f;
    }
  }
  if (w <= 0.0f) {
    w = 1.0f;
  }
  if (h <= 0.0f) {
    h = 1.0f;
  }

  *out_x = x;
  *out_y = y;
  *out_w = w;
  *out_h = h;
}

HRESULT convert_xyzrhw_to_clipspace_locked(
    Device* dev,
    const void* src_vertices,
    uint32_t stride_bytes,
    uint32_t vertex_count,
    std::vector<uint8_t>* out_bytes) {
  if (!out_bytes) {
    return E_INVALIDARG;
  }
  out_bytes->clear();
  if (!dev || !src_vertices || stride_bytes < 20 || vertex_count == 0) {
    return E_INVALIDARG;
  }

  float vp_x = 0.0f;
  float vp_y = 0.0f;
  float vp_w = 1.0f;
  float vp_h = 1.0f;
  get_viewport_dims_locked(dev, &vp_x, &vp_y, &vp_w, &vp_h);

  const uint64_t total_bytes_u64 = static_cast<uint64_t>(stride_bytes) * static_cast<uint64_t>(vertex_count);
  if (total_bytes_u64 == 0 || total_bytes_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }
  try {
    out_bytes->resize(static_cast<size_t>(total_bytes_u64));
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  const uint8_t* src_base = reinterpret_cast<const uint8_t*>(src_vertices);
  uint8_t* dst_base = out_bytes->data();

  for (uint32_t i = 0; i < vertex_count; i++) {
    const uint8_t* src = src_base + static_cast<size_t>(i) * stride_bytes;
    uint8_t* dst = dst_base + static_cast<size_t>(i) * stride_bytes;

    // Preserve any trailing fields (diffuse color etc).
    std::memcpy(dst, src, stride_bytes);

    const float x = read_f32_unaligned(src + 0);
    const float y = read_f32_unaligned(src + 4);
    const float z = read_f32_unaligned(src + 8);
    const float rhw = read_f32_unaligned(src + 12);

    const float w = (rhw != 0.0f) ? (1.0f / rhw) : 1.0f;
    // D3D9's viewport transform uses a -0.5 pixel center convention. Invert it
    // so typical D3D9 pre-transformed vertex coordinates line up with pixel
    // centers.
    const float ndc_x = ((x + 0.5f - vp_x) / vp_w) * 2.0f - 1.0f;
    const float ndc_y = 1.0f - ((y + 0.5f - vp_y) / vp_h) * 2.0f;
    const float ndc_z = z;

    write_f32_unaligned(dst + 0, ndc_x * w);
    write_f32_unaligned(dst + 4, ndc_y * w);
    write_f32_unaligned(dst + 8, ndc_z * w);
    write_f32_unaligned(dst + 12, w);
  }
  return S_OK;
}

// -----------------------------------------------------------------------------
// Submission
// -----------------------------------------------------------------------------
//
// Shared allocations must use stable `alloc_id` values that are extremely
// unlikely to collide across guest processes: DWM can reference many redirected
// surfaces from different processes in a single submission, and the KMD's
// per-submit allocation table is keyed by `alloc_id`.
//
// The D3D9 UMD uses a best-effort cross-process monotonic counter (implemented
// via a named file mapping) to derive 31-bit alloc_id values for shared
// allocations.
//
// The mapping name is stable across processes in the current session and is
// keyed by the adapter LUID so multiple adapters don't alias the same counter.
uint64_t allocate_shared_alloc_id_token(Adapter* adapter) {
  if (!adapter) {
    return 0;
  }

#if defined(_WIN32)
  {
    std::lock_guard<std::mutex> lock(adapter->share_token_mutex);

    if (!adapter->share_token_view) {
      wchar_t name[128];
      // Keep the object name stable across processes within a session.
      // Multiple adapters can disambiguate via LUID when available.
      swprintf(name,
               sizeof(name) / sizeof(name[0]),
               L"Local\\AeroGPU.D3D9.ShareToken.%08X%08X",
               static_cast<unsigned>(adapter->luid.HighPart),
               static_cast<unsigned>(adapter->luid.LowPart));

      // This mapping backs the cross-process alloc_id allocator used for D3D9Ex
      // shared surfaces. DWM may open and submit shared allocations from many
      // *different* processes in a single batch, so alloc_id values must be
      // unique across guest processes, not just within one process.
      //
      // Use a permissive DACL so the mapping can be opened by other processes in
      // the session (e.g. DWM, sandboxed apps, different integrity levels).
      HANDLE mapping =
          win32::CreateFileMappingWBestEffortLowIntegrity(
              INVALID_HANDLE_VALUE, PAGE_READWRITE, 0, sizeof(uint64_t), name);
      if (mapping) {
        void* view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(uint64_t));
        if (view) {
          adapter->share_token_mapping = mapping;
          adapter->share_token_view = view;
        } else {
          CloseHandle(mapping);
        }
      }
    }

    if (adapter->share_token_view) {
      auto* counter = reinterpret_cast<volatile LONG64*>(adapter->share_token_view);
      LONG64 token = InterlockedIncrement64(counter);
      const uint32_t alloc_id =
          static_cast<uint32_t>(static_cast<uint64_t>(token) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      if (alloc_id == 0) {
        token = InterlockedIncrement64(counter);
      }
      return static_cast<uint64_t>(token);
    }
  }

  // If we fail to set up the cross-process allocator, we must still return a
  // value that produces an alloc_id unlikely to collide across processes.
  //
  // NOTE: alloc_id is derived by masking to 31 bits
  // (`token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX`). A previous PID+counter fallback
  // placed the PID in the high 32 bits, which are discarded by the mask, making
  // collisions across processes *deterministic* (every process would generate
  // alloc_id=1,2,3,...).
  static std::once_flag warn_once;
  std::call_once(warn_once, [] {
    logf("aerogpu-d3d9: alloc_id allocator: shared mapping unavailable; using RNG fallback\n");
  });

  // Best-effort: use the same crypto RNG strategy as the shared-surface
  // ShareTokenAllocator so collisions across processes are vanishingly unlikely.
  for (;;) {
    const uint64_t token = adapter->share_token_allocator.allocate_share_token();
    const uint32_t alloc_id =
        static_cast<uint32_t>(token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
    if (alloc_id != 0) {
      return token;
    }
  }
#else
  (void)adapter;
  static std::atomic<uint64_t> next_token{1};
  return next_token.fetch_add(1);
#endif
}

uint32_t allocate_umd_alloc_id(Adapter* adapter) {
  if (!adapter) {
    return 0;
  }

  // Use the same cross-process monotonic allocator used by shared resources so
  // alloc_id values never collide when DWM batches resources from many
  // processes in a single submission.
  for (;;) {
    const uint64_t token = allocate_shared_alloc_id_token(adapter);
    if (token == 0) {
      return 0;
    }

    const uint32_t alloc_id = static_cast<uint32_t>(token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
    if (alloc_id != 0) {
      return alloc_id;
    }
  }
}

namespace {
#if defined(_WIN32)
template <typename T, typename = void>
struct has_pfnRenderCb : std::false_type {};
template <typename T>
struct has_pfnRenderCb<T, std::void_t<decltype(std::declval<T>().pfnRenderCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnPresentCb : std::false_type {};
template <typename T>
struct has_pfnPresentCb<T, std::void_t<decltype(std::declval<T>().pfnPresentCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnSubmitCommandCb : std::false_type {};
template <typename T>
struct has_pfnSubmitCommandCb<T, std::void_t<decltype(std::declval<T>().pfnSubmitCommandCb)>> : std::true_type {};

template <typename T, typename = void>
struct has_pfnAllocateCb : std::false_type {};
template <typename T>
struct has_pfnAllocateCb<T, std::void_t<decltype(std::declval<T>().pfnAllocateCb)>> {
  using MemberT = decltype(std::declval<T>().pfnAllocateCb);
  static constexpr bool value =
      std::is_pointer_v<MemberT> && std::is_function_v<std::remove_pointer_t<MemberT>>;
};

template <typename T, typename = void>
struct has_pfnDeallocateCb : std::false_type {};
template <typename T>
struct has_pfnDeallocateCb<T, std::void_t<decltype(std::declval<T>().pfnDeallocateCb)>> {
  using MemberT = decltype(std::declval<T>().pfnDeallocateCb);
  static constexpr bool value =
      std::is_pointer_v<MemberT> && std::is_function_v<std::remove_pointer_t<MemberT>>;
};

template <typename T, typename = void>
struct has_pfnGetCommandBufferCb : std::false_type {};
template <typename T>
struct has_pfnGetCommandBufferCb<T, std::void_t<decltype(std::declval<T>().pfnGetCommandBufferCb)>> {
  using MemberT = decltype(std::declval<T>().pfnGetCommandBufferCb);
  static constexpr bool value =
      std::is_pointer_v<MemberT> && std::is_function_v<std::remove_pointer_t<MemberT>>;
};

template <typename Fn>
struct fn_first_param;

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(__stdcall*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename T, typename = void>
struct has_member_hContext : std::false_type {};
template <typename T>
struct has_member_hContext<T, std::void_t<decltype(std::declval<T>().hContext)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hDevice : std::false_type {};
template <typename T>
struct has_member_hDevice<T, std::void_t<decltype(std::declval<T>().hDevice)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pCommandBuffer<T, std::void_t<decltype(std::declval<T>().pCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDmaBuffer : std::false_type {};
template <typename T>
struct has_member_pDmaBuffer<T, std::void_t<decltype(std::declval<T>().pDmaBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CommandLength : std::false_type {};
template <typename T>
struct has_member_CommandLength<T, std::void_t<decltype(std::declval<T>().CommandLength)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CommandBufferSize : std::false_type {};
template <typename T>
struct has_member_CommandBufferSize<T, std::void_t<decltype(std::declval<T>().CommandBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DmaBufferSize : std::false_type {};
template <typename T>
struct has_member_DmaBufferSize<T, std::void_t<decltype(std::declval<T>().DmaBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocationList : std::false_type {};
template <typename T>
struct has_member_pAllocationList<T, std::void_t<decltype(std::declval<T>().pAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AllocationListSize : std::false_type {};
template <typename T>
struct has_member_AllocationListSize<T, std::void_t<decltype(std::declval<T>().AllocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NumAllocations : std::false_type {};
template <typename T>
struct has_member_NumAllocations<T, std::void_t<decltype(std::declval<T>().NumAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pPatchLocationList : std::false_type {};
template <typename T>
struct has_member_pPatchLocationList<T, std::void_t<decltype(std::declval<T>().pPatchLocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_PatchLocationListSize : std::false_type {};
template <typename T>
struct has_member_PatchLocationListSize<T, std::void_t<decltype(std::declval<T>().PatchLocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NumPatchLocations : std::false_type {};
template <typename T>
struct has_member_NumPatchLocations<T, std::void_t<decltype(std::declval<T>().NumPatchLocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Flags : std::false_type {};
template <typename T>
struct has_member_Flags<T, std::void_t<decltype(std::declval<T>().Flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Present : std::false_type {};
template <typename T>
struct has_member_Present<T, std::void_t<decltype(std::declval<T>().Present)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewCommandBuffer : std::false_type {};
template <typename T>
struct has_member_pNewCommandBuffer<T, std::void_t<decltype(std::declval<T>().pNewCommandBuffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewCommandBufferSize : std::false_type {};
template <typename T>
struct has_member_NewCommandBufferSize<T, std::void_t<decltype(std::declval<T>().NewCommandBufferSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewAllocationList : std::false_type {};
template <typename T>
struct has_member_pNewAllocationList<T, std::void_t<decltype(std::declval<T>().pNewAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewAllocationListSize : std::false_type {};
template <typename T>
struct has_member_NewAllocationListSize<T, std::void_t<decltype(std::declval<T>().NewAllocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pNewPatchLocationList : std::false_type {};
template <typename T>
struct has_member_pNewPatchLocationList<T, std::void_t<decltype(std::declval<T>().pNewPatchLocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewPatchLocationListSize : std::false_type {};
template <typename T>
struct has_member_NewPatchLocationListSize<T, std::void_t<decltype(std::declval<T>().NewPatchLocationListSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SubmissionFenceId : std::false_type {};
template <typename T>
struct has_member_SubmissionFenceId<T, std::void_t<decltype(std::declval<T>().SubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NewFenceValue : std::false_type {};
template <typename T>
struct has_member_NewFenceValue<T, std::void_t<decltype(std::declval<T>().NewFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pSubmissionFenceId : std::false_type {};
template <typename T>
struct has_member_pSubmissionFenceId<T, std::void_t<decltype(std::declval<T>().pSubmissionFenceId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_FenceValue : std::false_type {};
template <typename T>
struct has_member_FenceValue<T, std::void_t<decltype(std::declval<T>().FenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pFenceValue : std::false_type {};
template <typename T>
struct has_member_pFenceValue<T, std::void_t<decltype(std::declval<T>().pFenceValue)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDmaBufferPrivateData : std::false_type {};
template <typename T>
struct has_member_pDmaBufferPrivateData<T, std::void_t<decltype(std::declval<T>().pDmaBufferPrivateData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DmaBufferPrivateDataSize : std::false_type {};
template <typename T>
struct has_member_DmaBufferPrivateDataSize<T, std::void_t<decltype(std::declval<T>().DmaBufferPrivateDataSize)>>
    : std::true_type {};

template <typename ArgsT>
constexpr bool submit_args_can_signal_present() {
  if constexpr (has_member_Present<ArgsT>::value) {
    using PresentT = std::remove_reference_t<decltype(std::declval<ArgsT>().Present)>;
    if constexpr (std::is_integral_v<PresentT>) {
      return true;
    }
  }
  if constexpr (has_member_Flags<ArgsT>::value) {
    using FlagsT = std::remove_reference_t<decltype(std::declval<ArgsT>().Flags)>;
    if constexpr (has_member_Present<FlagsT>::value) {
      return true;
    }
  }
  return false;
}

template <typename CallbackFn>
constexpr bool submit_callback_can_signal_present() {
  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  return submit_args_can_signal_present<Arg>();
}

template <typename ArgsT>
void fill_submit_args(ArgsT& args, Device* dev, uint32_t command_length_bytes, bool is_present) {
  [[maybe_unused]] const bool patch_list_available = (dev->wddm_context.pPatchLocationList != nullptr);
  [[maybe_unused]] const uint32_t patch_list_used = patch_list_available ? dev->wddm_context.patch_location_entries_used : 0;
  [[maybe_unused]] const uint32_t patch_list_capacity = patch_list_available ? dev->wddm_context.PatchLocationListSize : 0;
  if constexpr (has_member_hDevice<ArgsT>::value) {
    args.hDevice = dev->wddm_device;
  }
  if constexpr (has_member_hContext<ArgsT>::value) {
    args.hContext = dev->wddm_context.hContext;
  }
  if constexpr (has_member_pCommandBuffer<ArgsT>::value) {
    args.pCommandBuffer = dev->wddm_context.pCommandBuffer;
  }
  if constexpr (has_member_pDmaBuffer<ArgsT>::value) {
    args.pDmaBuffer = dev->wddm_context.pDmaBuffer ? dev->wddm_context.pDmaBuffer : dev->wddm_context.pCommandBuffer;
  }
  if constexpr (has_member_CommandLength<ArgsT>::value) {
    args.CommandLength = command_length_bytes;
  }
  if constexpr (has_member_CommandBufferSize<ArgsT>::value) {
    args.CommandBufferSize = dev->wddm_context.CommandBufferSize;
  }
  if constexpr (has_member_DmaBufferSize<ArgsT>::value) {
    // DmaBufferSize is consistently interpreted by Win7-era callback structs as
    // the number of bytes used in the DMA buffer (not the total capacity).
    // Populate it with the used byte count to avoid dxgkrnl/KMD reading
    // uninitialized command buffer bytes.
    args.DmaBufferSize = command_length_bytes;
  }
  if constexpr (has_member_pAllocationList<ArgsT>::value) {
    args.pAllocationList = dev->wddm_context.pAllocationList;
  }
  if constexpr (has_member_AllocationListSize<ArgsT>::value) {
    // DDI structs disagree on whether AllocationListSize means "capacity" or
    // "entries used". When NumAllocations is present, treat AllocationListSize
    // as the capacity returned by CreateContext. Otherwise fall back to the used
    // count (legacy submit structs with only a single size field).
    if constexpr (has_member_NumAllocations<ArgsT>::value) {
      args.AllocationListSize = dev->wddm_context.AllocationListSize;
    } else {
      args.AllocationListSize = dev->wddm_context.allocation_list_entries_used;
    }
  }
  if constexpr (has_member_NumAllocations<ArgsT>::value) {
    args.NumAllocations = dev->wddm_context.allocation_list_entries_used;
  }
  if constexpr (has_member_pPatchLocationList<ArgsT>::value) {
    args.pPatchLocationList = patch_list_available ? dev->wddm_context.pPatchLocationList : nullptr;
  }
  if constexpr (has_member_PatchLocationListSize<ArgsT>::value) {
    // AeroGPU intentionally submits with an empty patch-location list.
    //
    // - Callback structs that split capacity vs. used across
    //   {PatchLocationListSize, NumPatchLocations} expect PatchLocationListSize to
    //   describe the list capacity returned by CreateContext.
    // - Legacy structs with only PatchLocationListSize interpret it as the number
    //   of patch locations used.
    if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
      args.PatchLocationListSize = patch_list_capacity;
    } else {
      args.PatchLocationListSize = patch_list_used;
    }
  }
  if constexpr (has_member_NumPatchLocations<ArgsT>::value) {
    args.NumPatchLocations = patch_list_used;
  }
  if constexpr (has_member_pDmaBufferPrivateData<ArgsT>::value) {
    args.pDmaBufferPrivateData = dev->wddm_context.pDmaBufferPrivateData;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<ArgsT>::value) {
    // Clamp to the driver-private ABI size so dxgkrnl doesn't copy extra
    // user-mode bytes into kernel buffers.
    args.DmaBufferPrivateDataSize = dev->wddm_context.DmaBufferPrivateDataSize;
    if (args.DmaBufferPrivateDataSize > AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
      args.DmaBufferPrivateDataSize = AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
    }
  }

  // Some WDDM callback arg structs include flags distinguishing render vs present.
  // If such flags are present, populate them so present submissions prefer the
  // DxgkDdiPresent path when routed via RenderCb fallback.
  if constexpr (has_member_Present<ArgsT>::value) {
    using PresentT = std::remove_reference_t<decltype(args.Present)>;
    if constexpr (std::is_integral_v<PresentT>) {
      args.Present = is_present ? 1 : 0;
    }
  }
  if constexpr (has_member_Flags<ArgsT>::value) {
    using FlagsT = std::remove_reference_t<decltype(args.Flags)>;
    if constexpr (has_member_Present<FlagsT>::value) {
      args.Flags.Present = is_present ? 1 : 0;
    }
  }
}

template <typename ArgsT>
void update_context_from_submit_args(Device* dev, const ArgsT& args) {
  const uint8_t* prev_cmd_buffer = dev->wddm_context.pCommandBuffer;
  bool updated_cmd_buffer = false;
  if constexpr (has_member_pNewCommandBuffer<ArgsT>::value && has_member_NewCommandBufferSize<ArgsT>::value) {
    if (args.pNewCommandBuffer && args.NewCommandBufferSize) {
      dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(args.pNewCommandBuffer);
      dev->wddm_context.CommandBufferSize = args.NewCommandBufferSize;
      updated_cmd_buffer = true;
    }
  }

  if (!updated_cmd_buffer) {
    if constexpr (has_member_pCommandBuffer<ArgsT>::value) {
      if (args.pCommandBuffer) {
        dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(args.pCommandBuffer);
      }
    }
    if constexpr (has_member_CommandBufferSize<ArgsT>::value) {
      if (args.CommandBufferSize) {
        dev->wddm_context.CommandBufferSize = args.CommandBufferSize;
      }
    }
  }

  // Track pDmaBuffer separately when exposed by the callback struct. Some WDK
  // vintages include both pDmaBuffer and pCommandBuffer; preserve the DMA buffer
  // pointer so we can pass it back to dxgkrnl.
  bool updated_dma_buffer = false;
  if constexpr (has_member_pDmaBuffer<ArgsT>::value) {
    if (args.pDmaBuffer) {
      dev->wddm_context.pDmaBuffer = static_cast<uint8_t*>(args.pDmaBuffer);
      updated_dma_buffer = true;
    }
  }
  if (!updated_dma_buffer && dev->wddm_context.pCommandBuffer) {
    // If pDmaBuffer is unset (or was previously tracking the old command buffer
    // pointer), keep it in sync with the current command buffer.
    if (!dev->wddm_context.pDmaBuffer || dev->wddm_context.pDmaBuffer == prev_cmd_buffer) {
      dev->wddm_context.pDmaBuffer = dev->wddm_context.pCommandBuffer;
    }
  }

  bool updated_allocation_list = false;
  if constexpr (has_member_pNewAllocationList<ArgsT>::value && has_member_NewAllocationListSize<ArgsT>::value) {
    if (args.pNewAllocationList && args.NewAllocationListSize) {
      dev->wddm_context.pAllocationList = args.pNewAllocationList;
      dev->wddm_context.AllocationListSize = args.NewAllocationListSize;
      updated_allocation_list = true;
    }
  }

  if (!updated_allocation_list) {
    if constexpr (has_member_pAllocationList<ArgsT>::value) {
      if (args.pAllocationList) {
        dev->wddm_context.pAllocationList = args.pAllocationList;
      }
    }
    if constexpr (has_member_AllocationListSize<ArgsT>::value && has_member_NumAllocations<ArgsT>::value) {
      if (args.AllocationListSize) {
        dev->wddm_context.AllocationListSize = args.AllocationListSize;
      }
    }
  }

  bool updated_patch_list = false;
  if constexpr (has_member_pNewPatchLocationList<ArgsT>::value && has_member_NewPatchLocationListSize<ArgsT>::value) {
    // Some runtimes can legitimately provide a 0-sized patch list. Treat the
    // pointer as the authoritative signal that a new patch list is being rotated
    // in, and always copy the size (even if it is 0).
    if (args.pNewPatchLocationList) {
      dev->wddm_context.pPatchLocationList = args.pNewPatchLocationList;
      dev->wddm_context.PatchLocationListSize = args.NewPatchLocationListSize;
      updated_patch_list = true;
    }
  }

  if (!updated_patch_list) {
    if constexpr (has_member_pPatchLocationList<ArgsT>::value) {
      dev->wddm_context.pPatchLocationList = args.pPatchLocationList;
    }
    if constexpr (has_member_PatchLocationListSize<ArgsT>::value && has_member_NumPatchLocations<ArgsT>::value) {
      dev->wddm_context.PatchLocationListSize = args.PatchLocationListSize;
    }
  }

  // pDmaBufferPrivateData is required by the AeroGPU Win7 KMD (DxgkDdiRender /
  // DxgkDdiPresent expect it to be non-null). The runtime may rotate it along
  // with the command buffer, so treat it as an in/out field.
  if constexpr (has_member_pDmaBufferPrivateData<ArgsT>::value) {
    if (args.pDmaBufferPrivateData) {
      dev->wddm_context.pDmaBufferPrivateData = args.pDmaBufferPrivateData;
    }
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<ArgsT>::value) {
    if (args.DmaBufferPrivateDataSize) {
      dev->wddm_context.DmaBufferPrivateDataSize = args.DmaBufferPrivateDataSize;
    }
  }
}

template <typename CallbackFn>
HRESULT invoke_submit_callback(Device* dev,
                               CallbackFn cb,
                               uint32_t command_length_bytes,
                               bool is_present,
                               uint64_t* out_submission_fence) {
  if (out_submission_fence) {
    *out_submission_fence = 0;
  }

  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;

  // Zero-initialize the entire callback struct (including any padding). The D3D9
  // runtime may copy these bytes into kernel mode; leaving padding uninitialized
  // can leak stack bytes and make submission behavior nondeterministic.
  Arg args;
  std::memset(&args, 0, sizeof(args));
  fill_submit_args(args, dev, command_length_bytes, is_present);

  if constexpr (has_member_NewFenceValue<Arg>::value) {
    args.NewFenceValue = 0;
  }

  // Security: `pDmaBufferPrivateData` is copied by dxgkrnl from user mode to
  // kernel mode for every submission. Ensure the blob is explicitly initialized
  // so we never leak uninitialized user-mode stack/heap bytes into the kernel
  // copy.
  //
  // The AeroGPU Win7 KMD overwrites AEROGPU_DMA_PRIV in DxgkDdiRender /
  // DxgkDdiPresent, but some runtimes route submissions through SubmitCommandCb
  // (bypassing those DDIs). Always stamp a deterministic AEROGPU_DMA_PRIV header
  // before invoking the runtime submission callback.
  const uint32_t expected_dma_priv_bytes = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  void* dma_priv_ptr = dev ? dev->wddm_context.pDmaBufferPrivateData : nullptr;
  uint32_t dma_priv_bytes = dev ? dev->wddm_context.DmaBufferPrivateDataSize : 0;
  if constexpr (has_member_pDmaBufferPrivateData<Arg>::value) {
    dma_priv_ptr = args.pDmaBufferPrivateData;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    dma_priv_bytes = args.DmaBufferPrivateDataSize;
  }

  if (!InitWin7DmaBufferPrivateData(dma_priv_ptr, dma_priv_bytes, is_present)) {
    std::call_once(g_dma_priv_invalid_once, [dma_priv_ptr, dma_priv_bytes, expected_dma_priv_bytes] {
      aerogpu::logf("aerogpu-d3d9: submit missing/invalid dma private data ptr=%p bytes=%u (need >=%u)\n",
                    dma_priv_ptr,
                    static_cast<unsigned>(dma_priv_bytes),
                    static_cast<unsigned>(expected_dma_priv_bytes));
    });
    return E_INVALIDARG;
  }

  // Safety: if the runtime reports a larger private-data size than the KMD/UMD
  // contract, clamp to the expected size so dxgkrnl does not copy extra bytes of
  // user-mode memory into kernel-mode buffers.
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    const uint32_t runtime_bytes = dev ? static_cast<uint32_t>(dev->wddm_context.DmaBufferPrivateDataSize) : 0;
    if (runtime_bytes > expected_dma_priv_bytes) {
      std::call_once(g_dma_priv_size_mismatch_once, [runtime_bytes, expected_dma_priv_bytes] {
        aerogpu::logf("aerogpu-d3d9: runtime DmaBufferPrivateDataSize=%u (expected=%u); clamping\n",
                      static_cast<unsigned>(runtime_bytes),
                      static_cast<unsigned>(expected_dma_priv_bytes));
      });
    }
    if (args.DmaBufferPrivateDataSize > expected_dma_priv_bytes) {
      args.DmaBufferPrivateDataSize = expected_dma_priv_bytes;
    }
  }

  uint64_t submission_fence = 0;

  HRESULT hr = E_FAIL;
  if constexpr (has_member_SubmissionFenceId<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.SubmissionFenceId)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      // Some header/interface versions expose SubmissionFenceId as an output
      // pointer rather than an in-struct value. Provide a valid storage location
      // so the runtime can write back the exact per-submission fence ID.
      args.SubmissionFenceId = &fence_storage;
    } else {
      args.SubmissionFenceId = 0;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        } else {
          submission_fence = static_cast<uint64_t>(args.SubmissionFenceId);
        }
      } else {
        if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        } else {
          submission_fence = static_cast<uint64_t>(args.SubmissionFenceId);
        }
      }
    }
  } else if constexpr (has_member_pSubmissionFenceId<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.pSubmissionFenceId)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      args.pSubmissionFenceId = &fence_storage;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        }
      } else if constexpr (std::is_pointer<FenceMemberT>::value) {
        submission_fence = static_cast<uint64_t>(fence_storage);
      }
    }
  } else if constexpr (has_member_pFenceValue<Arg>::value) {
    using FenceMemberT = std::remove_reference_t<decltype(args.pFenceValue)>;
    using FenceStorageT = std::remove_pointer_t<FenceMemberT>;
    FenceStorageT fence_storage{};

    if constexpr (std::is_pointer<FenceMemberT>::value) {
      args.pFenceValue = &fence_storage;
    }

    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else if constexpr (std::is_pointer<FenceMemberT>::value) {
          submission_fence = static_cast<uint64_t>(fence_storage);
        }
      } else if constexpr (std::is_pointer<FenceMemberT>::value) {
        submission_fence = static_cast<uint64_t>(fence_storage);
      }
    }
  } else if constexpr (has_member_FenceValue<Arg>::value) {
    args.FenceValue = 0;
    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        if (args.NewFenceValue) {
          submission_fence = static_cast<uint64_t>(args.NewFenceValue);
        } else {
          submission_fence = static_cast<uint64_t>(args.FenceValue);
        }
      } else {
        submission_fence = static_cast<uint64_t>(args.FenceValue);
      }
    }
  } else {
    hr = cb(static_cast<ArgPtr>(&args));
    if (SUCCEEDED(hr)) {
      if constexpr (has_member_NewFenceValue<Arg>::value) {
        submission_fence = static_cast<uint64_t>(args.NewFenceValue);
      }
    }
  }

  if (FAILED(hr)) {
    return hr;
  }

  if (out_submission_fence) {
    *out_submission_fence = submission_fence;
  }

  // The runtime may rotate command buffers/lists after a submission. Preserve the
  // updated pointers and reset the book-keeping so the next submission starts
  // from a clean command stream header.
  update_context_from_submit_args(dev, args);
  // Keep the command stream writer bound to the currently active command buffer.
  // The runtime is allowed to return a new DMA buffer pointer/size in the
  // callback out-params; failing to rebind would cause us to write into a stale
  // buffer on the next submission.
  if (dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= sizeof(aerogpu_cmd_stream_header)) {
    dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  }
  dev->wddm_context.reset_submission_buffers();
  return hr;
}
#endif
} // namespace

#if defined(_WIN32)
template <typename CallbackFn>
void wddm_deallocate_buffers_impl(Device* dev,
                                  CallbackFn cb,
                                  void* dma_buffer,
                                  void* command_buffer,
                                  WddmAllocationList* allocation_list,
                                  WddmPatchLocationList* patch_location_list,
                                  void* dma_priv,
                                  uint32_t dma_priv_bytes) {
  if (!dev || !cb) {
    return;
  }

  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;
  Arg args;
  std::memset(&args, 0, sizeof(args));

  if constexpr (has_member_hContext<Arg>::value) {
    args.hContext = dev->wddm_context.hContext;
  }
  if constexpr (has_member_hDevice<Arg>::value) {
    args.hDevice = dev->wddm_device;
  }
  if constexpr (has_member_pDmaBuffer<Arg>::value) {
    args.pDmaBuffer = dma_buffer;
  }
  if constexpr (has_member_pCommandBuffer<Arg>::value) {
    args.pCommandBuffer = command_buffer;
  }
  if constexpr (has_member_pAllocationList<Arg>::value) {
    args.pAllocationList = allocation_list;
  }
  if constexpr (has_member_pPatchLocationList<Arg>::value) {
    args.pPatchLocationList = patch_location_list;
  }
  if constexpr (has_member_pDmaBufferPrivateData<Arg>::value) {
    args.pDmaBufferPrivateData = dma_priv;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    args.DmaBufferPrivateDataSize = dma_priv_bytes;
  }

  (void)cb(static_cast<ArgPtr>(&args));
}

void wddm_deallocate_active_buffers(Device* dev) {
  if (!dev || !dev->adapter) {
    return;
  }
  if (dev->wddm_context.hContext == 0 || !dev->wddm_context.buffers_need_deallocate) {
    return;
  }

  // Snapshot the pointers returned by AllocateCb (the submit callback is allowed
  // to rotate the context's live pointers).
  void* dma_buffer = dev->wddm_context.allocated_pDmaBuffer;
  void* cmd_buffer = dev->wddm_context.allocated_pCommandBuffer;
  WddmAllocationList* alloc_list = dev->wddm_context.allocated_pAllocationList;
  WddmPatchLocationList* patch_list = dev->wddm_context.allocated_pPatchLocationList;
  void* dma_priv = dev->wddm_context.allocated_pDmaBufferPrivateData;
  uint32_t dma_priv_bytes = dev->wddm_context.allocated_DmaBufferPrivateDataSize;
  const bool dma_priv_from_allocate = dev->wddm_context.dma_priv_from_allocate;

  if constexpr (has_pfnDeallocateCb<WddmDeviceCallbacks>::value) {
    if (dev->wddm_callbacks.pfnDeallocateCb) {
      wddm_deallocate_buffers_impl(dev, dev->wddm_callbacks.pfnDeallocateCb, dma_buffer, cmd_buffer, alloc_list, patch_list, dma_priv, dma_priv_bytes);
    }
  }

  // Prevent use-after-free on any deallocated runtime-provided buffers.
  //
  // In the AllocateCb/DeallocateCb acquisition model, treat any "rotated" submit
  // buffer pointers (pNewCommandBuffer/pNewAllocationList/...) as advisory: once
  // we return the AllocateCb buffers, the rotated pointers are not guaranteed to
  // remain valid. Force the next `ensure_cmd_space()` to reacquire buffers via
  // GetCommandBufferCb/AllocateCb.
  dev->wddm_context.pDmaBuffer = nullptr;
  dev->wddm_context.pCommandBuffer = nullptr;
  dev->wddm_context.CommandBufferSize = 0;
  dev->wddm_context.pAllocationList = nullptr;
  dev->wddm_context.AllocationListSize = 0;
  dev->wddm_context.pPatchLocationList = nullptr;
  dev->wddm_context.PatchLocationListSize = 0;
  if (dma_priv_from_allocate || (dma_priv && dev->wddm_context.pDmaBufferPrivateData == dma_priv)) {
    dev->wddm_context.pDmaBufferPrivateData = nullptr;
    dev->wddm_context.DmaBufferPrivateDataSize = 0;
  }
  dev->wddm_context.dma_priv_from_allocate = false;

  dev->wddm_context.buffers_need_deallocate = false;
  dev->wddm_context.allocated_pDmaBuffer = nullptr;
  dev->wddm_context.allocated_pCommandBuffer = nullptr;
  dev->wddm_context.allocated_pAllocationList = nullptr;
  dev->wddm_context.allocated_pPatchLocationList = nullptr;
  dev->wddm_context.allocated_pDmaBufferPrivateData = nullptr;
  dev->wddm_context.allocated_DmaBufferPrivateDataSize = 0;

  dev->cmd.set_span(nullptr, 0);
  dev->alloc_list_tracker.rebind(nullptr, 0, dev->adapter->max_allocation_list_slot_id);
}

template <typename CallbackFn>
HRESULT wddm_acquire_submit_buffers_allocate_impl(Device* dev, CallbackFn cb, uint32_t request_bytes) {
  if (!dev || !dev->adapter || !cb) {
    return E_INVALIDARG;
  }

  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;

  Arg args;
  std::memset(&args, 0, sizeof(args));
  if constexpr (has_member_hContext<Arg>::value) {
    args.hContext = dev->wddm_context.hContext;
  }
  if constexpr (has_member_hDevice<Arg>::value) {
    args.hDevice = dev->wddm_device;
  }
  if constexpr (has_member_DmaBufferSize<Arg>::value) {
    args.DmaBufferSize = request_bytes;
  }
  if constexpr (has_member_CommandBufferSize<Arg>::value) {
    args.CommandBufferSize = request_bytes;
  }
  if constexpr (has_member_AllocationListSize<Arg>::value) {
    // Some runtimes treat AllocationListSize as an input (capacity request) and
    // will fail or return a 0-sized list if it is left at 0. Request a generous
    // default so allocation tracking can work even when CreateContext did not
    // provide a persistent allocation list.
    uint32_t request_entries = std::max<uint32_t>(
        dev->wddm_context.AllocationListSize ? dev->wddm_context.AllocationListSize : 0u, 4096u);
    // We assign allocation-list slot IDs densely as 0..N-1. Clamp the requested
    // list size to the KMD-advertised max slot ID (+1) so we don't ask the
    // runtime for more entries than we can legally reference.
    if (dev->adapter && dev->adapter->max_allocation_list_slot_id != std::numeric_limits<uint32_t>::max()) {
      request_entries = std::min<uint32_t>(request_entries, dev->adapter->max_allocation_list_slot_id + 1u);
    }
    args.AllocationListSize = request_entries;
  }
  if constexpr (has_member_PatchLocationListSize<Arg>::value) {
    args.PatchLocationListSize = 0;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    // Ensure the runtime allocates enough DMA private data for the Win7 AeroGPU
    // contract (AEROGPU_DMA_PRIV).
    args.DmaBufferPrivateDataSize = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  }

  const HRESULT hr = cb(static_cast<ArgPtr>(&args));

  void* cmd_ptr = nullptr;
  void* dma_ptr = nullptr;
  uint32_t cap = 0;
  bool cap_from_dma_buffer_size = false;

  if constexpr (has_member_pDmaBuffer<Arg>::value) {
    dma_ptr = args.pDmaBuffer;
    cmd_ptr = args.pDmaBuffer;
  }
  if constexpr (has_member_pCommandBuffer<Arg>::value) {
    if (args.pCommandBuffer) {
      cmd_ptr = args.pCommandBuffer;
    }
  }
  if constexpr (has_member_DmaBufferSize<Arg>::value) {
    cap = static_cast<uint32_t>(args.DmaBufferSize);
    cap_from_dma_buffer_size = (cap != 0);
  }
  if constexpr (has_member_CommandBufferSize<Arg>::value) {
    if (cap == 0) {
      cap = static_cast<uint32_t>(args.CommandBufferSize);
    }
  }
  if (!cmd_ptr) {
    cmd_ptr = dma_ptr;
  }
  if (!dma_ptr) {
    dma_ptr = cmd_ptr;
  }
  if (cap_from_dma_buffer_size) {
    cap = AdjustCommandBufferSizeFromDmaBuffer(dma_ptr, cmd_ptr, cap);
  }

  WddmAllocationList* alloc_list = nullptr;
  uint32_t alloc_entries = 0;
  if constexpr (has_member_pAllocationList<Arg>::value) {
    alloc_list = args.pAllocationList;
  }
  if constexpr (has_member_AllocationListSize<Arg>::value) {
    alloc_entries = static_cast<uint32_t>(args.AllocationListSize);
  }

  WddmPatchLocationList* patch_list = nullptr;
  uint32_t patch_entries = 0;
  if constexpr (has_member_pPatchLocationList<Arg>::value) {
    patch_list = args.pPatchLocationList;
  }
  if constexpr (has_member_PatchLocationListSize<Arg>::value) {
    patch_entries = static_cast<uint32_t>(args.PatchLocationListSize);
  }

  void* dma_priv = nullptr;
  uint32_t dma_priv_bytes = 0;
  if constexpr (has_member_pDmaBufferPrivateData<Arg>::value) {
    dma_priv = args.pDmaBufferPrivateData;
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    dma_priv_bytes = static_cast<uint32_t>(args.DmaBufferPrivateDataSize);
  }
  const uint32_t expected_dma_priv_bytes = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  if (dma_priv && dma_priv_bytes == 0) {
    dma_priv_bytes = expected_dma_priv_bytes;
  }

  if (FAILED(hr) || !cmd_ptr || cap == 0 || !alloc_list || alloc_entries == 0) {
    if constexpr (has_pfnDeallocateCb<WddmDeviceCallbacks>::value) {
      if (dev->wddm_callbacks.pfnDeallocateCb) {
        if (cmd_ptr || dma_ptr || alloc_list || patch_list || dma_priv) {
          wddm_deallocate_buffers_impl(dev,
                                       dev->wddm_callbacks.pfnDeallocateCb,
                                       dma_ptr,
                                       cmd_ptr,
                                       alloc_list,
                                       patch_list,
                                       dma_priv,
                                       dma_priv_bytes);
        }
      }
    }
    return FAILED(hr) ? hr : E_OUTOFMEMORY;
  }

  dev->wddm_context.buffers_need_deallocate = true;
  dev->wddm_context.allocated_pDmaBuffer = dma_ptr;
  dev->wddm_context.allocated_pCommandBuffer = cmd_ptr;
  dev->wddm_context.allocated_pAllocationList = alloc_list;
  dev->wddm_context.allocated_pPatchLocationList = patch_list;
  dev->wddm_context.allocated_pDmaBufferPrivateData = dma_priv;
  dev->wddm_context.allocated_DmaBufferPrivateDataSize = dma_priv_bytes;

  dev->wddm_context.pDmaBuffer = static_cast<uint8_t*>(dma_ptr ? dma_ptr : cmd_ptr);
  dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(cmd_ptr);
  dev->wddm_context.CommandBufferSize = cap;
  dev->wddm_context.pAllocationList = alloc_list;
  dev->wddm_context.AllocationListSize = alloc_entries;
  dev->wddm_context.pPatchLocationList = patch_list;
  dev->wddm_context.PatchLocationListSize = patch_entries;

  // Prefer the per-buffer DMA private data returned by AllocateCb when it is
  // available. Some runtimes associate this blob with the allocated DMA buffer
  // and may rotate it alongside the command buffer.
  if (dma_priv && dma_priv_bytes >= expected_dma_priv_bytes) {
    dev->wddm_context.pDmaBufferPrivateData = dma_priv;
    dev->wddm_context.DmaBufferPrivateDataSize = dma_priv_bytes;
    dev->wddm_context.dma_priv_from_allocate = true;
  } else {
    dev->wddm_context.dma_priv_from_allocate = false;
  }

  dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  dev->wddm_context.reset_submission_buffers();
  dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                 dev->wddm_context.AllocationListSize,
                                 dev->adapter->max_allocation_list_slot_id);
  return S_OK;
}

template <typename CallbackFn>
HRESULT wddm_acquire_submit_buffers_get_command_buffer_impl(Device* dev, CallbackFn cb) {
  if (!dev || !dev->adapter || !cb) {
    return E_INVALIDARG;
  }

  const uint32_t expected_dma_priv_bytes = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);

  using ArgPtr = typename fn_first_param<CallbackFn>::type;
  using Arg = std::remove_const_t<std::remove_pointer_t<ArgPtr>>;

  Arg args;
  std::memset(&args, 0, sizeof(args));
  if constexpr (has_member_hContext<Arg>::value) {
    args.hContext = dev->wddm_context.hContext;
  }
  if constexpr (has_member_hDevice<Arg>::value) {
    args.hDevice = dev->wddm_device;
  }

  const HRESULT hr = cb(static_cast<ArgPtr>(&args));
  if (FAILED(hr)) {
    return hr;
  }

  void* cmd_ptr = nullptr;
  void* dma_ptr = nullptr;
  uint32_t cap = 0;
  bool cap_from_dma_buffer_size = false;
  if constexpr (has_member_pDmaBuffer<Arg>::value) {
    dma_ptr = args.pDmaBuffer;
    cmd_ptr = args.pDmaBuffer;
  }
  if constexpr (has_member_pCommandBuffer<Arg>::value) {
    if (args.pCommandBuffer) {
      cmd_ptr = args.pCommandBuffer;
    }
  }
  if constexpr (has_member_CommandBufferSize<Arg>::value) {
    cap = static_cast<uint32_t>(args.CommandBufferSize);
  }
  if constexpr (has_member_DmaBufferSize<Arg>::value) {
    if (cap == 0) {
      cap = static_cast<uint32_t>(args.DmaBufferSize);
      cap_from_dma_buffer_size = (cap != 0);
    }
  }
  if (!cmd_ptr) {
    cmd_ptr = dma_ptr;
  }
  if (!dma_ptr) {
    dma_ptr = cmd_ptr;
  }
  if (cap_from_dma_buffer_size) {
    cap = AdjustCommandBufferSizeFromDmaBuffer(dma_ptr, cmd_ptr, cap);
  }

  // Some runtimes only return the new command buffer via GetCommandBufferCb and
  // keep the allocation/patch lists stable from CreateContext. Start from the
  // current context pointers and override with any callback-provided values.
  WddmAllocationList* alloc_list = dev->wddm_context.pAllocationList;
  uint32_t alloc_entries = dev->wddm_context.AllocationListSize;
  if constexpr (has_member_pAllocationList<Arg>::value) {
    if (args.pAllocationList) {
      alloc_list = args.pAllocationList;
    }
  }
  if constexpr (has_member_AllocationListSize<Arg>::value) {
    if (args.AllocationListSize) {
      alloc_entries = static_cast<uint32_t>(args.AllocationListSize);
    }
  }

  WddmPatchLocationList* patch_list = dev->wddm_context.pPatchLocationList;
  uint32_t patch_entries = dev->wddm_context.PatchLocationListSize;
  if constexpr (has_member_pPatchLocationList<Arg>::value) {
    if (args.pPatchLocationList) {
      patch_list = args.pPatchLocationList;
    }
  }
  if constexpr (has_member_PatchLocationListSize<Arg>::value) {
    if (args.PatchLocationListSize) {
      patch_entries = static_cast<uint32_t>(args.PatchLocationListSize);
    }
  }

  void* dma_priv = dev->wddm_context.pDmaBufferPrivateData;
  uint32_t dma_priv_bytes = dev->wddm_context.DmaBufferPrivateDataSize;
  if constexpr (has_member_pDmaBufferPrivateData<Arg>::value) {
    if (args.pDmaBufferPrivateData) {
      dma_priv = args.pDmaBufferPrivateData;
    }
  }
  if constexpr (has_member_DmaBufferPrivateDataSize<Arg>::value) {
    if (args.DmaBufferPrivateDataSize) {
      dma_priv_bytes = static_cast<uint32_t>(args.DmaBufferPrivateDataSize);
    }
  }
  if (dma_priv && dma_priv_bytes == 0) {
    dma_priv_bytes = expected_dma_priv_bytes;
  }

  // Validate the required submission contract. If GetCommandBufferCb cannot
  // provide it, return a failure so callers can fall back to AllocateCb.
  if (!cmd_ptr || cap == 0 || !alloc_list || alloc_entries == 0) {
    return E_OUTOFMEMORY;
  }
  if (!dma_priv || dma_priv_bytes < expected_dma_priv_bytes) {
    return E_OUTOFMEMORY;
  }

  dev->wddm_context.buffers_need_deallocate = false;
  dev->wddm_context.allocated_pDmaBuffer = nullptr;
  dev->wddm_context.allocated_pCommandBuffer = nullptr;
  dev->wddm_context.allocated_pAllocationList = nullptr;
  dev->wddm_context.allocated_pPatchLocationList = nullptr;
  dev->wddm_context.allocated_pDmaBufferPrivateData = nullptr;
  dev->wddm_context.allocated_DmaBufferPrivateDataSize = 0;

  dev->wddm_context.pDmaBuffer = static_cast<uint8_t*>(dma_ptr ? dma_ptr : cmd_ptr);
  dev->wddm_context.pCommandBuffer = static_cast<uint8_t*>(cmd_ptr);
  dev->wddm_context.CommandBufferSize = cap;
  dev->wddm_context.pAllocationList = alloc_list;
  dev->wddm_context.AllocationListSize = alloc_entries;
  dev->wddm_context.pPatchLocationList = patch_list;
  dev->wddm_context.PatchLocationListSize = patch_entries;

  // Treat DMA private data as an in/out pointer: GetCommandBufferCb may rotate it
  // alongside the command buffer.
  dev->wddm_context.pDmaBufferPrivateData = dma_priv;
  dev->wddm_context.DmaBufferPrivateDataSize = dma_priv_bytes;
  dev->wddm_context.dma_priv_from_allocate = false;

  dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  dev->wddm_context.reset_submission_buffers();
  dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                 dev->wddm_context.AllocationListSize,
                                 dev->adapter->max_allocation_list_slot_id);
  return S_OK;
}

bool wddm_ensure_recording_buffers(Device* dev, size_t bytes_needed) {
  if (!dev || !dev->adapter) {
    return false;
  }
  if (dev->wddm_context.hContext == 0) {
    return true;
  }

  const uint32_t expected_dma_priv_bytes = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  // All command packets are 4-byte aligned and must at minimum contain a packet
  // header. Ensure the DMA buffer is large enough for the stream header plus at
  // least one packet header (or the caller's requested packet size).
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  const size_t packet_bytes = std::max(bytes_needed, min_packet);
  const size_t min_buffer_bytes_sz = sizeof(aerogpu_cmd_stream_header) + packet_bytes;
  if (min_buffer_bytes_sz > std::numeric_limits<uint32_t>::max()) {
    return false;
  }
  const uint32_t min_buffer_bytes = static_cast<uint32_t>(min_buffer_bytes_sz);
  const bool have_persistent_buffers =
      dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= min_buffer_bytes &&
      dev->wddm_context.pAllocationList &&
      dev->wddm_context.AllocationListSize != 0 &&
      dev->wddm_context.pDmaBufferPrivateData &&
      dev->wddm_context.DmaBufferPrivateDataSize >= expected_dma_priv_bytes;

  if (have_persistent_buffers) {
    // Ensure the writer + allocation list tracker are bound to the active runtime
    // buffers (the runtime is allowed to rotate pointers after a submit).
    if (!dev->wddm_context.pDmaBuffer) {
      dev->wddm_context.pDmaBuffer = dev->wddm_context.pCommandBuffer;
    }
    if (dev->cmd.data() != dev->wddm_context.pCommandBuffer) {
      dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
    }

    if (dev->alloc_list_tracker.list_base() != reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList) ||
        dev->alloc_list_tracker.list_capacity() != dev->wddm_context.AllocationListSize) {
      dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                     dev->wddm_context.AllocationListSize,
                                     dev->adapter->max_allocation_list_slot_id);
    }
    return true;
  }

  // If AllocateCb handed us buffers but we never emitted anything, return them
  // before acquiring a new set.
  if (dev->wddm_context.buffers_need_deallocate && dev->cmd.empty()) {
    wddm_deallocate_active_buffers(dev);
  }

  const uint32_t request_bytes = min_buffer_bytes;

  // Prefer GetCommandBufferCb when available; fall back to AllocateCb for older
  // runtimes that require explicit per-submit allocation + DeallocateCb.
  bool tried_get_command_buffer = false;
  HRESULT get_command_buffer_hr = E_NOTIMPL;
  HRESULT hr = E_NOTIMPL;
  if constexpr (has_pfnGetCommandBufferCb<WddmDeviceCallbacks>::value) {
    if (dev->wddm_callbacks.pfnGetCommandBufferCb) {
      tried_get_command_buffer = true;
      get_command_buffer_hr = wddm_acquire_submit_buffers_get_command_buffer_impl(dev, dev->wddm_callbacks.pfnGetCommandBufferCb);
      hr = get_command_buffer_hr;
    }
  }
  // If GetCommandBufferCb succeeds but returns an undersized buffer for the
  // current packet, allow AllocateCb to satisfy the minimum size.
  if (SUCCEEDED(hr)) {
    const bool have_required =
        dev->wddm_context.pCommandBuffer &&
        dev->wddm_context.CommandBufferSize >= min_buffer_bytes &&
        dev->wddm_context.pAllocationList &&
        dev->wddm_context.AllocationListSize != 0 &&
        dev->wddm_context.pDmaBufferPrivateData &&
        dev->wddm_context.DmaBufferPrivateDataSize >= expected_dma_priv_bytes;
    if (!have_required) {
      if (tried_get_command_buffer) {
        static std::once_flag log_once;
        const void* cmd_ptr = dev->wddm_context.pCommandBuffer;
        const uint32_t cmd_bytes = dev->wddm_context.CommandBufferSize;
        const void* alloc_ptr = dev->wddm_context.pAllocationList;
        const uint32_t alloc_entries = dev->wddm_context.AllocationListSize;
        const void* dma_priv_ptr = dev->wddm_context.pDmaBufferPrivateData;
        const uint32_t dma_priv_bytes = dev->wddm_context.DmaBufferPrivateDataSize;
        std::call_once(log_once,
                       [cmd_ptr, cmd_bytes, min_buffer_bytes, alloc_ptr, alloc_entries, dma_priv_ptr, dma_priv_bytes, expected_dma_priv_bytes] {
          aerogpu::logf("aerogpu-d3d9: GetCommandBufferCb returned incomplete/undersized buffers; "
                        "falling back to AllocateCb (cmd=%p bytes=%u need=%u alloc=%p entries=%u dma_priv=%p bytes=%u need>=%u)\n",
                        cmd_ptr,
                        static_cast<unsigned>(cmd_bytes),
                        static_cast<unsigned>(min_buffer_bytes),
                        alloc_ptr,
                        static_cast<unsigned>(alloc_entries),
                        dma_priv_ptr,
                        static_cast<unsigned>(dma_priv_bytes),
                        static_cast<unsigned>(expected_dma_priv_bytes));
        });
      }
      hr = E_FAIL;
    }
  }
  if (FAILED(hr)) {
    if (tried_get_command_buffer && FAILED(get_command_buffer_hr)) {
      static std::once_flag log_once;
      const unsigned hr_code = static_cast<unsigned>(get_command_buffer_hr);
      std::call_once(log_once, [hr_code] {
        aerogpu::logf("aerogpu-d3d9: GetCommandBufferCb failed hr=0x%08x; falling back to AllocateCb\n", hr_code);
      });
    }
    HRESULT allocate_hr = E_NOTIMPL;
    if constexpr (has_pfnAllocateCb<WddmDeviceCallbacks>::value && has_pfnDeallocateCb<WddmDeviceCallbacks>::value) {
      if (dev->wddm_callbacks.pfnAllocateCb && dev->wddm_callbacks.pfnDeallocateCb) {
        allocate_hr = wddm_acquire_submit_buffers_allocate_impl(dev, dev->wddm_callbacks.pfnAllocateCb, request_bytes);
        hr = allocate_hr;
      }
    }
    if (FAILED(hr)) {
      static std::once_flag log_once;
      const unsigned get_hr_code = static_cast<unsigned>(get_command_buffer_hr);
      const unsigned alloc_hr_code = static_cast<unsigned>(allocate_hr);
      std::call_once(log_once, [get_hr_code, alloc_hr_code] {
        aerogpu::logf("aerogpu-d3d9: failed to acquire WDDM submit buffers (GetCommandBufferCb hr=0x%08x AllocateCb hr=0x%08x)\n",
                      get_hr_code,
                      alloc_hr_code);
      });
    }
  }
  if (FAILED(hr)) {
    return false;
  }

  // Re-check required buffers.
  const bool have_required =
      dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= min_buffer_bytes &&
      dev->wddm_context.pAllocationList &&
      dev->wddm_context.AllocationListSize != 0 &&
      dev->wddm_context.pDmaBufferPrivateData &&
      dev->wddm_context.DmaBufferPrivateDataSize >= expected_dma_priv_bytes;
  if (!have_required && dev->wddm_context.buffers_need_deallocate) {
    // Prevent leaking AllocateCb-owned buffers if the runtime did not return the
    // full submission contract (e.g. missing DMA private data).
    wddm_deallocate_active_buffers(dev);
  }
  return have_required;
}
#endif // _WIN32

static void resolve_pending_event_queries(Device* dev, uint64_t fence_value) {
  if (!dev) {
    return;
  }
  if (dev->pending_event_queries.empty()) {
    return;
  }

  for (Query* q : dev->pending_event_queries) {
    if (!q) {
      continue;
    }
    // Some call sites may pre-populate the fence value (e.g. when Issue(END)
    // submits work but we intentionally defer making the query "ready" until a
    // later boundary). Only stamp when still unset.
    if (q->fence_value.load(std::memory_order_relaxed) == 0) {
      q->fence_value.store(fence_value, std::memory_order_release);
    }
    q->submitted.store(true, std::memory_order_release);
  }
  dev->pending_event_queries.clear();
}

uint64_t submit(Device* dev, bool is_present) {
  if (!dev) {
    return 0;
  }

  Adapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  if (dev->cmd.empty()) {
    // Even if there's nothing to submit, callers may use submit() as a "split"
    // point when the per-submit allocation list is full. Reset submission-local
    // tracking state so subsequent commands start with a fresh allocation list
    // without issuing an empty DMA buffer to the kernel.
#if defined(_WIN32)
    if (dev->wddm_context.buffers_need_deallocate) {
      wddm_deallocate_active_buffers(dev);
    }
#endif
    const uint64_t fence = dev->last_submission_fence;
    resolve_pending_event_queries(dev, fence);
    dev->cmd.rewind();
    dev->alloc_list_tracker.reset();
    dev->wddm_context.reset_submission_buffers();
    return fence;
  }

  dev->cmd.finalize();
  const uint64_t cmd_bytes = static_cast<uint64_t>(dev->cmd.size());

  bool submitted_to_kmd = false;
  uint64_t submission_fence = 0;
  bool did_submit = false;
#if defined(_WIN32)
  // WDDM submission path: hand the runtime-provided DMA/alloc list buffers back
  // to dxgkrnl via the device callbacks captured at CreateDevice time.
  //
  // The patch-location list is intentionally kept empty; guest-backed memory is
  // referenced via stable `alloc_id` values and resolved by the KMD's per-submit
  // allocation table.
  if (dev->wddm_context.hContext != 0 && dev->wddm_context.pCommandBuffer && dev->wddm_context.CommandBufferSize) {
    if (cmd_bytes <= dev->wddm_context.CommandBufferSize) {
      // CmdStreamWriter can be span-backed and write directly into the runtime
      // DMA buffer. Avoid memcpy on identical ranges (overlap is UB for memcpy).
      if (dev->cmd.data() != dev->wddm_context.pCommandBuffer) {
        std::memcpy(dev->wddm_context.pCommandBuffer, dev->cmd.data(), static_cast<size_t>(cmd_bytes));
      }
      dev->wddm_context.command_buffer_bytes_used = static_cast<uint32_t>(cmd_bytes);
      dev->wddm_context.allocation_list_entries_used = dev->alloc_list_tracker.list_len();
      dev->wddm_context.patch_location_entries_used = 0;
      const uint32_t allocs_used = dev->wddm_context.allocation_list_entries_used;
      const bool needs_allocation_table = (allocs_used != 0);

      // Keep the DMA-private-data pointer/size used for this submission so we can
      // validate the KMD-filled AEROGPU_DMA_PRIV even if the runtime rotates
      // pointers in the callback out-params.
      void* submit_priv_ptr = dev->wddm_context.pDmaBufferPrivateData;
      const uint32_t submit_priv_size = dev->wddm_context.DmaBufferPrivateDataSize;

      HRESULT submit_hr = E_NOTIMPL;
      enum class SubmitCbKind { kNone, kSubmitCommandCb, kRenderCb, kPresentCb };
      SubmitCbKind submit_kind = SubmitCbKind::kNone;
      const uint32_t cmd_len = static_cast<uint32_t>(cmd_bytes);
      // Win7 D3D9 runtimes expose several possible submission callbacks. Prefer
      // Render/Present so dxgkrnl routes through DxgkDdiRender/DxgkDdiPresent and
      // the KMD can stamp AEROGPU_DMA_PRIV + per-submit allocation-table metadata
      // before DxgkDdiSubmitCommand.
      if (is_present) {
        if constexpr (has_pfnPresentCb<WddmDeviceCallbacks>::value) {
          if (dev->wddm_callbacks.pfnPresentCb) {
            submission_fence = 0;
            submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnPresentCb, cmd_len, /*is_present=*/true,
                                               &submission_fence);
            if (SUCCEEDED(submit_hr)) {
              submit_kind = SubmitCbKind::kPresentCb;
            }
          }
        }

        if (!SUCCEEDED(submit_hr)) {
          if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
            if (dev->wddm_callbacks.pfnRenderCb) {
              using RenderCbT = decltype(dev->wddm_callbacks.pfnRenderCb);
              if constexpr (submit_callback_can_signal_present<RenderCbT>()) {
                // Some callback-table variants expose only RenderCb for both render
                // and present submissions (with an explicit Present flag in the
                // args). Prefer that path over SubmitCommandCb so the KMD can
                // attach a MetaHandle in DxgkDdiPresent.
                submission_fence = 0;
                submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnRenderCb, cmd_len, /*is_present=*/true,
                                                   &submission_fence);
                if (SUCCEEDED(submit_hr)) {
                  submit_kind = SubmitCbKind::kRenderCb;
                }
              }
            }
          }
        }

        if (!SUCCEEDED(submit_hr)) {
          // Next preference: SubmitCommandCb. This can bypass DxgkDdiPresent, so
          // the KMD may not have stamped MetaHandle, but it can still build the
          // allocation-table metadata on-demand from the submit args.
          if constexpr (has_pfnSubmitCommandCb<WddmDeviceCallbacks>::value) {
            if (dev->wddm_callbacks.pfnSubmitCommandCb) {
              submission_fence = 0;
              submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnSubmitCommandCb, cmd_len, /*is_present=*/true,
                                                 &submission_fence);
              if (SUCCEEDED(submit_hr)) {
                submit_kind = SubmitCbKind::kSubmitCommandCb;
              }
            }
          }
        }

        // Last resort: RenderCb even if it cannot explicitly signal "present".
        // This may misclassify the submission, but is still preferable to
        // failing outright in callback-table variants that lack PresentCb and
        // SubmitCommandCb.
        if (!SUCCEEDED(submit_hr)) {
          if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
            if (dev->wddm_callbacks.pfnRenderCb) {
              using RenderCbT = decltype(dev->wddm_callbacks.pfnRenderCb);
              if constexpr (!submit_callback_can_signal_present<RenderCbT>()) {
                submission_fence = 0;
                submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnRenderCb, cmd_len, /*is_present=*/true,
                                                   &submission_fence);
                if (SUCCEEDED(submit_hr)) {
                  submit_kind = SubmitCbKind::kRenderCb;
                }
              }
            }
          }
        }
      } else {
        if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
          if (dev->wddm_callbacks.pfnRenderCb) {
            submission_fence = 0;
            submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnRenderCb, cmd_len, /*is_present=*/false,
                                               &submission_fence);
            if (SUCCEEDED(submit_hr)) {
              submit_kind = SubmitCbKind::kRenderCb;
            }
          }
        }

        if (!SUCCEEDED(submit_hr)) {
          // Fallback: SubmitCommandCb (bypasses DxgkDdiRender). This is less
          // desirable than RenderCb, but still allows the KMD to build per-submit
          // allocation metadata on-demand.
          if constexpr (has_pfnSubmitCommandCb<WddmDeviceCallbacks>::value) {
            if (dev->wddm_callbacks.pfnSubmitCommandCb) {
              submission_fence = 0;
              submit_hr = invoke_submit_callback(dev, dev->wddm_callbacks.pfnSubmitCommandCb, cmd_len, /*is_present=*/false,
                                                 &submission_fence);
              if (SUCCEEDED(submit_hr)) {
                submit_kind = SubmitCbKind::kSubmitCommandCb;
              }
            }
          }
        }
      }

      if (SUCCEEDED(submit_hr)) {
        if (needs_allocation_table && submit_kind != SubmitCbKind::kSubmitCommandCb && submit_priv_ptr &&
            submit_priv_size >= AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
          AEROGPU_DMA_PRIV priv{};
          std::memcpy(&priv, submit_priv_ptr, sizeof(priv));
          if (priv.MetaHandle == 0) {
            static std::atomic<uint32_t> g_missing_meta_logs{0};
            const uint32_t n = g_missing_meta_logs.fetch_add(1, std::memory_order_relaxed);
            if ((n < 8) || ((n & 1023u) == 0)) {
              logf("aerogpu-d3d9: submit missing MetaHandle (allocs=%u present=%u type=%u)\n",
                   static_cast<unsigned>(allocs_used),
                   is_present ? 1u : 0u,
                   static_cast<unsigned>(priv.Type));
            }
          }
        }
        submitted_to_kmd = true;
        did_submit = true;
        if (dev->wddm_context.buffers_need_deallocate) {
          // AllocateCb/DeallocateCb model: return the per-submit buffers after
          // the submission callback completes.
          wddm_deallocate_active_buffers(dev);
        } else {
          dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                         dev->wddm_context.AllocationListSize,
                                         adapter->max_allocation_list_slot_id);
        }
      } else {
        if (dev->wddm_context.buffers_need_deallocate) {
          // The runtime can still require DeallocateCb even if the submit call
          // fails (best-effort; prevents leaking callback-owned buffers).
          wddm_deallocate_active_buffers(dev);
        }
        logf("aerogpu-d3d9: submit callbacks failed hr=0x%08x\n", static_cast<unsigned>(submit_hr));
      }
    } else {
      logf("aerogpu-d3d9: submit command buffer too large (cmd=%llu cap=%u)\n",
           static_cast<unsigned long long>(cmd_bytes),
           static_cast<unsigned>(dev->wddm_context.CommandBufferSize));
    }
  }
#endif

  uint64_t fence = 0;
  // Fence value associated with this specific submission (as returned by the
  // runtime callback, or (rarely) the KMD query fallback). Keep this separate
  // from adapter-wide tracking so concurrent submissions cannot cause us to
  // return a "too-new" fence.
  uint64_t per_submission_fence = 0;
  bool updated = false;
#if defined(_WIN32)
  if (submitted_to_kmd) {
    // Critical: capture the exact per-submission fence returned by the runtime
    // callback for *this* submission (SubmissionFenceId/NewFenceValue).
    fence = submission_fence;

    // Some WDK header vintages do not expose the callback fence outputs. In
    // that case, fall back to querying the KMD's fence counters via DxgkDdiEscape
    // (D3DKMTEscape) so we still return a real fence value and never "fake
    // complete" fences in-process.
    uint64_t kmd_submitted = 0;
    uint64_t kmd_completed = 0;
    bool kmd_ok = false;
    if (fence == 0 && adapter->kmd_query_available.load(std::memory_order_acquire)) {
      kmd_ok = adapter->kmd_query.QueryFence(&kmd_submitted, &kmd_completed);
      if (!kmd_ok) {
        adapter->kmd_query_available.store(false, std::memory_order_release);
      } else {
        fence = kmd_submitted;
      }
    }

    per_submission_fence = fence;

    if (kmd_ok) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      const uint64_t prev_completed = adapter->completed_fence;
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, kmd_submitted);
      adapter->completed_fence = std::max(adapter->completed_fence, kmd_completed);
      adapter->next_fence = std::max(adapter->next_fence, adapter->last_submitted_fence + 1);
      adapter->last_kmd_fence_query_ms = monotonic_ms();
      updated = (adapter->last_submitted_fence != prev_submitted) || (adapter->completed_fence != prev_completed);
    }

    if (per_submission_fence) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, per_submission_fence);
      adapter->next_fence = std::max(adapter->next_fence, adapter->last_submitted_fence + 1);
      updated = updated || (adapter->last_submitted_fence != prev_submitted);
    }
  }
#endif

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
  if (fence == 0) {
    {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      if (adapter->next_fence <= adapter->last_submitted_fence) {
        adapter->next_fence = adapter->last_submitted_fence + 1;
      }

      const uint64_t stub_fence = adapter->next_fence++;
      const uint64_t prev_submitted = adapter->last_submitted_fence;
      const uint64_t prev_completed = adapter->completed_fence;
      // Never allow the cached fence values to go backwards: they may be advanced
      // by the KMD query path (or, in a real WDDM build, by runtime-provided fence
      // callbacks).
      adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, stub_fence);
      adapter->completed_fence = std::max(adapter->completed_fence, stub_fence);
      fence = stub_fence;
      updated = updated || (adapter->last_submitted_fence != prev_submitted) || (adapter->completed_fence != prev_completed);
    }
    did_submit = true;
  }
  per_submission_fence = fence;
#endif

  if (per_submission_fence == 0) {
    per_submission_fence = fence;
  }

  if (updated) {
    adapter->fence_cv.notify_all();
  }

  if (did_submit) {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (is_present) {
      adapter->present_submit_count++;
    } else {
      adapter->render_submit_count++;
    }
  }

  if (submit_log_enabled()) {
    logf("aerogpu-d3d9: submit cmd_bytes=%llu fence=%llu present=%u\n",
         static_cast<unsigned long long>(cmd_bytes),
         static_cast<unsigned long long>(per_submission_fence),
         is_present ? 1u : 0u);
  }

  dev->last_submission_fence = per_submission_fence;
  resolve_pending_event_queries(dev, per_submission_fence);
  dev->cmd.rewind();
  dev->alloc_list_tracker.reset();
  dev->wddm_context.reset_submission_buffers();
  return per_submission_fence;
}

HRESULT flush_locked(Device* dev) {
  // Flushing an empty command buffer should be a no-op. This matters for
  // D3DGETDATA_FLUSH polling loops (e.g. DWM EVENT queries): if we submit an
  // empty buffer every poll we can flood the KMD/emulator with redundant
  // submissions and increase CPU usage.
  if (!dev) {
    return S_OK;
  }
  if (dev->cmd.empty()) {
    // If we have pending EVENT queries waiting for a submission fence, allow
    // this flush call to "resolve" them without forcing an empty DMA buffer to
    // the kernel. `submit()`'s empty-path stamps queries with
    // `last_submission_fence`.
    if (!dev->pending_event_queries.empty()) {
      (void)submit(dev);
    }
    return S_OK;
  }
  // If we cannot fit an explicit FLUSH marker into the remaining space, just
  // submit the current buffer; the submission boundary is already a flush point.
  const size_t flush_bytes = align_up(sizeof(aerogpu_cmd_flush), 4);
  if (dev->cmd.bytes_remaining() < flush_bytes) {
    submit(dev);
    return S_OK;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_flush>(dev, AEROGPU_CMD_FLUSH);
  if (cmd) {
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  submit(dev);
  return S_OK;
}

HRESULT copy_surface_bytes(Device* dev, const Resource* src, Resource* dst) {
  if (!dev || !src || !dst) {
    return E_INVALIDARG;
  }
  if (src->width != dst->width || src->height != dst->height) {
    return E_INVALIDARG;
  }
  if (src->format != dst->format) {
    return E_INVALIDARG;
  }

  const bool bc = is_block_compressed_format(src->format);
  uint32_t row_copy_bytes = 0;
  uint32_t rows = 0;
  if (bc) {
    // For BC formats the resource layout is in 4x4 blocks. `row_pitch` already
    // represents the bytes-per-row of blocks; copy whole rows.
    row_copy_bytes = src->row_pitch;
    rows = std::max(1u, (src->height + 3u) / 4u);
  } else {
    const uint32_t bpp = bytes_per_pixel(src->format);
    row_copy_bytes = src->width * bpp;
    rows = src->height;
  }
  if (src->row_pitch < row_copy_bytes || dst->row_pitch < row_copy_bytes) {
    return E_FAIL;
  }

  struct Map {
    void* ptr = nullptr;
    bool wddm_locked = false;
  };

  Map src_map{};
  Map dst_map{};
  const uint8_t* src_base = nullptr;
  uint8_t* dst_base = nullptr;

  const uint64_t bytes_needed = static_cast<uint64_t>(src->row_pitch) * rows;
  if (bytes_needed == 0 || bytes_needed > src->size_bytes || bytes_needed > dst->size_bytes) {
    return E_FAIL;
  }

  bool use_src_storage = src->storage.size() >= bytes_needed;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  // Guest-backed resources may still allocate a CPU shadow buffer (e.g. shared
  // resources opened via OpenResource). On real WDDM builds the authoritative
  // bytes live in the WDDM allocation, so prefer mapping it directly.
  if (src->backing_alloc_id != 0) {
    use_src_storage = false;
  }
#endif
  if (use_src_storage) {
    src_base = src->storage.data();
  } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (src->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      const HRESULT hr = wddm_lock_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              src->wddm_hAllocation,
                                              0,
                                              bytes_needed,
                                              kD3DLOCK_READONLY,
                                              &src_map.ptr,
                                              dev->wddm_context.hContext);
      if (FAILED(hr) || !src_map.ptr) {
        return FAILED(hr) ? hr : E_FAIL;
      }
      src_map.wddm_locked = true;
      src_base = static_cast<const uint8_t*>(src_map.ptr);
    } else
#endif
    {
      return E_FAIL;
    }
  }

  bool use_dst_storage = dst->storage.size() >= bytes_needed;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id != 0) {
    use_dst_storage = false;
  }
#endif
  if (use_dst_storage) {
    dst_base = dst->storage.data();
  } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (dst->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      const HRESULT hr = wddm_lock_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              dst->wddm_hAllocation,
                                              0,
                                              bytes_needed,
                                              &dst_map.ptr,
                                              dev->wddm_context.hContext);
      if (FAILED(hr) || !dst_map.ptr) {
        if (src_map.wddm_locked) {
          (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                       dev->wddm_device,
                                       src->wddm_hAllocation,
                                       dev->wddm_context.hContext);
        }
        return FAILED(hr) ? hr : E_FAIL;
      }
      dst_map.wddm_locked = true;
      dst_base = static_cast<uint8_t*>(dst_map.ptr);
    } else
#endif
    {
      if (src_map.wddm_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
      return E_FAIL;
    }
  }
  for (uint32_t y = 0; y < rows; y++) {
    std::memcpy(dst_base + static_cast<size_t>(y) * dst->row_pitch,
                src_base + static_cast<size_t>(y) * src->row_pitch,
                row_copy_bytes);
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst_map.wddm_locked) {
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 dst->wddm_hAllocation,
                                 dev->wddm_context.hContext);
  }
  if (src_map.wddm_locked) {
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 src->wddm_hAllocation,
                                 dev->wddm_context.hContext);
  }
#endif
  return S_OK;
}

// -----------------------------------------------------------------------------
// Adapter DDIs
// -----------------------------------------------------------------------------

uint64_t luid_to_u64(const LUID& luid) {
  const uint64_t hi = static_cast<uint64_t>(static_cast<uint32_t>(luid.HighPart));
  const uint64_t lo = static_cast<uint64_t>(luid.LowPart);
  return (hi << 32) | lo;
}

LUID default_luid() {
  LUID luid{};
  luid.LowPart = 0;
  luid.HighPart = 0;
  return luid;
}

std::mutex g_adapter_cache_mutex;
std::unordered_map<uint64_t, Adapter*> g_adapter_cache;

Adapter* acquire_adapter(const LUID& luid,
                         UINT interface_version,
                         UINT umd_version,
                         D3DDDI_ADAPTERCALLBACKS* callbacks,
                         D3DDDI_ADAPTERCALLBACKS2* callbacks2) {
  std::lock_guard<std::mutex> lock(g_adapter_cache_mutex);

  const uint64_t key = luid_to_u64(luid);
  auto it = g_adapter_cache.find(key);
  if (it != g_adapter_cache.end()) {
    Adapter* adapter = it->second;
    adapter->open_count.fetch_add(1);
    adapter->interface_version = interface_version;
    adapter->umd_version = umd_version;
    adapter->adapter_callbacks = callbacks;
    adapter->adapter_callbacks2 = callbacks2;
    adapter->share_token_allocator.set_adapter_luid(luid);
    if (callbacks) {
      adapter->adapter_callbacks_copy = *callbacks;
      adapter->adapter_callbacks_valid = true;
    } else {
      adapter->adapter_callbacks_copy = {};
      adapter->adapter_callbacks_valid = false;
    }
    if (callbacks2) {
      adapter->adapter_callbacks2_copy = *callbacks2;
      adapter->adapter_callbacks2_valid = true;
    } else {
      adapter->adapter_callbacks2_copy = {};
      adapter->adapter_callbacks2_valid = false;
    }
    return adapter;
  }

  auto* adapter = new (std::nothrow) Adapter();
  if (!adapter) {
    return nullptr;
  }
  adapter->luid = luid;
  adapter->share_token_allocator.set_adapter_luid(luid);
  adapter->open_count.store(1);
  adapter->interface_version = interface_version;
  adapter->umd_version = umd_version;
  adapter->adapter_callbacks = callbacks;
  adapter->adapter_callbacks2 = callbacks2;
  if (callbacks) {
    adapter->adapter_callbacks_copy = *callbacks;
    adapter->adapter_callbacks_valid = true;
  } else {
    adapter->adapter_callbacks_copy = {};
    adapter->adapter_callbacks_valid = false;
  }
  if (callbacks2) {
    adapter->adapter_callbacks2_copy = *callbacks2;
    adapter->adapter_callbacks2_valid = true;
  } else {
    adapter->adapter_callbacks2_copy = {};
    adapter->adapter_callbacks2_valid = false;
  }

#if defined(_WIN32)
  // Initialize a best-effort primary display mode so GetDisplayModeEx returns a
  // stable value even when the runtime opens the adapter via the LUID path (as
  // DWM commonly does).
  const int w = GetSystemMetrics(SM_CXSCREEN);
  const int h = GetSystemMetrics(SM_CYSCREEN);
  if (w > 0) {
    adapter->primary_width = static_cast<uint32_t>(w);
  }
  if (h > 0) {
    adapter->primary_height = static_cast<uint32_t>(h);
  }

  DEVMODEA dm{};
  dm.dmSize = sizeof(dm);
  if (EnumDisplaySettingsA(nullptr, ENUM_CURRENT_SETTINGS, &dm)) {
    if (dm.dmPelsWidth > 0) {
      adapter->primary_width = static_cast<uint32_t>(dm.dmPelsWidth);
    }
    if (dm.dmPelsHeight > 0) {
      adapter->primary_height = static_cast<uint32_t>(dm.dmPelsHeight);
    }
    if (dm.dmDisplayFrequency > 0) {
      adapter->primary_refresh_hz = static_cast<uint32_t>(dm.dmDisplayFrequency);
    }
  }
#endif

  g_adapter_cache.emplace(key, adapter);
  return adapter;
}

void release_adapter(Adapter* adapter) {
  if (!adapter) {
    return;
  }

  std::lock_guard<std::mutex> lock(g_adapter_cache_mutex);
  const uint32_t remaining = adapter->open_count.fetch_sub(1) - 1;
  if (remaining != 0) {
    return;
  }

  g_adapter_cache.erase(luid_to_u64(adapter->luid));

#if defined(_WIN32)
  // Release cross-process alloc_id token allocator state.
  {
    std::lock_guard<std::mutex> share_lock(adapter->share_token_mutex);
    if (adapter->share_token_view) {
      UnmapViewOfFile(adapter->share_token_view);
      adapter->share_token_view = nullptr;
    }
    if (adapter->share_token_mapping) {
      CloseHandle(adapter->share_token_mapping);
      adapter->share_token_mapping = nullptr;
    }
  }
#endif
  delete adapter;
}

HRESULT AEROGPU_D3D9_CALL adapter_close(D3DDDI_HADAPTER hAdapter) {
  D3d9TraceCall trace(D3d9TraceFunc::AdapterClose, d3d9_trace_arg_ptr(hAdapter.pDrvPrivate), 0, 0, 0);
  release_adapter(as_adapter(hAdapter));
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL adapter_get_caps(
    D3DDDI_HADAPTER hAdapter,
    const D3D9DDIARG_GETCAPS* pGetCaps) {
  D3d9TraceCall trace(D3d9TraceFunc::AdapterGetCaps,
                      d3d9_trace_arg_ptr(hAdapter.pDrvPrivate),
                      pGetCaps ? static_cast<uint64_t>(pGetCaps->Type) : 0,
                      pGetCaps ? static_cast<uint64_t>(pGetCaps->DataSize) : 0,
                      pGetCaps ? d3d9_trace_arg_ptr(pGetCaps->pData) : 0);
  auto* adapter = as_adapter(hAdapter);
  if (!adapter || !pGetCaps) {
    return trace.ret(E_INVALIDARG);
  }
  return trace.ret(aerogpu::get_caps(adapter, pGetCaps));
}

HRESULT AEROGPU_D3D9_CALL adapter_query_adapter_info(
    D3DDDI_HADAPTER hAdapter,
    const D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo) {
  uint64_t data_ptr = 0;
  uint32_t size = 0;
  if (pQueryAdapterInfo) {
    data_ptr = d3d9_trace_arg_ptr(pQueryAdapterInfo->pPrivateDriverData);
    size = pQueryAdapterInfo->PrivateDriverDataSize;
  }

  D3d9TraceCall trace(D3d9TraceFunc::AdapterQueryAdapterInfo,
                      d3d9_trace_arg_ptr(hAdapter.pDrvPrivate),
                      pQueryAdapterInfo ? static_cast<uint64_t>(pQueryAdapterInfo->Type) : 0,
                      static_cast<uint64_t>(size),
                      data_ptr);

  auto* adapter = as_adapter(hAdapter);
  if (!adapter || !pQueryAdapterInfo) {
    return trace.ret(E_INVALIDARG);
  }
  void* data = pQueryAdapterInfo->pPrivateDriverData;
  size = pQueryAdapterInfo->PrivateDriverDataSize;

  return trace.ret(aerogpu::query_adapter_info(adapter, pQueryAdapterInfo));
}

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    D3D9DDI_DEVICEFUNCS* pDeviceFuncs);

// -----------------------------------------------------------------------------
// Device DDIs
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL device_destroy(D3DDDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroy, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(S_OK);
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->recording_state_block) {
      delete dev->recording_state_block;
      dev->recording_state_block = nullptr;
    }
    // Ensure we are not holding on to a DMA buffer that references allocations we
    // are about to destroy (e.g. swapchain backbuffers created but never
    // submitted). This matches the per-resource destroy path, but we do it once
    // for the whole device teardown.
    (void)submit(dev);

    // Tear down internal objects that the runtime does not know about.
    if (dev->fvf_vertex_decl) {
      (void)emit_destroy_input_layout_locked(dev, dev->fvf_vertex_decl->handle);
      delete dev->fvf_vertex_decl;
      dev->fvf_vertex_decl = nullptr;
    }
    if (dev->fixedfunc_vs) {
      (void)emit_destroy_shader_locked(dev, dev->fixedfunc_vs->handle);
      delete dev->fixedfunc_vs;
      dev->fixedfunc_vs = nullptr;
    }
    if (dev->fixedfunc_ps) {
      (void)emit_destroy_shader_locked(dev, dev->fixedfunc_ps->handle);
      delete dev->fixedfunc_ps;
      dev->fixedfunc_ps = nullptr;
    }
    if (dev->up_vertex_buffer) {
      (void)emit_destroy_resource_locked(dev, dev->up_vertex_buffer->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      if (dev->up_vertex_buffer->wddm_hAllocation != 0 && dev->wddm_device != 0) {
        (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                      dev->wddm_device,
                                      dev->up_vertex_buffer->wddm_hAllocation,
                                      dev->wddm_context.hContext);
        dev->up_vertex_buffer->wddm_hAllocation = 0;
      }
#endif
      delete dev->up_vertex_buffer;
      dev->up_vertex_buffer = nullptr;
    }
    if (dev->up_index_buffer) {
      (void)emit_destroy_resource_locked(dev, dev->up_index_buffer->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      if (dev->up_index_buffer->wddm_hAllocation != 0 && dev->wddm_device != 0) {
        (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                      dev->wddm_device,
                                      dev->up_index_buffer->wddm_hAllocation,
                                      dev->wddm_context.hContext);
        dev->up_index_buffer->wddm_hAllocation = 0;
      }
#endif
      delete dev->up_index_buffer;
      dev->up_index_buffer = nullptr;
    }
    destroy_blit_objects_locked(dev);
    for (SwapChain* sc : dev->swapchains) {
      if (!sc) {
        continue;
      }
      for (Resource* bb : sc->backbuffers) {
        if (!bb) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, bb->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (bb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
          (void)wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, bb->wddm_hAllocation, dev->wddm_context.hContext);
          bb->wddm_hAllocation = 0;
        }
#endif
        delete bb;
      }
      delete sc;
    }
    dev->swapchains.clear();
    dev->current_swapchain = nullptr;
    flush_locked(dev);
  }

#if defined(_WIN32)
  // Ensure we return any AllocateCb-owned per-submit buffers before destroying
  // the context/device. Some runtimes allocate these even if we never end up
  // submitting (e.g. device teardown during initialization failures).
  if (dev->wddm_context.buffers_need_deallocate) {
    wddm_deallocate_active_buffers(dev);
  }
  dev->wddm_context.destroy(dev->wddm_callbacks);
  wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
  dev->wddm_device = 0;
#endif
  delete dev;
  return trace.ret(S_OK);
}

static void consume_wddm_alloc_priv(Resource* res,
                                   const void* priv_data,
                                   uint32_t priv_data_size,
                                   bool is_shared_resource) {
  if (!res || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return;
  }

  aerogpu_wddm_alloc_priv priv{};
  std::memcpy(&priv, priv_data, sizeof(priv));

  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC ||
      (priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION && priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION_2)) {
    return;
  }

  res->backing_alloc_id = priv.alloc_id;
  res->share_token = priv.share_token;
  if (res->size_bytes == 0 && priv.size_bytes != 0 && priv.size_bytes <= 0xFFFFFFFFull) {
    res->size_bytes = static_cast<uint32_t>(priv.size_bytes);
  }
  if (priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) {
    res->is_shared = true;
  }
  (void)is_shared_resource;
}

static aerogpu_wddm_u64 encode_wddm_alloc_priv_desc(uint32_t format, uint32_t width, uint32_t height) {
  if (format == 0 || width == 0 || height == 0) {
    return 0;
  }
  width = std::min<uint32_t>(width, static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH));
  height = std::min<uint32_t>(height, static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT));
  if (width == 0 || height == 0) {
    return 0;
  }
  return AEROGPU_WDDM_ALLOC_PRIV_DESC_PACK(format, width, height);
}

static bool decode_wddm_alloc_priv_desc(aerogpu_wddm_u64 desc, uint32_t* format_out, uint32_t* width_out, uint32_t* height_out) {
  if (!format_out || !width_out || !height_out) {
    return false;
  }
  if (!AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(desc)) {
    return false;
  }
  const uint32_t format = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_FORMAT(desc));
  const uint32_t width = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_WIDTH(desc));
  const uint32_t height = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_HEIGHT(desc));
  if (format == 0 || width == 0 || height == 0) {
    return false;
  }
  *format_out = format;
  *width_out = width;
  *height_out = height;
  return true;
}

HRESULT create_backbuffer_locked(Device* dev, Resource* res, uint32_t format, uint32_t width, uint32_t height) {
  if (!dev || !dev->adapter || !res) {
    return E_INVALIDARG;
  }

  const uint32_t bpp = bytes_per_pixel(format);
  width = std::max(1u, width);
  height = std::max(1u, height);

  res->handle = allocate_global_handle(dev->adapter);
  res->kind = ResourceKind::Surface;
  res->type = 0;
  res->format = format;
  res->width = width;
  res->height = height;
  res->depth = 1;
  res->mip_levels = 1;
  res->usage = kD3DUsageRenderTarget;
  res->pool = kD3DPOOL_DEFAULT;
  res->backing_alloc_id = 0;
  res->backing_offset_bytes = 0;
  res->share_token = 0;
  res->is_shared = false;
  res->is_shared_alias = false;
  res->wddm_hAllocation = 0;
  res->row_pitch = width * bpp;
  res->slice_pitch = res->row_pitch * height;
  res->locked = false;
  res->locked_offset = 0;
  res->locked_size = 0;
  res->locked_flags = 0;
  res->locked_ptr = nullptr;

  uint64_t total = static_cast<uint64_t>(res->slice_pitch);
  if (total > 0x7FFFFFFFu) {
    return E_OUTOFMEMORY;
  }
  res->size_bytes = static_cast<uint32_t>(total);

  bool has_wddm_allocation = false;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dev->wddm_device != 0) {
    const uint32_t alloc_id = allocate_umd_alloc_id(dev->adapter);
    if (alloc_id == 0) {
      return E_OUTOFMEMORY;
    }
    res->backing_alloc_id = alloc_id;

    aerogpu_wddm_alloc_priv priv{};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv.alloc_id = alloc_id;
    priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE;
    priv.share_token = 0;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv.reserved0 = encode_wddm_alloc_priv_desc(res->format, res->width, res->height);

    const HRESULT hr = wddm_create_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              res->size_bytes,
                                              &priv,
                                              sizeof(priv),
                                              &res->wddm_hAllocation,
                                              dev->wddm_context.hContext);
    if (FAILED(hr) || res->wddm_hAllocation == 0) {
      return FAILED(hr) ? hr : E_FAIL;
    }

    has_wddm_allocation = true;
  }
#endif

  if (!has_wddm_allocation) {
    // Fallback (non-WDDM builds): allocate CPU shadow storage and treat the host
    // object as "host allocated" (backing_alloc_id remains 0).
    try {
      res->storage.resize(res->size_bytes);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
    res->wddm_hAllocation = 0;
    res->backing_alloc_id = 0;
  }

  if (!emit_create_resource_locked(dev, res)) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      (void)wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, res->wddm_hAllocation, dev->wddm_context.hContext);
      res->wddm_hAllocation = 0;
    }
#endif
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_create_resource(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_CREATERESOURCE* pCreateResource) {
  const uint64_t type_format =
      pCreateResource
          ? d3d9_trace_pack_u32_u32(d3d9_resource_type(*pCreateResource), d3d9_resource_format(*pCreateResource))
          : 0;
  const uint64_t wh =
      pCreateResource
          ? d3d9_trace_pack_u32_u32(d3d9_resource_width(*pCreateResource), d3d9_resource_height(*pCreateResource))
          : 0;
  const uint64_t usage_pool =
      pCreateResource ? d3d9_trace_pack_u32_u32(d3d9_resource_usage(*pCreateResource), d3d9_resource_pool(*pCreateResource)) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceCreateResource, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), type_format, wh, usage_pool);
  if (!hDevice.pDrvPrivate || !pCreateResource) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool wants_shared = (pCreateResource->pSharedHandle != nullptr);
  const bool open_existing_shared = wants_shared && (*pCreateResource->pSharedHandle != nullptr);
  const uint32_t requested_mip_levels = d3d9_resource_mip_levels(*pCreateResource);
  const uint32_t mip_levels = std::max(1u, requested_mip_levels);
  if (wants_shared && requested_mip_levels != 1) {
    // MVP: shared surfaces must be single-allocation (no mip chains/arrays).
    return trace.ret(D3DERR_INVALIDCALL);
  }

  auto res = std::make_unique<Resource>();
  res->handle = allocate_global_handle(dev->adapter);
  res->type = d3d9_resource_type(*pCreateResource);
  res->format = d3d9_resource_format(*pCreateResource);
  res->width = d3d9_resource_width(*pCreateResource);
  res->height = d3d9_resource_height(*pCreateResource);
  res->depth = std::max(1u, d3d9_resource_depth(*pCreateResource));
  res->mip_levels = mip_levels;
  res->usage = d3d9_resource_usage(*pCreateResource);
  res->pool = d3d9_resource_pool(*pCreateResource);
  res->wddm_hAllocation = get_wddm_allocation_from_create_resource(pCreateResource);
  res->is_shared = wants_shared;
  res->is_shared_alias = open_existing_shared;

  /*
   * Only treat KMD allocation private data as an INPUT when opening an existing
   * shared resource.
   *
   * For normal resource creation, `pPrivateDriverData` is an output buffer
   * owned by the runtime; consuming it before we populate it risks picking up
   * stale bytes from a previous call (e.g. reusing an old alloc_id/share_token),
   * which can lead to cross-process collisions and host-side shared-surface
   * table corruption.
   */
  if (open_existing_shared) {
    consume_wddm_alloc_priv(res.get(),
                            pCreateResource->pPrivateDriverData,
                            pCreateResource->PrivateDriverDataSize,
                            /*is_shared_resource=*/true);
  }

  const uint32_t create_size_bytes = d3d9_resource_size(*pCreateResource);
  // Heuristic: if size is provided, treat as buffer; otherwise treat as a 2D image.
  if (create_size_bytes) {
    res->kind = ResourceKind::Buffer;
    const uint64_t requested = static_cast<uint64_t>(create_size_bytes);
    const uint64_t aligned = (requested + 3ull) & ~3ull;
    if (aligned == 0 || aligned > 0x7FFFFFFFu) {
      return trace.ret(E_OUTOFMEMORY);
    }
    res->size_bytes = static_cast<uint32_t>(aligned);
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else if (res->width && res->height) {
    // Surface/Texture2D share the same storage layout for now.
    res->kind = (res->mip_levels > 1) ? ResourceKind::Texture2D : ResourceKind::Surface;

    Texture2dLayout layout{};
    if (!calc_texture2d_layout(res->format, res->width, res->height, res->mip_levels, res->depth, &layout)) {
      return trace.ret(E_OUTOFMEMORY);
    }
    if (layout.total_size_bytes > 0x7FFFFFFFu) {
      return trace.ret(E_OUTOFMEMORY);
    }

    res->row_pitch = layout.row_pitch_bytes;
    res->slice_pitch = layout.slice_pitch_bytes;
    res->size_bytes = static_cast<uint32_t>(layout.total_size_bytes);
  } else {
    return trace.ret(E_INVALIDARG);
  }

  if (res->pool != kD3DPOOL_SYSTEMMEM && res->kind != ResourceKind::Buffer) {
    const uint32_t agpu_format = d3d9_format_to_aerogpu(res->format);
    if (agpu_format == AEROGPU_FORMAT_INVALID) {
      return trace.ret(D3DERR_INVALIDCALL);
    }

    // BC formats were introduced in the guest↔host ABI in minor version 2.
    // Older emulators will treat these as invalid; gate them so the UMD can run
    // against older hosts.
    if (is_block_compressed_format(res->format) && !SupportsBcFormats(dev)) {
      return trace.ret(D3DERR_INVALIDCALL);
    }
  }

  // System-memory pool resources (e.g. CreateOffscreenPlainSurface with
  // D3DPOOL_SYSTEMMEM) are used by the D3D9 runtime for readback
  // (GetRenderTargetData). In WDDM builds we back these with a guest allocation
  // so the host can write pixels directly into guest memory
  // (AEROGPU_COPY_FLAG_WRITEBACK_DST) and the CPU can lock the allocation to
  // read them.
  if (res->pool == kD3DPOOL_SYSTEMMEM) {
    if (wants_shared) {
      return trace.ret(D3DERR_INVALIDCALL);
    }
    // In non-WDDM/portable builds there is no allocation-table plumbing, so keep
    // systemmem resources CPU-only (no host object).
    //
    // NOTE: Some portable tests set `wddm_context.hContext` to a non-zero value to
    // exercise allocation-list tracking logic without a real WDDM runtime. Only
    // the WDK build provides allocation lock callbacks, so keep systemmem resources
    // CPU-only unless we're built for WDDM and have a real WDDM device.
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    const bool allow_wddm_systemmem = (dev->wddm_device != 0);
#else
    const bool allow_wddm_systemmem = false;
#endif
    if (!allow_wddm_systemmem) {
      try {
        res->storage.resize(res->size_bytes);
      } catch (...) {
        return trace.ret(E_OUTOFMEMORY);
      }
      res->handle = 0;
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
      res->share_token = 0;
      res->wddm_hAllocation = 0;
      pCreateResource->hResource.pDrvPrivate = res.release();
      return trace.ret(S_OK);
    }

    // WDDM path: back the systemmem surface with a guest allocation so the host
    // can write pixels back into guest memory (WRITEBACK_DST) and the CPU can
    // lock/map the allocation to read them.
    const bool have_runtime_priv =
        (pCreateResource->pPrivateDriverData &&
         pCreateResource->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv));
    if (res->wddm_hAllocation != 0 && !have_runtime_priv) {
      // If the runtime already attached a kernel allocation handle, we need a
      // private-driver-data buffer to communicate the alloc_id to the KMD.
      logf("aerogpu-d3d9: Create systemmem resource missing private data buffer for existing hAllocation (have=%u need=%u)\n",
           pCreateResource->PrivateDriverDataSize,
           static_cast<unsigned>(sizeof(aerogpu_wddm_alloc_priv)));
      return trace.ret(D3DERR_INVALIDCALL);
    }

    // WRITEBACK_DST requires the destination to have a host resource.
    if (d3d9_format_to_aerogpu(res->format) == AEROGPU_FORMAT_INVALID) {
      return trace.ret(D3DERR_INVALIDCALL);
    }

    const uint32_t alloc_id = allocate_umd_alloc_id(dev->adapter);
    if (!alloc_id) {
      logf("aerogpu-d3d9: Failed to allocate systemmem alloc_id (handle=%u)\n",
           static_cast<unsigned>(res->handle));
      return trace.ret(E_FAIL);
    }

    aerogpu_wddm_alloc_priv priv{};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv.alloc_id = alloc_id;
    priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE;
    priv.share_token = 0;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv.reserved0 = encode_wddm_alloc_priv_desc(res->format, res->width, res->height);
    if (have_runtime_priv) {
      std::memcpy(pCreateResource->pPrivateDriverData, &priv, sizeof(priv));
    }

    res->backing_alloc_id = alloc_id;
    res->backing_offset_bytes = 0;
    res->share_token = 0;
    res->is_shared = false;
    res->is_shared_alias = false;

    bool allocation_created = false;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    // Some D3D9 runtimes do not attach a WDDM allocation handle to systemmem pool
    // resources. For AeroGPU we still want a real guest-backed allocation so the
    // host can write pixels directly into guest memory (WRITEBACK_DST) and the
    // CPU can map it via LockRect. Create a system-memory segment allocation if
    // the runtime did not supply one.
    if (res->wddm_hAllocation == 0 && dev->wddm_device != 0) {
      const HRESULT hr = wddm_create_allocation(dev->wddm_callbacks,
                                                dev->wddm_device,
                                                res->size_bytes,
                                                &priv,
                                                sizeof(priv),
                                                &res->wddm_hAllocation,
                                                dev->wddm_context.hContext);
      if (FAILED(hr) || res->wddm_hAllocation == 0) {
        logf("aerogpu-d3d9: AllocateCb failed for systemmem resource hr=0x%08lx handle=%u alloc_id=%u\n",
             static_cast<unsigned long>(hr),
             static_cast<unsigned>(res->handle),
             static_cast<unsigned>(res->backing_alloc_id));
        return trace.ret(FAILED(hr) ? hr : E_FAIL);
      }
      allocation_created = true;
    }
#endif

    if (res->wddm_hAllocation == 0) {
      // Without a WDDM allocation handle we cannot participate in the alloc-table
      // protocol, so WRITEBACK_DST readback is not supported.
      logf("aerogpu-d3d9: systemmem resource missing WDDM hAllocation (handle=%u alloc_id=%u)\n",
           static_cast<unsigned>(res->handle),
           static_cast<unsigned>(res->backing_alloc_id));
      return trace.ret(E_FAIL);
    }

    // Ensure CPU copies/locks map the allocation rather than reading stale
    // `storage` bytes.
    res->storage.clear();

    if (!emit_create_resource_locked(dev, res.get())) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      if (allocation_created && res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
        (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                      dev->wddm_device,
                                      res->wddm_hAllocation,
                                      dev->wddm_context.hContext);
        res->wddm_hAllocation = 0;
      }
#endif
      return trace.ret(E_OUTOFMEMORY);
    }
    pCreateResource->hResource.pDrvPrivate = res.release();
    return trace.ret(S_OK);
  }

  // On the real WDDM path we want GPU resources to be backed by WDDM allocations
  // and referenced in the command stream via a stable per-allocation `alloc_id`
  // (carried in aerogpu_wddm_alloc_priv and resolved via the per-submit allocation
  // table).
  if (!wants_shared && dev->wddm_context.hContext != 0) {
    if (!res->backing_alloc_id) {
      const bool have_runtime_priv =
          (pCreateResource->pPrivateDriverData &&
           pCreateResource->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv));
      if (res->wddm_hAllocation != 0 && !have_runtime_priv) {
        // If the runtime already attached an allocation handle, we have no other
        // way to communicate the alloc_id into the KMD allocation record.
        logf("aerogpu-d3d9: CreateResource missing private data buffer for existing hAllocation (have=%u need=%u)\n",
             pCreateResource->PrivateDriverDataSize,
             static_cast<unsigned>(sizeof(aerogpu_wddm_alloc_priv)));
        return trace.ret(D3DERR_INVALIDCALL);
      }

      // Use the same cross-process allocator as shared surfaces so alloc_id values
      // never collide within a submission (DWM can reference shared + non-shared
      // allocations together).
      uint64_t alloc_token = 0;
      uint32_t alloc_id = 0;
      do {
        alloc_token = allocate_shared_alloc_id_token(dev->adapter);
        alloc_id = static_cast<uint32_t>(alloc_token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      } while (alloc_token != 0 && alloc_id == 0);

      if (!alloc_token || !alloc_id) {
        logf("aerogpu-d3d9: Failed to allocate alloc_id for non-shared resource (token=%llu alloc_id=%u)\n",
             static_cast<unsigned long long>(alloc_token),
             static_cast<unsigned>(alloc_id));
        return E_FAIL;
      }

      aerogpu_wddm_alloc_priv priv{};
      priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
      priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
      priv.alloc_id = alloc_id;
      priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE;
      priv.share_token = 0;
      priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
      priv.reserved0 = encode_wddm_alloc_priv_desc(res->format, res->width, res->height);
      if (have_runtime_priv) {
        std::memcpy(pCreateResource->pPrivateDriverData, &priv, sizeof(priv));
      }

      res->backing_alloc_id = alloc_id;
      res->backing_offset_bytes = 0;
      res->share_token = 0;
    }
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  // Guest-backed textures currently only support mip 0 / array layer 0. Reject
  // multi-subresource layouts until the host executor and protocol are extended.
  if (!wants_shared && (res->mip_levels > 1 || res->depth > 1)) {
    return E_NOTIMPL;
  }
#endif

  if (wants_shared && !open_existing_shared) {
    if (!pCreateResource->pPrivateDriverData ||
        pCreateResource->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_priv)) {
      logf("aerogpu-d3d9: Create shared resource missing private data buffer (have=%u need=%u)\n",
           pCreateResource->PrivateDriverDataSize,
           static_cast<unsigned>(sizeof(aerogpu_wddm_alloc_priv)));
      return trace.ret(D3DERR_INVALIDCALL);
    }

    uint64_t share_token = 0;
#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
    share_token = dev->adapter->share_token_allocator.allocate_share_token();
#endif

    // Allocate a stable cross-process alloc_id (31-bit) and persist it in
    // allocation private data so it survives OpenResource/OpenAllocation in
    // another process.
    //
    // The Win7 KMD fills `aerogpu_wddm_alloc_priv.share_token` during
    // DxgkDdiCreateAllocation. For shared allocations, dxgkrnl preserves and
    // replays the private-data blob on cross-process opens so other guest
    // processes observe the same token.
    //
    // NOTE: DWM may compose many shared surfaces from *different* processes in a
    // single submission. alloc_id values must therefore avoid collisions across
    // guest processes (not just within one process).
    uint32_t alloc_id = 0;
    {
      // `allocate_shared_alloc_id_token()` provides a monotonic 64-bit counter shared
      // across guest processes (best effort). Derive a 31-bit alloc_id from it.
      uint64_t alloc_token = 0;
      do {
        alloc_token = allocate_shared_alloc_id_token(dev->adapter);
        alloc_id = static_cast<uint32_t>(alloc_token & AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
      } while (alloc_token != 0 && alloc_id == 0);

      if (!alloc_token || !alloc_id) {
        logf("aerogpu-d3d9: Failed to allocate shared alloc_id (token=%llu alloc_id=%u)\n",
             static_cast<unsigned long long>(alloc_token),
             static_cast<unsigned>(alloc_id));
        return trace.ret(E_FAIL);
      }
    }

    aerogpu_wddm_alloc_priv priv{};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv.alloc_id = alloc_id;
    priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
    priv.share_token = static_cast<aerogpu_wddm_u64>(share_token);
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv.reserved0 = encode_wddm_alloc_priv_desc(res->format, res->width, res->height);
    std::memcpy(pCreateResource->pPrivateDriverData, &priv, sizeof(priv));

    res->backing_alloc_id = alloc_id;
    res->share_token = share_token;
  }

  bool has_wddm_allocation = (res->wddm_hAllocation != 0);
  bool allocation_created = false;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (!has_wddm_allocation && !open_existing_shared && dev->wddm_device != 0) {
    uint32_t alloc_id = res->backing_alloc_id;
    if (alloc_id == 0) {
      alloc_id = allocate_umd_alloc_id(dev->adapter);
      if (alloc_id == 0) {
        return E_OUTOFMEMORY;
      }
      res->backing_alloc_id = alloc_id;
    }

    aerogpu_wddm_alloc_priv priv_local{};
    aerogpu_wddm_alloc_priv* priv = &priv_local;

    // Prefer the runtime-provided private-data buffer when available: it avoids
    // passing a pointer to stack memory across the user↔kernel boundary.
    if (pCreateResource->pPrivateDriverData &&
        pCreateResource->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv)) {
      priv = reinterpret_cast<aerogpu_wddm_alloc_priv*>(pCreateResource->pPrivateDriverData);
    }

    // Treat the struct as in/out. Clear it so we never pick up stale bytes from
    // a previous call (which can cause cross-process collisions).
    std::memset(priv, 0, sizeof(*priv));
    priv->magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv->version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv->alloc_id = alloc_id;
    priv->flags = wants_shared ? AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED : AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE;
    // The Win7 KMD owns share_token generation; provide 0 as a placeholder.
    priv->share_token = 0;
    priv->size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv->reserved0 = encode_wddm_alloc_priv_desc(res->format, res->width, res->height);

    const HRESULT hr = wddm_create_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              res->size_bytes,
                                              priv,
                                              sizeof(*priv),
                                              &res->wddm_hAllocation,
                                              dev->wddm_context.hContext);
    if (FAILED(hr) || res->wddm_hAllocation == 0) {
      return FAILED(hr) ? hr : E_FAIL;
    }

    consume_wddm_alloc_priv(res.get(), priv, sizeof(*priv), wants_shared);
    if (wants_shared && res->share_token == 0) {
      logf("aerogpu-d3d9: KMD did not return share_token for shared alloc_id=%u\n", res->backing_alloc_id);
      return E_FAIL;
    }

    has_wddm_allocation = true;
    allocation_created = true;
  }
#endif

  if (!has_wddm_allocation) {
    // Fallback (non-WDDM builds): allocate CPU shadow storage.
    //
    // For non-shared resources, treat the host object as "host allocated" and
    // clear `backing_alloc_id` so update paths fall back to inline uploads
    // instead of alloc-table indirections (portable builds have no guest
    // allocation table backing).
    //
    // Shared resources still need a stable alloc_id/share_token contract for
    // EXPORT/IMPORT, so preserve `backing_alloc_id` even in portable builds.
    try {
      res->storage.resize(res->size_bytes);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
    res->wddm_hAllocation = 0;
    res->backing_offset_bytes = 0;
    if (!res->is_shared) {
      res->backing_alloc_id = 0;
    }

    // Portable builds do not have a Win7 KMD to generate a stable share_token for
    // shared allocations. Generate one in user mode and persist it into the
    // allocation private data blob so simulated cross-process opens observe the
    // same token.
    if (res->is_shared && res->share_token == 0 && dev->adapter) {
      res->share_token = dev->adapter->share_token_allocator.allocate_share_token();
      if (pCreateResource->pPrivateDriverData && pCreateResource->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv)) {
        auto* priv = reinterpret_cast<aerogpu_wddm_alloc_priv*>(pCreateResource->pPrivateDriverData);
        if (priv->magic == AEROGPU_WDDM_ALLOC_PRIV_MAGIC &&
            (priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION || priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2)) {
          priv->share_token = res->share_token;
        }
      }
    }
  }

  if (open_existing_shared) {
    if (!res->share_token) {
      logf("aerogpu-d3d9: Open shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      return trace.ret(E_FAIL);
    }
    // Shared surface open (D3D9Ex): the host already has the original resource,
    // so we only create a new alias handle and IMPORT it.
    if (!emit_import_shared_surface_locked(dev, res.get())) {
      return trace.ret(E_OUTOFMEMORY);
    }
  } else {
    if (!emit_create_resource_locked(dev, res.get())) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      if (allocation_created && res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
        (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                      dev->wddm_device,
                                      res->wddm_hAllocation,
                                      dev->wddm_context.hContext);
        res->wddm_hAllocation = 0;
      }
#endif
      return trace.ret(E_OUTOFMEMORY);
    }

    if (res->is_shared) {
      if (!res->share_token) {
        logf("aerogpu-d3d9: Create shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      } else {
        // Shared surface create (D3D9Ex): export exactly once so other guest
        // processes can IMPORT using the same stable share_token.
        if (!emit_export_shared_surface_locked(dev, res.get())) {
          return trace.ret(E_OUTOFMEMORY);
        }

        // Shared surfaces must be importable by other processes immediately
        // after CreateResource returns. Since AeroGPU resource creation is
        // expressed in the command stream, force a submission so the host
        // observes the export.
        submit(dev);

        logf("aerogpu-d3d9: export shared_surface res=%u token=%llu\n",
             res->handle,
             static_cast<unsigned long long>(res->share_token));
      }
    }
  }

  pCreateResource->hResource.pDrvPrivate = res.release();
  return trace.ret(S_OK);
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
namespace {
template <typename T, typename = void>
struct aerogpu_has_member_hAllocation : std::false_type {};
template <typename T>
struct aerogpu_has_member_hAllocation<T, std::void_t<decltype(std::declval<T>().hAllocation)>> : std::true_type {};

template <typename T, typename = void>
struct aerogpu_has_member_hAllocations : std::false_type {};
template <typename T>
struct aerogpu_has_member_hAllocations<T, std::void_t<decltype(std::declval<T>().hAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct aerogpu_has_member_phAllocation : std::false_type {};
template <typename T>
struct aerogpu_has_member_phAllocation<T, std::void_t<decltype(std::declval<T>().phAllocation)>> : std::true_type {};

template <typename T, typename = void>
struct aerogpu_has_member_NumAllocations : std::false_type {};
template <typename T>
struct aerogpu_has_member_NumAllocations<T, std::void_t<decltype(std::declval<T>().NumAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct aerogpu_has_member_pOpenAllocationInfo : std::false_type {};
template <typename T>
struct aerogpu_has_member_pOpenAllocationInfo<T, std::void_t<decltype(std::declval<T>().pOpenAllocationInfo)>> : std::true_type {};

template <typename T, typename = void>
struct aerogpu_has_member_pAllocations : std::false_type {};
template <typename T>
struct aerogpu_has_member_pAllocations<T, std::void_t<decltype(std::declval<T>().pAllocations)>> : std::true_type {};
} // namespace

template <typename OpenResourceT>
WddmAllocationHandle get_wddm_allocation_from_openresource(const OpenResourceT* open_resource) {
  if (!open_resource) {
    return 0;
  }

  if constexpr (aerogpu_has_member_hAllocation<OpenResourceT>::value) {
    const auto h = static_cast<WddmAllocationHandle>(open_resource->hAllocation);
    if (h != 0) {
      return h;
    }
  }

  if constexpr (aerogpu_has_member_hAllocations<OpenResourceT>::value) {
    const auto& h_allocations = open_resource->hAllocations;
    using AllocationsT = std::remove_reference_t<decltype(h_allocations)>;
    if constexpr (std::is_array_v<AllocationsT>) {
      const auto h = static_cast<WddmAllocationHandle>(h_allocations[0]);
      if (h != 0) {
        return h;
      }
    } else if constexpr (std::is_pointer_v<AllocationsT>) {
      if (h_allocations) {
        const auto h = static_cast<WddmAllocationHandle>(h_allocations[0]);
        if (h != 0) {
          return h;
        }
      }
    } else if constexpr (std::is_integral_v<AllocationsT>) {
      const auto h = static_cast<WddmAllocationHandle>(h_allocations);
      if (h != 0) {
        return h;
      }
    }
  }

  if constexpr (aerogpu_has_member_phAllocation<OpenResourceT>::value && aerogpu_has_member_NumAllocations<OpenResourceT>::value) {
    if (open_resource->phAllocation && open_resource->NumAllocations) {
      const auto h = static_cast<WddmAllocationHandle>(open_resource->phAllocation[0]);
      if (h != 0) {
        return h;
      }
    }
  }

  if constexpr (aerogpu_has_member_pOpenAllocationInfo<OpenResourceT>::value && aerogpu_has_member_NumAllocations<OpenResourceT>::value) {
    if (open_resource->pOpenAllocationInfo && open_resource->NumAllocations) {
      using InfoT = std::remove_pointer_t<decltype(open_resource->pOpenAllocationInfo)>;
      if constexpr (aerogpu_has_member_hAllocation<InfoT>::value) {
        const auto h = static_cast<WddmAllocationHandle>(open_resource->pOpenAllocationInfo[0].hAllocation);
        if (h != 0) {
          return h;
        }
      }
    }
  }

  if constexpr (aerogpu_has_member_pAllocations<OpenResourceT>::value) {
    const auto* allocs = open_resource->pAllocations;
    if (allocs) {
      using Elem = std::remove_pointer_t<decltype(allocs)>;
      if constexpr (std::is_class_v<Elem> && aerogpu_has_member_hAllocation<Elem>::value) {
        const auto h = static_cast<WddmAllocationHandle>(allocs[0].hAllocation);
        if (h != 0) {
          return h;
        }
      } else if constexpr (!std::is_class_v<Elem> && std::is_convertible_v<Elem, WddmAllocationHandle>) {
        const auto h = static_cast<WddmAllocationHandle>(allocs[0]);
        if (h != 0) {
          return h;
        }
      }
    }
  }

  return 0;
}
#endif

static HRESULT device_open_resource_impl(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  if (!hDevice.pDrvPrivate || !pOpenResource) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  const void* priv_data = d3d9_private_driver_data_ptr(*pOpenResource);
  const uint32_t priv_data_size = d3d9_private_driver_data_size(*pOpenResource);

  if (!priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return E_INVALIDARG;
  }

  aerogpu_wddm_alloc_priv priv{};
  std::memcpy(&priv, priv_data, sizeof(priv));
  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC ||
      (priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION && priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION_2)) {
    return E_INVALIDARG;
  }
  if ((priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) == 0 || priv.share_token == 0 || priv.alloc_id == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto res = std::make_unique<Resource>();
  res->handle = allocate_global_handle(dev->adapter);

  res->is_shared = true;
  res->is_shared_alias = true;
  res->share_token = priv.share_token;
  res->backing_alloc_id = priv.alloc_id;
  res->backing_offset_bytes = 0;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  res->wddm_hAllocation = get_wddm_allocation_from_openresource(pOpenResource);
#else
  res->wddm_hAllocation = static_cast<WddmAllocationHandle>(pOpenResource->wddm_hAllocation);
#endif
  if (dev->wddm_context.hContext != 0 && res->backing_alloc_id != 0 && res->wddm_hAllocation == 0) {
    logf("aerogpu-d3d9: OpenResource missing WDDM hAllocation (alloc_id=%u)\n", res->backing_alloc_id);
    return E_FAIL;
  }

  // OpenResource DDI structs vary across WDK header vintages. Some header sets do
  // not include a full resource description, so treat all description fields as
  // optional and fall back to the encoded `priv.reserved0` description when
  // available.
  res->type = d3d9_optional_resource_type(*pOpenResource);
  res->format = static_cast<D3DDDIFORMAT>(d3d9_optional_resource_format(*pOpenResource));
  res->width = d3d9_optional_resource_width(*pOpenResource);
  res->height = d3d9_optional_resource_height(*pOpenResource);
  res->depth = std::max(1u, d3d9_resource_depth(*pOpenResource));
  res->mip_levels = std::max(1u, d3d9_resource_mip_levels(*pOpenResource));
  res->usage = d3d9_resource_usage(*pOpenResource);
  res->pool = d3d9_resource_pool(*pOpenResource);
  const uint32_t open_size_bytes = d3d9_resource_size(*pOpenResource);

  uint32_t desc_format = 0;
  uint32_t desc_width = 0;
  uint32_t desc_height = 0;
  if (decode_wddm_alloc_priv_desc(priv.reserved0, &desc_format, &desc_width, &desc_height)) {
    if (res->format == 0) {
      res->format = desc_format;
    }
    if (res->width == 0) {
      res->width = desc_width;
    }
    if (res->height == 0) {
      res->height = desc_height;
    }
  }

  // Prefer a reconstructed size when the runtime provides a description; fall
  // back to the size_bytes persisted in allocation private data.
  if (open_size_bytes) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = open_size_bytes;
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else if (res->width && res->height) {
    res->kind = (res->mip_levels > 1) ? ResourceKind::Texture2D : ResourceKind::Surface;

    Texture2dLayout layout{};
    if (!calc_texture2d_layout(res->format, res->width, res->height, res->mip_levels, res->depth, &layout)) {
      return E_OUTOFMEMORY;
    }
    if (layout.total_size_bytes > 0x7FFFFFFFu) {
      return E_OUTOFMEMORY;
    }

    res->row_pitch = layout.row_pitch_bytes;
    res->slice_pitch = layout.slice_pitch_bytes;
    res->size_bytes = static_cast<uint32_t>(layout.total_size_bytes);
  } else if (priv.size_bytes != 0 && priv.size_bytes <= 0x7FFFFFFFu) {
    res->kind = ResourceKind::Surface;
    res->size_bytes = static_cast<uint32_t>(priv.size_bytes);
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else {
    return E_INVALIDARG;
  }

  if (res->kind != ResourceKind::Buffer) {
    const uint32_t agpu_format = d3d9_format_to_aerogpu(res->format);
    if (agpu_format == AEROGPU_FORMAT_INVALID) {
      return E_INVALIDARG;
    }

    if (is_block_compressed_format(res->format) && !SupportsBcFormats(dev)) {
      return E_INVALIDARG;
    }
  }

  if (!res->size_bytes) {
    return E_INVALIDARG;
  }

  try {
    res->storage.resize(res->size_bytes);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (!emit_import_shared_surface_locked(dev, res.get())) {
    return E_OUTOFMEMORY;
  }

  logf("aerogpu-d3d9: import shared_surface out_res=%u token=%llu alloc_id=%u hAllocation=0x%08x\n",
       res->handle,
        static_cast<unsigned long long>(res->share_token),
        static_cast<unsigned>(res->backing_alloc_id),
        static_cast<unsigned>(res->wddm_hAllocation));

  pOpenResource->hResource.pDrvPrivate = res.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_open_resource(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  uint64_t arg0 = d3d9_trace_arg_ptr(hDevice.pDrvPrivate);
  uint64_t arg1 = d3d9_trace_arg_ptr(pOpenResource);
  uint64_t arg2 = 0;
  uint64_t arg3 = 0;
  if constexpr (aerogpu_d3d9_has_member_Type<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_type<D3D9DDIARG_OPENRESOURCE>::value ||
                aerogpu_d3d9_has_member_Width<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_width<D3D9DDIARG_OPENRESOURCE>::value ||
                aerogpu_d3d9_has_member_Height<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_height<D3D9DDIARG_OPENRESOURCE>::value) {
    arg1 = pOpenResource
               ? d3d9_trace_pack_u32_u32(d3d9_optional_resource_type(*pOpenResource), d3d9_optional_resource_format(*pOpenResource))
               : 0;
    arg2 = pOpenResource
               ? d3d9_trace_pack_u32_u32(d3d9_optional_resource_width(*pOpenResource), d3d9_optional_resource_height(*pOpenResource))
               : 0;
    arg3 = pOpenResource ? d3d9_trace_pack_u32_u32(d3d9_resource_usage(*pOpenResource), d3d9_private_driver_data_size(*pOpenResource)) : 0;
  }
  D3d9TraceCall trace(D3d9TraceFunc::DeviceOpenResource, arg0, arg1, arg2, arg3);
  return trace.ret(device_open_resource_impl(hDevice, pOpenResource));
}

HRESULT AEROGPU_D3D9_CALL device_open_resource2(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_OPENRESOURCE* pOpenResource) {
  uint64_t arg0 = d3d9_trace_arg_ptr(hDevice.pDrvPrivate);
  uint64_t arg1 = d3d9_trace_arg_ptr(pOpenResource);
  uint64_t arg2 = 0;
  uint64_t arg3 = 0;
  if constexpr (aerogpu_d3d9_has_member_Type<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_type<D3D9DDIARG_OPENRESOURCE>::value ||
                aerogpu_d3d9_has_member_Width<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_width<D3D9DDIARG_OPENRESOURCE>::value ||
                aerogpu_d3d9_has_member_Height<D3D9DDIARG_OPENRESOURCE>::value || aerogpu_d3d9_has_member_height<D3D9DDIARG_OPENRESOURCE>::value) {
    arg1 = pOpenResource
               ? d3d9_trace_pack_u32_u32(d3d9_optional_resource_type(*pOpenResource), d3d9_optional_resource_format(*pOpenResource))
               : 0;
    arg2 = pOpenResource
               ? d3d9_trace_pack_u32_u32(d3d9_optional_resource_width(*pOpenResource), d3d9_optional_resource_height(*pOpenResource))
               : 0;
    arg3 = pOpenResource ? d3d9_trace_pack_u32_u32(d3d9_resource_usage(*pOpenResource), d3d9_private_driver_data_size(*pOpenResource)) : 0;
  }
  D3d9TraceCall trace(D3d9TraceFunc::DeviceOpenResource2, arg0, arg1, arg2, arg3);
  return trace.ret(device_open_resource_impl(hDevice, pOpenResource));
}

HRESULT AEROGPU_D3D9_CALL device_destroy_resource(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hResource) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyResource,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hResource.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* res = as_resource(hResource);
  if (!dev || !res) {
    delete res;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Ensure any queued commands referencing this allocation are submitted before
  // we release the kernel allocation handle.
  (void)submit(dev);

  for (SwapChain* sc : dev->swapchains) {
    if (!sc) {
      continue;
    }
    auto& bbs = sc->backbuffers;
    bbs.erase(std::remove(bbs.begin(), bbs.end(), res), bbs.end());
  }

  // Defensive: DWM and other D3D9Ex clients can destroy resources while they are
  // still bound. Clear any cached bindings that point at the resource before we
  // delete it so subsequent command emission does not dereference a dangling
  // pointer.
  bool rt_changed = false;
  for (uint32_t i = 0; i < 4; ++i) {
    if (dev->render_targets[i] == res) {
      dev->render_targets[i] = nullptr;
      rt_changed = true;
    }
  }
  if (dev->depth_stencil == res) {
    dev->depth_stencil = nullptr;
    rt_changed = true;
  }

  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (dev->textures[stage] != res) {
      continue;
    }
    dev->textures[stage] = nullptr;
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (dev->streams[stream].vb != res) {
      continue;
    }
    dev->streams[stream] = {};

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = 0;
    binding.stride_bytes = 0;
    binding.offset_bytes = 0;
    binding.reserved0 = 0;

    if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
            dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
      cmd->start_slot = stream;
      cmd->buffer_count = 1;
    }
  }

  if (dev->index_buffer == res) {
    dev->index_buffer = nullptr;
    dev->index_offset_bytes = 0;
    dev->index_format = kD3dFmtIndex16;

    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
      cmd->buffer = 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = 0;
      cmd->reserved0 = 0;
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }
  // Shared surfaces are refcounted host-side: DESTROY_RESOURCE releases a single
  // handle (original or alias) and the underlying surface is freed once the last
  // reference is gone.
  (void)emit_destroy_resource_locked(dev, res->handle);

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
    // Ensure the allocation handle is no longer referenced by the current DMA
    // buffer before we destroy it.
    (void)submit(dev);
    const HRESULT hr =
        wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, res->wddm_hAllocation, dev->wddm_context.hContext);
    if (FAILED(hr)) {
      logf("aerogpu-d3d9: DestroyAllocation failed hr=0x%08lx alloc_id=%u hAllocation=%llu\n",
           static_cast<unsigned long>(hr),
           static_cast<unsigned>(res->backing_alloc_id),
           static_cast<unsigned long long>(res->wddm_hAllocation));
    }
    res->wddm_hAllocation = 0;
  }
#endif
  delete res;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_swap_chain(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_CREATESWAPCHAIN* pCreateSwapChain) {
  const D3D9DDI_PRESENT_PARAMETERS* trace_pp = pCreateSwapChain ? d3d9_get_present_params(*pCreateSwapChain) : nullptr;
  const uint64_t bb_wh =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_width(*trace_pp), d3d9_pp_backbuffer_height(*trace_pp)) : 0;
  const uint64_t fmt_count =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_format(*trace_pp), d3d9_pp_backbuffer_count(*trace_pp)) : 0;
  const uint64_t interval_flags =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_presentation_interval(*trace_pp), d3d9_pp_flags(*trace_pp)) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceCreateSwapChain, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  if (!hDevice.pDrvPrivate || !pCreateSwapChain) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  const D3D9DDI_PRESENT_PARAMETERS* pp = d3d9_get_present_params(*pCreateSwapChain);
  if (!pp) {
    return trace.ret(E_INVALIDARG);
  }
  if (d3d9_format_to_aerogpu(d3d9_pp_backbuffer_format(*pp)) == AEROGPU_FORMAT_INVALID) {
    return trace.ret(E_INVALIDARG);
  }

  const uint32_t width = d3d9_pp_backbuffer_width(*pp) ? d3d9_pp_backbuffer_width(*pp) : 1u;
  const uint32_t height = d3d9_pp_backbuffer_height(*pp) ? d3d9_pp_backbuffer_height(*pp) : 1u;
  const uint32_t backbuffer_count = std::max(1u, d3d9_pp_backbuffer_count(*pp));

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto sc = std::make_unique<SwapChain>();
  sc->handle = allocate_global_handle(dev->adapter);
  sc->hwnd = pp->hDeviceWindow;
  sc->width = width;
  sc->height = height;
  sc->format = d3d9_pp_backbuffer_format(*pp);
  sc->sync_interval = d3d9_pp_presentation_interval(*pp);
  sc->swap_effect = d3d9_pp_swap_effect(*pp);
  sc->flags = d3d9_pp_flags(*pp);

  sc->backbuffers.reserve(backbuffer_count);
  for (uint32_t i = 0; i < backbuffer_count; i++) {
    auto bb = std::make_unique<Resource>();
    HRESULT hr = create_backbuffer_locked(dev, bb.get(), sc->format, sc->width, sc->height);
    if (hr < 0) {
      // Best-effort cleanup: emit host-side destroys for any already-created
      // backbuffers, submit so the runtime sees a consistent alloc list, then
      // destroy the per-process WDDM allocations.
      for (Resource* created : sc->backbuffers) {
        if (!created) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, created->handle);
      }
      (void)submit(dev);

      for (Resource* created : sc->backbuffers) {
        if (!created) {
          continue;
        }
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (created->wddm_hAllocation != 0 && dev->wddm_device != 0) {
          (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                        dev->wddm_device,
                                        created->wddm_hAllocation,
                                        dev->wddm_context.hContext);
          created->wddm_hAllocation = 0;
        }
#endif
        delete created;
      }
      return trace.ret(hr);
    }
    sc->backbuffers.push_back(bb.release());
  }

  Resource* first_backbuffer = sc->backbuffers.empty() ? nullptr : sc->backbuffers[0];

  // Default D3D9 behavior: the first backbuffer is bound as render target 0.
  if (!dev->render_targets[0] && first_backbuffer) {
    dev->render_targets[0] = first_backbuffer;
    if (!emit_set_render_targets_locked(dev)) {
      // Keep driver state consistent with the host by rolling back the implicit
      // binding and tearing down the partially-created swapchain.
      dev->render_targets[0] = nullptr;
      for (Resource* created : sc->backbuffers) {
        if (!created) {
          continue;
        }
        (void)emit_destroy_resource_locked(dev, created->handle);
        delete created;
      }
      return trace.ret(E_OUTOFMEMORY);
    }
  }

  pCreateSwapChain->hBackBuffer.pDrvPrivate = first_backbuffer;
  pCreateSwapChain->hSwapChain.pDrvPrivate = sc.get();

  dev->swapchains.push_back(sc.release());
  if (!dev->current_swapchain) {
    dev->current_swapchain = dev->swapchains.back();
  }

  return trace.ret(S_OK);
}

HRESULT copy_surface_rects(Device* dev, const Resource* src, Resource* dst, const RECT* rects, uint32_t rect_count) {
  if (!rects || rect_count == 0) {
    return copy_surface_bytes(dev, src, dst);
  }
  if (!dev || !src || !dst) {
    return E_INVALIDARG;
  }
  if (src->format != dst->format) {
    return E_INVALIDARG;
  }
  if (is_block_compressed_format(src->format)) {
    // Rect-based copies operate in pixels and do not support BC formats.
    return E_INVALIDARG;
  }

  const uint32_t bpp = bytes_per_pixel(src->format);

  struct Map {
    void* ptr = nullptr;
    bool wddm_locked = false;
  };

  Map src_map{};
  Map dst_map{};
  const uint8_t* src_base = nullptr;
  uint8_t* dst_base = nullptr;

  const uint64_t src_bytes = src->slice_pitch;
  const uint64_t dst_bytes = dst->slice_pitch;
  if (src_bytes == 0 || dst_bytes == 0 || src_bytes > src->size_bytes || dst_bytes > dst->size_bytes) {
    return E_FAIL;
  }

  bool use_src_storage = src->storage.size() >= src_bytes;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (src->backing_alloc_id != 0) {
    use_src_storage = false;
  }
#endif
  if (use_src_storage) {
    src_base = src->storage.data();
  } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (src->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      const HRESULT hr = wddm_lock_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              src->wddm_hAllocation,
                                              0,
                                              src_bytes,
                                              kD3DLOCK_READONLY,
                                              &src_map.ptr,
                                              dev->wddm_context.hContext);
      if (FAILED(hr) || !src_map.ptr) {
        return FAILED(hr) ? hr : E_FAIL;
      }
      src_map.wddm_locked = true;
      src_base = static_cast<const uint8_t*>(src_map.ptr);
    } else
#endif
    {
      return E_FAIL;
    }
  }

  bool use_dst_storage = dst->storage.size() >= dst_bytes;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id != 0) {
    use_dst_storage = false;
  }
#endif
  if (use_dst_storage) {
    dst_base = dst->storage.data();
  } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (dst->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      const HRESULT hr = wddm_lock_allocation(dev->wddm_callbacks,
                                              dev->wddm_device,
                                              dst->wddm_hAllocation,
                                              0,
                                              dst_bytes,
                                              &dst_map.ptr,
                                              dev->wddm_context.hContext);
      if (FAILED(hr) || !dst_map.ptr) {
        if (src_map.wddm_locked) {
          (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                       dev->wddm_device,
                                       src->wddm_hAllocation,
                                       dev->wddm_context.hContext);
        }
        return FAILED(hr) ? hr : E_FAIL;
      }
      dst_map.wddm_locked = true;
      dst_base = static_cast<uint8_t*>(dst_map.ptr);
    } else
#endif
    {
      if (src_map.wddm_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
      return E_FAIL;
    }
  }

  for (uint32_t i = 0; i < rect_count; ++i) {
    const RECT& r = rects[i];
    if (r.right <= r.left || r.bottom <= r.top) {
      continue;
    }

    const uint32_t left = static_cast<uint32_t>(std::max<long>(0, r.left));
    const uint32_t top = static_cast<uint32_t>(std::max<long>(0, r.top));
    const uint32_t right = static_cast<uint32_t>(std::max<long>(0, r.right));
    const uint32_t bottom = static_cast<uint32_t>(std::max<long>(0, r.bottom));

    const uint32_t clamped_right = std::min<uint32_t>({right, src->width, dst->width});
    const uint32_t clamped_bottom = std::min<uint32_t>({bottom, src->height, dst->height});

    if (left >= clamped_right || top >= clamped_bottom) {
      continue;
    }

    const uint32_t row_bytes = (clamped_right - left) * bpp;
    for (uint32_t y = top; y < clamped_bottom; ++y) {
      const size_t src_off = static_cast<size_t>(y) * src->row_pitch + static_cast<size_t>(left) * bpp;
      const size_t dst_off = static_cast<size_t>(y) * dst->row_pitch + static_cast<size_t>(left) * bpp;
      if (src_off + row_bytes > src_bytes || dst_off + row_bytes > dst_bytes) {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (dst_map.wddm_locked) {
          (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                       dev->wddm_device,
                                       dst->wddm_hAllocation,
                                       dev->wddm_context.hContext);
        }
        if (src_map.wddm_locked) {
          (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                       dev->wddm_device,
                                       src->wddm_hAllocation,
                                       dev->wddm_context.hContext);
        }
#endif
        return E_INVALIDARG;
      }
      std::memcpy(dst_base + dst_off, src_base + src_off, row_bytes);
    }
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst_map.wddm_locked) {
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 dst->wddm_hAllocation,
                                 dev->wddm_context.hContext);
  }
  if (src_map.wddm_locked) {
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 src->wddm_hAllocation,
                                 dev->wddm_context.hContext);
  }
#endif

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_swap_chain(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSWAPCHAIN hSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroySwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSwapChain.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* sc = as_swapchain(hSwapChain);
  if (!dev || !sc) {
    delete sc;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Ensure we are not about to destroy an allocation handle that is still
  // referenced by the current DMA buffer.
  (void)submit(dev);

  auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
  if (it != dev->swapchains.end()) {
    dev->swapchains.erase(it);
  }
  if (dev->current_swapchain == sc) {
    dev->current_swapchain = dev->swapchains.empty() ? nullptr : dev->swapchains[0];
  }

  bool rt_changed = false;
  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    for (uint32_t i = 0; i < 4; ++i) {
      if (dev->render_targets[i] == bb) {
        dev->render_targets[i] = nullptr;
        rt_changed = true;
      }
    }
    if (dev->depth_stencil == bb) {
      dev->depth_stencil = nullptr;
      rt_changed = true;
    }

    for (uint32_t stage = 0; stage < 16; ++stage) {
      if (dev->textures[stage] != bb) {
        continue;
      }
      dev->textures[stage] = nullptr;
      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
        cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
        cmd->slot = stage;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }

    for (uint32_t stream = 0; stream < 16; ++stream) {
      if (dev->streams[stream].vb != bb) {
        continue;
      }
      dev->streams[stream] = {};

      aerogpu_vertex_buffer_binding binding{};
      binding.buffer = 0;
      binding.stride_bytes = 0;
      binding.offset_bytes = 0;
      binding.reserved0 = 0;

      if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
              dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
        cmd->start_slot = stream;
        cmd->buffer_count = 1;
      }
    }

    if (dev->index_buffer == bb) {
      dev->index_buffer = nullptr;
      dev->index_offset_bytes = 0;
      dev->index_format = kD3dFmtIndex16;

      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
        cmd->buffer = 0;
        cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
        cmd->offset_bytes = 0;
        cmd->reserved0 = 0;
      }
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }

  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    (void)emit_destroy_resource_locked(dev, bb->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (bb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      (void)wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, bb->wddm_hAllocation, dev->wddm_context.hContext);
      bb->wddm_hAllocation = 0;
    }
#endif
    delete bb;
  }

  delete sc;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_swap_chain(
    D3DDDI_HDEVICE hDevice,
    uint32_t index,
    D3D9DDI_HSWAPCHAIN* phSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetSwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(index),
                      d3d9_trace_arg_ptr(phSwapChain),
                      0);
  if (!hDevice.pDrvPrivate || !phSwapChain) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (index >= dev->swapchains.size()) {
    phSwapChain->pDrvPrivate = nullptr;
    return trace.ret(E_INVALIDARG);
  }
  phSwapChain->pDrvPrivate = dev->swapchains[index];
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_swap_chain(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSWAPCHAIN hSwapChain) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetSwapChain,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSwapChain.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  auto* sc = as_swapchain(hSwapChain);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (sc) {
    auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
    if (it == dev->swapchains.end()) {
      return trace.ret(E_INVALIDARG);
    }
  }
  dev->current_swapchain = sc;
  return trace.ret(S_OK);
}

HRESULT reset_swap_chain_locked(Device* dev, SwapChain* sc, const D3D9DDI_PRESENT_PARAMETERS& pp) {
  if (!dev || !dev->adapter || !sc) {
    return E_INVALIDARG;
  }

  // Reset/backbuffer recreation destroys WDDM allocation handles. Ensure pending
  // command buffers are flushed first so we don't hand dxgkrnl stale handles in
  // a later submission.
  (void)submit(dev);

  if (d3d9_format_to_aerogpu(d3d9_pp_backbuffer_format(pp)) == AEROGPU_FORMAT_INVALID) {
    return E_INVALIDARG;
  }

  const uint32_t new_width = d3d9_pp_backbuffer_width(pp) ? d3d9_pp_backbuffer_width(pp) : sc->width;
  const uint32_t new_height = d3d9_pp_backbuffer_height(pp) ? d3d9_pp_backbuffer_height(pp) : sc->height;
  const uint32_t new_count = std::max(1u, d3d9_pp_backbuffer_count(pp));

  sc->hwnd = pp.hDeviceWindow ? pp.hDeviceWindow : sc->hwnd;
  sc->width = new_width;
  sc->height = new_height;
  sc->format = d3d9_pp_backbuffer_format(pp);
  sc->sync_interval = d3d9_pp_presentation_interval(pp);
  sc->swap_effect = d3d9_pp_swap_effect(pp);
  sc->flags = d3d9_pp_flags(pp);

  // Reset destroys/recreates backbuffers. Flush any queued commands first so we
  // don't destroy allocations still referenced by an unsubmitted command buffer.
  (void)submit(dev);

  // Grow/shrink backbuffer array if needed.
  std::vector<Resource*> removed_backbuffers;
  while (sc->backbuffers.size() > new_count) {
    removed_backbuffers.push_back(sc->backbuffers.back());
    sc->backbuffers.pop_back();
  }

  bool rt_changed = false;
  for (Resource* bb : removed_backbuffers) {
    if (!bb) {
      continue;
    }
    for (uint32_t i = 0; i < 4; ++i) {
      if (dev->render_targets[i] == bb) {
        dev->render_targets[i] = nullptr;
        rt_changed = true;
      }
    }
    if (dev->depth_stencil == bb) {
      dev->depth_stencil = nullptr;
      rt_changed = true;
    }

    for (uint32_t stage = 0; stage < 16; ++stage) {
      if (dev->textures[stage] != bb) {
        continue;
      }
      dev->textures[stage] = nullptr;
      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
        cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
        cmd->slot = stage;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }

    for (uint32_t stream = 0; stream < 16; ++stream) {
      if (dev->streams[stream].vb != bb) {
        continue;
      }
      dev->streams[stream] = {};

      aerogpu_vertex_buffer_binding binding{};
      binding.buffer = 0;
      binding.stride_bytes = 0;
      binding.offset_bytes = 0;
      binding.reserved0 = 0;

      if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
              dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
        cmd->start_slot = stream;
        cmd->buffer_count = 1;
      }
    }

    if (dev->index_buffer == bb) {
      dev->index_buffer = nullptr;
      dev->index_offset_bytes = 0;
      dev->index_format = kD3dFmtIndex16;

      if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
        cmd->buffer = 0;
        cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
        cmd->offset_bytes = 0;
        cmd->reserved0 = 0;
      }
    }
  }

  if (rt_changed) {
    (void)emit_set_render_targets_locked(dev);
  }

  for (Resource* bb : removed_backbuffers) {
    if (!bb) {
      continue;
    }
    emit_destroy_resource_locked(dev, bb->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (bb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      (void)wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, bb->wddm_hAllocation, dev->wddm_context.hContext);
      bb->wddm_hAllocation = 0;
    }
#endif
    delete bb;
  }
  while (sc->backbuffers.size() < new_count) {
    auto bb = std::make_unique<Resource>();
    HRESULT hr = create_backbuffer_locked(dev, bb.get(), sc->format, sc->width, sc->height);
    if (hr < 0) {
      return hr;
    }
    sc->backbuffers.push_back(bb.release());
  }

  // Recreate backbuffer storage/handles.
  for (Resource* bb : sc->backbuffers) {
    if (!bb) {
      continue;
    }
    (void)emit_destroy_resource_locked(dev, bb->handle);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (bb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      (void)wddm_destroy_allocation(dev->wddm_callbacks, dev->wddm_device, bb->wddm_hAllocation, dev->wddm_context.hContext);
      bb->wddm_hAllocation = 0;
    }
#endif
    HRESULT hr = create_backbuffer_locked(dev, bb, sc->format, sc->width, sc->height);
    if (hr < 0) {
      return hr;
    }
  }

  auto is_backbuffer = [sc](const Resource* res) -> bool {
    if (!sc || !res) {
      return false;
    }
    return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
  };

  // Reset recreates swapchain backbuffer handles. If any of the backbuffers are
  // currently bound via other state (textures / IA bindings), re-emit the bind
  // commands so the host uses the updated handles.
  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (!is_backbuffer(dev->textures[stage])) {
      continue;
    }
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE)) {
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
      cmd->reserved0 = 0;
    }
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (!is_backbuffer(dev->streams[stream].vb)) {
      continue;
    }

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
    binding.stride_bytes = dev->streams[stream].stride_bytes;
    binding.offset_bytes = dev->streams[stream].offset_bytes;
    binding.reserved0 = 0;

    if (auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
            dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding))) {
      cmd->start_slot = stream;
      cmd->buffer_count = 1;
    }
  }

  if (is_backbuffer(dev->index_buffer)) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER)) {
      cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = dev->index_offset_bytes;
      cmd->reserved0 = 0;
    }
  }

  if (!dev->render_targets[0] && !sc->backbuffers.empty()) {
    dev->render_targets[0] = sc->backbuffers[0];
  }
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_reset(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_RESET* pReset) {
  const D3D9DDI_PRESENT_PARAMETERS* trace_pp = pReset ? d3d9_get_present_params(*pReset) : nullptr;
  const uint64_t bb_wh =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_width(*trace_pp), d3d9_pp_backbuffer_height(*trace_pp)) : 0;
  const uint64_t fmt_count =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_format(*trace_pp), d3d9_pp_backbuffer_count(*trace_pp)) : 0;
  const uint64_t interval_flags =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_presentation_interval(*trace_pp), d3d9_pp_flags(*trace_pp)) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceReset, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  if (!hDevice.pDrvPrivate || !pReset) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  // Reset implies a new frame queue; drop any in-flight present fences so
  // max-frame-latency throttling doesn't block the first presents after a reset.
  dev->inflight_present_fences.clear();
  SwapChain* sc = dev->current_swapchain;
  if (!sc && !dev->swapchains.empty()) {
    sc = dev->swapchains[0];
  }
  if (!sc) {
    return trace.ret(S_OK);
  }

  const D3D9DDI_PRESENT_PARAMETERS* pp = d3d9_get_present_params(*pReset);
  if (!pp) {
    return trace.ret(E_INVALIDARG);
  }
  return trace.ret(reset_swap_chain_locked(dev, sc, *pp));
}

HRESULT AEROGPU_D3D9_CALL device_reset_ex(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_RESET* pReset) {
  const D3D9DDI_PRESENT_PARAMETERS* trace_pp = pReset ? d3d9_get_present_params(*pReset) : nullptr;
  const uint64_t bb_wh =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_width(*trace_pp), d3d9_pp_backbuffer_height(*trace_pp)) : 0;
  const uint64_t fmt_count =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_backbuffer_format(*trace_pp), d3d9_pp_backbuffer_count(*trace_pp)) : 0;
  const uint64_t interval_flags =
      trace_pp ? d3d9_trace_pack_u32_u32(d3d9_pp_presentation_interval(*trace_pp), d3d9_pp_flags(*trace_pp)) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceResetEx, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), bb_wh, fmt_count, interval_flags);
  return trace.ret(device_reset(hDevice, pReset));
}

HRESULT AEROGPU_D3D9_CALL device_check_device_state(
    D3DDDI_HDEVICE hDevice,
    HWND hWnd) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCheckDeviceState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hWnd),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
#if defined(_WIN32)
  if (hWnd) {
    if (IsIconic(hWnd)) {
      return trace.ret(kSPresentOccluded);
    }
  }
#endif
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_rotate_resource_identities(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE* pResources,
    uint32_t resource_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceRotateResourceIdentities,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(resource_count),
                      d3d9_trace_arg_ptr(pResources),
                      0);
  if (!hDevice.pDrvPrivate || !pResources || resource_count < 2) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  std::vector<Resource*> resources;
  resources.reserve(resource_count);
  for (uint32_t i = 0; i < resource_count; ++i) {
    auto* res = as_resource(pResources[i]);
    if (!res) {
      return trace.ret(E_INVALIDARG);
    }
    if (std::find(resources.begin(), resources.end(), res) != resources.end()) {
      // Reject duplicates: D3D9 expects a set of distinct resources.
      return trace.ret(E_INVALIDARG);
    }
    resources.push_back(res);
  }

  auto matches_desc = [&resources](const Resource* res) -> bool {
    const Resource* base = resources.empty() ? nullptr : resources[0];
    if (!base || !res) {
      return false;
    }
    return res->kind == base->kind &&
           res->type == base->type &&
           res->format == base->format &&
           res->width == base->width &&
           res->height == base->height &&
           res->depth == base->depth &&
           res->mip_levels == base->mip_levels &&
           res->usage == base->usage &&
           res->pool == base->pool &&
           res->size_bytes == base->size_bytes &&
           res->row_pitch == base->row_pitch &&
           res->slice_pitch == base->slice_pitch;
  };

  for (Resource* res : resources) {
    if (!matches_desc(res)) {
      return trace.ret(kD3DErrInvalidCall);
    }
    if (res->locked) {
      return trace.ret(kD3DErrInvalidCall);
    }
    // Shared resources have stable identities (`share_token`); rotating them is
    // likely to break EXPORT/IMPORT semantics across processes.
    if (res->is_shared || res->is_shared_alias || res->share_token != 0) {
      return trace.ret(kD3DErrInvalidCall);
    }
  }

  auto is_rotated = [&resources](const Resource* res) -> bool {
    if (!res) {
      return false;
    }
    return std::find(resources.begin(), resources.end(), res) != resources.end();
  };

  // Rotating resource identities swaps the host handles/backing allocations
  // attached to the affected Resource objects. If any of those resources are
  // currently bound via device state, we must re-emit the corresponding binds
  // using the *new* handles so the host does not keep referencing the old
  // handles.
  size_t needed_bytes = align_up(sizeof(aerogpu_cmd_set_render_targets), 4);
  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (is_rotated(dev->textures[stage])) {
      needed_bytes += align_up(sizeof(aerogpu_cmd_set_texture), 4);
    }
  }
  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (is_rotated(dev->streams[stream].vb)) {
      needed_bytes += align_up(sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding), 4);
    }
  }
  if (is_rotated(dev->index_buffer)) {
    needed_bytes += align_up(sizeof(aerogpu_cmd_set_index_buffer), 4);
  }

  // Ensure the DMA buffer has enough space for all rebinding packets before we
  // rotate identities and track allocations; tracking may force a submission
  // split, and command-buffer splits must not occur after tracking or the
  // allocation list would be out of sync.
  if (!ensure_cmd_space(dev, needed_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  struct ResourceIdentity {
    aerogpu_handle_t handle = 0;
    uint32_t backing_alloc_id = 0;
    uint32_t backing_offset_bytes = 0;
    uint64_t share_token = 0;
    bool is_shared = false;
    bool is_shared_alias = false;
    bool locked = false;
    uint32_t locked_offset = 0;
    uint32_t locked_size = 0;
    uint32_t locked_flags = 0;
    WddmAllocationHandle wddm_hAllocation = 0;
    std::vector<uint8_t> storage;
    std::vector<uint8_t> shared_private_driver_data;
  };

  auto take_identity = [](Resource* res) -> ResourceIdentity {
    ResourceIdentity id{};
    id.handle = res->handle;
    id.backing_alloc_id = res->backing_alloc_id;
    id.backing_offset_bytes = res->backing_offset_bytes;
    id.share_token = res->share_token;
    id.is_shared = res->is_shared;
    id.is_shared_alias = res->is_shared_alias;
    id.locked = res->locked;
    id.locked_offset = res->locked_offset;
    id.locked_size = res->locked_size;
    id.locked_flags = res->locked_flags;
    id.wddm_hAllocation = res->wddm_hAllocation;
    id.storage = std::move(res->storage);
    id.shared_private_driver_data = std::move(res->shared_private_driver_data);
    return id;
  };

  auto put_identity = [](Resource* res, ResourceIdentity&& id) {
    res->handle = id.handle;
    res->backing_alloc_id = id.backing_alloc_id;
    res->backing_offset_bytes = id.backing_offset_bytes;
    res->share_token = id.share_token;
    res->is_shared = id.is_shared;
    res->is_shared_alias = id.is_shared_alias;
    res->locked = id.locked;
    res->locked_offset = id.locked_offset;
    res->locked_size = id.locked_size;
    res->locked_flags = id.locked_flags;
    res->wddm_hAllocation = id.wddm_hAllocation;
    res->storage = std::move(id.storage);
    res->shared_private_driver_data = std::move(id.shared_private_driver_data);
  };

  auto undo_rotation = [&resources, resource_count, &take_identity, &put_identity]() {
    // Undo the rotation (rotate right by one).
    ResourceIdentity undo_saved = take_identity(resources[resource_count - 1]);
    for (uint32_t i = resource_count - 1; i > 0; --i) {
      put_identity(resources[i], take_identity(resources[i - 1]));
    }
    put_identity(resources[0], std::move(undo_saved));
  };

  // Perform the identity rotation (rotate left by one).
  ResourceIdentity saved = take_identity(resources[0]);
  for (uint32_t i = 0; i + 1 < resource_count; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[resource_count - 1], std::move(saved));

  if (dev->wddm_context.hContext != 0 &&
      dev->alloc_list_tracker.list_base() != nullptr &&
      dev->alloc_list_tracker.list_capacity_effective() != 0) {
    // The rebinding packets reference multiple resources. `track_resource_allocation_locked`
    // can internally split the submission (submit+retry) when the allocation list
    // is full. If that happens mid-sequence, earlier tracked allocations would be
    // dropped and the submission would be missing required alloc-table entries.
    //
    // Pre-scan all allocations referenced by the rebinding commands and split once
    // up front when the remaining allocation-list capacity is insufficient.
    std::array<UINT, 4 + 1 + 16 + 16 + 1> unique_allocs{};
    size_t unique_alloc_len = 0;
    auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
      if (!res) {
        return;
      }
      if (res->backing_alloc_id == 0) {
        return;
      }
      if (res->wddm_hAllocation == 0) {
        return;
      }
      const UINT alloc_id = res->backing_alloc_id;
      for (size_t i = 0; i < unique_alloc_len; ++i) {
        if (unique_allocs[i] == alloc_id) {
          return;
        }
      }
      unique_allocs[unique_alloc_len++] = alloc_id;
    };

    for (uint32_t i = 0; i < 4; ++i) {
      add_alloc(dev->render_targets[i]);
    }
    add_alloc(dev->depth_stencil);
    for (uint32_t stage = 0; stage < 16; ++stage) {
      if (is_rotated(dev->textures[stage])) {
        add_alloc(dev->textures[stage]);
      }
    }
    for (uint32_t stream = 0; stream < 16; ++stream) {
      if (is_rotated(dev->streams[stream].vb)) {
        add_alloc(dev->streams[stream].vb);
      }
    }
    if (is_rotated(dev->index_buffer)) {
      add_alloc(dev->index_buffer);
    }

    const UINT needed_total = static_cast<UINT>(unique_alloc_len);
    if (needed_total != 0) {
      const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
      if (needed_total > cap) {
        logf("aerogpu-d3d9: rotate identities requires %u allocations but allocation list capacity is %u\n",
             static_cast<unsigned>(needed_total),
             static_cast<unsigned>(cap));
        undo_rotation();
        return trace.ret(E_FAIL);
      }

      UINT needed_new = 0;
      for (size_t i = 0; i < unique_alloc_len; ++i) {
        if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
          needed_new++;
        }
      }
      const UINT existing = dev->alloc_list_tracker.list_len();
      if (existing > cap || needed_new > cap - existing) {
        (void)submit(dev);
      }
    }

    // If the allocation-list pre-scan split the submission, re-check command space
    // so we don't end up splitting the command buffer after allocation tracking.
    if (!ensure_cmd_space(dev, needed_bytes)) {
      undo_rotation();
      return trace.ret(E_OUTOFMEMORY);
    }
  }

  // Track allocations referenced by the rebinding commands so the KMD/emulator
  // can resolve alloc_id -> GPA even if the submission contains only state
  // updates (no draw).
  HRESULT hr = track_render_targets_locked(dev);
  if (FAILED(hr)) {
    undo_rotation();
    return trace.ret(hr);
  }
  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (!is_rotated(dev->textures[stage])) {
      continue;
    }
    const HRESULT track_hr = track_resource_allocation_locked(dev, dev->textures[stage], /*write=*/false);
    if (FAILED(track_hr)) {
      undo_rotation();
      return trace.ret(track_hr);
    }
  }
  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (!is_rotated(dev->streams[stream].vb)) {
      continue;
    }
    const HRESULT track_hr = track_resource_allocation_locked(dev, dev->streams[stream].vb, /*write=*/false);
    if (FAILED(track_hr)) {
      undo_rotation();
      return trace.ret(track_hr);
    }
  }
  if (is_rotated(dev->index_buffer)) {
    const HRESULT track_hr = track_resource_allocation_locked(dev, dev->index_buffer, /*write=*/false);
    if (FAILED(track_hr)) {
      undo_rotation();
      return trace.ret(track_hr);
    }
  }

  // Re-emit binds so the host observes the updated handles.
  bool ok = emit_set_render_targets_locked(dev);
  for (uint32_t stage = 0; ok && stage < 16; ++stage) {
    if (!is_rotated(dev->textures[stage])) {
      continue;
    }
    auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
    if (!cmd) {
      ok = false;
      break;
    }
    cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->slot = stage;
    cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
    cmd->reserved0 = 0;
  }

  for (uint32_t stream = 0; ok && stream < 16; ++stream) {
    if (!is_rotated(dev->streams[stream].vb)) {
      continue;
    }

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
    binding.stride_bytes = dev->streams[stream].stride_bytes;
    binding.offset_bytes = dev->streams[stream].offset_bytes;
    binding.reserved0 = 0;

    auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
        dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
    if (!cmd) {
      ok = false;
      break;
    }
    cmd->start_slot = stream;
    cmd->buffer_count = 1;
  }

  if (ok && is_rotated(dev->index_buffer)) {
    auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (!cmd) {
      ok = false;
    } else {
      cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
      cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
      cmd->offset_bytes = dev->index_offset_bytes;
      cmd->reserved0 = 0;
    }
  }

  if (!ok) {
    // Preserve device/host state consistency: if we cannot emit the rebinding
    // commands (command buffer too small), undo the rotation so future draws
    // still target the host's current bindings.
    undo_rotation();
    return trace.ret(E_OUTOFMEMORY);
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_lock(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_LOCK* pLock,
    D3DDDI_LOCKEDBOX* pLockedBox) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceLock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pLock ? d3d9_trace_arg_ptr(pLock->hResource.pDrvPrivate) : 0,
                      pLock ? d3d9_trace_pack_u32_u32(d3d9_lock_offset(*pLock), d3d9_lock_size(*pLock)) : 0,
                       pLock ? static_cast<uint64_t>(d3d9_lock_flags(*pLock)) : 0);
  if (!hDevice.pDrvPrivate || !pLock || !pLockedBox) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* res = as_resource(pLock->hResource);
  if (!dev || !res) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->locked) {
    return trace.ret(E_FAIL);
  }

  const uint32_t offset = d3d9_lock_offset(*pLock);
  const uint32_t requested_size = d3d9_lock_size(*pLock);
  uint32_t size = requested_size ? requested_size : (res->size_bytes - offset);
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return trace.ret(E_INVALIDARG);
  }

  res->locked = true;
  res->locked_offset = offset;
  res->locked_size = size;
  res->locked_flags = d3d9_lock_flags(*pLock);
  res->locked_ptr = nullptr;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
    void* ptr = nullptr;
    const HRESULT hr = wddm_lock_allocation(dev->wddm_callbacks,
                                           dev->wddm_device,
                                           res->wddm_hAllocation,
                                           offset,
                                           size,
                                           res->locked_flags,
                                           &ptr,
                                           dev->wddm_context.hContext);
    if (FAILED(hr) || !ptr) {
      res->locked = false;
      res->locked_flags = 0;
      return trace.ret(FAILED(hr) ? hr : E_FAIL);
    }
    res->locked_ptr = ptr;
    d3d9_locked_box_set_ptr(pLockedBox, ptr);
  } else
#endif
  {
    if (res->storage.size() < res->size_bytes) {
      res->locked = false;
      res->locked_flags = 0;
      return trace.ret(E_FAIL);
    }
    res->locked_ptr = res->storage.data() + offset;
    d3d9_locked_box_set_ptr(pLockedBox, res->locked_ptr);
  }
  d3d9_locked_box_set_row_pitch(pLockedBox, res->row_pitch);
  d3d9_locked_box_set_slice_pitch(pLockedBox, res->slice_pitch);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_unlock(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_UNLOCK* pUnlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUnlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pUnlock ? d3d9_trace_arg_ptr(pUnlock->hResource.pDrvPrivate) : 0,
                      pUnlock ? d3d9_trace_pack_u32_u32(d3d9_unlock_offset(*pUnlock), d3d9_unlock_size(*pUnlock)) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pUnlock) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* res = as_resource(pUnlock->hResource);
  if (!dev || !res) {
    return trace.ret(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!res->locked) {
    return trace.ret(E_FAIL);
  }

  const uint32_t unlock_offset = d3d9_unlock_offset(*pUnlock);
  const uint32_t unlock_size = d3d9_unlock_size(*pUnlock);
  const uint32_t offset = unlock_offset ? unlock_offset : res->locked_offset;
  const uint32_t size = unlock_size ? unlock_size : res->locked_size;
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return trace.ret(E_INVALIDARG);
  }

  res->locked = false;
  res->locked_ptr = nullptr;

  const uint32_t locked_flags = res->locked_flags;
  res->locked_flags = 0;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (res->wddm_hAllocation != 0 && dev->wddm_device != 0) {
    const HRESULT hr =
        wddm_unlock_allocation(dev->wddm_callbacks, dev->wddm_device, res->wddm_hAllocation, dev->wddm_context.hContext);
    if (FAILED(hr)) {
      logf("aerogpu-d3d9: UnlockCb failed hr=0x%08lx alloc_id=%u hAllocation=%llu\n",
           static_cast<unsigned long>(hr),
           static_cast<unsigned>(res->backing_alloc_id),
           static_cast<unsigned long long>(res->wddm_hAllocation));
      return trace.ret(hr);
    }
  }
#endif

  // CPU writes into allocation-backed resources are observed by the host via the
  // guest physical memory. Notify the host that the backing bytes changed so it
  // can re-upload on demand.
  if (res->handle != 0 && res->backing_alloc_id != 0 && (locked_flags & kD3DLOCK_READONLY) == 0 && size) {
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
      return trace.ret(E_OUTOFMEMORY);
    }

    const HRESULT hr = track_resource_allocation_locked(dev, res, /*write=*/false);
    if (FAILED(hr)) {
      return trace.ret(hr);
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!cmd) {
      return trace.ret(E_OUTOFMEMORY);
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = static_cast<uint64_t>(offset);
    cmd->size_bytes = static_cast<uint64_t>(size);
    return trace.ret(S_OK);
  }

  // Fallback: host-allocated resources are updated by embedding raw bytes in the
  // command stream.
  if (res->handle != 0 && (locked_flags & kD3DLOCK_READONLY) == 0 && size) {
    const bool is_buffer = (res->kind == ResourceKind::Buffer);

    uint32_t upload_offset = offset;
    uint32_t upload_size = size;
    if (is_buffer) {
      const uint32_t start = upload_offset & ~3u;
      const uint64_t end_u64 = static_cast<uint64_t>(upload_offset) + static_cast<uint64_t>(upload_size);
      const uint32_t end = static_cast<uint32_t>((end_u64 + 3ull) & ~3ull);
      if (end > res->size_bytes || end < start) {
        return trace.ret(E_INVALIDARG);
      }
      upload_offset = start;
      upload_size = end - start;
    }

    const uint8_t* src = res->storage.data() + upload_offset;
    uint32_t remaining = upload_size;
    uint32_t cur_offset = upload_offset;

    while (remaining) {
      const size_t min_payload = is_buffer ? 4 : 1;
      const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + min_payload, 4);
      if (!ensure_cmd_space(dev, min_needed)) {
        return trace.ret(E_OUTOFMEMORY);
      }

      // Uploads write into the resource. Track its backing allocation so the
      // KMD/emulator can resolve the destination memory via the per-submit alloc
      // table even though we keep the patch-location list empty.
      HRESULT track_hr = track_resource_allocation_locked(dev, res, /*write=*/true);
      if (FAILED(track_hr)) {
        return trace.ret(track_hr);
      }

      // Allocation tracking may have split/flushed the submission; ensure we
      // still have room for at least a minimal upload packet before sizing the
      // next chunk.
      if (!ensure_cmd_space(dev, min_needed)) {
        return trace.ret(E_OUTOFMEMORY);
      }

      const size_t avail = dev->cmd.bytes_remaining();
      size_t chunk = 0;
      if (avail > sizeof(aerogpu_cmd_upload_resource)) {
        chunk = std::min<size_t>(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
      }

      if (is_buffer) {
        chunk &= ~static_cast<size_t>(3);
      } else {
        while (chunk && align_up(sizeof(aerogpu_cmd_upload_resource) + chunk, 4) > avail) {
          chunk--;
        }
      }
      if (!chunk) {
        submit(dev);
        continue;
      }

      auto* cmd = append_with_payload_locked<aerogpu_cmd_upload_resource>(
          dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }

      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = cur_offset;
      cmd->size_bytes = chunk;

      src += chunk;
      cur_offset += static_cast<uint32_t>(chunk);
      remaining -= static_cast<uint32_t>(chunk);
    }
  }
  return trace.ret(S_OK);
}

static bool SupportsTransfer(const Device* dev) {
  if (!dev || !dev->adapter || !dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  if ((blob.device_features & AEROGPU_UMDPRIV_FEATURE_TRANSFER) == 0) {
    return false;
  }
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 1);
}

HRESULT AEROGPU_D3D9_CALL device_get_render_target_data(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_GETRENDERTARGETDATA* pGetRenderTargetData) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetRenderTargetData,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pGetRenderTargetData ? d3d9_trace_arg_ptr(pGetRenderTargetData->hSrcResource.pDrvPrivate) : 0,
                      pGetRenderTargetData ? d3d9_trace_arg_ptr(pGetRenderTargetData->hDstResource.pDrvPrivate) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pGetRenderTargetData) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* src = as_resource(pGetRenderTargetData->hSrcResource);
  auto* dst = as_resource(pGetRenderTargetData->hDstResource);
  if (!dev || !src || !dst) {
    return trace.ret(E_INVALIDARG);
  }

  // GetRenderTargetData copies from a GPU render target/backbuffer into a
  // system-memory surface.
  if (dst->pool != kD3DPOOL_SYSTEMMEM) {
    return trace.ret(E_INVALIDARG);
  }
  if (dst->locked) {
    return trace.ret(E_FAIL);
  }

  if (src->width != dst->width || src->height != dst->height || src->format != dst->format) {
    return trace.ret(kD3DErrInvalidCall);
  }
  const uint32_t bpp = bytes_per_pixel(src->format);
  if (bpp != 4) {
    return trace.ret(kD3DErrInvalidCall);
  }
  if (!src->handle || !dst->handle) {
    return trace.ret(kD3DErrInvalidCall);
  }
  if (dst->backing_alloc_id == 0) {
    // Writeback requires a guest allocation backing the destination so the host
    // can populate the systemmem surface bytes.
    return trace.ret(kD3DErrInvalidCall);
  }

  const bool transfer_supported = SupportsTransfer(dev);

  if (!transfer_supported) {
    // Fallback: when the device does not advertise transfer/copy support, avoid
    // emitting COPY_TEXTURE2D. Instead, submit any pending GPU work and copy via
    // CPU-visible storage/allocation mappings.
    uint64_t fence = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      fence = submit(dev);
    }

    const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
    if (wait_res == FenceWaitResult::Failed) {
      return trace.ret(E_FAIL);
    }
    if (wait_res == FenceWaitResult::NotReady) {
      return trace.ret(kD3dErrWasStillDrawing);
    }

    const HRESULT hr = copy_surface_rects(dev, src, dst, /*rects=*/nullptr, /*rect_count=*/0);
    if (FAILED(hr)) {
      return trace.ret(hr);
    }

    // If the destination is allocation-backed, the host only observes CPU writes
    // when we mark the allocation dirty.
    if (dst->handle != 0 && dst->backing_alloc_id != 0 && dst->size_bytes) {
      std::lock_guard<std::mutex> lock(dev->mutex);

      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
        return trace.ret(E_OUTOFMEMORY);
      }
      const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
      if (FAILED(track_hr)) {
        return trace.ret(track_hr);
      }
      auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }
      cmd->resource_handle = dst->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = 0;
      cmd->size_bytes = dst->size_bytes;
    }

    return trace.ret(S_OK);
  }

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);

    // Ensure we can fit the copy packet before tracking allocations: allocation
    // tracking can force a submission split, and we must not split after
    // populating the allocation list for this command.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_copy_texture2d), 4))) {
      return trace.ret(E_OUTOFMEMORY);
    }

    if (track_resource_allocation_locked(dev, dst, /*write=*/true) < 0) {
      return trace.ret(E_FAIL);
    }
    if (track_resource_allocation_locked(dev, src, /*write=*/false) < 0) {
      return trace.ret(E_FAIL);
    }
    // Allocation tracking can flush/split the current submission if the runtime
    // allocation list is full. If tracking `src` forced a split, the allocation
    // list has been reset and we must re-track `dst` so the final submission
    // references both allocations.
    if (track_resource_allocation_locked(dev, dst, /*write=*/true) < 0) {
      return trace.ret(E_FAIL);
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_copy_texture2d>(dev, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!cmd) {
      return trace.ret(E_OUTOFMEMORY);
    }
    cmd->dst_texture = dst->handle;
    cmd->src_texture = src->handle;
    cmd->dst_mip_level = 0;
    cmd->dst_array_layer = 0;
    cmd->src_mip_level = 0;
    cmd->src_array_layer = 0;
    cmd->dst_x = 0;
    cmd->dst_y = 0;
    cmd->src_x = 0;
    cmd->src_y = 0;
    cmd->width = dst->width;
    cmd->height = dst->height;
    cmd->flags = AEROGPU_COPY_FLAG_WRITEBACK_DST;
    cmd->reserved0 = 0;

    fence = submit(dev);
  }

  // Wait for completion so the CPU sees final pixels.
  const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  if (wait_res == FenceWaitResult::NotReady) {
    return trace.ret(kD3dErrWasStillDrawing);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_copy_rects(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_COPYRECTS* pCopyRects) {
  const uint64_t src_ptr = pCopyRects ? d3d9_trace_arg_ptr(pCopyRects->hSrcResource.pDrvPrivate) : 0;
  const uint64_t dst_ptr = pCopyRects ? d3d9_trace_arg_ptr(pCopyRects->hDstResource.pDrvPrivate) : 0;
  const RECT* rect_list = pCopyRects ? d3d9_copy_rects_rects(*pCopyRects) : nullptr;
  const uint32_t rect_count = pCopyRects ? d3d9_copy_rects_count(*pCopyRects) : 0;
  const uint64_t rects =
      pCopyRects ? d3d9_trace_pack_u32_u32(rect_count, rect_list != nullptr ? 1u : 0u) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCopyRects, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), src_ptr, dst_ptr, rects);
  if (!hDevice.pDrvPrivate || !pCopyRects) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* src = as_resource(pCopyRects->hSrcResource);
  auto* dst = as_resource(pCopyRects->hDstResource);
  if (!dev || !src || !dst) {
    return trace.ret(E_INVALIDARG);
  }

  // Fast path: GPU -> systemmem copy (readback). If the destination is a
  // systemmem surface backed by a guest allocation, emit a host copy with
  // WRITEBACK_DST so the bytes land in guest memory for CPU LockRect.
  if (dst->pool == kD3DPOOL_SYSTEMMEM &&
      dst->backing_alloc_id != 0 &&
      SupportsTransfer(dev) &&
      src->handle != 0 &&
      dst->handle != 0 &&
      src->format == dst->format &&
      (!pCopyRects->pSrcRects || pCopyRects->rect_count == 0)) {
    const uint32_t width = std::min<uint32_t>(src->width, dst->width);
    const uint32_t height = std::min<uint32_t>(src->height, dst->height);
    if (width == 0 || height == 0) {
      return trace.ret(S_OK);
    }

    uint64_t fence = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);

      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_copy_texture2d), 4))) {
        return trace.ret(E_OUTOFMEMORY);
      }

      if (track_resource_allocation_locked(dev, dst, /*write=*/true) < 0) {
        return trace.ret(E_FAIL);
      }
      if (track_resource_allocation_locked(dev, src, /*write=*/false) < 0) {
        return trace.ret(E_FAIL);
      }
      if (track_resource_allocation_locked(dev, dst, /*write=*/true) < 0) {
        return trace.ret(E_FAIL);
      }

      auto* cmd = append_fixed_locked<aerogpu_cmd_copy_texture2d>(dev, AEROGPU_CMD_COPY_TEXTURE2D);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }
      cmd->dst_texture = dst->handle;
      cmd->src_texture = src->handle;
      cmd->dst_mip_level = 0;
      cmd->dst_array_layer = 0;
      cmd->src_mip_level = 0;
      cmd->src_array_layer = 0;
      cmd->dst_x = 0;
      cmd->dst_y = 0;
      cmd->src_x = 0;
      cmd->src_y = 0;
      cmd->width = width;
      cmd->height = height;
      cmd->flags = AEROGPU_COPY_FLAG_WRITEBACK_DST;
      cmd->reserved0 = 0;

      fence = submit(dev);
    }

    const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
    if (wait_res == FenceWaitResult::Failed) {
      return trace.ret(E_FAIL);
    }
    if (wait_res == FenceWaitResult::NotReady) {
      return trace.ret(kD3dErrWasStillDrawing);
    }
    return trace.ret(S_OK);
  }

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence = submit(dev);
  }
  const FenceWaitResult wait_res = wait_for_fence(dev, fence, /*timeout_ms=*/2000);
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  if (wait_res == FenceWaitResult::NotReady) {
    return trace.ret(kD3dErrWasStillDrawing);
  }

  const HRESULT hr = copy_surface_rects(dev, src, dst, rect_list, rect_count);
  if (FAILED(hr)) {
    return trace.ret(hr);
  }

  // If the destination is allocation-backed, the host only observes CPU writes
  // when we mark the allocation dirty.
  if (dst->handle != 0 && dst->backing_alloc_id != 0 && dst->size_bytes) {
    std::lock_guard<std::mutex> lock(dev->mutex);

    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
      return trace.ret(E_OUTOFMEMORY);
    }
    const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
    if (FAILED(track_hr)) {
      return trace.ret(track_hr);
    }
    auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!cmd) {
      return trace.ret(E_OUTOFMEMORY);
    }
    cmd->resource_handle = dst->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = 0;
    cmd->size_bytes = dst->size_bytes;
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_render_target(
    D3DDDI_HDEVICE hDevice,
    uint32_t slot,
    D3DDDI_HRESOURCE hSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetRenderTarget,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(slot),
                      d3d9_trace_arg_ptr(hSurface.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (slot >= 4) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  Resource* saved_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};

  if (surf && slot > 0) {
    for (uint32_t i = 0; i < slot; ++i) {
      if (!dev->render_targets[i]) {
        return trace.ret(kD3DErrInvalidCall);
      }
    }
  }

  dev->render_targets[slot] = surf;
  if (!surf) {
    // Maintain contiguity: clearing an earlier slot implicitly clears any later
    // render targets so the host never sees a gapped binding.
    for (uint32_t i = slot + 1; i < 4; ++i) {
      dev->render_targets[i] = nullptr;
    }
  }

  bool changed = false;
  for (uint32_t i = 0; i < 4; ++i) {
    if (dev->render_targets[i] != saved_rts[i]) {
      changed = true;
      break;
    }
  }
  if (!changed) {
    stateblock_record_render_target_locked(dev, slot, dev->render_targets[slot]);
    if (!surf) {
      for (uint32_t i = slot + 1; i < 4; ++i) {
        stateblock_record_render_target_locked(dev, i, dev->render_targets[i]);
      }
    }
    return trace.ret(S_OK);
  }

  if (!emit_set_render_targets_locked(dev)) {
    for (uint32_t i = 0; i < 4; ++i) {
      dev->render_targets[i] = saved_rts[i];
    }
    return trace.ret(E_OUTOFMEMORY);
  }
  stateblock_record_render_target_locked(dev, slot, dev->render_targets[slot]);
  if (!surf) {
    for (uint32_t i = slot + 1; i < 4; ++i) {
      stateblock_record_render_target_locked(dev, i, dev->render_targets[i]);
    }
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_depth_stencil(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetDepthStencil,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hSurface.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->depth_stencil == surf) {
    stateblock_record_depth_stencil_locked(dev, surf);
    return trace.ret(S_OK);
  }
  dev->depth_stencil = surf;
  if (!emit_set_render_targets_locked(dev)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  stateblock_record_depth_stencil_locked(dev, surf);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_viewport(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIVIEWPORTINFO* pViewport) {
  const uint64_t xy = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->X), f32_bits(pViewport->Y)) : 0;
  const uint64_t wh = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->Width), f32_bits(pViewport->Height)) : 0;
  const uint64_t zz = pViewport ? d3d9_trace_pack_u32_u32(f32_bits(pViewport->MinZ), f32_bits(pViewport->MaxZ)) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetViewport, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), xy, wh, zz);
  if (!hDevice.pDrvPrivate || !pViewport) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->viewport = *pViewport;
  stateblock_record_viewport_locked(dev, dev->viewport);

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_viewport>(dev, AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->x_f32 = f32_bits(pViewport->X);
  cmd->y_f32 = f32_bits(pViewport->Y);
  cmd->width_f32 = f32_bits(pViewport->Width);
  cmd->height_f32 = f32_bits(pViewport->Height);
  cmd->min_depth_f32 = f32_bits(pViewport->MinZ);
  cmd->max_depth_f32 = f32_bits(pViewport->MaxZ);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_scissor(
    D3DDDI_HDEVICE hDevice,
    const RECT* pRect,
    BOOL enabled) {
  const uint64_t lt = pRect ? d3d9_trace_pack_u32_u32(static_cast<uint32_t>(pRect->left), static_cast<uint32_t>(pRect->top)) : 0;
  const uint64_t rb =
      pRect ? d3d9_trace_pack_u32_u32(static_cast<uint32_t>(pRect->right), static_cast<uint32_t>(pRect->bottom)) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::DeviceSetScissorRect, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), lt, rb, static_cast<uint64_t>(enabled));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (pRect) {
    dev->scissor_rect = *pRect;
  }
  dev->scissor_enabled = enabled;
  stateblock_record_scissor_locked(dev, dev->scissor_rect, dev->scissor_enabled);

  int32_t x = 0;
  int32_t y = 0;
  int32_t w = 0x7FFFFFFF;
  int32_t h = 0x7FFFFFFF;
  if (enabled && pRect) {
    x = static_cast<int32_t>(pRect->left);
    y = static_cast<int32_t>(pRect->top);
    w = static_cast<int32_t>(pRect->right - pRect->left);
    h = static_cast<int32_t>(pRect->bottom - pRect->top);
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_scissor>(dev, AEROGPU_CMD_SET_SCISSOR);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->x = x;
  cmd->y = y;
  cmd->width = w;
  cmd->height = h;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_texture(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    D3DDDI_HRESOURCE hTexture) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetTexture,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_arg_ptr(hTexture.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stage >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* tex = as_resource(hTexture);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->textures[stage] == tex) {
    stateblock_record_texture_locked(dev, stage, tex);
    return trace.ret(S_OK);
  }
  dev->textures[stage] = tex;
  stateblock_record_texture_locked(dev, stage, tex);

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->texture = tex ? tex->handle : 0;
  cmd->reserved0 = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_sampler_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetSamplerState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      static_cast<uint64_t>(state),
                      static_cast<uint64_t>(value));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stage >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (stage < 16 && state < 16) {
    dev->sampler_states[stage][state] = value;
  }
  stateblock_record_sampler_state_locked(dev, stage, state, value);

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_sampler_state>(dev, AEROGPU_CMD_SET_SAMPLER_STATE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->state = state;
  cmd->value = value;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_render_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t state,
    uint32_t value) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetRenderState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(state),
                      static_cast<uint64_t>(value),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (state < 256) {
    dev->render_states[state] = value;
  }
  stateblock_record_render_state_locked(dev, state, value);

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_state>(dev, AEROGPU_CMD_SET_RENDER_STATE);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->state = state;
  cmd->value = value;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    const void* pDecl,
    uint32_t decl_size,
    D3D9DDI_HVERTEXDECL* phDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(decl_size),
                      d3d9_trace_arg_ptr(pDecl),
                      d3d9_trace_arg_ptr(phDecl));
  if (!hDevice.pDrvPrivate || !pDecl || !phDecl || decl_size == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto decl = std::make_unique<VertexDecl>();
  decl->handle = allocate_global_handle(dev->adapter);
  decl->blob.resize(decl_size);
  std::memcpy(decl->blob.data(), pDecl, decl_size);

  if (!emit_create_input_layout_locked(dev, decl.get())) {
    return trace.ret(E_OUTOFMEMORY);
  }

  phDecl->pDrvPrivate = decl.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HVERTEXDECL hDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hDecl.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!emit_set_input_layout_locked(dev, decl)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Some runtimes implement SetFVF by synthesizing a declaration and calling
  // SetVertexDecl. Detect the specific `XYZRHW | DIFFUSE` layout used by the
  // Win7 bring-up test so we can enable the fixed-function fallback path even
  // if `pfnSetFVF` is not invoked.
  bool matches_fvf_xyzrhw_diffuse = false;
  if (decl && decl->blob.size() >= sizeof(D3DVERTEXELEMENT9_COMPAT) * 3) {
    const auto* elems = reinterpret_cast<const D3DVERTEXELEMENT9_COMPAT*>(decl->blob.data());
    const auto& e0 = elems[0];
    const auto& e1 = elems[1];
    const auto& e2 = elems[2];

    const bool e0_ok = (e0.Stream == 0) && (e0.Offset == 0) && (e0.Type == kD3dDeclTypeFloat4) &&
                       (e0.Method == kD3dDeclMethodDefault) &&
                       (e0.Usage == kD3dDeclUsagePositionT || e0.Usage == 0) && (e0.UsageIndex == 0);
    const bool e1_ok = (e1.Stream == 0) && (e1.Offset == 16) && (e1.Type == kD3dDeclTypeD3dColor) &&
                       (e1.Method == kD3dDeclMethodDefault) && (e1.Usage == kD3dDeclUsageColor) && (e1.UsageIndex == 0);
    const bool e2_ok = (e2.Stream == 0xFF) && (e2.Type == kD3dDeclTypeUnused);
    matches_fvf_xyzrhw_diffuse = e0_ok && e1_ok && e2_ok;
  }
  dev->fvf = matches_fvf_xyzrhw_diffuse ? kSupportedFvfXyzrhwDiffuse : 0;
  stateblock_record_vertex_decl_locked(dev, decl, dev->fvf);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HVERTEXDECL hDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hDecl.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);
  if (!dev || !decl) {
    delete decl;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->vertex_decl == decl) {
    dev->vertex_decl = nullptr;
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_set_input_layout>(dev, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
      cmd->input_layout_handle = 0;
      cmd->reserved0 = 0;
    }
  }
  (void)emit_destroy_input_layout_locked(dev, decl->handle);
  delete decl;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_fvf(D3DDDI_HDEVICE hDevice, uint32_t fvf) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetFVF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(fvf),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (fvf == dev->fvf) {
    stateblock_record_vertex_decl_locked(dev, dev->vertex_decl, dev->fvf);
    return trace.ret(S_OK);
  }

  if (fvf != 0 && fvf != kSupportedFvfXyzrhwDiffuse) {
    return trace.ret(D3DERR_INVALIDCALL);
  }

  if (fvf == 0) {
    dev->fvf = 0;
    stateblock_record_vertex_decl_locked(dev, dev->vertex_decl, dev->fvf);
    return trace.ret(S_OK);
  }

  if (!dev->fvf_vertex_decl) {
    // Build the declaration for this FVF. For bring-up we only support the
    // `XYZRHW | DIFFUSE` path used by the Win7 d3d9ex_triangle test.
    const D3DVERTEXELEMENT9_COMPAT elems[] = {
        // stream, offset, type, method, usage, usage_index
        {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
        {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
    };

    dev->fvf_vertex_decl = create_internal_vertex_decl_locked(dev, elems, sizeof(elems));
    if (!dev->fvf_vertex_decl) {
      return trace.ret(E_OUTOFMEMORY);
    }
  }

  if (!emit_set_input_layout_locked(dev, dev->fvf_vertex_decl)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  dev->fvf = fvf;
  stateblock_record_vertex_decl_locked(dev, dev->fvf_vertex_decl, dev->fvf);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_shader(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    const void* pBytecode,
    uint32_t bytecode_size,
    D3D9DDI_HSHADER* phShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      static_cast<uint64_t>(bytecode_size),
                      d3d9_trace_arg_ptr(pBytecode));
  if (!hDevice.pDrvPrivate || !pBytecode || !phShader || bytecode_size == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto sh = std::make_unique<Shader>();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = stage;
  sh->bytecode.resize(bytecode_size);
  std::memcpy(sh->bytecode.data(), pBytecode, bytecode_size);

  if (!emit_create_shader_locked(dev, sh.get())) {
    return trace.ret(E_OUTOFMEMORY);
  }

  phShader->pDrvPrivate = sh.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_shader(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    D3D9DDI_HSHADER hShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_arg_ptr(hShader.pDrvPrivate),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);

  std::lock_guard<std::mutex> lock(dev->mutex);

  Shader** user_slot = (stage == kD3d9ShaderStageVs) ? &dev->user_vs : &dev->user_ps;
  if (*user_slot == sh) {
    stateblock_record_shader_locked(dev, stage, sh);
    return trace.ret(S_OK);
  }

  *user_slot = sh;
  stateblock_record_shader_locked(dev, stage, sh);

  // Bind exactly what the runtime requested. Fixed-function fallbacks are
  // re-bound lazily at draw time when `user_vs/user_ps` are both null.
  dev->vs = dev->user_vs;
  dev->ps = dev->user_ps;

  if (!emit_bind_shaders_locked(dev)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_shader(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSHADER hShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hShader.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);
  if (!dev || !sh) {
    delete sh;
    return trace.ret(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  bool bindings_changed = false;

  // The runtime may destroy a shader while it is still bound. Clear both the
  // public "user" bindings and the currently-bound shader slots so subsequent
  // draws can re-bind the fixed-function fallback if needed.
  if (dev->user_vs == sh) {
    dev->user_vs = nullptr;
    bindings_changed = true;
  }
  if (dev->user_ps == sh) {
    dev->user_ps = nullptr;
    bindings_changed = true;
  }
  if (dev->vs == sh) {
    dev->vs = nullptr;
    bindings_changed = true;
  }
  if (dev->ps == sh) {
    dev->ps = nullptr;
    bindings_changed = true;
  }

  if (bindings_changed) {
    (void)emit_bind_shaders_locked(dev);
  }
  (void)emit_destroy_shader_locked(dev, sh->handle);
  delete sh;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_shader_const_f(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t start_reg,
    const float* pData,
    uint32_t vec4_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetShaderConstF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stage),
                      d3d9_trace_pack_u32_u32(start_reg, vec4_count),
                      d3d9_trace_arg_ptr(pData));
  if (!hDevice.pDrvPrivate || !pData || vec4_count == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  float* dst = (stage == kD3d9ShaderStageVs) ? dev->vs_consts_f : dev->ps_consts_f;
  if (start_reg < 256) {
    const uint32_t write_regs = std::min(vec4_count, 256u - start_reg);
    std::memcpy(dst + start_reg * 4, pData, static_cast<size_t>(write_regs) * 4 * sizeof(float));
  }
  stateblock_record_shader_const_f_locked(dev, stage, start_reg, pData, vec4_count);

  const size_t payload_size = static_cast<size_t>(vec4_count) * 4 * sizeof(float);
  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_shader_constants_f>(
      dev, AEROGPU_CMD_SET_SHADER_CONSTANTS_F, pData, payload_size);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->stage = d3d9_stage_to_aerogpu_stage(stage);
  cmd->start_register = start_reg;
  cmd->vec4_count = vec4_count;
  cmd->reserved0 = 0;

  return trace.ret(S_OK);
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
// -----------------------------------------------------------------------------
// State block DDIs
// -----------------------------------------------------------------------------

static void stateblock_init_for_type_locked(Device* dev, StateBlock* sb, uint32_t type_u32) {
  if (!dev || !sb) {
    return;
  }

  // Reset to a deterministic baseline.
  *sb = StateBlock{};

  // D3DSTATEBLOCKTYPE values (d3d9types.h):
  //   D3DSBT_ALL = 1
  //   D3DSBT_PIXELSTATE = 2
  //   D3DSBT_VERTEXSTATE = 3
  const bool is_all = (type_u32 == 1u) || (type_u32 == 0u);
  const bool is_pixel = is_all || (type_u32 == 2u);
  const bool is_vertex = is_all || (type_u32 == 3u);

  // Render states are treated as common state: include them in all block types
  // we support since the UMD forwards them generically.
  for (uint32_t i = 0; i < 256; ++i) {
    sb->render_state_mask.set(i);
    sb->render_state_values[i] = dev->render_states[i];
  }

  if (is_pixel) {
    for (uint32_t stage = 0; stage < 16; ++stage) {
      sb->texture_mask.set(stage);
      sb->textures[stage] = dev->textures[stage];
      for (uint32_t s = 0; s < 16; ++s) {
        const uint32_t idx = stage * 16u + s;
        sb->sampler_state_mask.set(idx);
        sb->sampler_state_values[idx] = dev->sampler_states[stage][s];
      }
    }

    for (uint32_t i = 0; i < 4; ++i) {
      sb->render_target_mask.set(i);
      sb->render_targets[i] = dev->render_targets[i];
    }
    sb->depth_stencil_set = true;
    sb->depth_stencil = dev->depth_stencil;

    sb->viewport_set = true;
    sb->viewport = dev->viewport;
    sb->scissor_set = true;
    sb->scissor_rect = dev->scissor_rect;
    sb->scissor_enabled = dev->scissor_enabled;

    sb->user_ps_set = true;
    sb->user_ps = dev->user_ps;
    for (uint32_t r = 0; r < 256; ++r) {
      sb->ps_const_mask.set(r);
    }
    std::memcpy(sb->ps_consts.data(), dev->ps_consts_f, sizeof(float) * 256u * 4u);
  }

  if (is_vertex) {
    sb->vertex_decl_set = true;
    sb->vertex_decl = dev->vertex_decl;
    sb->fvf_set = true;
    sb->fvf = dev->fvf;

    for (uint32_t stream = 0; stream < 16; ++stream) {
      sb->stream_mask.set(stream);
      sb->streams[stream] = dev->streams[stream];
    }

    sb->index_buffer_set = true;
    sb->index_buffer = dev->index_buffer;
    sb->index_format = dev->index_format;
    sb->index_offset_bytes = dev->index_offset_bytes;

    sb->user_vs_set = true;
    sb->user_vs = dev->user_vs;
    for (uint32_t r = 0; r < 256; ++r) {
      sb->vs_const_mask.set(r);
    }
    std::memcpy(sb->vs_consts.data(), dev->vs_consts_f, sizeof(float) * 256u * 4u);
  }
}

static void stateblock_capture_locked(Device* dev, StateBlock* sb) {
  if (!dev || !sb) {
    return;
  }

  for (uint32_t i = 0; i < 256; ++i) {
    if (sb->render_state_mask.test(i)) {
      sb->render_state_values[i] = dev->render_states[i];
    }
  }

  for (uint32_t idx = 0; idx < 16u * 16u; ++idx) {
    if (sb->sampler_state_mask.test(idx)) {
      const uint32_t stage = idx / 16u;
      const uint32_t s = idx % 16u;
      sb->sampler_state_values[idx] = dev->sampler_states[stage][s];
    }
  }

  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (sb->texture_mask.test(stage)) {
      sb->textures[stage] = dev->textures[stage];
    }
  }

  for (uint32_t i = 0; i < 4; ++i) {
    if (sb->render_target_mask.test(i)) {
      sb->render_targets[i] = dev->render_targets[i];
    }
  }
  if (sb->depth_stencil_set) {
    sb->depth_stencil = dev->depth_stencil;
  }

  if (sb->viewport_set) {
    sb->viewport = dev->viewport;
  }
  if (sb->scissor_set) {
    sb->scissor_rect = dev->scissor_rect;
    sb->scissor_enabled = dev->scissor_enabled;
  }

  if (sb->vertex_decl_set) {
    sb->vertex_decl = dev->vertex_decl;
  }
  if (sb->fvf_set) {
    sb->fvf = dev->fvf;
  }

  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (sb->stream_mask.test(stream)) {
      sb->streams[stream] = dev->streams[stream];
    }
  }

  if (sb->index_buffer_set) {
    sb->index_buffer = dev->index_buffer;
    sb->index_format = dev->index_format;
    sb->index_offset_bytes = dev->index_offset_bytes;
  }

  if (sb->user_vs_set) {
    sb->user_vs = dev->user_vs;
  }
  if (sb->user_ps_set) {
    sb->user_ps = dev->user_ps;
  }

  for (uint32_t r = 0; r < 256; ++r) {
    if (sb->vs_const_mask.test(r)) {
      std::memcpy(sb->vs_consts.data() + static_cast<size_t>(r) * 4,
                  dev->vs_consts_f + static_cast<size_t>(r) * 4,
                  4 * sizeof(float));
    }
    if (sb->ps_const_mask.test(r)) {
      std::memcpy(sb->ps_consts.data() + static_cast<size_t>(r) * 4,
                  dev->ps_consts_f + static_cast<size_t>(r) * 4,
                  4 * sizeof(float));
    }
  }
}

static HRESULT stateblock_apply_locked(Device* dev, const StateBlock* sb) {
  if (!dev || !sb) {
    return E_INVALIDARG;
  }

  // Render targets / depth-stencil first.
  if (sb->render_target_mask.any() || sb->depth_stencil_set) {
    Resource* old_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};
    Resource* old_ds = dev->depth_stencil;

    for (uint32_t slot = 0; slot < 4; ++slot) {
      if (!sb->render_target_mask.test(slot)) {
        continue;
      }

      Resource* rt = sb->render_targets[slot];
      if (rt && slot > 0) {
        for (uint32_t i = 0; i < slot; ++i) {
          if (!dev->render_targets[i]) {
            return kD3DErrInvalidCall;
          }
        }
      }

      dev->render_targets[slot] = rt;
      if (!rt) {
        // Maintain contiguity: clearing an earlier slot implicitly clears any
        // later slots.
        for (uint32_t i = slot + 1; i < 4; ++i) {
          dev->render_targets[i] = nullptr;
        }
      }
    }

    if (sb->depth_stencil_set) {
      dev->depth_stencil = sb->depth_stencil;
    }

    bool changed = (dev->depth_stencil != old_ds);
    for (uint32_t i = 0; i < 4 && !changed; ++i) {
      changed = (dev->render_targets[i] != old_rts[i]);
    }

    if (changed) {
      if (!emit_set_render_targets_locked(dev)) {
        dev->depth_stencil = old_ds;
        for (uint32_t i = 0; i < 4; ++i) {
          dev->render_targets[i] = old_rts[i];
        }
        return E_OUTOFMEMORY;
      }
    }

    for (uint32_t i = 0; i < 4; ++i) {
      if (sb->render_target_mask.test(i)) {
        stateblock_record_render_target_locked(dev, i, dev->render_targets[i]);
      }
    }
    if (sb->depth_stencil_set) {
      stateblock_record_depth_stencil_locked(dev, dev->depth_stencil);
    }
  }

  if (sb->viewport_set) {
    dev->viewport = sb->viewport;
    auto* cmd = append_fixed_locked<aerogpu_cmd_set_viewport>(dev, AEROGPU_CMD_SET_VIEWPORT);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->x_f32 = f32_bits(sb->viewport.X);
    cmd->y_f32 = f32_bits(sb->viewport.Y);
    cmd->width_f32 = f32_bits(sb->viewport.Width);
    cmd->height_f32 = f32_bits(sb->viewport.Height);
    cmd->min_depth_f32 = f32_bits(sb->viewport.MinZ);
    cmd->max_depth_f32 = f32_bits(sb->viewport.MaxZ);
    stateblock_record_viewport_locked(dev, dev->viewport);
  }

  if (sb->scissor_set) {
    dev->scissor_rect = sb->scissor_rect;
    dev->scissor_enabled = sb->scissor_enabled;

    int32_t x = 0;
    int32_t y = 0;
    int32_t w = 0x7FFFFFFF;
    int32_t h = 0x7FFFFFFF;
    if (dev->scissor_enabled) {
      x = static_cast<int32_t>(dev->scissor_rect.left);
      y = static_cast<int32_t>(dev->scissor_rect.top);
      w = static_cast<int32_t>(dev->scissor_rect.right - dev->scissor_rect.left);
      h = static_cast<int32_t>(dev->scissor_rect.bottom - dev->scissor_rect.top);
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_set_scissor>(dev, AEROGPU_CMD_SET_SCISSOR);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->x = x;
    cmd->y = y;
    cmd->width = w;
    cmd->height = h;
    stateblock_record_scissor_locked(dev, dev->scissor_rect, dev->scissor_enabled);
  }

  // Render states.
  for (uint32_t i = 0; i < 256; ++i) {
    if (!sb->render_state_mask.test(i)) {
      continue;
    }
    dev->render_states[i] = sb->render_state_values[i];
    auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_state>(dev, AEROGPU_CMD_SET_RENDER_STATE);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->state = i;
    cmd->value = sb->render_state_values[i];
    stateblock_record_render_state_locked(dev, i, sb->render_state_values[i]);
  }

  // Samplers/textures.
  for (uint32_t stage = 0; stage < 16; ++stage) {
    if (sb->texture_mask.test(stage)) {
      Resource* tex = sb->textures[stage];
      dev->textures[stage] = tex;
      auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->texture = tex ? tex->handle : 0;
      cmd->reserved0 = 0;
      stateblock_record_texture_locked(dev, stage, tex);
    }

    for (uint32_t s = 0; s < 16; ++s) {
      const uint32_t idx = stage * 16u + s;
      if (!sb->sampler_state_mask.test(idx)) {
        continue;
      }
      const uint32_t value = sb->sampler_state_values[idx];
      dev->sampler_states[stage][s] = value;
      auto* cmd = append_fixed_locked<aerogpu_cmd_set_sampler_state>(dev, AEROGPU_CMD_SET_SAMPLER_STATE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = stage;
      cmd->state = s;
      cmd->value = value;
      stateblock_record_sampler_state_locked(dev, stage, s, value);
    }
  }

  // Input layout / FVF.
  if (sb->vertex_decl_set) {
    if (!emit_set_input_layout_locked(dev, sb->vertex_decl)) {
      return E_OUTOFMEMORY;
    }
  }
  if (sb->fvf_set) {
    dev->fvf = sb->fvf;
  }
  if (sb->vertex_decl_set || sb->fvf_set) {
    stateblock_record_vertex_decl_locked(dev, dev->vertex_decl, dev->fvf);
  }

  // VB streams.
  for (uint32_t stream = 0; stream < 16; ++stream) {
    if (!sb->stream_mask.test(stream)) {
      continue;
    }
    const DeviceStateStream& ss = sb->streams[stream];
    if (!emit_set_stream_source_locked(dev, stream, ss.vb, ss.offset_bytes, ss.stride_bytes)) {
      return E_OUTOFMEMORY;
    }
    stateblock_record_stream_source_locked(dev, stream, dev->streams[stream]);
  }

  // Index buffer.
  if (sb->index_buffer_set) {
    dev->index_buffer = sb->index_buffer;
    dev->index_format = sb->index_format;
    dev->index_offset_bytes = sb->index_offset_bytes;
    stateblock_record_index_buffer_locked(dev, dev->index_buffer, dev->index_format, dev->index_offset_bytes);

    auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
    cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
    cmd->offset_bytes = dev->index_offset_bytes;
    cmd->reserved0 = 0;
  }

  // Shaders.
  bool shaders_dirty = false;
  if (sb->user_vs_set && dev->user_vs != sb->user_vs) {
    dev->user_vs = sb->user_vs;
    shaders_dirty = true;
  }
  if (sb->user_ps_set && dev->user_ps != sb->user_ps) {
    dev->user_ps = sb->user_ps;
    shaders_dirty = true;
  }

  // If ApplyStateBlock is invoked while Begin/EndStateBlock recording is active,
  // we must record the shader bindings even when they are already bound (no-op
  // apply). Otherwise, the recorded state block would omit shader state and
  // would not reproduce the intended bindings when applied later.
  if (sb->user_vs_set) {
    stateblock_record_shader_locked(dev, kD3d9ShaderStageVs, dev->user_vs);
  }
  if (sb->user_ps_set) {
    stateblock_record_shader_locked(dev, kD3d9ShaderStagePs, dev->user_ps);
  }
  if (shaders_dirty) {
    dev->vs = dev->user_vs;
    dev->ps = dev->user_ps;
    if (!emit_bind_shaders_locked(dev)) {
      return E_OUTOFMEMORY;
    }
  }

  // Shader constants.
  auto apply_consts = [&](uint32_t stage,
                          const std::bitset<256>& mask,
                          const float* src,
                          float* dst) -> HRESULT {
    uint32_t reg = 0;
    while (reg < 256) {
      if (!mask.test(reg)) {
        ++reg;
        continue;
      }
      uint32_t start = reg;
      uint32_t end = reg;
      while (end + 1 < 256 && mask.test(end + 1)) {
        ++end;
      }
      const uint32_t count = (end - start + 1);
      std::memcpy(dst + static_cast<size_t>(start) * 4,
                  src + static_cast<size_t>(start) * 4,
                  static_cast<size_t>(count) * 4 * sizeof(float));

      const float* payload = src + static_cast<size_t>(start) * 4;
      const size_t payload_size = static_cast<size_t>(count) * 4 * sizeof(float);
      auto* cmd = append_with_payload_locked<aerogpu_cmd_set_shader_constants_f>(
          dev, AEROGPU_CMD_SET_SHADER_CONSTANTS_F, payload, payload_size);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->stage = d3d9_stage_to_aerogpu_stage(stage);
      cmd->start_register = start;
      cmd->vec4_count = count;
      cmd->reserved0 = 0;

      stateblock_record_shader_const_f_locked(dev, stage, start, payload, count);

      reg = end + 1;
    }
    return S_OK;
  };

  if (sb->vs_const_mask.any()) {
    const HRESULT hr = apply_consts(kD3d9ShaderStageVs, sb->vs_const_mask, sb->vs_consts.data(), dev->vs_consts_f);
    if (FAILED(hr)) {
      return hr;
    }
  }
  if (sb->ps_const_mask.any()) {
    const HRESULT hr = apply_consts(kD3d9ShaderStagePs, sb->ps_const_mask, sb->ps_consts.data(), dev->ps_consts_f);
    if (FAILED(hr)) {
      return hr;
    }
  }

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_begin_state_block(D3DDDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceBeginStateBlock, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->recording_state_block) {
    return trace.ret(kD3DErrInvalidCall);
  }

  try {
    dev->recording_state_block = new StateBlock();
  } catch (...) {
    dev->recording_state_block = nullptr;
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_end_state_block(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK* phStateBlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceEndStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(phStateBlock),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !phStateBlock) {
    return trace.ret(E_INVALIDARG);
  }
  phStateBlock->pDrvPrivate = nullptr;

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!dev->recording_state_block) {
    return trace.ret(kD3DErrInvalidCall);
  }

  phStateBlock->pDrvPrivate = dev->recording_state_block;
  dev->recording_state_block = nullptr;
  return trace.ret(S_OK);
}

template <typename CreateArgsT>
HRESULT device_create_state_block_from_args(D3DDDI_HDEVICE hDevice, CreateArgsT* pCreateStateBlock) {
  uint32_t type_u32 = 1u; // D3DSBT_ALL
  if (pCreateStateBlock) {
    if constexpr (aerogpu_d3d9_has_member_StateBlockType<CreateArgsT>::value) {
      type_u32 = static_cast<uint32_t>(pCreateStateBlock->StateBlockType);
    } else if constexpr (aerogpu_d3d9_has_member_Type<CreateArgsT>::value) {
      type_u32 = static_cast<uint32_t>(pCreateStateBlock->Type);
    } else if constexpr (aerogpu_d3d9_has_member_type<CreateArgsT>::value) {
      type_u32 = static_cast<uint32_t>(pCreateStateBlock->type);
    }
  }
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type_u32),
                      d3d9_trace_arg_ptr(pCreateStateBlock),
                      0);
  if (!hDevice.pDrvPrivate || !pCreateStateBlock) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  // Resolve the output handle field.
  if constexpr (!aerogpu_d3d9_has_member_hStateBlock<CreateArgsT>::value) {
    return trace.ret(E_INVALIDARG);
  }

  pCreateStateBlock->hStateBlock.pDrvPrivate = nullptr;

  std::unique_ptr<StateBlock> sb;
  try {
    sb = std::make_unique<StateBlock>();
  } catch (...) {
    return trace.ret(E_OUTOFMEMORY);
  }

  stateblock_init_for_type_locked(dev, sb.get(), type_u32);
  pCreateStateBlock->hStateBlock.pDrvPrivate = sb.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_state_block(D3DDDI_HDEVICE hDevice,
                                                    uint32_t type_u32,
                                                    D3D9DDI_HSTATEBLOCK* phStateBlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type_u32),
                      d3d9_trace_arg_ptr(phStateBlock),
                      0);
  if (!hDevice.pDrvPrivate || !phStateBlock) {
    return trace.ret(E_INVALIDARG);
  }
  phStateBlock->pDrvPrivate = nullptr;

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  std::unique_ptr<StateBlock> sb;
  try {
    sb = std::make_unique<StateBlock>();
  } catch (...) {
    return trace.ret(E_OUTOFMEMORY);
  }

  stateblock_init_for_type_locked(dev, sb.get(), type_u32);
  phStateBlock->pDrvPrivate = sb.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_delete_state_block(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDeleteStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hStateBlock.pDrvPrivate),
                      0,
                      0);
  (void)as_device(hDevice);
  delete as_state_block(hStateBlock);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_capture_state_block(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCaptureStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hStateBlock.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* sb = as_state_block(hStateBlock);
  if (!sb) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  stateblock_capture_locked(dev, sb);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_apply_state_block(D3DDDI_HDEVICE hDevice, D3D9DDI_HSTATEBLOCK hStateBlock) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceApplyStateBlock,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hStateBlock.pDrvPrivate),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* sb = as_state_block(hStateBlock);
  if (!sb) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(stateblock_apply_locked(dev, sb));
}

template <typename ValidateArgsT>
HRESULT device_validate_device_from_args(D3DDDI_HDEVICE hDevice, ValidateArgsT* pValidateDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceValidateDevice,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pValidateDevice),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pValidateDevice) {
    return trace.ret(E_INVALIDARG);
  }

  // Conservative: we currently report a single pass for the supported shader
  // pipeline. Unknown/legacy state is forwarded to the host, which may choose
  // to emulate it.
  if constexpr (aerogpu_d3d9_has_member_pNumPasses<ValidateArgsT>::value) {
    if (pValidateDevice->pNumPasses) {
      *pValidateDevice->pNumPasses = 1;
    }
  } else if constexpr (aerogpu_d3d9_has_member_NumPasses<ValidateArgsT>::value) {
    pValidateDevice->NumPasses = 1;
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_validate_device(D3DDDI_HDEVICE hDevice, uint32_t* pNumPasses) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceValidateDevice,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pNumPasses),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pNumPasses) {
    return trace.ret(E_INVALIDARG);
  }
  *pNumPasses = 1;
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_create_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_create_state_block_from_args(args...);
  } else if constexpr (sizeof...(Args) == 3) {
    return device_create_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename... Args>
HRESULT device_delete_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_delete_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename... Args>
HRESULT device_capture_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_capture_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename... Args>
HRESULT device_apply_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_apply_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename... Args>
HRESULT device_begin_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 1) {
    return device_begin_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename... Args>
HRESULT device_end_state_block_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_end_state_block(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename T>
HRESULT device_validate_device_dispatch(D3DDDI_HDEVICE hDevice, T* arg) {
  // Could be either `DWORD*` or a ValidateDevice args struct pointer.
  if constexpr (std::is_integral_v<T>) {
    return device_validate_device(hDevice, reinterpret_cast<uint32_t*>(arg));
  } else {
    return device_validate_device_from_args(hDevice, arg);
  }
}

template <typename... Args>
HRESULT device_validate_device_dispatch(Args... /*args*/) {
  return D3DERR_NOTAVAILABLE;
}

template <typename Fn>
struct aerogpu_d3d9_impl_pfnCreateStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnCreateStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnCreateStateBlock(Args... args) {
    return static_cast<Ret>(device_create_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnCreateStateBlock<Ret(*)(Args...)> {
  static Ret pfnCreateStateBlock(Args... args) {
    return static_cast<Ret>(device_create_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnDeleteStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnDeleteStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnDeleteStateBlock(Args... args) {
    return static_cast<Ret>(device_delete_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnDeleteStateBlock<Ret(*)(Args...)> {
  static Ret pfnDeleteStateBlock(Args... args) {
    return static_cast<Ret>(device_delete_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnCaptureStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnCaptureStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnCaptureStateBlock(Args... args) {
    return static_cast<Ret>(device_capture_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnCaptureStateBlock<Ret(*)(Args...)> {
  static Ret pfnCaptureStateBlock(Args... args) {
    return static_cast<Ret>(device_capture_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnApplyStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnApplyStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnApplyStateBlock(Args... args) {
    return static_cast<Ret>(device_apply_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnApplyStateBlock<Ret(*)(Args...)> {
  static Ret pfnApplyStateBlock(Args... args) {
    return static_cast<Ret>(device_apply_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnBeginStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnBeginStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnBeginStateBlock(Args... args) {
    return static_cast<Ret>(device_begin_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnBeginStateBlock<Ret(*)(Args...)> {
  static Ret pfnBeginStateBlock(Args... args) {
    return static_cast<Ret>(device_begin_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnEndStateBlock;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnEndStateBlock<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnEndStateBlock(Args... args) {
    return static_cast<Ret>(device_end_state_block_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnEndStateBlock<Ret(*)(Args...)> {
  static Ret pfnEndStateBlock(Args... args) {
    return static_cast<Ret>(device_end_state_block_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnValidateDevice;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnValidateDevice<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnValidateDevice(Args... args) {
    return static_cast<Ret>(device_validate_device_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnValidateDevice<Ret(*)(Args...)> {
  static Ret pfnValidateDevice(Args... args) {
    return static_cast<Ret>(device_validate_device_dispatch(args...));
  }
};

// -----------------------------------------------------------------------------
// Minimal D3D9 "Get*" state DDIs
// -----------------------------------------------------------------------------
// Many D3D9 runtimes can call these (directly or indirectly via state blocks).
// Return the UMD's cached state for the subset we currently track.

template <typename T>
uint32_t d3d9_to_u32(T v) {
  if constexpr (std::is_enum_v<T>) {
    using Under = std::underlying_type_t<T>;
    return static_cast<uint32_t>(static_cast<Under>(v));
  } else if constexpr (std::is_integral_v<T>) {
    return static_cast<uint32_t>(v);
  } else {
    return 0u;
  }
}

template <typename T>
void d3d9_write_u32(T* out, uint32_t v) {
  if (!out) {
    return;
  }
  using OutT = std::remove_reference_t<decltype(*out)>;
  if constexpr (std::is_enum_v<OutT>) {
    *out = static_cast<OutT>(v);
  } else if constexpr (std::is_integral_v<OutT>) {
    *out = static_cast<OutT>(v);
  } else {
    (void)v;
  }
}

template <typename HandleT>
void d3d9_write_handle(HandleT* out, void* pDrvPrivate) {
  if (!out) {
    return;
  }
  out->pDrvPrivate = pDrvPrivate;
}

template <typename StateT, typename ValueT>
HRESULT device_get_render_state_impl(D3DDDI_HDEVICE hDevice, StateT state, ValueT* pValue) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetRenderState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(d3d9_to_u32(state)),
                      d3d9_trace_arg_ptr(pValue),
                      0);
  if (!hDevice.pDrvPrivate || !pValue) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint32_t s = d3d9_to_u32(state);
  if (s >= 256) {
    return trace.ret(kD3DErrInvalidCall);
  }
  d3d9_write_u32(pValue, dev->render_states[s]);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_render_state_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 3) {
    return device_get_render_state_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename StageT, typename StateT, typename ValueT>
HRESULT device_get_sampler_state_impl(D3DDDI_HDEVICE hDevice, StageT stage, StateT state, ValueT* pValue) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetSamplerState,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_pack_u32_u32(d3d9_to_u32(stage), d3d9_to_u32(state)),
                      d3d9_trace_arg_ptr(pValue),
                      0);
  if (!hDevice.pDrvPrivate || !pValue) {
    return trace.ret(E_INVALIDARG);
  }
  const uint32_t st = d3d9_to_u32(stage);
  const uint32_t ss = d3d9_to_u32(state);
  if (st >= 16 || ss >= 16) {
    return trace.ret(kD3DErrInvalidCall);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_u32(pValue, dev->sampler_states[st][ss]);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_sampler_state_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 4) {
    return device_get_sampler_state_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename StageT, typename HandleT>
HRESULT device_get_texture_impl(D3DDDI_HDEVICE hDevice, StageT stage, HandleT* phTexture) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetTexture,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(d3d9_to_u32(stage)),
                      d3d9_trace_arg_ptr(phTexture),
                      0);
  if (!hDevice.pDrvPrivate || !phTexture) {
    return trace.ret(E_INVALIDARG);
  }
  const uint32_t st = d3d9_to_u32(stage);
  if (st >= 16) {
    return trace.ret(kD3DErrInvalidCall);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_handle(phTexture, dev->textures[st]);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_texture_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 3) {
    return device_get_texture_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename SlotT, typename HandleT>
HRESULT device_get_render_target_impl(D3DDDI_HDEVICE hDevice, SlotT slot, HandleT* phSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetRenderTarget,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(d3d9_to_u32(slot)),
                      d3d9_trace_arg_ptr(phSurface),
                      0);
  if (!hDevice.pDrvPrivate || !phSurface) {
    return trace.ret(E_INVALIDARG);
  }
  const uint32_t idx = d3d9_to_u32(slot);
  if (idx >= 4) {
    return trace.ret(kD3DErrInvalidCall);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_handle(phSurface, dev->render_targets[idx]);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_render_target_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 3) {
    return device_get_render_target_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename HandleT>
HRESULT device_get_depth_stencil_impl(D3DDDI_HDEVICE hDevice, HandleT* phSurface) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetDepthStencil,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(phSurface),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !phSurface) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_handle(phSurface, dev->depth_stencil);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_depth_stencil_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_get_depth_stencil_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename ViewportT>
HRESULT device_get_viewport_impl(D3DDDI_HDEVICE hDevice, ViewportT* pViewport) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetViewport,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pViewport),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pViewport) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  pViewport->X = static_cast<decltype(pViewport->X)>(dev->viewport.X);
  pViewport->Y = static_cast<decltype(pViewport->Y)>(dev->viewport.Y);
  pViewport->Width = static_cast<decltype(pViewport->Width)>(dev->viewport.Width);
  pViewport->Height = static_cast<decltype(pViewport->Height)>(dev->viewport.Height);
  pViewport->MinZ = static_cast<decltype(pViewport->MinZ)>(dev->viewport.MinZ);
  pViewport->MaxZ = static_cast<decltype(pViewport->MaxZ)>(dev->viewport.MaxZ);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_viewport_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_get_viewport_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename RectT, typename BoolT>
HRESULT device_get_scissor_rect_impl(D3DDDI_HDEVICE hDevice, RectT* pRect, BoolT* pEnabled) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetScissorRect,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pRect),
                      d3d9_trace_arg_ptr(pEnabled),
                      0);
  if (!hDevice.pDrvPrivate || !pRect) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pRect = dev->scissor_rect;
  if (pEnabled) {
    d3d9_write_u32(pEnabled, static_cast<uint32_t>(dev->scissor_enabled));
  }
  return trace.ret(S_OK);
}

template <typename RectT>
HRESULT device_get_scissor_rect_impl(D3DDDI_HDEVICE hDevice, RectT* pRect) {
  return device_get_scissor_rect_impl(hDevice, pRect, static_cast<BOOL*>(nullptr));
}

template <typename... Args>
HRESULT device_get_scissor_rect_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_get_scissor_rect_impl(args...);
  } else if constexpr (sizeof...(Args) == 3) {
    return device_get_scissor_rect_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename StreamT, typename HandleT, typename OffsetT, typename StrideT>
HRESULT device_get_stream_source_impl(
    D3DDDI_HDEVICE hDevice,
    StreamT stream,
    HandleT* phVb,
    OffsetT* pOffset,
    StrideT* pStride) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetStreamSource,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(d3d9_to_u32(stream)),
                      d3d9_trace_arg_ptr(phVb),
                      d3d9_trace_pack_u32_u32(d3d9_trace_arg_ptr(pOffset) != 0 ? 1u : 0u,
                                              d3d9_trace_arg_ptr(pStride) != 0 ? 1u : 0u));
  if (!hDevice.pDrvPrivate || !phVb || !pOffset || !pStride) {
    return trace.ret(E_INVALIDARG);
  }
  const uint32_t st = d3d9_to_u32(stream);
  if (st >= 16) {
    return trace.ret(kD3DErrInvalidCall);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  const DeviceStateStream& ss = dev->streams[st];
  d3d9_write_handle(phVb, ss.vb);
  d3d9_write_u32(pOffset, ss.offset_bytes);
  d3d9_write_u32(pStride, ss.stride_bytes);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_stream_source_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 5) {
    return device_get_stream_source_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename HandleT, typename FormatT, typename OffsetT>
HRESULT device_get_indices_impl(D3DDDI_HDEVICE hDevice, HandleT* phIb, FormatT* pFormat, OffsetT* pOffset) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetIndices,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(phIb),
                      d3d9_trace_arg_ptr(pFormat),
                      d3d9_trace_arg_ptr(pOffset));
  if (!hDevice.pDrvPrivate || !phIb || !pFormat || !pOffset) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_handle(phIb, dev->index_buffer);
  *pFormat = static_cast<FormatT>(dev->index_format);
  d3d9_write_u32(pOffset, dev->index_offset_bytes);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_indices_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 4) {
    return device_get_indices_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename StageT, typename HandleT>
HRESULT device_get_shader_impl(D3DDDI_HDEVICE hDevice, StageT stage, HandleT* phShader) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetShader,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(d3d9_to_u32(stage)),
                      d3d9_trace_arg_ptr(phShader),
                      0);
  if (!hDevice.pDrvPrivate || !phShader) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  Shader* sh = (d3d9_to_u32(stage) == kD3d9ShaderStageVs) ? dev->user_vs : dev->user_ps;
  phShader->pDrvPrivate = sh;
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_shader_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 3) {
    return device_get_shader_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename StageT, typename StartT, typename DataT, typename CountT>
HRESULT device_get_shader_const_f_impl(
    D3DDDI_HDEVICE hDevice,
    StageT stage,
    StartT start_reg,
    DataT* pData,
    CountT vec4_count) {
  const uint32_t st = d3d9_to_u32(stage);
  const uint32_t start = d3d9_to_u32(start_reg);
  const uint32_t count = d3d9_to_u32(vec4_count);
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetShaderConstF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(st),
                      d3d9_trace_pack_u32_u32(start, count),
                      d3d9_trace_arg_ptr(pData));
  if (!hDevice.pDrvPrivate || !pData || count == 0) {
    return trace.ret(E_INVALIDARG);
  }
  if (start >= 256) {
    return trace.ret(kD3DErrInvalidCall);
  }
  if (count > 256u - start) {
    return trace.ret(kD3DErrInvalidCall);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  const float* src = (st == kD3d9ShaderStageVs) ? dev->vs_consts_f : dev->ps_consts_f;
  std::memcpy(pData, src + start * 4, static_cast<size_t>(count) * 4 * sizeof(float));
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_shader_const_f_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 5) {
    return device_get_shader_const_f_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename ValueT>
HRESULT device_get_fvf_impl(D3DDDI_HDEVICE hDevice, ValueT* pFvf) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetFVF,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pFvf),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pFvf) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  d3d9_write_u32(pFvf, dev->fvf);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_fvf_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_get_fvf_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename HandleT>
HRESULT device_get_vertex_decl_impl(D3DDDI_HDEVICE hDevice, HandleT* phDecl) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetVertexDecl,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(phDecl),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !phDecl) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  phDecl->pDrvPrivate = dev->vertex_decl;
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_vertex_decl_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 2) {
    return device_get_vertex_decl_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetViewport;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetViewport<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetViewport(Args... args) {
    return static_cast<Ret>(device_get_viewport_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetViewport<Ret(*)(Args...)> {
  static Ret pfnGetViewport(Args... args) {
    return static_cast<Ret>(device_get_viewport_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetScissorRect;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetScissorRect<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetScissorRect(Args... args) {
    return static_cast<Ret>(device_get_scissor_rect_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetScissorRect<Ret(*)(Args...)> {
  static Ret pfnGetScissorRect(Args... args) {
    return static_cast<Ret>(device_get_scissor_rect_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetRenderTarget;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRenderTarget<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetRenderTarget(Args... args) {
    return static_cast<Ret>(device_get_render_target_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRenderTarget<Ret(*)(Args...)> {
  static Ret pfnGetRenderTarget(Args... args) {
    return static_cast<Ret>(device_get_render_target_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetDepthStencil;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetDepthStencil<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetDepthStencil(Args... args) {
    return static_cast<Ret>(device_get_depth_stencil_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetDepthStencil<Ret(*)(Args...)> {
  static Ret pfnGetDepthStencil(Args... args) {
    return static_cast<Ret>(device_get_depth_stencil_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetTexture;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetTexture<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetTexture(Args... args) {
    return static_cast<Ret>(device_get_texture_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetTexture<Ret(*)(Args...)> {
  static Ret pfnGetTexture(Args... args) {
    return static_cast<Ret>(device_get_texture_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetSamplerState;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetSamplerState<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetSamplerState(Args... args) {
    return static_cast<Ret>(device_get_sampler_state_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetSamplerState<Ret(*)(Args...)> {
  static Ret pfnGetSamplerState(Args... args) {
    return static_cast<Ret>(device_get_sampler_state_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetRenderState;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRenderState<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetRenderState(Args... args) {
    return static_cast<Ret>(device_get_render_state_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRenderState<Ret(*)(Args...)> {
  static Ret pfnGetRenderState(Args... args) {
    return static_cast<Ret>(device_get_render_state_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetStreamSource;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetStreamSource<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetStreamSource(Args... args) {
    return static_cast<Ret>(device_get_stream_source_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetStreamSource<Ret(*)(Args...)> {
  static Ret pfnGetStreamSource(Args... args) {
    return static_cast<Ret>(device_get_stream_source_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetIndices;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetIndices<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetIndices(Args... args) {
    return static_cast<Ret>(device_get_indices_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetIndices<Ret(*)(Args...)> {
  static Ret pfnGetIndices(Args... args) {
    return static_cast<Ret>(device_get_indices_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetShader;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetShader<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetShader(Args... args) {
    return static_cast<Ret>(device_get_shader_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetShader<Ret(*)(Args...)> {
  static Ret pfnGetShader(Args... args) {
    return static_cast<Ret>(device_get_shader_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetShaderConstF;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetShaderConstF<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetShaderConstF(Args... args) {
    return static_cast<Ret>(device_get_shader_const_f_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetShaderConstF<Ret(*)(Args...)> {
  static Ret pfnGetShaderConstF(Args... args) {
    return static_cast<Ret>(device_get_shader_const_f_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetFVF;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetFVF<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetFVF(Args... args) {
    return static_cast<Ret>(device_get_fvf_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetFVF<Ret(*)(Args...)> {
  static Ret pfnGetFVF(Args... args) {
    return static_cast<Ret>(device_get_fvf_dispatch(args...));
  }
};

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetVertexDecl;
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetVertexDecl<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetVertexDecl(Args... args) {
    return static_cast<Ret>(device_get_vertex_decl_dispatch(args...));
  }
};
template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetVertexDecl<Ret(*)(Args...)> {
  static Ret pfnGetVertexDecl(Args... args) {
    return static_cast<Ret>(device_get_vertex_decl_dispatch(args...));
  }
};

#endif // _WIN32 && AEROGPU_D3D9_USE_WDK_DDI

HRESULT AEROGPU_D3D9_CALL device_blt(D3DDDI_HDEVICE hDevice, const D3D9DDIARG_BLT* pBlt) {
  const D3DDDI_HRESOURCE src_h = pBlt ? d3d9_arg_src_resource(*pBlt) : D3DDDI_HRESOURCE{};
  const D3DDDI_HRESOURCE dst_h = pBlt ? d3d9_arg_dst_resource(*pBlt) : D3DDDI_HRESOURCE{};
  const uint32_t filter = pBlt ? d3d9_blt_filter(*pBlt) : 0;
  const uint32_t flags = pBlt ? d3d9_present_flags(*pBlt) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceBlt,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pBlt ? d3d9_trace_arg_ptr(src_h.pDrvPrivate) : 0,
                      pBlt ? d3d9_trace_arg_ptr(dst_h.pDrvPrivate) : 0,
                      pBlt ? d3d9_trace_pack_u32_u32(filter, flags) : 0);
  if (!hDevice.pDrvPrivate || !pBlt) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(src_h);
  auto* dst = as_resource(dst_h);

  std::lock_guard<std::mutex> lock(dev->mutex);

  return trace.ret(blit_locked(dev, dst, d3d9_update_surface_dst_rect(*pBlt), src, pBlt->pSrcRect, filter));
}

HRESULT AEROGPU_D3D9_CALL device_color_fill(D3DDDI_HDEVICE hDevice,
                                              const D3D9DDIARG_COLORFILL* pColorFill) {
  const D3DDDI_HRESOURCE dst_h = pColorFill ? d3d9_arg_dst_resource(*pColorFill) : D3DDDI_HRESOURCE{};
  const uint32_t color = pColorFill ? d3d9_color_fill_color(*pColorFill) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceColorFill,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pColorFill ? d3d9_trace_arg_ptr(dst_h.pDrvPrivate) : 0,
                      pColorFill ? static_cast<uint64_t>(color) : 0,
                      pColorFill ? static_cast<uint64_t>(pColorFill->pRect != nullptr ? 1u : 0u) : 0);
  if (!hDevice.pDrvPrivate || !pColorFill) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dst = as_resource(dst_h);
  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(color_fill_locked(dev, dst, pColorFill->pRect, color));
}

HRESULT AEROGPU_D3D9_CALL device_update_surface(D3DDDI_HDEVICE hDevice,
                                                   const D3D9DDIARG_UPDATESURFACE* pUpdateSurface) {
  const D3DDDI_HRESOURCE src_h = pUpdateSurface ? d3d9_arg_src_resource(*pUpdateSurface) : D3DDDI_HRESOURCE{};
  const D3DDDI_HRESOURCE dst_h = pUpdateSurface ? d3d9_arg_dst_resource(*pUpdateSurface) : D3DDDI_HRESOURCE{};
  const RECT* dst_rect = pUpdateSurface ? d3d9_update_surface_dst_rect(*pUpdateSurface) : nullptr;
  const uint64_t rect_flags = pUpdateSurface ? d3d9_trace_pack_u32_u32(pUpdateSurface->pSrcRect != nullptr ? 1u : 0u,
                                                                       dst_rect != nullptr ? 1u : 0u)
                                             : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUpdateSurface,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                       pUpdateSurface ? d3d9_trace_arg_ptr(src_h.pDrvPrivate) : 0,
                       pUpdateSurface ? d3d9_trace_arg_ptr(dst_h.pDrvPrivate) : 0,
                       rect_flags);
  if (!hDevice.pDrvPrivate || !pUpdateSurface) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(src_h);
  auto* dst = as_resource(dst_h);

  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(update_surface_locked(dev, src, pUpdateSurface->pSrcRect, dst, d3d9_update_surface_dst_point(*pUpdateSurface)));
}

HRESULT AEROGPU_D3D9_CALL device_update_texture(D3DDDI_HDEVICE hDevice,
                                                   const D3D9DDIARG_UPDATETEXTURE* pUpdateTexture) {
  const D3DDDI_HRESOURCE src_h = pUpdateTexture ? d3d9_arg_src_resource(*pUpdateTexture) : D3DDDI_HRESOURCE{};
  const D3DDDI_HRESOURCE dst_h = pUpdateTexture ? d3d9_arg_dst_resource(*pUpdateTexture) : D3DDDI_HRESOURCE{};
  D3d9TraceCall trace(D3d9TraceFunc::DeviceUpdateTexture,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pUpdateTexture ? d3d9_trace_arg_ptr(src_h.pDrvPrivate) : 0,
                      pUpdateTexture ? d3d9_trace_arg_ptr(dst_h.pDrvPrivate) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pUpdateTexture) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  auto* src = as_resource(src_h);
  auto* dst = as_resource(dst_h);

  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(update_texture_locked(dev, src, dst));
}

HRESULT AEROGPU_D3D9_CALL device_set_stream_source(
    D3DDDI_HDEVICE hDevice,
    uint32_t stream,
    D3DDDI_HRESOURCE hVb,
    uint32_t offset_bytes,
    uint32_t stride_bytes) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetStreamSource,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(stream),
                      d3d9_trace_arg_ptr(hVb.pDrvPrivate),
                      d3d9_trace_pack_u32_u32(offset_bytes, stride_bytes));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (stream >= 16) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* vb = as_resource(hVb);

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!emit_set_stream_source_locked(dev, stream, vb, offset_bytes, stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  stateblock_record_stream_source_locked(dev, stream, dev->streams[stream]);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_indices(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hIb,
    D3DDDIFORMAT fmt,
    uint32_t offset_bytes) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetIndices,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hIb.pDrvPrivate),
                      d3d9_trace_pack_u32_u32(static_cast<uint32_t>(fmt), offset_bytes),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  auto* ib = as_resource(hIb);

  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->index_buffer = ib;
  dev->index_format = fmt;
  dev->index_offset_bytes = offset_bytes;
  stateblock_record_index_buffer_locked(dev, ib, fmt, offset_bytes);

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->buffer = ib ? ib->handle : 0;
  cmd->format = d3d9_index_format_to_aerogpu(fmt);
  cmd->offset_bytes = offset_bytes;
  cmd->reserved0 = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_begin_scene(D3DDDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->scene_depth++;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_end_scene(D3DDDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->scene_depth > 0) {
    dev->scene_depth--;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_clear(
    D3DDDI_HDEVICE hDevice,
    uint32_t flags,
    uint32_t color_rgba8,
    float depth,
    uint32_t stencil) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceClear,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(flags),
                      static_cast<uint64_t>(color_rgba8),
                      d3d9_trace_pack_u32_u32(f32_bits(depth), stencil));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_clear), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_render_targets_locked(dev);
  if (hr < 0) {
    return hr;
  }

  const float a = static_cast<float>((color_rgba8 >> 24) & 0xFF) / 255.0f;
  const float r = static_cast<float>((color_rgba8 >> 16) & 0xFF) / 255.0f;
  const float g = static_cast<float>((color_rgba8 >> 8) & 0xFF) / 255.0f;
  const float b = static_cast<float>((color_rgba8 >> 0) & 0xFF) / 255.0f;

  auto* cmd = append_fixed_locked<aerogpu_cmd_clear>(dev, AEROGPU_CMD_CLEAR);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->flags = flags;
  cmd->color_rgba_f32[0] = f32_bits(r);
  cmd->color_rgba_f32[1] = f32_bits(g);
  cmd->color_rgba_f32[2] = f32_bits(b);
  cmd->color_rgba_f32[3] = f32_bits(a);
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawPrimitive,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      d3d9_trace_pack_u32_u32(start_vertex, primitive_count),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (primitive_count == 0) {
    return trace.ret(S_OK);
  }

  // Fixed-function emulation path: for XYZRHW vertices we upload a transformed
  // (clip-space) copy of the referenced vertices into a scratch VB and draw
  // using a built-in shader pair.
  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    DeviceStateStream saved = dev->streams[0];
    DeviceStateStream& ss = dev->streams[0];
    if (!ss.vb || ss.stride_bytes < 20) {
      return E_FAIL;
    }

    const uint32_t vertex_count = vertex_count_from_primitive(type, primitive_count);
    const uint64_t src_offset_u64 =
        static_cast<uint64_t>(ss.offset_bytes) + static_cast<uint64_t>(start_vertex) * ss.stride_bytes;
    const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * ss.stride_bytes;
    const uint64_t vb_size_u64 = ss.vb->size_bytes;
    if (src_offset_u64 > vb_size_u64 || size_u64 > vb_size_u64 - src_offset_u64) {
      return E_INVALIDARG;
    }

    const uint8_t* src_vertices = nullptr;
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    void* vb_ptr = nullptr;
    bool vb_locked = false;
#endif

    bool use_vb_storage = ss.vb->storage.size() >= static_cast<size_t>(src_offset_u64 + size_u64);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    // Guest-backed buffers may still allocate a CPU shadow buffer (e.g. shared
    // resources opened via OpenResource). On real WDDM builds the authoritative
    // bytes live in the WDDM allocation, so prefer mapping it directly.
    if (ss.vb->backing_alloc_id != 0) {
      use_vb_storage = false;
    }
#endif

    if (use_vb_storage) {
      src_vertices = ss.vb->storage.data() + static_cast<size_t>(src_offset_u64);
    } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (ss.vb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
          const HRESULT lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                       dev->wddm_device,
                                                       ss.vb->wddm_hAllocation,
                                                       src_offset_u64,
                                                       size_u64,
                                                       kD3DLOCK_READONLY,
                                                       &vb_ptr,
                                                       dev->wddm_context.hContext);
          if (FAILED(lock_hr) || !vb_ptr) {
            return FAILED(lock_hr) ? lock_hr : E_FAIL;
          }
        vb_locked = true;
        src_vertices = static_cast<const uint8_t*>(vb_ptr);
      } else
#endif
      {
        return E_INVALIDARG;
      }
    }

    std::vector<uint8_t> converted;
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev,
        src_vertices,
        ss.stride_bytes,
        vertex_count,
        &converted);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (vb_locked) {
      const HRESULT unlock_hr =
          wddm_unlock_allocation(dev->wddm_callbacks, dev->wddm_device, ss.vb->wddm_hAllocation, dev->wddm_context.hContext);
      if (FAILED(unlock_hr)) {
        logf("aerogpu-d3d9: draw_primitive fixedfunc: UnlockCb failed hr=0x%08lx alloc_id=%u hAllocation=%llu\n",
             static_cast<unsigned long>(unlock_hr),
             static_cast<unsigned>(ss.vb->backing_alloc_id),
             static_cast<unsigned long long>(ss.vb->wddm_hAllocation));
        return unlock_hr;
      }
    }
#endif
    if (FAILED(hr)) {
      return hr;
    }

    hr = ensure_up_vertex_buffer_locked(dev, static_cast<uint32_t>(converted.size()));
    if (FAILED(hr)) {
      return hr;
    }
    hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, converted.data(), static_cast<uint32_t>(converted.size()));
    if (FAILED(hr)) {
      return hr;
    }

    if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, ss.stride_bytes)) {
      return E_OUTOFMEMORY;
    }

    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }

    const uint32_t topology = d3d9_prim_to_topology(type);
    if (!emit_set_topology_locked(dev, topology)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }

    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }
    hr = track_draw_state_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
    if (!cmd) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return E_OUTOFMEMORY;
    }
    cmd->vertex_count = vertex_count;
    cmd->instance_count = 1;
    cmd->first_vertex = 0;
    cmd->first_instance = 0;

    if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
      return E_OUTOFMEMORY;
    }
    return S_OK;
  }

  const size_t draw_bytes = align_up(sizeof(aerogpu_cmd_set_primitive_topology), 4) +
                            align_up(sizeof(aerogpu_cmd_draw), 4);
  if (!ensure_cmd_space(dev, draw_bytes)) {
    return E_OUTOFMEMORY;
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_draw_state_locked(dev);
  if (hr < 0) {
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->vertex_count = vertex_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t primitive_count,
    const void* pVertexData,
    uint32_t stride_bytes) {
  const uint64_t packed = d3d9_trace_pack_u32_u32(primitive_count, stride_bytes);
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawPrimitiveUP,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      packed,
                      d3d9_trace_arg_ptr(pVertexData));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (primitive_count == 0) {
    return trace.ret(S_OK);
  }
  if (!pVertexData || stride_bytes == 0) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t vertex_count = vertex_count_from_primitive(type, primitive_count);
  const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * stride_bytes;
  if (size_u64 == 0 || size_u64 > 0x7FFFFFFFu) {
    return trace.ret(E_INVALIDARG);
  }

  DeviceStateStream saved = dev->streams[0];

  std::vector<uint8_t> converted;
  const void* upload_data = pVertexData;
  uint32_t upload_size = static_cast<uint32_t>(size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(dev, pVertexData, stride_bytes, vertex_count, &converted);
    if (FAILED(hr)) {
      return trace.ret(hr);
    }
    upload_data = converted.data();
    upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, upload_size);
  if (FAILED(hr)) {
    return trace.ret(hr);
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, upload_data, upload_size);
  if (FAILED(hr)) {
    return trace.ret(hr);
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return trace.ret(hr);
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }
  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(hr);
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
  cmd->first_instance = 0;

  if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
    return trace.ret(E_OUTOFMEMORY);
  }
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWINDEXEDPRIMITIVE2* pDraw);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t min_vertex_index,
    uint32_t num_vertices,
    uint32_t primitive_count,
    const void* pIndexData,
    D3DDDIFORMAT index_data_format,
    const void* pVertexData,
    uint32_t stride_bytes) {
  const uint64_t min_num = d3d9_trace_pack_u32_u32(min_vertex_index, num_vertices);
  const uint64_t pc_stride = d3d9_trace_pack_u32_u32(primitive_count, stride_bytes);
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawIndexedPrimitiveUP,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      min_num,
                      pc_stride);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  if (primitive_count == 0) {
    return trace.ret(S_OK);
  }
  if (!pVertexData || stride_bytes == 0 || !pIndexData || num_vertices == 0) {
    return trace.ret(E_INVALIDARG);
  }
  // Only INDEX16/INDEX32 are valid for DrawIndexedPrimitiveUP.
  if (index_data_format != kD3dFmtIndex16 && index_data_format != kD3dFmtIndex32) {
    return trace.ret(E_INVALIDARG);
  }

  D3DDDIARG_DRAWINDEXEDPRIMITIVE2 draw{};
  draw.PrimitiveType = type;
  draw.PrimitiveCount = primitive_count;
  draw.MinIndex = min_vertex_index;
  draw.NumVertices = num_vertices;
  draw.pIndexData = pIndexData;
  draw.IndexDataFormat = index_data_format;
  draw.pVertexStreamZeroData = pVertexData;
  draw.VertexStreamZeroStride = stride_bytes;
  return trace.ret(device_draw_indexed_primitive2(hDevice, &draw));
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWPRIMITIVE2* pDraw) {
  if (!hDevice.pDrvPrivate || !pDraw) {
    return E_INVALIDARG;
  }
  if (pDraw->PrimitiveCount == 0) {
    return S_OK;
  }
  if (!pDraw->pVertexStreamZeroData || pDraw->VertexStreamZeroStride == 0) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t vertex_count = vertex_count_from_primitive(pDraw->PrimitiveType, pDraw->PrimitiveCount);
  const uint64_t size_u64 = static_cast<uint64_t>(vertex_count) * pDraw->VertexStreamZeroStride;
  if (size_u64 == 0 || size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }

  DeviceStateStream saved = dev->streams[0];

  std::vector<uint8_t> converted;
  const void* upload_data = pDraw->pVertexStreamZeroData;
  uint32_t upload_size = static_cast<uint32_t>(size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev, pDraw->pVertexStreamZeroData, pDraw->VertexStreamZeroStride, vertex_count, &converted);
    if (FAILED(hr)) {
      return hr;
    }
    upload_data = converted.data();
    upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, upload_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, upload_data, upload_size);
  if (FAILED(hr)) {
    return hr;
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, pDraw->VertexStreamZeroStride)) {
    return E_OUTOFMEMORY;
  }

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
      return hr;
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(pDraw->PrimitiveType);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }

  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes);
    return E_OUTOFMEMORY;
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
  cmd->first_instance = 0;

  if (!emit_set_stream_source_locked(dev, 0, saved.vb, saved.offset_bytes, saved.stride_bytes)) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWINDEXEDPRIMITIVE2* pDraw) {
  if (!hDevice.pDrvPrivate || !pDraw) {
    return E_INVALIDARG;
  }
  if (pDraw->PrimitiveCount == 0) {
    return S_OK;
  }
  if (!pDraw->pVertexStreamZeroData || pDraw->VertexStreamZeroStride == 0 || !pDraw->pIndexData) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t index_count = index_count_from_primitive(pDraw->PrimitiveType, pDraw->PrimitiveCount);
  const uint32_t index_size = (pDraw->IndexDataFormat == kD3dFmtIndex32) ? 4u : 2u;
  const uint64_t ib_size_u64 = static_cast<uint64_t>(index_count) * index_size;
  if (ib_size_u64 == 0 || ib_size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }
  const uint32_t ib_size = static_cast<uint32_t>(ib_size_u64);

  const uint64_t vertex_count_u64 = static_cast<uint64_t>(pDraw->MinIndex) + static_cast<uint64_t>(pDraw->NumVertices);
  const uint64_t vb_size_u64 = vertex_count_u64 * static_cast<uint64_t>(pDraw->VertexStreamZeroStride);
  if (vertex_count_u64 == 0 || vb_size_u64 == 0 || vb_size_u64 > 0x7FFFFFFFu) {
    return E_INVALIDARG;
  }

  DeviceStateStream saved_stream = dev->streams[0];
  Resource* saved_ib = dev->index_buffer;
  const D3DDDIFORMAT saved_fmt = dev->index_format;
  const uint32_t saved_offset = dev->index_offset_bytes;

  std::vector<uint8_t> converted;
  const void* vb_upload_data = pDraw->pVertexStreamZeroData;
  uint32_t vb_upload_size = static_cast<uint32_t>(vb_size_u64);

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    HRESULT hr = convert_xyzrhw_to_clipspace_locked(
        dev, pDraw->pVertexStreamZeroData, pDraw->VertexStreamZeroStride, static_cast<uint32_t>(vertex_count_u64), &converted);
    if (FAILED(hr)) {
      return hr;
    }
    vb_upload_data = converted.data();
    vb_upload_size = static_cast<uint32_t>(converted.size());
  }

  HRESULT hr = ensure_up_vertex_buffer_locked(dev, vb_upload_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, vb_upload_data, vb_upload_size);
  if (FAILED(hr)) {
    return hr;
  }

  hr = ensure_up_index_buffer_locked(dev, ib_size);
  if (FAILED(hr)) {
    return hr;
  }
  hr = emit_upload_buffer_locked(dev, dev->up_index_buffer, pDraw->pIndexData, ib_size);
  if (FAILED(hr)) {
    return hr;
  }

  if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, pDraw->VertexStreamZeroStride)) {
    return E_OUTOFMEMORY;
  }

  dev->index_buffer = dev->up_index_buffer;
  dev->index_format = pDraw->IndexDataFormat;
  dev->index_offset_bytes = 0;

  auto* ib_cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!ib_cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    return E_OUTOFMEMORY;
  }
  ib_cmd->buffer = dev->up_index_buffer ? dev->up_index_buffer->handle : 0;
  ib_cmd->format = d3d9_index_format_to_aerogpu(pDraw->IndexDataFormat);
  ib_cmd->offset_bytes = 0;
  ib_cmd->reserved0 = 0;

  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      // Restore IB state.
      dev->index_buffer = saved_ib;
      dev->index_format = saved_fmt;
      dev->index_offset_bytes = saved_offset;
      auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
      if (restore) {
        restore->buffer = saved_ib ? saved_ib->handle : 0;
        restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
        restore->offset_bytes = saved_offset;
        restore->reserved0 = 0;
      }
      return hr;
    }
  }

  const uint32_t topology = d3d9_prim_to_topology(pDraw->PrimitiveType);
  if (!emit_set_topology_locked(dev, topology)) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw_indexed), 4))) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }

  hr = track_draw_state_locked(dev);
  if (FAILED(hr)) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw_indexed>(dev, AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
    // Restore IB state.
    dev->index_buffer = saved_ib;
    dev->index_format = saved_fmt;
    dev->index_offset_bytes = saved_offset;
    auto* restore = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
    if (restore) {
      restore->buffer = saved_ib ? saved_ib->handle : 0;
      restore->format = d3d9_index_format_to_aerogpu(saved_fmt);
      restore->offset_bytes = saved_offset;
      restore->reserved0 = 0;
    }
    return E_OUTOFMEMORY;
  }
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = 0;
  cmd->base_vertex = 0;
  cmd->first_instance = 0;

  // Restore stream source 0.
  if (!emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes)) {
    return E_OUTOFMEMORY;
  }

  // Restore index buffer binding.
  dev->index_buffer = saved_ib;
  dev->index_format = saved_fmt;
  dev->index_offset_bytes = saved_offset;
  auto* restore_cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!restore_cmd) {
    return E_OUTOFMEMORY;
  }
  restore_cmd->buffer = saved_ib ? saved_ib->handle : 0;
  restore_cmd->format = d3d9_index_format_to_aerogpu(saved_fmt);
  restore_cmd->offset_bytes = saved_offset;
  restore_cmd->reserved0 = 0;

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    int32_t base_vertex,
    uint32_t /*min_index*/,
    uint32_t /*num_vertices*/,
    uint32_t start_index,
    uint32_t primitive_count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDrawIndexedPrimitive,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(type),
                      d3d9_trace_pack_u32_u32(static_cast<uint32_t>(base_vertex), start_index),
                      static_cast<uint64_t>(primitive_count));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (primitive_count == 0) {
    return trace.ret(S_OK);
  }

  // Fixed-function emulation for indexed draws: expand indices into a temporary
  // vertex stream and issue a non-indexed draw. This is intentionally
  // conservative but is sufficient for bring-up.
  if (dev->fvf == kSupportedFvfXyzrhwDiffuse && !dev->user_vs && !dev->user_ps) {
    DeviceStateStream saved_stream = dev->streams[0];
    DeviceStateStream& ss = dev->streams[0];

    if (!ss.vb || ss.stride_bytes < 20) {
      return E_FAIL;
    }
    if (!dev->index_buffer) {
      return E_FAIL;
    }

    const uint32_t index_count = index_count_from_primitive(type, primitive_count);
    const uint32_t index_size = (dev->index_format == kD3dFmtIndex32) ? 4u : 2u;
    const uint64_t index_bytes_u64 = static_cast<uint64_t>(index_count) * index_size;
    const uint64_t index_offset_u64 =
        static_cast<uint64_t>(dev->index_offset_bytes) + static_cast<uint64_t>(start_index) * index_size;

    std::vector<uint8_t> expanded;
    const uint64_t expanded_bytes_u64 = static_cast<uint64_t>(index_count) * ss.stride_bytes;
    if (expanded_bytes_u64 == 0 || expanded_bytes_u64 > 0x7FFFFFFFu) {
      return E_INVALIDARG;
    }

    const uint64_t ib_size_u64 = dev->index_buffer->size_bytes;
    if (index_offset_u64 > ib_size_u64 || index_bytes_u64 > ib_size_u64 - index_offset_u64) {
      return E_INVALIDARG;
    }

    {
      const uint8_t* index_data = nullptr;
      const uint8_t* vb_base = nullptr;
      uint32_t min_vtx = 0;
      uint32_t max_vtx = 0;
      bool have_bounds = false;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      struct AutoUnlock {
        Device* dev = nullptr;
        WddmAllocationHandle hAllocation = 0;
        uint32_t alloc_id = 0;
        const char* tag = nullptr;
        bool locked = false;

        AutoUnlock(Device* dev, WddmAllocationHandle hAllocation, uint32_t alloc_id, const char* tag)
            : dev(dev), hAllocation(hAllocation), alloc_id(alloc_id), tag(tag) {}

        ~AutoUnlock() {
          if (locked && dev && dev->wddm_device != 0 && hAllocation != 0) {
            const HRESULT hr = wddm_unlock_allocation(dev->wddm_callbacks, dev->wddm_device, hAllocation, dev->wddm_context.hContext);
            if (FAILED(hr)) {
              logf("aerogpu-d3d9: draw_indexed_primitive fixedfunc: UnlockCb(%s) failed hr=0x%08lx alloc_id=%u hAllocation=%llu\n",
                   tag ? tag : "?",
                   static_cast<unsigned long>(hr),
                   static_cast<unsigned>(alloc_id),
                   static_cast<unsigned long long>(hAllocation));
            }
          }
        }
      };

      AutoUnlock ib_lock(dev, dev->index_buffer->wddm_hAllocation, dev->index_buffer->backing_alloc_id, "IB");
      AutoUnlock vb_lock(dev, ss.vb->wddm_hAllocation, ss.vb->backing_alloc_id, "VB");
      void* ib_ptr = nullptr;
      void* vb_ptr = nullptr;
#endif

      // Lock index buffer if we don't have a CPU shadow copy.
      bool use_ib_storage = dev->index_buffer->storage.size() >= static_cast<size_t>(index_offset_u64 + index_bytes_u64);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      // Guest-backed buffers can have a CPU shadow allocation when they are
      // shared/OpenResource'd; in WDDM builds the underlying allocation memory is
      // authoritative.
      if (dev->index_buffer->backing_alloc_id != 0) {
        use_ib_storage = false;
      }
#endif
      if (use_ib_storage) {
        index_data = dev->index_buffer->storage.data() + static_cast<size_t>(index_offset_u64);
      } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (dev->index_buffer->wddm_hAllocation != 0 && dev->wddm_device != 0) {
           const HRESULT lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                        dev->wddm_device,
                                                        dev->index_buffer->wddm_hAllocation,
                                                        index_offset_u64,
                                                        index_bytes_u64,
                                                        kD3DLOCK_READONLY,
                                                        &ib_ptr,
                                                        dev->wddm_context.hContext);
           if (FAILED(lock_hr) || !ib_ptr) {
             return FAILED(lock_hr) ? lock_hr : E_FAIL;
           }
          ib_lock.locked = true;
          index_data = static_cast<const uint8_t*>(ib_ptr);
        } else
#endif
        {
          return E_INVALIDARG;
        }
      }

      // First pass: compute min/max referenced vertex index so we can map a single
      // contiguous vertex range.
      for (uint32_t i = 0; i < index_count; ++i) {
        uint32_t idx = 0;
        if (index_size == 4) {
          std::memcpy(&idx, index_data + static_cast<size_t>(i) * 4, sizeof(idx));
        } else {
          uint16_t idx16 = 0;
          std::memcpy(&idx16, index_data + static_cast<size_t>(i) * 2, sizeof(idx16));
          idx = idx16;
        }

        const int64_t vtx = static_cast<int64_t>(base_vertex) + static_cast<int64_t>(idx);
        if (vtx < 0) {
          return E_INVALIDARG;
        }
        const uint32_t vtx_u32 = static_cast<uint32_t>(vtx);
        if (!have_bounds) {
          min_vtx = vtx_u32;
          max_vtx = vtx_u32;
          have_bounds = true;
        } else {
          min_vtx = std::min(min_vtx, vtx_u32);
          max_vtx = std::max(max_vtx, vtx_u32);
        }
      }
      if (!have_bounds) {
        return E_INVALIDARG;
      }

      const uint64_t vb_size_u64 = ss.vb->size_bytes;
      const uint64_t vb_range_offset =
          static_cast<uint64_t>(ss.offset_bytes) + static_cast<uint64_t>(min_vtx) * ss.stride_bytes;
      const uint64_t vb_range_size =
          (static_cast<uint64_t>(max_vtx) - static_cast<uint64_t>(min_vtx) + 1) * ss.stride_bytes;
      if (vb_range_offset > vb_size_u64 || vb_range_size > vb_size_u64 - vb_range_offset) {
        return E_INVALIDARG;
      }

      bool use_vb_storage = ss.vb->storage.size() >= static_cast<size_t>(vb_range_offset + vb_range_size);
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
      if (ss.vb->backing_alloc_id != 0) {
        use_vb_storage = false;
      }
#endif
      if (use_vb_storage) {
        vb_base = ss.vb->storage.data() + static_cast<size_t>(vb_range_offset);
      } else {
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
        if (ss.vb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
           const HRESULT lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                        dev->wddm_device,
                                                        ss.vb->wddm_hAllocation,
                                                        vb_range_offset,
                                                        vb_range_size,
                                                        kD3DLOCK_READONLY,
                                                        &vb_ptr,
                                                        dev->wddm_context.hContext);
           if (FAILED(lock_hr) || !vb_ptr) {
             return FAILED(lock_hr) ? lock_hr : E_FAIL;
           }
          vb_lock.locked = true;
          vb_base = static_cast<const uint8_t*>(vb_ptr);
        } else
#endif
        {
          return E_INVALIDARG;
        }
      }

      try {
        expanded.resize(static_cast<size_t>(expanded_bytes_u64));
      } catch (...) {
        return E_OUTOFMEMORY;
      }

      float vp_x = 0.0f;
      float vp_y = 0.0f;
      float vp_w = 1.0f;
      float vp_h = 1.0f;
      get_viewport_dims_locked(dev, &vp_x, &vp_y, &vp_w, &vp_h);

      for (uint32_t i = 0; i < index_count; i++) {
        uint32_t idx = 0;
        if (index_size == 4) {
          std::memcpy(&idx, index_data + static_cast<size_t>(i) * 4, sizeof(idx));
        } else {
          uint16_t idx16 = 0;
          std::memcpy(&idx16, index_data + static_cast<size_t>(i) * 2, sizeof(idx16));
          idx = idx16;
        }

        const int64_t vtx = static_cast<int64_t>(base_vertex) + static_cast<int64_t>(idx);
        if (vtx < 0) {
          return E_INVALIDARG;
        }
        const uint32_t vtx_u32 = static_cast<uint32_t>(vtx);
        const uint64_t local_off =
            (static_cast<uint64_t>(vtx_u32) - static_cast<uint64_t>(min_vtx)) * ss.stride_bytes;
        if (local_off + ss.stride_bytes > vb_range_size) {
          return E_INVALIDARG;
        }

        const uint8_t* src = vb_base + static_cast<size_t>(local_off);
        uint8_t* dst = expanded.data() + static_cast<size_t>(i) * ss.stride_bytes;
        std::memcpy(dst, src, ss.stride_bytes);

        const float x = read_f32_unaligned(src + 0);
        const float y = read_f32_unaligned(src + 4);
        const float z = read_f32_unaligned(src + 8);
        const float rhw = read_f32_unaligned(src + 12);

        const float w = (rhw != 0.0f) ? (1.0f / rhw) : 1.0f;
        // D3D9's viewport transform uses a -0.5 pixel center convention. Invert it
        // so typical D3D9 pre-transformed vertex coordinates line up with pixel
        // centers.
        const float ndc_x = ((x + 0.5f - vp_x) / vp_w) * 2.0f - 1.0f;
        const float ndc_y = 1.0f - ((y + 0.5f - vp_y) / vp_h) * 2.0f;
        const float ndc_z = z;

        write_f32_unaligned(dst + 0, ndc_x * w);
        write_f32_unaligned(dst + 4, ndc_y * w);
        write_f32_unaligned(dst + 8, ndc_z * w);
        write_f32_unaligned(dst + 12, w);
      }
    }

    HRESULT hr = ensure_up_vertex_buffer_locked(dev, static_cast<uint32_t>(expanded.size()));
    if (FAILED(hr)) {
      return hr;
    }
    hr = emit_upload_buffer_locked(dev, dev->up_vertex_buffer, expanded.data(), static_cast<uint32_t>(expanded.size()));
    if (FAILED(hr)) {
      return hr;
    }

    if (!emit_set_stream_source_locked(dev, 0, dev->up_vertex_buffer, 0, ss.stride_bytes)) {
      return E_OUTOFMEMORY;
    }

    hr = ensure_fixedfunc_pipeline_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return hr;
    }

    const uint32_t topology = d3d9_prim_to_topology(type);
    if (!emit_set_topology_locked(dev, topology)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }

    // Ensure the command buffer has space before we track allocations; tracking
    // may force a submission split, and command-buffer splits must not occur
    // after tracking or the allocation list would be out of sync.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }
    hr = track_draw_state_locked(dev);
    if (FAILED(hr)) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return hr;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
    if (!cmd) {
      (void)emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes);
      return E_OUTOFMEMORY;
    }
    cmd->vertex_count = index_count;
    cmd->instance_count = 1;
    cmd->first_vertex = 0;
    cmd->first_instance = 0;

    if (!emit_set_stream_source_locked(dev, 0, saved_stream.vb, saved_stream.offset_bytes, saved_stream.stride_bytes)) {
      return E_OUTOFMEMORY;
    }
    return S_OK;
  }

  const size_t draw_bytes = align_up(sizeof(aerogpu_cmd_set_primitive_topology), 4) +
                            align_up(sizeof(aerogpu_cmd_draw_indexed), 4);
  if (!ensure_cmd_space(dev, draw_bytes)) {
    return E_OUTOFMEMORY;
  }

  const uint32_t topology = d3d9_prim_to_topology(type);
  if (!emit_set_topology_locked(dev, topology)) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw_indexed), 4))) {
    return E_OUTOFMEMORY;
  }

  HRESULT hr = track_draw_state_locked(dev);
  if (hr < 0) {
    return hr;
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_draw_indexed>(dev, AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    return trace.ret(E_OUTOFMEMORY);
  }
  cmd->index_count = index_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_present_ex(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_PRESENTEX* pPresentEx) {
  const uint64_t wnd = pPresentEx ? d3d9_trace_arg_ptr(d3d9_present_hwnd(*pPresentEx)) : 0;
  const uint64_t sync_flags = pPresentEx ? d3d9_trace_pack_u32_u32(d3d9_present_sync_interval(*pPresentEx),
                                                                   d3d9_present_flags(*pPresentEx))
                                         : 0;
  const uint64_t src = pPresentEx ? d3d9_trace_arg_ptr(d3d9_present_src(*pPresentEx).pDrvPrivate) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DevicePresentEx, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), wnd, sync_flags, src);
  if (!hDevice.pDrvPrivate || !pPresentEx) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  const D3DDDI_HRESOURCE src_handle = d3d9_present_src(*pPresentEx);
  const uint32_t sync_interval = d3d9_present_sync_interval(*pPresentEx);
  const uint32_t present_flags = d3d9_present_flags(*pPresentEx);
  uint32_t present_count = 0;
  HRESULT present_hr = S_OK;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);

    bool occluded = false;
#if defined(_WIN32)
    // Returning S_PRESENT_OCCLUDED from PresentEx helps some D3D9Ex clients avoid
    // pathological present loops when their target window is minimized.
    // Keep the check cheap and never block on it.
    HWND hwnd = d3d9_present_hwnd(*pPresentEx);
    if (!hwnd) {
      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      hwnd = sc ? sc->hwnd : nullptr;
    }
    if (hwnd) {
      if (IsIconic(hwnd)) {
        occluded = true;
      }
    }
#endif

    if (occluded) {
      // Even when occluded, Present/PresentEx act as a flush point and must
      // advance D3D9Ex present statistics (GetPresentStats/GetLastPresentCount).
      retire_completed_presents_locked(dev);
      (void)submit(dev, /*is_present=*/false);

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();

      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
      }

      present_hr = kSPresentOccluded;
    } else {
      HRESULT hr = throttle_presents_locked(dev, present_flags);
      if (hr != S_OK) {
        return trace.ret(hr);
      }

      // Submit any pending render work via the Render callback before issuing a
      // Present submission. This ensures the KMD/emulator observes distinct
      // render vs present submissions (DxgkDdiRender vs DxgkDdiPresent).
      (void)submit(dev, /*is_present=*/false);

      // Track the present source allocation so the KMD can resolve the backing
      // `alloc_id` via the per-submit allocation table even though we keep the
      // patch-location list empty.
      //
      // Ensure command space before tracking: tracking may split/submit and must
      // not occur after command-buffer overflow handling.
      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_present_ex), 4))) {
        return trace.ret(E_OUTOFMEMORY);
      }
      if (auto* src_res = as_resource(src_handle)) {
        const HRESULT track_hr = track_resource_allocation_locked(dev, src_res, /*write=*/false);
        if (track_hr < 0) {
          return trace.ret(track_hr);
        }
      }

      auto* cmd = append_fixed_locked<aerogpu_cmd_present_ex>(dev, AEROGPU_CMD_PRESENT_EX);
      if (!cmd) {
        return trace.ret(E_OUTOFMEMORY);
      }
      cmd->scanout_id = 0;
      bool vsync = (sync_interval != 0) && (sync_interval != kD3dPresentIntervalImmediate);
      if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
        // Only request vblank-paced presents when the active device reports vblank support.
        vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
      }
      cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
      cmd->d3d9_present_flags = present_flags;
      cmd->reserved0 = 0;

      const uint64_t submit_fence = submit(dev, /*is_present=*/true);
      const uint64_t present_fence = submit_fence;
      if (present_fence) {
        dev->inflight_present_fences.push_back(present_fence);
      }

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();
      SwapChain* sc = dev->current_swapchain;
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
        sc->last_present_fence = present_fence;
        if (sc->backbuffers.size() > 1 && sc->swap_effect != 0u) {
          auto is_backbuffer = [sc](const Resource* res) -> bool {
            if (!sc || !res) {
              return false;
            }
            return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
          };

          // Present-style backbuffer rotation swaps the underlying identities
          // (host handle + backing allocation) attached to the backbuffer Resource
          // objects. If any backbuffers are currently bound via device state (RTs,
          // textures, IA buffers), we must re-emit those binds so the host stops
          // referencing the old handles.
          size_t needed_bytes = align_up(sizeof(aerogpu_cmd_set_render_targets), 4);
          for (uint32_t stage = 0; stage < 16; ++stage) {
            if (is_backbuffer(dev->textures[stage])) {
              needed_bytes += align_up(sizeof(aerogpu_cmd_set_texture), 4);
            }
          }
          for (uint32_t stream = 0; stream < 16; ++stream) {
            if (is_backbuffer(dev->streams[stream].vb)) {
              needed_bytes += align_up(sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding), 4);
            }
          }
          if (is_backbuffer(dev->index_buffer)) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_index_buffer), 4);
          }

          if (ensure_cmd_space(dev, needed_bytes)) {
            struct ResourceIdentity {
              aerogpu_handle_t handle = 0;
              uint32_t backing_alloc_id = 0;
              uint32_t backing_offset_bytes = 0;
              uint64_t share_token = 0;
              bool is_shared = false;
              bool is_shared_alias = false;
              bool locked = false;
              uint32_t locked_offset = 0;
              uint32_t locked_size = 0;
              uint32_t locked_flags = 0;
              WddmAllocationHandle wddm_hAllocation = 0;
              std::vector<uint8_t> storage;
              std::vector<uint8_t> shared_private_driver_data;
            };

            auto take_identity = [](Resource* res) -> ResourceIdentity {
              ResourceIdentity id{};
              id.handle = res->handle;
              id.backing_alloc_id = res->backing_alloc_id;
              id.backing_offset_bytes = res->backing_offset_bytes;
              id.share_token = res->share_token;
              id.is_shared = res->is_shared;
              id.is_shared_alias = res->is_shared_alias;
              id.locked = res->locked;
              id.locked_offset = res->locked_offset;
              id.locked_size = res->locked_size;
              id.locked_flags = res->locked_flags;
              id.wddm_hAllocation = res->wddm_hAllocation;
              id.storage = std::move(res->storage);
              id.shared_private_driver_data = std::move(res->shared_private_driver_data);
              return id;
            };

            auto put_identity = [](Resource* res, ResourceIdentity&& id) {
              res->handle = id.handle;
              res->backing_alloc_id = id.backing_alloc_id;
              res->backing_offset_bytes = id.backing_offset_bytes;
              res->share_token = id.share_token;
              res->is_shared = id.is_shared;
              res->is_shared_alias = id.is_shared_alias;
              res->locked = id.locked;
              res->locked_offset = id.locked_offset;
              res->locked_size = id.locked_size;
              res->locked_flags = id.locked_flags;
              res->wddm_hAllocation = id.wddm_hAllocation;
              res->storage = std::move(id.storage);
              res->shared_private_driver_data = std::move(id.shared_private_driver_data);
            };

            auto undo_rotation = [sc, &take_identity, &put_identity]() {
              // Undo the rotation (rotate right by one).
              ResourceIdentity undo_saved = take_identity(sc->backbuffers.back());
              for (size_t i = sc->backbuffers.size() - 1; i > 0; --i) {
                put_identity(sc->backbuffers[i], take_identity(sc->backbuffers[i - 1]));
              }
              put_identity(sc->backbuffers[0], std::move(undo_saved));
            };

            // Rotate left by one.
             ResourceIdentity saved = take_identity(sc->backbuffers[0]);
             for (size_t i = 0; i + 1 < sc->backbuffers.size(); ++i) {
               put_identity(sc->backbuffers[i], take_identity(sc->backbuffers[i + 1]));
             }
             put_identity(sc->backbuffers.back(), std::move(saved));
 
             bool ok = true;
             if (dev->wddm_context.hContext != 0 &&
                 dev->alloc_list_tracker.list_base() != nullptr &&
                 dev->alloc_list_tracker.list_capacity_effective() != 0) {
               // The rebinding commands reference multiple resources. Individual
               // allocation tracking calls can internally split the submission when
               // the allocation list is full; if that happens mid-sequence, earlier
               // tracked allocations are dropped and the submission would be missing
               // alloc-table entries for some binds. Pre-scan and split once before
               // tracking.
               std::array<UINT, 4 + 1 + 16 + 16 + 1> unique_allocs{};
               size_t unique_alloc_len = 0;
               auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
                 if (!res) {
                   return;
                 }
                 if (res->backing_alloc_id == 0) {
                   return;
                 }
                 if (res->wddm_hAllocation == 0) {
                   return;
                 }
                 const UINT alloc_id = res->backing_alloc_id;
                 for (size_t i = 0; i < unique_alloc_len; ++i) {
                   if (unique_allocs[i] == alloc_id) {
                     return;
                   }
                 }
                 unique_allocs[unique_alloc_len++] = alloc_id;
               };
 
               for (uint32_t i = 0; i < 4; ++i) {
                 add_alloc(dev->render_targets[i]);
               }
               add_alloc(dev->depth_stencil);
               for (uint32_t stage = 0; stage < 16; ++stage) {
                 if (is_backbuffer(dev->textures[stage])) {
                   add_alloc(dev->textures[stage]);
                 }
               }
               for (uint32_t stream = 0; stream < 16; ++stream) {
                 if (is_backbuffer(dev->streams[stream].vb)) {
                   add_alloc(dev->streams[stream].vb);
                 }
               }
               if (is_backbuffer(dev->index_buffer)) {
                 add_alloc(dev->index_buffer);
               }
 
               const UINT needed_total = static_cast<UINT>(unique_alloc_len);
               if (needed_total != 0) {
                 const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
                 if (needed_total > cap) {
                   ok = false;
                 } else {
                   UINT needed_new = 0;
                   for (size_t i = 0; i < unique_alloc_len; ++i) {
                     if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
                       needed_new++;
                     }
                   }
                   const UINT existing = dev->alloc_list_tracker.list_len();
                   if (existing > cap || needed_new > cap - existing) {
                     (void)submit(dev);
                     if (!ensure_cmd_space(dev, needed_bytes)) {
                       ok = false;
                     }
                   }
                 }
               }
             }
 
             // Track allocations referenced by the rebinding commands so the KMD can
             // resolve alloc_id -> GPA even if no draw occurs before the next
             // flush/present.
             if (ok && track_render_targets_locked(dev) < 0) {
               ok = false;
             }
             for (uint32_t stage = 0; ok && stage < 16; ++stage) {
               if (!is_backbuffer(dev->textures[stage])) {
                 continue;
               }
               if (track_resource_allocation_locked(dev, dev->textures[stage], /*write=*/false) < 0) {
                ok = false;
              }
            }
            for (uint32_t stream = 0; ok && stream < 16; ++stream) {
              if (!is_backbuffer(dev->streams[stream].vb)) {
                continue;
              }
              if (track_resource_allocation_locked(dev, dev->streams[stream].vb, /*write=*/false) < 0) {
                ok = false;
              }
            }
            if (ok && is_backbuffer(dev->index_buffer)) {
              if (track_resource_allocation_locked(dev, dev->index_buffer, /*write=*/false) < 0) {
                ok = false;
              }
            }

            ok = ok && emit_set_render_targets_locked(dev);
            for (uint32_t stage = 0; ok && stage < 16; ++stage) {
              if (!is_backbuffer(dev->textures[stage])) {
                continue;
              }
              auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
              if (!cmd) {
                ok = false;
                break;
              }
              cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
              cmd->slot = stage;
              cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
              cmd->reserved0 = 0;
            }

            for (uint32_t stream = 0; ok && stream < 16; ++stream) {
              if (!is_backbuffer(dev->streams[stream].vb)) {
                continue;
              }

              aerogpu_vertex_buffer_binding binding{};
              binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
              binding.stride_bytes = dev->streams[stream].stride_bytes;
              binding.offset_bytes = dev->streams[stream].offset_bytes;
              binding.reserved0 = 0;

              auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
                  dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
              if (!cmd) {
                ok = false;
                break;
              }
              cmd->start_slot = stream;
              cmd->buffer_count = 1;
            }

            if (ok && is_backbuffer(dev->index_buffer)) {
              auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
              if (!cmd) {
                ok = false;
              } else {
                cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
                cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
                cmd->offset_bytes = dev->index_offset_bytes;
                cmd->reserved0 = 0;
              }
            }

            if (!ok) {
              // Preserve device/host state consistency: if we cannot emit the
              // rebinding commands, undo the rotation so future draws still target
              // the host's current bindings.
              undo_rotation();
              dev->cmd.reset();
              dev->alloc_list_tracker.reset();
            }
          }
        }
      }
    }
  }

  d3d9_trace_maybe_dump_on_present(present_count);
  return trace.ret(present_hr);
}

HRESULT AEROGPU_D3D9_CALL device_present(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_PRESENT* pPresent) {
  const uint64_t sc_ptr = pPresent ? d3d9_trace_arg_ptr(pPresent->hSwapChain.pDrvPrivate) : 0;
  const uint64_t src_ptr = pPresent ? d3d9_trace_arg_ptr(d3d9_present_src(*pPresent).pDrvPrivate) : 0;
  const uint64_t sync_flags = pPresent ? d3d9_trace_pack_u32_u32(d3d9_present_sync_interval(*pPresent),
                                                                 d3d9_present_flags(*pPresent))
                                       : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DevicePresent, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), sc_ptr, src_ptr, sync_flags);
  if (!hDevice.pDrvPrivate || !pPresent) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  const D3DDDI_HRESOURCE src_handle = d3d9_present_src(*pPresent);
  const uint32_t sync_interval = d3d9_present_sync_interval(*pPresent);
  const uint32_t present_flags = d3d9_present_flags(*pPresent);
  const HWND wnd = d3d9_present_hwnd(*pPresent);
  uint32_t present_count = 0;
  HRESULT present_hr = S_OK;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);

    bool occluded = false;
#if defined(_WIN32)
    HWND hwnd = wnd;
    if (!hwnd) {
      SwapChain* sc = as_swapchain(pPresent->hSwapChain);
      if (sc) {
        auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
        if (it == dev->swapchains.end()) {
          sc = nullptr;
        }
      }
      if (!sc) {
        sc = dev->current_swapchain;
      }
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      hwnd = sc ? sc->hwnd : nullptr;
    }
    if (hwnd) {
      if (IsIconic(hwnd)) {
        occluded = true;
      }
    }
#endif

    if (occluded) {
      retire_completed_presents_locked(dev);
      (void)submit(dev, /*is_present=*/false);

      dev->present_count++;
      present_count = dev->present_count;
      dev->present_refresh_count = dev->present_count;
      dev->sync_refresh_count = dev->present_count;
      dev->last_present_qpc = qpc_now();

      SwapChain* sc = as_swapchain(pPresent->hSwapChain);
      if (sc) {
        auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
        if (it == dev->swapchains.end()) {
          sc = nullptr;
        }
      }
      if (!sc) {
        sc = dev->current_swapchain;
      }
      if (!sc && !dev->swapchains.empty()) {
        sc = dev->swapchains[0];
      }
      if (sc) {
        sc->present_count++;
      }

      present_hr = kSPresentOccluded;
    } else {
    HRESULT hr = throttle_presents_locked(dev, present_flags);
    if (hr != S_OK) {
      return trace.ret(hr);
    }

    // Submit any pending render work via the Render callback before issuing a
    // Present submission. This ensures the KMD/emulator observes distinct
    // render vs present submissions (DxgkDdiRender vs DxgkDdiPresent).
    (void)submit(dev, /*is_present=*/false);

    // Track the present source allocation so the KMD can resolve it when the
    // Present callback hands the DMA buffer to the kernel.
    if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_present_ex), 4))) {
      return trace.ret(E_OUTOFMEMORY);
    }
    if (auto* src_res = as_resource(src_handle)) {
      const HRESULT track_hr = track_resource_allocation_locked(dev, src_res, /*write=*/false);
      if (track_hr < 0) {
        return trace.ret(track_hr);
      }
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_present_ex>(dev, AEROGPU_CMD_PRESENT_EX);
    if (!cmd) {
      return trace.ret(E_OUTOFMEMORY);
    }
    cmd->scanout_id = 0;
    bool vsync = (sync_interval != 0) && (sync_interval != kD3dPresentIntervalImmediate);
    if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
      vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
    }
    cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
    cmd->d3d9_present_flags = present_flags;
    cmd->reserved0 = 0;

    const uint64_t submit_fence = submit(dev, /*is_present=*/true);
    const uint64_t present_fence = submit_fence;
    if (present_fence) {
      dev->inflight_present_fences.push_back(present_fence);
    }

    dev->present_count++;
    present_count = dev->present_count;
    dev->present_refresh_count = dev->present_count;
    dev->sync_refresh_count = dev->present_count;
    dev->last_present_qpc = qpc_now();
    SwapChain* sc = as_swapchain(pPresent->hSwapChain);
    if (sc) {
      auto it = std::find(dev->swapchains.begin(), dev->swapchains.end(), sc);
      if (it == dev->swapchains.end()) {
        sc = nullptr;
      }
    }
    if (!sc) {
      sc = dev->current_swapchain;
    }
    if (!sc && (wnd || src_handle.pDrvPrivate)) {
      for (SwapChain* candidate : dev->swapchains) {
        if (!candidate) {
          continue;
        }
        if (wnd && candidate->hwnd == wnd) {
          sc = candidate;
          break;
        }
        if (src_handle.pDrvPrivate) {
          auto* src = as_resource(src_handle);
          if (src && std::find(candidate->backbuffers.begin(), candidate->backbuffers.end(), src) != candidate->backbuffers.end()) {
            sc = candidate;
            break;
          }
        }
      }
    }
    if (!sc && !dev->swapchains.empty()) {
      sc = dev->swapchains[0];
    }
    if (sc) {
      sc->present_count++;
      sc->last_present_fence = present_fence;
      if (sc->backbuffers.size() > 1 && sc->swap_effect != 0u) {
        auto is_backbuffer = [sc](const Resource* res) -> bool {
          if (!sc || !res) {
            return false;
          }
          return std::find(sc->backbuffers.begin(), sc->backbuffers.end(), res) != sc->backbuffers.end();
        };

        size_t needed_bytes = align_up(sizeof(aerogpu_cmd_set_render_targets), 4);
        for (uint32_t stage = 0; stage < 16; ++stage) {
          if (is_backbuffer(dev->textures[stage])) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_texture), 4);
          }
        }
        for (uint32_t stream = 0; stream < 16; ++stream) {
          if (is_backbuffer(dev->streams[stream].vb)) {
            needed_bytes += align_up(sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding), 4);
          }
        }
        if (is_backbuffer(dev->index_buffer)) {
          needed_bytes += align_up(sizeof(aerogpu_cmd_set_index_buffer), 4);
        }

        if (ensure_cmd_space(dev, needed_bytes)) {
          struct ResourceIdentity {
            aerogpu_handle_t handle = 0;
            uint32_t backing_alloc_id = 0;
            uint32_t backing_offset_bytes = 0;
            uint64_t share_token = 0;
            bool is_shared = false;
            bool is_shared_alias = false;
            bool locked = false;
            uint32_t locked_offset = 0;
            uint32_t locked_size = 0;
            uint32_t locked_flags = 0;
            WddmAllocationHandle wddm_hAllocation = 0;
            std::vector<uint8_t> storage;
            std::vector<uint8_t> shared_private_driver_data;
          };

          auto take_identity = [](Resource* res) -> ResourceIdentity {
            ResourceIdentity id{};
            id.handle = res->handle;
            id.backing_alloc_id = res->backing_alloc_id;
            id.backing_offset_bytes = res->backing_offset_bytes;
            id.share_token = res->share_token;
            id.is_shared = res->is_shared;
            id.is_shared_alias = res->is_shared_alias;
            id.locked = res->locked;
            id.locked_offset = res->locked_offset;
            id.locked_size = res->locked_size;
            id.locked_flags = res->locked_flags;
            id.wddm_hAllocation = res->wddm_hAllocation;
            id.storage = std::move(res->storage);
            id.shared_private_driver_data = std::move(res->shared_private_driver_data);
            return id;
          };

          auto put_identity = [](Resource* res, ResourceIdentity&& id) {
            res->handle = id.handle;
            res->backing_alloc_id = id.backing_alloc_id;
            res->backing_offset_bytes = id.backing_offset_bytes;
            res->share_token = id.share_token;
            res->is_shared = id.is_shared;
            res->is_shared_alias = id.is_shared_alias;
            res->locked = id.locked;
            res->locked_offset = id.locked_offset;
            res->locked_size = id.locked_size;
            res->locked_flags = id.locked_flags;
            res->wddm_hAllocation = id.wddm_hAllocation;
            res->storage = std::move(id.storage);
            res->shared_private_driver_data = std::move(id.shared_private_driver_data);
          };

          auto undo_rotation = [sc, &take_identity, &put_identity]() {
            ResourceIdentity undo_saved = take_identity(sc->backbuffers.back());
            for (size_t i = sc->backbuffers.size() - 1; i > 0; --i) {
              put_identity(sc->backbuffers[i], take_identity(sc->backbuffers[i - 1]));
            }
            put_identity(sc->backbuffers[0], std::move(undo_saved));
          };

          ResourceIdentity saved = take_identity(sc->backbuffers[0]);
          for (size_t i = 0; i + 1 < sc->backbuffers.size(); ++i) {
            put_identity(sc->backbuffers[i], take_identity(sc->backbuffers[i + 1]));
          }
          put_identity(sc->backbuffers.back(), std::move(saved));

          bool ok = true;
          if (dev->wddm_context.hContext != 0 &&
              dev->alloc_list_tracker.list_base() != nullptr &&
              dev->alloc_list_tracker.list_capacity_effective() != 0) {
            // See PresentEx: pre-scan all allocations referenced by the rebinding
            // commands and split once before tracking so we don't drop earlier
            // allocations when the list is full.
            std::array<UINT, 4 + 1 + 16 + 16 + 1> unique_allocs{};
            size_t unique_alloc_len = 0;
            auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
              if (!res) {
                return;
              }
              if (res->backing_alloc_id == 0) {
                return;
              }
              if (res->wddm_hAllocation == 0) {
                return;
              }
              const UINT alloc_id = res->backing_alloc_id;
              for (size_t i = 0; i < unique_alloc_len; ++i) {
                if (unique_allocs[i] == alloc_id) {
                  return;
                }
              }
              unique_allocs[unique_alloc_len++] = alloc_id;
            };

            for (uint32_t i = 0; i < 4; ++i) {
              add_alloc(dev->render_targets[i]);
            }
            add_alloc(dev->depth_stencil);
            for (uint32_t stage = 0; stage < 16; ++stage) {
              if (is_backbuffer(dev->textures[stage])) {
                add_alloc(dev->textures[stage]);
              }
            }
            for (uint32_t stream = 0; stream < 16; ++stream) {
              if (is_backbuffer(dev->streams[stream].vb)) {
                add_alloc(dev->streams[stream].vb);
              }
            }
            if (is_backbuffer(dev->index_buffer)) {
              add_alloc(dev->index_buffer);
            }

            const UINT needed_total = static_cast<UINT>(unique_alloc_len);
            if (needed_total != 0) {
              const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
              if (needed_total > cap) {
                ok = false;
              } else {
                UINT needed_new = 0;
                for (size_t i = 0; i < unique_alloc_len; ++i) {
                  if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
                    needed_new++;
                  }
                }
                const UINT existing = dev->alloc_list_tracker.list_len();
                if (existing > cap || needed_new > cap - existing) {
                  (void)submit(dev);
                  if (!ensure_cmd_space(dev, needed_bytes)) {
                    ok = false;
                  }
                }
              }
            }
          }

          if (ok && track_render_targets_locked(dev) < 0) {
            ok = false;
          }
          for (uint32_t stage = 0; ok && stage < 16; ++stage) {
            if (!is_backbuffer(dev->textures[stage])) {
              continue;
            }
            if (track_resource_allocation_locked(dev, dev->textures[stage], /*write=*/false) < 0) {
              ok = false;
            }
          }
          for (uint32_t stream = 0; ok && stream < 16; ++stream) {
            if (!is_backbuffer(dev->streams[stream].vb)) {
              continue;
            }
            if (track_resource_allocation_locked(dev, dev->streams[stream].vb, /*write=*/false) < 0) {
              ok = false;
            }
          }
          if (ok && is_backbuffer(dev->index_buffer)) {
            if (track_resource_allocation_locked(dev, dev->index_buffer, /*write=*/false) < 0) {
              ok = false;
            }
          }

          ok = ok && emit_set_render_targets_locked(dev);
          for (uint32_t stage = 0; ok && stage < 16; ++stage) {
            if (!is_backbuffer(dev->textures[stage])) {
              continue;
            }
            auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
            if (!cmd) {
              ok = false;
              break;
            }
            cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
            cmd->slot = stage;
            cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
            cmd->reserved0 = 0;
          }

          for (uint32_t stream = 0; ok && stream < 16; ++stream) {
            if (!is_backbuffer(dev->streams[stream].vb)) {
              continue;
            }

            aerogpu_vertex_buffer_binding binding{};
            binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
            binding.stride_bytes = dev->streams[stream].stride_bytes;
            binding.offset_bytes = dev->streams[stream].offset_bytes;
            binding.reserved0 = 0;

            auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
                dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
            if (!cmd) {
              ok = false;
              break;
            }
            cmd->start_slot = stream;
            cmd->buffer_count = 1;
          }

          if (ok && is_backbuffer(dev->index_buffer)) {
            auto* cmd = append_fixed_locked<aerogpu_cmd_set_index_buffer>(dev, AEROGPU_CMD_SET_INDEX_BUFFER);
            if (!cmd) {
              ok = false;
            } else {
              cmd->buffer = dev->index_buffer ? dev->index_buffer->handle : 0;
              cmd->format = d3d9_index_format_to_aerogpu(dev->index_format);
              cmd->offset_bytes = dev->index_offset_bytes;
              cmd->reserved0 = 0;
            }
          }

          if (!ok) {
            undo_rotation();
            dev->cmd.reset();
            dev->alloc_list_tracker.reset();
          }
        }
      }
    }
    }
  }

  d3d9_trace_maybe_dump_on_present(present_count);
  return trace.ret(present_hr);
}

HRESULT AEROGPU_D3D9_CALL device_set_maximum_frame_latency(
    D3DDDI_HDEVICE hDevice,
    uint32_t max_frame_latency) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetMaximumFrameLatency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(max_frame_latency),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (max_frame_latency == 0) {
    return trace.ret(E_INVALIDARG);
  }
  dev->max_frame_latency = std::clamp(max_frame_latency, kMaxFrameLatencyMin, kMaxFrameLatencyMax);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_maximum_frame_latency(
    D3DDDI_HDEVICE hDevice,
    uint32_t* pMaxFrameLatency) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetMaximumFrameLatency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pMaxFrameLatency),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pMaxFrameLatency) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pMaxFrameLatency = dev->max_frame_latency;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_present_stats(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_PRESENTSTATS* pStats) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetPresentStats,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pStats),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pStats) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  std::memset(pStats, 0, sizeof(*pStats));
  pStats->PresentCount = dev->present_count;
  pStats->PresentRefreshCount = dev->present_refresh_count;
  pStats->SyncRefreshCount = dev->sync_refresh_count;
  pStats->SyncQPCTime = static_cast<int64_t>(dev->last_present_qpc);
  pStats->SyncGPUTime = 0;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_last_present_count(
    D3DDDI_HDEVICE hDevice,
    uint32_t* pLastPresentCount) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetLastPresentCount,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pLastPresentCount),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pLastPresentCount) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pLastPresentCount = dev->present_count;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_set_gpu_thread_priority(D3DDDI_HDEVICE hDevice, int32_t priority) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceSetGPUThreadPriority,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(static_cast<uint32_t>(priority)),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->gpu_thread_priority = std::clamp(priority, kMinGpuThreadPriority, kMaxGpuThreadPriority);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_gpu_thread_priority(D3DDDI_HDEVICE hDevice, int32_t* pPriority) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetGPUThreadPriority,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pPriority),
                      0,
                      0);
  if (!hDevice.pDrvPrivate || !pPriority) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pPriority = dev->gpu_thread_priority;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_query_resource_residency(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_QUERYRESOURCERESIDENCY* pArgs) {
  const uint32_t resource_count = pArgs ? d3d9_query_resource_residency_count(*pArgs) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceQueryResourceResidency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(resource_count),
                      pArgs ? d3d9_trace_arg_ptr(pArgs->pResidencyStatus) : 0,
                      d3d9_trace_arg_ptr(pArgs));
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  // System-memory-only model: resources are always considered resident.
  AEROGPU_D3D9_STUB_LOG_ONCE();

  if (pArgs && pArgs->pResidencyStatus) {
    for (uint32_t i = 0; i < resource_count; i++) {
      pArgs->pResidencyStatus[i] = 1;
    }
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_display_mode_ex(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_GETDISPLAYMODEEX* pGetModeEx) {
  const uint64_t mode_ptr = pGetModeEx ? d3d9_trace_arg_ptr(pGetModeEx->pMode) : 0;
  const uint64_t rotation_ptr = pGetModeEx ? d3d9_trace_arg_ptr(pGetModeEx->pRotation) : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetDisplayModeEx,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pGetModeEx),
                      mode_ptr,
                      rotation_ptr);
  if (!hDevice.pDrvPrivate || !pGetModeEx) {
    return trace.ret(E_INVALIDARG);
  }

  AEROGPU_D3D9_STUB_LOG_ONCE();

  auto* dev = as_device(hDevice);
  Adapter* adapter = dev->adapter;
  if (!adapter) {
    return trace.ret(E_FAIL);
  }

  if (pGetModeEx->pMode) {
    D3DDDI_DISPLAYMODEEX mode{};
    mode.Size = sizeof(D3DDDI_DISPLAYMODEEX);
    mode.Width = adapter->primary_width;
    mode.Height = adapter->primary_height;
    mode.RefreshRate = adapter->primary_refresh_hz;
    mode.Format = adapter->primary_format;
    // D3DDDI_SCANLINEORDERING_PROGRESSIVE (Win7) - numeric value.
    mode.ScanLineOrdering = 1;
    *pGetModeEx->pMode = mode;
  }

  if (pGetModeEx->pRotation) {
    *pGetModeEx->pRotation = static_cast<D3DDDI_ROTATION>(adapter->primary_rotation);
  }

  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_compose_rects(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_COMPOSERECTS* pComposeRects) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceComposeRects,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(pComposeRects),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  // ComposeRects is used by some D3D9Ex clients (including DWM in some modes).
  // Initial bring-up: accept and no-op to keep composition alive.
  AEROGPU_D3D9_STUB_LOG_ONCE();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_flush(D3DDDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceFlush, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  return trace.ret(flush_locked(dev));
}

HRESULT AEROGPU_D3D9_CALL device_wait_for_vblank(D3DDDI_HDEVICE hDevice, uint32_t swap_chain_index) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceWaitForVBlank,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(swap_chain_index),
                      0,
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    sleep_ms(16);
    return trace.ret(S_OK);
  }

#if defined(_WIN32)
  uint32_t period_ms = 16;
  if (dev->adapter->primary_refresh_hz != 0) {
    period_ms = std::max<uint32_t>(1, 1000u / dev->adapter->primary_refresh_hz);
  }
  // Some display stacks (particularly remote/virtualised ones) can report bizarre
  // refresh rates (e.g. 1Hz, or extremely high values that would otherwise lead
  // to near-zero sleep times). Clamp the computed period so WaitForVBlank
  // remains bounded and DWM never stalls for seconds or devolves into a busy
  // loop.
  period_ms = std::clamp<uint32_t>(period_ms, 4u, 50u);

  // Prefer a real vblank wait when possible (KMD-backed scanline polling),
  // but always keep the wait bounded so DWM cannot hang if vblank delivery is
  // broken.
  const uint32_t timeout_ms = std::min<uint32_t>(40, std::max<uint32_t>(1, period_ms * 2));
  uint32_t vid_pn_source_id = 0;
  if (dev->adapter->vid_pn_source_id_valid) {
    vid_pn_source_id = dev->adapter->vid_pn_source_id;
  }
  if (dev->adapter->kmd_query.WaitForVBlank(vid_pn_source_id, timeout_ms)) {
    return trace.ret(S_OK);
  }
  sleep_ms(std::min<uint32_t>(period_ms, timeout_ms));
#else
  sleep_ms(16);
#endif
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_check_resource_residency(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE* pResources,
    uint32_t count) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCheckResourceResidency,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      static_cast<uint64_t>(count),
                      d3d9_trace_arg_ptr(pResources),
                      0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  // System-memory-only model: resources are always considered resident.
  AEROGPU_D3D9_STUB_LOG_ONCE();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_create_query(
    D3DDDI_HDEVICE hDevice,
    D3D9DDIARG_CREATEQUERY* pCreateQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceCreateQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pCreateQuery ? static_cast<uint64_t>(d3d9_query_type(*pCreateQuery)) : 0,
                      d3d9_trace_arg_ptr(pCreateQuery),
                      0);
  if (!hDevice.pDrvPrivate || !pCreateQuery) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  Adapter* adapter = dev->adapter;
  const uint32_t query_type = d3d9_query_type(*pCreateQuery);
  bool is_event = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (!adapter->event_query_type_known.load(std::memory_order_acquire)) {
      // Accept both the public D3DQUERYTYPE_EVENT (8) encoding and the DDI-style
      // encoding where EVENT is the first enum entry (0). Once observed, lock
      // in the value so we don't accidentally treat other query types as EVENT.
      if (query_type == 0u || query_type == kD3DQueryTypeEvent) {
        adapter->event_query_type.store(query_type, std::memory_order_relaxed);
        adapter->event_query_type_known.store(true, std::memory_order_release);
      }
    }
    const bool known = adapter->event_query_type_known.load(std::memory_order_acquire);
    const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
    is_event = known && (query_type == event_type);
  }

  if (!is_event) {
    pCreateQuery->hQuery.pDrvPrivate = nullptr;
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  auto q = std::make_unique<Query>();
  q->type = query_type;
  pCreateQuery->hQuery.pDrvPrivate = q.release();
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_destroy_query(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HQUERY hQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceDestroyQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      d3d9_trace_arg_ptr(hQuery.pDrvPrivate),
                      0,
                      0);
  auto* dev = as_device(hDevice);
  auto* q = as_query(hQuery);
  if (dev && q) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto& pending = dev->pending_event_queries;
    if (!pending.empty()) {
      pending.erase(std::remove(pending.begin(), pending.end(), q), pending.end());
    }
  }
  delete q;
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_issue_query(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_ISSUEQUERY* pIssueQuery) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceIssueQuery,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pIssueQuery ? d3d9_trace_arg_ptr(pIssueQuery->hQuery.pDrvPrivate) : 0,
                      pIssueQuery ? static_cast<uint64_t>(d3d9_present_flags(*pIssueQuery)) : 0,
                      0);
  if (!hDevice.pDrvPrivate || !pIssueQuery) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pIssueQuery->hQuery);
  if (!q) {
    return trace.ret(E_INVALIDARG);
  }
  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  Adapter* adapter = dev->adapter;
  const bool event_known = adapter->event_query_type_known.load(std::memory_order_acquire);
  const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
  const bool is_event =
      event_known ? (q->type == event_type) : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  const uint32_t flags = d3d9_present_flags(*pIssueQuery);
  // Some runtimes appear to pass 0 for END. Be permissive and treat both 0 and
  // the common END bit encodings as an END marker (0x1 in the public D3D9 API,
  // 0x2 in some DDI header vintages).
  const bool end = (flags == 0) || ((flags & kD3DIssueEnd) != 0) || ((flags & kD3DIssueEndAlt) != 0);
  if (!end) {
    return trace.ret(S_OK);
  }

  // D3D9Ex EVENT queries are polled by DWM using GetData(DONOTFLUSH). To keep
  // those polls non-blocking, we submit any recorded work here (so the query
  // latches a real per-submit fence value), but we intentionally do *not* make
  // the query visible to GetData(DONOTFLUSH) until a later explicit
  // flush/submission boundary (Flush/Present/GetData(FLUSH)).
  //
  const bool had_pending_cmds = !dev->cmd.empty();
  dev->pending_event_queries.erase(std::remove(dev->pending_event_queries.begin(),
                                               dev->pending_event_queries.end(),
                                               q),
                                   dev->pending_event_queries.end());
  q->issued.store(true, std::memory_order_release);
  q->completion_logged.store(false, std::memory_order_relaxed);

  if (!had_pending_cmds) {
    // No pending commands: associate the query with the most recent submission.
    q->fence_value.store(dev->last_submission_fence, std::memory_order_release);
    q->submitted.store(true, std::memory_order_release);
    return trace.ret(S_OK);
  }

  const uint64_t issue_fence = submit(dev);

  q->fence_value.store(issue_fence, std::memory_order_release);
  q->submitted.store(false, std::memory_order_relaxed);
  dev->pending_event_queries.push_back(q);
  return trace.ret(S_OK);
}

HRESULT AEROGPU_D3D9_CALL device_get_query_data(
    D3DDDI_HDEVICE hDevice,
    const D3D9DDIARG_GETQUERYDATA* pGetQueryData) {
  const uint64_t data_flags = pGetQueryData ? d3d9_trace_pack_u32_u32(d3d9_query_data_size(*pGetQueryData),
                                                                      d3d9_present_flags(*pGetQueryData))
                                            : 0;
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetQueryData,
                      d3d9_trace_arg_ptr(hDevice.pDrvPrivate),
                      pGetQueryData ? d3d9_trace_arg_ptr(pGetQueryData->hQuery.pDrvPrivate) : 0,
                      data_flags,
                      pGetQueryData ? d3d9_trace_arg_ptr(pGetQueryData->pData) : 0);
  if (!hDevice.pDrvPrivate || !pGetQueryData) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pGetQueryData->hQuery);
  if (!q) {
    return trace.ret(E_INVALIDARG);
  }

  if (!dev || !dev->adapter) {
    return trace.ret(E_FAIL);
  }
  Adapter* adapter = dev->adapter;
  const uint32_t data_size = d3d9_query_data_size(*pGetQueryData);
  const uint32_t flags = d3d9_present_flags(*pGetQueryData);

  const bool event_known = adapter->event_query_type_known.load(std::memory_order_acquire);
  const uint32_t event_type = adapter->event_query_type.load(std::memory_order_relaxed);
  const bool is_event =
      event_known ? (q->type == event_type) : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return trace.ret(D3DERR_NOTAVAILABLE);
  }

  const bool has_data_ptr = (pGetQueryData->pData != nullptr);
  const bool has_data_size = (data_size != 0);
  // Mirror IDirect3DQuery9::GetData validation: pData must be NULL iff data_size
  // is 0. Treat mismatched pointer/size as D3DERR_INVALIDCALL.
  if (has_data_ptr != has_data_size) {
    return trace.ret(D3DERR_INVALIDCALL);
  }

  // EVENT queries return a BOOL-like DWORD; validate the output buffer size even
  // when the query is not yet ready so callers observe D3DERR_INVALIDCALL.
  if (has_data_ptr && data_size < sizeof(uint32_t)) {
    return trace.ret(D3DERR_INVALIDCALL);
  }

  // If no output buffer provided, just report readiness via HRESULT.
  const bool need_data = has_data_ptr;

  if (!q->issued.load(std::memory_order_acquire)) {
    // D3D9 clients can call GetData before Issue(END). Treat it as "not ready"
    // rather than a hard error to keep polling code (DWM) robust.
    if (need_data && data_size >= sizeof(uint32_t)) {
      *reinterpret_cast<uint32_t*>(pGetQueryData->pData) = FALSE;
    }
    return trace.ret(S_FALSE);
  }

  // EVENT query has been issued but not yet associated with a submission fence.
  // This happens when Issue(END) was called but we have not hit a flush/submission
  // boundary yet.
  if (!q->submitted.load(std::memory_order_acquire)) {
    if (flags & kD3DGetDataFlush) {
      // Non-blocking GetData(FLUSH): attempt a single flush to force a submission
      // boundary, then re-check. Never wait here (DWM can call into GetData while
      // holding global locks). Also avoid blocking on the device mutex: if another
      // thread is inside the UMD we skip the flush attempt and fall back to
      // polling.
      std::unique_lock<std::mutex> dev_lock(dev->mutex, std::try_to_lock);
      if (dev_lock.owns_lock()) {
        (void)flush_locked(dev);
      }
    }
    if (!q->submitted.load(std::memory_order_acquire)) {
      return trace.ret(S_FALSE);
    }
  }

  uint64_t fence_value = q->fence_value.load(std::memory_order_acquire);

  FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  if (wait_res == FenceWaitResult::NotReady && (flags & kD3DGetDataFlush)) {
    // Non-blocking GetData(FLUSH): attempt a single flush then re-check. Never
    // wait here (DWM can call into GetData while holding global locks). Also
    // avoid blocking on the device mutex: if another thread is inside the UMD
    // we skip the flush attempt and fall back to polling.
    std::unique_lock<std::mutex> dev_lock(dev->mutex, std::try_to_lock);
    if (dev_lock.owns_lock()) {
      (void)flush_locked(dev);
    }
    fence_value = q->fence_value.load(std::memory_order_acquire);
    wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  }

  if (wait_res == FenceWaitResult::Complete) {
    if (need_data) {
      // D3DQUERYTYPE_EVENT expects a BOOL-like result.
      if (data_size < sizeof(uint32_t)) {
        return trace.ret(D3DERR_INVALIDCALL);
      }
      *reinterpret_cast<uint32_t*>(pGetQueryData->pData) = TRUE;
    }
    (void)q->completion_logged.exchange(true, std::memory_order_relaxed);
    return trace.ret(S_OK);
  }
  if (wait_res == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  return trace.ret(S_FALSE);
}

HRESULT AEROGPU_D3D9_CALL device_wait_for_idle(D3DDDI_HDEVICE hDevice) {
  D3d9TraceCall trace(D3d9TraceFunc::DeviceWaitForIdle, d3d9_trace_arg_ptr(hDevice.pDrvPrivate), 0, 0, 0);
  if (!hDevice.pDrvPrivate) {
    return trace.ret(E_INVALIDARG);
  }
  auto* dev = as_device(hDevice);
  if (!dev) {
    return trace.ret(E_INVALIDARG);
  }

  uint64_t fence_value = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence_value = submit(dev);
  }
  if (fence_value == 0) {
    return trace.ret(S_OK);
  }

  // Never block indefinitely in a DDI call. Waiting for idle should be best-effort:
  // if the GPU stops making forward progress we return a non-fatal "still drawing"
  // code so callers can decide how to proceed.
  const uint64_t deadline = monotonic_ms() + 2000;
  while (monotonic_ms() < deadline) {
    const uint64_t now = monotonic_ms();
    const uint64_t remaining = (deadline > now) ? (deadline - now) : 0;
    const uint64_t slice = std::min<uint64_t>(remaining, 250);

    const FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/slice);
    if (wait_res == FenceWaitResult::Complete) {
      return trace.ret(S_OK);
    }
    if (wait_res == FenceWaitResult::Failed) {
      return trace.ret(E_FAIL);
    }
  }

  const FenceWaitResult final_check = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  if (final_check == FenceWaitResult::Complete) {
    return trace.ret(S_OK);
  }
  if (final_check == FenceWaitResult::Failed) {
    return trace.ret(E_FAIL);
  }
  return trace.ret(kD3dErrWasStillDrawing);
}

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
namespace {

std::atomic<uint64_t> g_raster_status_sim_line{0};

template <typename T, typename = void>
struct has_member_InVBlank : std::false_type {};

template <typename T>
struct has_member_InVBlank<T, std::void_t<decltype(std::declval<T>().InVBlank)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_InVerticalBlank : std::false_type {};

template <typename T>
struct has_member_InVerticalBlank<T, std::void_t<decltype(std::declval<T>().InVerticalBlank)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ScanLine : std::false_type {};

template <typename T>
struct has_member_ScanLine<T, std::void_t<decltype(std::declval<T>().ScanLine)>> : std::true_type {};

template <typename RasterStatusT>
void write_raster_status(RasterStatusT* out, bool in_vblank, uint32_t scan_line) {
  if (!out) {
    return;
  }
  if constexpr (has_member_InVBlank<RasterStatusT>::value) {
    out->InVBlank = in_vblank ? TRUE : FALSE;
  } else if constexpr (has_member_InVerticalBlank<RasterStatusT>::value) {
    out->InVerticalBlank = in_vblank ? TRUE : FALSE;
  }
  if constexpr (has_member_ScanLine<RasterStatusT>::value) {
    out->ScanLine = scan_line;
  }
}

template <typename DeviceHandleT, typename SwapChainT, typename RasterStatusT>
HRESULT device_get_raster_status_impl(DeviceHandleT hDevice, SwapChainT swap_chain, RasterStatusT* pRasterStatus) {
  const auto packed = d3d9_stub_trace_args(hDevice, swap_chain, pRasterStatus);
  D3d9TraceCall trace(D3d9TraceFunc::DeviceGetRasterStatus, packed[0], packed[1], packed[2], packed[3]);

  if (!pRasterStatus) {
    return trace.ret(E_INVALIDARG);
  }

  void* drv_private = nullptr;
  if constexpr (aerogpu_has_member_pDrvPrivate<DeviceHandleT>::value) {
    drv_private = hDevice.pDrvPrivate;
  }
  if (!drv_private) {
    write_raster_status(pRasterStatus, /*in_vblank=*/false, /*scan_line=*/0);
    return trace.ret(E_INVALIDARG);
  }

  auto* dev = reinterpret_cast<Device*>(drv_private);
  Adapter* adapter = dev->adapter;
  if (!adapter) {
    write_raster_status(pRasterStatus, /*in_vblank=*/false, /*scan_line=*/0);
    return trace.ret(S_OK);
  }

  bool in_vblank = false;
  uint32_t scan_line = 0;
  const uint32_t vid_pn_source_id = adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0;
  const bool ok = adapter->kmd_query.GetScanLine(vid_pn_source_id, &in_vblank, &scan_line);
  if (!ok) {
    const uint32_t height = adapter->primary_height ? adapter->primary_height : 768u;
    const uint32_t vblank_lines = std::max<uint32_t>(1u, height / 20u);
    const uint32_t total_lines = height + vblank_lines;
    const uint64_t tick = g_raster_status_sim_line.fetch_add(1, std::memory_order_relaxed);
    const uint32_t pos = static_cast<uint32_t>(tick % total_lines);
    in_vblank = (pos >= height);
    scan_line = in_vblank ? 0u : pos;
  }

  write_raster_status(pRasterStatus, in_vblank, scan_line);
  return trace.ret(S_OK);
}

template <typename... Args>
HRESULT device_get_raster_status_dispatch(Args... args) {
  if constexpr (sizeof...(Args) == 3) {
    return device_get_raster_status_impl(args...);
  }
  return D3DERR_NOTAVAILABLE;
}

template <typename Fn>
struct aerogpu_d3d9_impl_pfnGetRasterStatus;

template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRasterStatus<Ret(__stdcall*)(Args...)> {
  static Ret __stdcall pfnGetRasterStatus(Args... args) {
    return static_cast<Ret>(device_get_raster_status_dispatch(args...));
  }
};

template <typename Ret, typename... Args>
struct aerogpu_d3d9_impl_pfnGetRasterStatus<Ret(*)(Args...)> {
  static Ret pfnGetRasterStatus(Args... args) {
    return static_cast<Ret>(device_get_raster_status_dispatch(args...));
  }
};

} // namespace
#endif

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    D3D9DDI_DEVICEFUNCS* pDeviceFuncs) {
  const uint64_t adapter_ptr = pCreateDevice ? d3d9_trace_arg_ptr(pCreateDevice->hAdapter.pDrvPrivate) : 0;
  const uint64_t flags = pCreateDevice ? static_cast<uint64_t>(pCreateDevice->Flags) : 0;
  D3d9TraceCall trace(
      D3d9TraceFunc::AdapterCreateDevice, adapter_ptr, flags, d3d9_trace_arg_ptr(pDeviceFuncs), d3d9_trace_arg_ptr(pCreateDevice));
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (!pCreateDevice || !pDeviceFuncs) {
    return trace.ret(E_INVALIDARG);
  }

  auto* adapter = as_adapter(pCreateDevice->hAdapter);
  if (!adapter) {
    return trace.ret(E_INVALIDARG);
  }

  std::unique_ptr<Device> dev;
  try {
    dev = std::make_unique<Device>(adapter);
  } catch (...) {
    return trace.ret(E_OUTOFMEMORY);
  }

  // Publish the device handle early so the runtime has a valid cookie for any
  // follow-up DDIs (including error paths).
  pCreateDevice->hDevice.pDrvPrivate = dev.get();

  if (!pCreateDevice->pCallbacks) {
    aerogpu::logf("aerogpu-d3d9: CreateDevice missing device callbacks\n");
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(E_INVALIDARG);
  }

  dev->wddm_callbacks = *pCreateDevice->pCallbacks;

  {
    static std::once_flag wddm_cb_once;
    std::call_once(wddm_cb_once, [dev] {
      const void* submit_cb = nullptr;
      const void* render_cb = nullptr;
      const void* present_cb = nullptr;
      bool submit_cb_can_present = false;
      bool render_cb_can_present = false;
      if constexpr (has_pfnSubmitCommandCb<WddmDeviceCallbacks>::value) {
        submit_cb = reinterpret_cast<const void*>(dev->wddm_callbacks.pfnSubmitCommandCb);
        using SubmitCbT = decltype(dev->wddm_callbacks.pfnSubmitCommandCb);
        submit_cb_can_present = submit_callback_can_signal_present<SubmitCbT>();
      }
      if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
        render_cb = reinterpret_cast<const void*>(dev->wddm_callbacks.pfnRenderCb);
        using RenderCbT = decltype(dev->wddm_callbacks.pfnRenderCb);
        render_cb_can_present = submit_callback_can_signal_present<RenderCbT>();
      }
      if constexpr (has_pfnPresentCb<WddmDeviceCallbacks>::value) {
        present_cb = reinterpret_cast<const void*>(dev->wddm_callbacks.pfnPresentCb);
      }
      aerogpu::logf("aerogpu-d3d9: WDDM callbacks SubmitCommandCb=%p RenderCb=%p PresentCb=%p\n",
                    submit_cb, render_cb, present_cb);
      if (submit_cb) {
        aerogpu::logf("aerogpu-d3d9: SubmitCommandCb can_signal_present=%u\n", submit_cb_can_present ? 1u : 0u);
      }
      if (render_cb) {
        aerogpu::logf("aerogpu-d3d9: RenderCb can_signal_present=%u\n", render_cb_can_present ? 1u : 0u);
      }
    });
  }

  HRESULT hr = wddm_create_device(dev->wddm_callbacks, adapter, &dev->wddm_device);
  if (FAILED(hr)) {
    aerogpu::logf("aerogpu-d3d9: CreateDeviceCb failed hr=0x%08x\n", static_cast<unsigned>(hr));
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(hr);
  }

  hr = wddm_create_context(dev->wddm_callbacks, dev->wddm_device, &dev->wddm_context);
  if (FAILED(hr)) {
    aerogpu::logf("aerogpu-d3d9: CreateContextCb failed hr=0x%08x\n", static_cast<unsigned>(hr));
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(hr);
  }

  // Some Win7-era header/runtime combinations may omit
  // `DmaBufferPrivateDataSize` even when providing `pDmaBufferPrivateData`. The
  // AeroGPU Win7 KMD expects the private-data blob to be present, and dxgkrnl
  // only forwards it when the size is non-zero.
  if (dev->wddm_context.pDmaBufferPrivateData && dev->wddm_context.DmaBufferPrivateDataSize == 0) {
    dev->wddm_context.DmaBufferPrivateDataSize = static_cast<uint32_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
  }

  // If the adapter wasn't opened through a path that initialized our KMD query
  // helper (e.g. missing HDC at OpenAdapter time), opportunistically initialize
  // it here. This enables fence polling when hSyncObject is absent/zero.
  if (!adapter->kmd_query_available.load(std::memory_order_acquire)) {
    bool kmd_ok = false;
    if (adapter->luid.LowPart != 0 || adapter->luid.HighPart != 0) {
      kmd_ok = adapter->kmd_query.InitFromLuid(adapter->luid);
    }
    if (!kmd_ok) {
      HDC hdc = GetDC(nullptr);
      if (hdc) {
        kmd_ok = adapter->kmd_query.InitFromHdc(hdc);
        ReleaseDC(nullptr, hdc);
      }
    }
    adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
  }

  // Populate best-effort adapter state that is normally discovered during
  // OpenAdapter* when the KMD query helper is initialized. Some runtimes can
  // reach CreateDevice without those paths having run (or without a usable HDC),
  // so refresh the values here once we have a working query channel.
  if (adapter->kmd_query_available.load(std::memory_order_acquire)) {
    if (!adapter->vid_pn_source_id_valid) {
      uint32_t vid_pn_source_id = 0;
      if (adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      }
    }

    if (!adapter->max_allocation_list_slot_id_logged.load(std::memory_order_acquire)) {
      uint32_t max_slot_id = 0;
      if (adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
        adapter->max_allocation_list_slot_id = max_slot_id;
        if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
          aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                        static_cast<unsigned>(max_slot_id));
        }
      }
    }

    if (!adapter->umd_private_valid) {
      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = static_cast<char>((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = static_cast<char>((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = static_cast<char>((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = static_cast<char>((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }

  // Determine whether CreateContext returned a usable persistent DMA buffer /
  // allocation list. If not, fall back to Allocate/GetCommandBuffer.
  const uint32_t min_cmd_buffer_size = static_cast<uint32_t>(
      sizeof(aerogpu_cmd_stream_header) + align_up(sizeof(aerogpu_cmd_set_render_targets), 4));
  const bool create_context_has_persistent_submit_buffers =
      dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= min_cmd_buffer_size &&
      dev->wddm_context.pAllocationList &&
      dev->wddm_context.AllocationListSize != 0 &&
      dev->wddm_context.pDmaBufferPrivateData &&
      dev->wddm_context.DmaBufferPrivateDataSize >= AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;

  if (!create_context_has_persistent_submit_buffers) {
    aerogpu::logf("aerogpu-d3d9: CreateContext did not provide persistent submit buffers; "
                  "will use Allocate/GetCommandBuffer (dma=%p cmd=%p size=%u alloc=%p entries=%u patch=%p entries=%u dma_priv=%p bytes=%u)\n",
                  dev->wddm_context.pDmaBuffer,
                  dev->wddm_context.pCommandBuffer,
                  static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                  dev->wddm_context.pAllocationList,
                  static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                  dev->wddm_context.pPatchLocationList,
                  static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                  dev->wddm_context.pDmaBufferPrivateData,
                  static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize));

    bool have_submit_cb = false;
    if constexpr (has_pfnSubmitCommandCb<WddmDeviceCallbacks>::value) {
      have_submit_cb = have_submit_cb || (dev->wddm_callbacks.pfnSubmitCommandCb != nullptr);
    }
    if constexpr (has_pfnRenderCb<WddmDeviceCallbacks>::value) {
      have_submit_cb = have_submit_cb || (dev->wddm_callbacks.pfnRenderCb != nullptr);
    }
    if constexpr (has_pfnPresentCb<WddmDeviceCallbacks>::value) {
      have_submit_cb = have_submit_cb || (dev->wddm_callbacks.pfnPresentCb != nullptr);
    }

    bool have_acquire_cb = false;
    if constexpr (has_pfnAllocateCb<WddmDeviceCallbacks>::value && has_pfnDeallocateCb<WddmDeviceCallbacks>::value) {
      have_acquire_cb = have_acquire_cb || (dev->wddm_callbacks.pfnAllocateCb != nullptr && dev->wddm_callbacks.pfnDeallocateCb != nullptr);
    }
    if constexpr (has_pfnGetCommandBufferCb<WddmDeviceCallbacks>::value) {
      have_acquire_cb = have_acquire_cb || (dev->wddm_callbacks.pfnGetCommandBufferCb != nullptr);
    }

    if (!have_submit_cb || !have_acquire_cb) {
      aerogpu::logf("aerogpu-d3d9: WDDM callbacks do not support submission without persistent buffers "
                    "(submit=%s acquire=%s)\n",
                    have_submit_cb ? "ok" : "missing",
                    have_acquire_cb ? "ok" : "missing");
      dev->wddm_context.destroy(dev->wddm_callbacks);
      wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
      dev->wddm_device = 0;
      pCreateDevice->hDevice.pDrvPrivate = nullptr;
      return trace.ret(E_FAIL);
    }
  }

  {
    static std::once_flag wddm_diag_once;
    const bool patch_list_present =
        dev->wddm_context.pPatchLocationList && dev->wddm_context.PatchLocationListSize != 0;

    const bool has_sync_object = (dev->wddm_context.hSyncObject != 0);
    const bool kmd_query_available = adapter->kmd_query_available.load(std::memory_order_acquire);
    AerogpuNtStatus sync_probe = kStatusNotSupported;
    if (has_sync_object) {
      sync_probe = static_cast<AerogpuNtStatus>(
          adapter->kmd_query.WaitForSyncObject(static_cast<uint32_t>(dev->wddm_context.hSyncObject),
                                               /*fence_value=*/1,
                                               /*timeout_ms=*/0));
    }
    const bool sync_object_wait_available =
        has_sync_object && (sync_probe == kStatusSuccess || sync_probe == kStatusTimeout);

    // `wait_for_fence()` uses different mechanisms depending on whether the caller
    // is doing a bounded wait (PresentEx throttling) or a non-blocking poll (EVENT
    // queries / GetData). Log both to make bring-up debugging on Win7 clearer.
    const char* bounded_wait_mode = "polling";
    if (sync_object_wait_available) {
      bounded_wait_mode = "sync_object";
    } else if (kmd_query_available) {
      bounded_wait_mode = "kmd_query";
    }

    const char* poll_wait_mode = "polling";
    if (kmd_query_available) {
      poll_wait_mode = "kmd_query";
    } else if (sync_object_wait_available) {
      poll_wait_mode = "sync_object";
    }

    std::call_once(wddm_diag_once,
                   [patch_list_present, bounded_wait_mode, poll_wait_mode, has_sync_object, kmd_query_available] {
      aerogpu::logf("aerogpu-d3d9: WDDM patch_list=%s (AeroGPU submits with NumPatchLocations=0)\n",
                    patch_list_present ? "present" : "absent");
      aerogpu::logf("aerogpu-d3d9: fence_wait bounded=%s poll=%s (hSyncObject=%s kmd_query=%s)\n",
                    bounded_wait_mode,
                    poll_wait_mode,
                    has_sync_object ? "present" : "absent",
                    kmd_query_available ? "available" : "unavailable");
    });
  }

  aerogpu::logf("aerogpu-d3d9: CreateDevice wddm_device=0x%08x hContext=0x%08x hSyncObject=0x%08x "
                "dma=%p cmd=%p bytes=%u alloc_list=%p entries=%u patch_list=%p entries=%u dma_priv=%p bytes=%u\n",
                static_cast<unsigned>(dev->wddm_device),
                static_cast<unsigned>(dev->wddm_context.hContext),
                static_cast<unsigned>(dev->wddm_context.hSyncObject),
                dev->wddm_context.pDmaBuffer,
                dev->wddm_context.pCommandBuffer,
                static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                dev->wddm_context.pAllocationList,
                static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                dev->wddm_context.pPatchLocationList,
                static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                dev->wddm_context.pDmaBufferPrivateData,
                static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize));

  // Wire the command stream builder to the runtime-provided DMA buffer so all
  // command emission paths write directly into `pCommandBuffer` (no per-submit
  // std::vector allocations). This is a prerequisite for real Win7 D3D9UMDDI
  // submission plumbing.
  if (dev->wddm_context.pCommandBuffer &&
      dev->wddm_context.CommandBufferSize >= sizeof(aerogpu_cmd_stream_header)) {
    dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
  }

  // Bind the per-submit allocation list tracker to the runtime-provided list so
  // command emission paths can populate D3DDDI_ALLOCATIONLIST entries as
  // resources are referenced (no patch list).
  dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                 dev->wddm_context.AllocationListSize,
                                 adapter->max_allocation_list_slot_id);

  std::memset(pDeviceFuncs, 0, sizeof(*pDeviceFuncs));

  pDeviceFuncs->pfnDestroyDevice = device_destroy;
  pDeviceFuncs->pfnCreateResource = device_create_resource;
  if constexpr (aerogpu_has_member_pfnOpenResource<D3D9DDI_DEVICEFUNCS>::value) {
    pDeviceFuncs->pfnOpenResource = device_open_resource;
  }
  if constexpr (aerogpu_has_member_pfnOpenResource2<D3D9DDI_DEVICEFUNCS>::value) {
    pDeviceFuncs->pfnOpenResource2 = device_open_resource2;
  }
  pDeviceFuncs->pfnDestroyResource = device_destroy_resource;
  pDeviceFuncs->pfnLock = device_lock;
  pDeviceFuncs->pfnUnlock = device_unlock;

  // Assign the remaining entrypoints through type-safe thunks so the compiler
  // enforces the WDK function pointer signatures.
#define AEROGPU_SET_D3D9DDI_FN(member, fn)                                                               \
  do {                                                                                                   \
    pDeviceFuncs->member = aerogpu_d3d9_ddi_thunk<decltype(pDeviceFuncs->member), fn>::thunk;            \
  } while (0)

  AEROGPU_SET_D3D9DDI_FN(pfnSetRenderTarget, device_set_render_target);
  AEROGPU_SET_D3D9DDI_FN(pfnSetDepthStencil, device_set_depth_stencil);
  AEROGPU_SET_D3D9DDI_FN(pfnSetViewport, device_set_viewport);
  AEROGPU_SET_D3D9DDI_FN(pfnSetScissorRect, device_set_scissor);
  AEROGPU_SET_D3D9DDI_FN(pfnSetTexture, device_set_texture);
  if constexpr (aerogpu_has_member_pfnSetTextureStageState<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetTextureStageState,
        aerogpu_d3d9_stub_pfnSetTextureStageState<decltype(pDeviceFuncs->pfnSetTextureStageState)>::pfnSetTextureStageState);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnSetSamplerState, device_set_sampler_state);
  AEROGPU_SET_D3D9DDI_FN(pfnSetRenderState, device_set_render_state);
  if constexpr (aerogpu_has_member_pfnSetMaterial<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetMaterial,
                           aerogpu_d3d9_stub_pfnSetMaterial<decltype(pDeviceFuncs->pfnSetMaterial)>::pfnSetMaterial);
  }
  if constexpr (aerogpu_has_member_pfnSetLight<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetLight, aerogpu_d3d9_stub_pfnSetLight<decltype(pDeviceFuncs->pfnSetLight)>::pfnSetLight);
  }
  if constexpr (aerogpu_has_member_pfnLightEnable<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnLightEnable,
        aerogpu_d3d9_stub_pfnLightEnable<decltype(pDeviceFuncs->pfnLightEnable)>::pfnLightEnable);
  }
  if constexpr (aerogpu_has_member_pfnSetNPatchMode<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetNPatchMode,
        aerogpu_d3d9_stub_pfnSetNPatchMode<decltype(pDeviceFuncs->pfnSetNPatchMode)>::pfnSetNPatchMode);
  }
  if constexpr (aerogpu_has_member_pfnSetGammaRamp<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetGammaRamp,
        aerogpu_d3d9_stub_pfnSetGammaRamp<decltype(pDeviceFuncs->pfnSetGammaRamp)>::pfnSetGammaRamp);
  }
  if constexpr (aerogpu_has_member_pfnSetTransform<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetTransform,
        aerogpu_d3d9_stub_pfnSetTransform<decltype(pDeviceFuncs->pfnSetTransform)>::pfnSetTransform);
  }
  if constexpr (aerogpu_has_member_pfnMultiplyTransform<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnMultiplyTransform,
        aerogpu_d3d9_stub_pfnMultiplyTransform<decltype(pDeviceFuncs->pfnMultiplyTransform)>::pfnMultiplyTransform);
  }
  if constexpr (aerogpu_has_member_pfnSetClipPlane<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetClipPlane,
        aerogpu_d3d9_stub_pfnSetClipPlane<decltype(pDeviceFuncs->pfnSetClipPlane)>::pfnSetClipPlane);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnCreateVertexDecl, device_create_vertex_decl);
  AEROGPU_SET_D3D9DDI_FN(pfnSetVertexDecl, device_set_vertex_decl);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyVertexDecl, device_destroy_vertex_decl);
  if constexpr (aerogpu_has_member_pfnSetFVF<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetFVF, device_set_fvf);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnCreateShader, device_create_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnSetShader, device_set_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyShader, device_destroy_shader);
  AEROGPU_SET_D3D9DDI_FN(pfnSetShaderConstF, device_set_shader_const_f);
  if constexpr (aerogpu_has_member_pfnSetShaderConstI<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetShaderConstI,
        aerogpu_d3d9_stub_pfnSetShaderConstI<decltype(pDeviceFuncs->pfnSetShaderConstI)>::pfnSetShaderConstI);
  }
  if constexpr (aerogpu_has_member_pfnSetShaderConstB<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetShaderConstB,
        aerogpu_d3d9_stub_pfnSetShaderConstB<decltype(pDeviceFuncs->pfnSetShaderConstB)>::pfnSetShaderConstB);
  }

  if constexpr (aerogpu_has_member_pfnCreateStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnCreateStateBlock,
        aerogpu_d3d9_impl_pfnCreateStateBlock<decltype(pDeviceFuncs->pfnCreateStateBlock)>::pfnCreateStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnDeleteStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnDeleteStateBlock,
        aerogpu_d3d9_impl_pfnDeleteStateBlock<decltype(pDeviceFuncs->pfnDeleteStateBlock)>::pfnDeleteStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnCaptureStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnCaptureStateBlock,
        aerogpu_d3d9_impl_pfnCaptureStateBlock<decltype(pDeviceFuncs->pfnCaptureStateBlock)>::pfnCaptureStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnApplyStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnApplyStateBlock,
        aerogpu_d3d9_impl_pfnApplyStateBlock<decltype(pDeviceFuncs->pfnApplyStateBlock)>::pfnApplyStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnValidateDevice<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnValidateDevice,
        aerogpu_d3d9_impl_pfnValidateDevice<decltype(pDeviceFuncs->pfnValidateDevice)>::pfnValidateDevice);
  }
  if constexpr (aerogpu_has_member_pfnSetSoftwareVertexProcessing<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetSoftwareVertexProcessing,
        aerogpu_d3d9_stub_pfnSetSoftwareVertexProcessing<decltype(
            pDeviceFuncs->pfnSetSoftwareVertexProcessing)>::pfnSetSoftwareVertexProcessing);
  }
  if constexpr (aerogpu_has_member_pfnSetCursorProperties<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetCursorProperties,
        aerogpu_d3d9_stub_pfnSetCursorProperties<decltype(pDeviceFuncs->pfnSetCursorProperties)>::pfnSetCursorProperties);
  }
  if constexpr (aerogpu_has_member_pfnSetCursorPosition<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetCursorPosition,
        aerogpu_d3d9_stub_pfnSetCursorPosition<decltype(pDeviceFuncs->pfnSetCursorPosition)>::pfnSetCursorPosition);
  }
  if constexpr (aerogpu_has_member_pfnShowCursor<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnShowCursor,
        aerogpu_d3d9_stub_pfnShowCursor<decltype(pDeviceFuncs->pfnShowCursor)>::pfnShowCursor);
  }
  if constexpr (aerogpu_has_member_pfnSetPaletteEntries<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetPaletteEntries,
        aerogpu_d3d9_stub_pfnSetPaletteEntries<decltype(pDeviceFuncs->pfnSetPaletteEntries)>::pfnSetPaletteEntries);
  }
  if constexpr (aerogpu_has_member_pfnSetCurrentTexturePalette<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetCurrentTexturePalette,
                           aerogpu_d3d9_stub_pfnSetCurrentTexturePalette<decltype(
                               pDeviceFuncs->pfnSetCurrentTexturePalette)>::pfnSetCurrentTexturePalette);
  }
  if constexpr (aerogpu_has_member_pfnSetClipStatus<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetClipStatus,
        aerogpu_d3d9_stub_pfnSetClipStatus<decltype(pDeviceFuncs->pfnSetClipStatus)>::pfnSetClipStatus);
  }
  if constexpr (aerogpu_has_member_pfnGetClipStatus<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetClipStatus,
        aerogpu_d3d9_stub_pfnGetClipStatus<decltype(pDeviceFuncs->pfnGetClipStatus)>::pfnGetClipStatus);
  }
  if constexpr (aerogpu_has_member_pfnGetGammaRamp<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetGammaRamp,
        aerogpu_d3d9_stub_pfnGetGammaRamp<decltype(pDeviceFuncs->pfnGetGammaRamp)>::pfnGetGammaRamp);
  }
  if constexpr (aerogpu_has_member_pfnDrawRectPatch<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnDrawRectPatch,
        aerogpu_d3d9_stub_pfnDrawRectPatch<decltype(pDeviceFuncs->pfnDrawRectPatch)>::pfnDrawRectPatch);
  }
  if constexpr (aerogpu_has_member_pfnDrawTriPatch<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnDrawTriPatch,
        aerogpu_d3d9_stub_pfnDrawTriPatch<decltype(pDeviceFuncs->pfnDrawTriPatch)>::pfnDrawTriPatch);
  }
  if constexpr (aerogpu_has_member_pfnDeletePatch<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnDeletePatch,
        aerogpu_d3d9_stub_pfnDeletePatch<decltype(pDeviceFuncs->pfnDeletePatch)>::pfnDeletePatch);
  }
  if constexpr (aerogpu_has_member_pfnProcessVertices<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnProcessVertices,
        aerogpu_d3d9_stub_pfnProcessVertices<decltype(pDeviceFuncs->pfnProcessVertices)>::pfnProcessVertices);
  }
  if constexpr (aerogpu_has_member_pfnGetRasterStatus<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetRasterStatus,
        aerogpu_d3d9_impl_pfnGetRasterStatus<decltype(pDeviceFuncs->pfnGetRasterStatus)>::pfnGetRasterStatus);
  }
  if constexpr (aerogpu_has_member_pfnSetDialogBoxMode<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetDialogBoxMode,
        aerogpu_d3d9_stub_pfnSetDialogBoxMode<decltype(pDeviceFuncs->pfnSetDialogBoxMode)>::pfnSetDialogBoxMode);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnSetStreamSource, device_set_stream_source);
  if constexpr (aerogpu_has_member_pfnSetStreamSourceFreq<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetStreamSourceFreq,
                           aerogpu_d3d9_stub_pfnSetStreamSourceFreq<decltype(
                               pDeviceFuncs->pfnSetStreamSourceFreq)>::pfnSetStreamSourceFreq);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnSetIndices, device_set_indices);
  if constexpr (aerogpu_has_member_pfnBeginScene<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnBeginScene, device_begin_scene);
  }
  if constexpr (aerogpu_has_member_pfnEndScene<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnEndScene, device_end_scene);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnClear, device_clear);
  AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitive, device_draw_primitive);
  if constexpr (aerogpu_has_member_pfnDrawPrimitiveUP<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitiveUP, device_draw_primitive_up);
  }
  if constexpr (aerogpu_has_member_pfnDrawIndexedPrimitiveUP<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawIndexedPrimitiveUP, device_draw_indexed_primitive_up);
  }
  AEROGPU_SET_D3D9DDI_FN(pfnDrawIndexedPrimitive, device_draw_indexed_primitive);
  if constexpr (aerogpu_has_member_pfnDrawPrimitive2<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawPrimitive2, device_draw_primitive2);
  }
  if constexpr (aerogpu_has_member_pfnDrawIndexedPrimitive2<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnDrawIndexedPrimitive2, device_draw_indexed_primitive2);
  }

  if constexpr (aerogpu_has_member_pfnGetSoftwareVertexProcessing<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetSoftwareVertexProcessing,
                           aerogpu_d3d9_stub_pfnGetSoftwareVertexProcessing<decltype(
                               pDeviceFuncs->pfnGetSoftwareVertexProcessing)>::pfnGetSoftwareVertexProcessing);
  }
  if constexpr (aerogpu_has_member_pfnGetTransform<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetTransform,
                           aerogpu_d3d9_stub_pfnGetTransform<decltype(pDeviceFuncs->pfnGetTransform)>::pfnGetTransform);
  }
  if constexpr (aerogpu_has_member_pfnGetClipPlane<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetClipPlane,
                           aerogpu_d3d9_stub_pfnGetClipPlane<decltype(pDeviceFuncs->pfnGetClipPlane)>::pfnGetClipPlane);
  }
  if constexpr (aerogpu_has_member_pfnGetViewport<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetViewport,
        aerogpu_d3d9_impl_pfnGetViewport<decltype(pDeviceFuncs->pfnGetViewport)>::pfnGetViewport);
  }
  if constexpr (aerogpu_has_member_pfnGetScissorRect<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetScissorRect,
        aerogpu_d3d9_impl_pfnGetScissorRect<decltype(pDeviceFuncs->pfnGetScissorRect)>::pfnGetScissorRect);
  }
  if constexpr (aerogpu_has_member_pfnBeginStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnBeginStateBlock,
        aerogpu_d3d9_impl_pfnBeginStateBlock<decltype(pDeviceFuncs->pfnBeginStateBlock)>::pfnBeginStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnEndStateBlock<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnEndStateBlock,
        aerogpu_d3d9_impl_pfnEndStateBlock<decltype(pDeviceFuncs->pfnEndStateBlock)>::pfnEndStateBlock);
  }
  if constexpr (aerogpu_has_member_pfnGetMaterial<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetMaterial,
                           aerogpu_d3d9_stub_pfnGetMaterial<decltype(pDeviceFuncs->pfnGetMaterial)>::pfnGetMaterial);
  }
  if constexpr (aerogpu_has_member_pfnGetLight<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetLight, aerogpu_d3d9_stub_pfnGetLight<decltype(pDeviceFuncs->pfnGetLight)>::pfnGetLight);
  }
  if constexpr (aerogpu_has_member_pfnGetLightEnable<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetLightEnable,
                           aerogpu_d3d9_stub_pfnGetLightEnable<decltype(
                               pDeviceFuncs->pfnGetLightEnable)>::pfnGetLightEnable);
  }
  if constexpr (aerogpu_has_member_pfnGetRenderTarget<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetRenderTarget,
        aerogpu_d3d9_impl_pfnGetRenderTarget<decltype(pDeviceFuncs->pfnGetRenderTarget)>::pfnGetRenderTarget);
  }
  if constexpr (aerogpu_has_member_pfnGetDepthStencil<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetDepthStencil,
        aerogpu_d3d9_impl_pfnGetDepthStencil<decltype(pDeviceFuncs->pfnGetDepthStencil)>::pfnGetDepthStencil);
  }
  if constexpr (aerogpu_has_member_pfnGetTexture<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetTexture,
        aerogpu_d3d9_impl_pfnGetTexture<decltype(pDeviceFuncs->pfnGetTexture)>::pfnGetTexture);
  }
  if constexpr (aerogpu_has_member_pfnGetTextureStageState<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetTextureStageState,
                           aerogpu_d3d9_stub_pfnGetTextureStageState<decltype(
                               pDeviceFuncs->pfnGetTextureStageState)>::pfnGetTextureStageState);
  }
  if constexpr (aerogpu_has_member_pfnGetSamplerState<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetSamplerState,
        aerogpu_d3d9_impl_pfnGetSamplerState<decltype(pDeviceFuncs->pfnGetSamplerState)>::pfnGetSamplerState);
  }
  if constexpr (aerogpu_has_member_pfnGetRenderState<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetRenderState,
        aerogpu_d3d9_impl_pfnGetRenderState<decltype(pDeviceFuncs->pfnGetRenderState)>::pfnGetRenderState);
  }
  if constexpr (aerogpu_has_member_pfnGetPaletteEntries<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetPaletteEntries,
                           aerogpu_d3d9_stub_pfnGetPaletteEntries<decltype(
                               pDeviceFuncs->pfnGetPaletteEntries)>::pfnGetPaletteEntries);
  }
  if constexpr (aerogpu_has_member_pfnGetCurrentTexturePalette<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetCurrentTexturePalette,
                           aerogpu_d3d9_stub_pfnGetCurrentTexturePalette<decltype(
                               pDeviceFuncs->pfnGetCurrentTexturePalette)>::pfnGetCurrentTexturePalette);
  }
  if constexpr (aerogpu_has_member_pfnGetNPatchMode<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetNPatchMode,
                           aerogpu_d3d9_stub_pfnGetNPatchMode<decltype(pDeviceFuncs->pfnGetNPatchMode)>::pfnGetNPatchMode);
  }
  if constexpr (aerogpu_has_member_pfnGetFVF<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetFVF,
        aerogpu_d3d9_impl_pfnGetFVF<decltype(pDeviceFuncs->pfnGetFVF)>::pfnGetFVF);
  }
  if constexpr (aerogpu_has_member_pfnGetVertexDecl<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetVertexDecl,
        aerogpu_d3d9_impl_pfnGetVertexDecl<decltype(pDeviceFuncs->pfnGetVertexDecl)>::pfnGetVertexDecl);
  }
  if constexpr (aerogpu_has_member_pfnGetStreamSource<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetStreamSource,
        aerogpu_d3d9_impl_pfnGetStreamSource<decltype(pDeviceFuncs->pfnGetStreamSource)>::pfnGetStreamSource);
  }
  if constexpr (aerogpu_has_member_pfnGetStreamSourceFreq<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetStreamSourceFreq,
                           aerogpu_d3d9_stub_pfnGetStreamSourceFreq<decltype(
                               pDeviceFuncs->pfnGetStreamSourceFreq)>::pfnGetStreamSourceFreq);
  }
  if constexpr (aerogpu_has_member_pfnGetIndices<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetIndices,
        aerogpu_d3d9_impl_pfnGetIndices<decltype(pDeviceFuncs->pfnGetIndices)>::pfnGetIndices);
  }
  if constexpr (aerogpu_has_member_pfnGetShader<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetShader,
        aerogpu_d3d9_impl_pfnGetShader<decltype(pDeviceFuncs->pfnGetShader)>::pfnGetShader);
  }
  if constexpr (aerogpu_has_member_pfnGetShaderConstF<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetShaderConstF,
        aerogpu_d3d9_impl_pfnGetShaderConstF<decltype(pDeviceFuncs->pfnGetShaderConstF)>::pfnGetShaderConstF);
  }
  if constexpr (aerogpu_has_member_pfnGetShaderConstI<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetShaderConstI,
                           aerogpu_d3d9_stub_pfnGetShaderConstI<decltype(
                               pDeviceFuncs->pfnGetShaderConstI)>::pfnGetShaderConstI);
  }
  if constexpr (aerogpu_has_member_pfnGetShaderConstB<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetShaderConstB,
                           aerogpu_d3d9_stub_pfnGetShaderConstB<decltype(
                               pDeviceFuncs->pfnGetShaderConstB)>::pfnGetShaderConstB);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnCreateSwapChain, device_create_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroySwapChain, device_destroy_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnGetSwapChain, device_get_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnSetSwapChain, device_set_swap_chain);
  AEROGPU_SET_D3D9DDI_FN(pfnReset, device_reset);
  AEROGPU_SET_D3D9DDI_FN(pfnResetEx, device_reset_ex);
  AEROGPU_SET_D3D9DDI_FN(pfnCheckDeviceState, device_check_device_state);

  if constexpr (aerogpu_has_member_pfnWaitForVBlank<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnWaitForVBlank, device_wait_for_vblank);
  }
  if constexpr (aerogpu_has_member_pfnSetGPUThreadPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetGPUThreadPriority, device_set_gpu_thread_priority);
  }
  if constexpr (aerogpu_has_member_pfnGetGPUThreadPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetGPUThreadPriority, device_get_gpu_thread_priority);
  }
  if constexpr (aerogpu_has_member_pfnCheckResourceResidency<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnCheckResourceResidency, device_check_resource_residency);
  }
  if constexpr (aerogpu_has_member_pfnQueryResourceResidency<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnQueryResourceResidency, device_query_resource_residency);
  }
  if constexpr (aerogpu_has_member_pfnSetPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnSetPriority,
                           aerogpu_d3d9_stub_pfnSetPriority<decltype(pDeviceFuncs->pfnSetPriority)>::pfnSetPriority);
  }
  if constexpr (aerogpu_has_member_pfnGetPriority<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetPriority,
                           aerogpu_d3d9_stub_pfnGetPriority<decltype(pDeviceFuncs->pfnGetPriority)>::pfnGetPriority);
  }
  if constexpr (aerogpu_has_member_pfnGetDisplayModeEx<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnGetDisplayModeEx, device_get_display_mode_ex);
  }
  if constexpr (aerogpu_has_member_pfnComposeRects<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(pfnComposeRects, device_compose_rects);
  }
  if constexpr (aerogpu_has_member_pfnSetConvolutionMonoKernel<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetConvolutionMonoKernel,
        aerogpu_d3d9_stub_pfnSetConvolutionMonoKernel<decltype(
            pDeviceFuncs->pfnSetConvolutionMonoKernel)>::pfnSetConvolutionMonoKernel);
  }
  if constexpr (aerogpu_has_member_pfnSetAutoGenFilterType<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnSetAutoGenFilterType,
        aerogpu_d3d9_stub_pfnSetAutoGenFilterType<decltype(
            pDeviceFuncs->pfnSetAutoGenFilterType)>::pfnSetAutoGenFilterType);
  }
  if constexpr (aerogpu_has_member_pfnGetAutoGenFilterType<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGetAutoGenFilterType,
        aerogpu_d3d9_stub_pfnGetAutoGenFilterType<decltype(
            pDeviceFuncs->pfnGetAutoGenFilterType)>::pfnGetAutoGenFilterType);
  }
  if constexpr (aerogpu_has_member_pfnGenerateMipSubLevels<D3D9DDI_DEVICEFUNCS>::value) {
    AEROGPU_SET_D3D9DDI_FN(
        pfnGenerateMipSubLevels,
        aerogpu_d3d9_stub_pfnGenerateMipSubLevels<decltype(
            pDeviceFuncs->pfnGenerateMipSubLevels)>::pfnGenerateMipSubLevels);
  }

  AEROGPU_SET_D3D9DDI_FN(pfnRotateResourceIdentities, device_rotate_resource_identities);
  pDeviceFuncs->pfnPresent = device_present;
  pDeviceFuncs->pfnPresentEx = device_present_ex;
  pDeviceFuncs->pfnFlush = device_flush;
  AEROGPU_SET_D3D9DDI_FN(pfnSetMaximumFrameLatency, device_set_maximum_frame_latency);
  AEROGPU_SET_D3D9DDI_FN(pfnGetMaximumFrameLatency, device_get_maximum_frame_latency);
  AEROGPU_SET_D3D9DDI_FN(pfnGetPresentStats, device_get_present_stats);
  AEROGPU_SET_D3D9DDI_FN(pfnGetLastPresentCount, device_get_last_present_count);

  AEROGPU_SET_D3D9DDI_FN(pfnCreateQuery, device_create_query);
  AEROGPU_SET_D3D9DDI_FN(pfnDestroyQuery, device_destroy_query);
  AEROGPU_SET_D3D9DDI_FN(pfnIssueQuery, device_issue_query);
  AEROGPU_SET_D3D9DDI_FN(pfnGetQueryData, device_get_query_data);
  AEROGPU_SET_D3D9DDI_FN(pfnGetRenderTargetData, device_get_render_target_data);
  AEROGPU_SET_D3D9DDI_FN(pfnCopyRects, device_copy_rects);
  AEROGPU_SET_D3D9DDI_FN(pfnWaitForIdle, device_wait_for_idle);

  AEROGPU_SET_D3D9DDI_FN(pfnBlt, device_blt);
  AEROGPU_SET_D3D9DDI_FN(pfnColorFill, device_color_fill);
  AEROGPU_SET_D3D9DDI_FN(pfnUpdateSurface, device_update_surface);
  AEROGPU_SET_D3D9DDI_FN(pfnUpdateTexture, device_update_texture);

#undef AEROGPU_SET_D3D9DDI_FN

  if (!d3d9_validate_nonnull_vtable(pDeviceFuncs, "D3D9DDI_DEVICEFUNCS")) {
    // Be defensive: if we ever miss wiring a function table entry (new WDK
    // members, missed stubs), fail device creation cleanly rather than returning
    // a partially-populated vtable that would crash the runtime on first call.
    aerogpu::logf("aerogpu-d3d9: CreateDevice: device vtable contains NULL entrypoints; failing\n");
    dev->wddm_context.destroy(dev->wddm_callbacks);
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(E_FAIL);
  }

  dev.release();
  return trace.ret(S_OK);
#else
  if (!pCreateDevice || !pDeviceFuncs) {
    return trace.ret(E_INVALIDARG);
  }
  auto* adapter = as_adapter(pCreateDevice->hAdapter);
  if (!adapter) {
    return trace.ret(E_INVALIDARG);
  }

  auto dev = std::make_unique<Device>(adapter);
  pCreateDevice->hDevice.pDrvPrivate = dev.get();

#if defined(_WIN32)
  if (pCreateDevice->pCallbacks) {
    dev->wddm_callbacks = *pCreateDevice->pCallbacks;

    HRESULT hr = wddm_create_device(dev->wddm_callbacks, adapter, &dev->wddm_device);
    if (FAILED(hr)) {
      aerogpu::logf("aerogpu-d3d9: CreateDeviceCb failed hr=0x%08x (falling back to stub submission)\n", static_cast<unsigned>(hr));
      dev->wddm_callbacks = {};
      dev->wddm_device = 0;
    } else {
      hr = wddm_create_context(dev->wddm_callbacks, dev->wddm_device, &dev->wddm_context);
      if (FAILED(hr)) {
        aerogpu::logf("aerogpu-d3d9: CreateContextCb failed hr=0x%08x (falling back to stub submission)\n", static_cast<unsigned>(hr));
        wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
        dev->wddm_device = 0;
        dev->wddm_callbacks = {};
      } else {
        // If the adapter wasn't opened through a path that initialized our KMD query
        // helper (e.g. missing HDC at OpenAdapter time), opportunistically initialize
        // it here. This enables fence polling when hSyncObject is absent/zero.
        if (!adapter->kmd_query_available.load(std::memory_order_acquire)) {
          bool kmd_ok = false;
          if (adapter->luid.LowPart != 0 || adapter->luid.HighPart != 0) {
            kmd_ok = adapter->kmd_query.InitFromLuid(adapter->luid);
          }
          if (!kmd_ok) {
            HDC hdc = GetDC(nullptr);
            if (hdc) {
              kmd_ok = adapter->kmd_query.InitFromHdc(hdc);
              ReleaseDC(nullptr, hdc);
            }
          }
          adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
        }

        if (adapter->kmd_query_available.load(std::memory_order_acquire)) {
          if (!adapter->vid_pn_source_id_valid) {
            uint32_t vid_pn_source_id = 0;
            if (adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
              adapter->vid_pn_source_id = vid_pn_source_id;
              adapter->vid_pn_source_id_valid = true;
            }
          }

          if (!adapter->max_allocation_list_slot_id_logged.load(std::memory_order_acquire)) {
            uint32_t max_slot_id = 0;
            if (adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
              adapter->max_allocation_list_slot_id = max_slot_id;
              if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
                aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                              static_cast<unsigned>(max_slot_id));
              }
            }
          }

          if (!adapter->umd_private_valid) {
            aerogpu_umd_private_v1 priv;
            std::memset(&priv, 0, sizeof(priv));
            if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
              adapter->umd_private = priv;
              adapter->umd_private_valid = true;

              char magicStr[5] = {0, 0, 0, 0, 0};
              magicStr[0] = static_cast<char>((priv.device_mmio_magic >> 0) & 0xFF);
              magicStr[1] = static_cast<char>((priv.device_mmio_magic >> 8) & 0xFF);
              magicStr[2] = static_cast<char>((priv.device_mmio_magic >> 16) & 0xFF);
              magicStr[3] = static_cast<char>((priv.device_mmio_magic >> 24) & 0xFF);

              aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                            priv.device_mmio_magic,
                            magicStr,
                            priv.device_abi_version_u32,
                            static_cast<unsigned long long>(priv.device_features),
                            priv.flags);
            }
          }
        }

        // Validate the runtime-provided submission buffers. These must be present for
        // DMA buffer construction.
        const uint32_t min_cmd_buffer_size = static_cast<uint32_t>(
            sizeof(aerogpu_cmd_stream_header) + align_up(sizeof(aerogpu_cmd_set_render_targets), 4));
        if (!dev->wddm_context.pCommandBuffer ||
            dev->wddm_context.CommandBufferSize < min_cmd_buffer_size ||
            !dev->wddm_context.pAllocationList || dev->wddm_context.AllocationListSize == 0 ||
            !dev->wddm_context.pDmaBufferPrivateData ||
            dev->wddm_context.DmaBufferPrivateDataSize < AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
          aerogpu::logf("aerogpu-d3d9: WDDM CreateContext returned invalid buffers "
                        "dma=%p cmd=%p size=%u alloc=%p size=%u patch=%p size=%u dma_priv=%p bytes=%u (need>=%u) sync=0x%08x\n",
                        dev->wddm_context.pDmaBuffer,
                        dev->wddm_context.pCommandBuffer,
                        static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                        dev->wddm_context.pAllocationList,
                        static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                        dev->wddm_context.pPatchLocationList,
                        static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                        dev->wddm_context.pDmaBufferPrivateData,
                        static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize),
                        static_cast<unsigned>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES),
                        static_cast<unsigned>(dev->wddm_context.hSyncObject));

          dev->wddm_context.destroy(dev->wddm_callbacks);
          wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
          dev->wddm_device = 0;
          dev->wddm_callbacks = {};
        } else {
          {
            static std::once_flag wddm_diag_once;
            const bool patch_list_present =
                dev->wddm_context.pPatchLocationList && dev->wddm_context.PatchLocationListSize != 0;

            const bool has_sync_object = (dev->wddm_context.hSyncObject != 0);
            const bool kmd_query_available = adapter->kmd_query_available.load(std::memory_order_acquire);
            AerogpuNtStatus sync_probe = kStatusNotSupported;
            if (has_sync_object) {
              sync_probe = static_cast<AerogpuNtStatus>(
                  adapter->kmd_query.WaitForSyncObject(static_cast<uint32_t>(dev->wddm_context.hSyncObject),
                                                       /*fence_value=*/1,
                                                       /*timeout_ms=*/0));
            }
            const bool sync_object_wait_available =
                has_sync_object && (sync_probe == kStatusSuccess || sync_probe == kStatusTimeout);

            // `wait_for_fence()` prefers different mechanisms depending on whether
            // the caller is doing a bounded wait (PresentEx throttling) or a poll
            // (EVENT queries / GetData). Log both so bring-up can quickly confirm
            // which fallback is active on a given runtime/configuration.
            const char* bounded_wait_mode = "polling";
            if (sync_object_wait_available) {
              bounded_wait_mode = "sync_object";
            } else if (kmd_query_available) {
              bounded_wait_mode = "kmd_query";
            }

            const char* poll_wait_mode = "polling";
            if (kmd_query_available) {
              poll_wait_mode = "kmd_query";
            } else if (sync_object_wait_available) {
              poll_wait_mode = "sync_object";
            }

            std::call_once(wddm_diag_once,
                           [patch_list_present, bounded_wait_mode, poll_wait_mode, has_sync_object, kmd_query_available] {
              aerogpu::logf("aerogpu-d3d9: WDDM patch_list=%s (AeroGPU submits with NumPatchLocations=0)\n",
                            patch_list_present ? "present" : "absent");
              aerogpu::logf("aerogpu-d3d9: fence_wait bounded=%s poll=%s (hSyncObject=%s kmd_query=%s)\n",
                            bounded_wait_mode,
                            poll_wait_mode,
                            has_sync_object ? "present" : "absent",
                            kmd_query_available ? "available" : "unavailable");
            });
          }

          aerogpu::logf("aerogpu-d3d9: CreateDevice wddm_device=0x%08x hContext=0x%08x hSyncObject=0x%08x "
                        "dma=%p cmd=%p bytes=%u alloc_list=%p entries=%u patch_list=%p entries=%u dma_priv=%p bytes=%u\n",
                        static_cast<unsigned>(dev->wddm_device),
                        static_cast<unsigned>(dev->wddm_context.hContext),
                        static_cast<unsigned>(dev->wddm_context.hSyncObject),
                        dev->wddm_context.pDmaBuffer,
                        dev->wddm_context.pCommandBuffer,
                        static_cast<unsigned>(dev->wddm_context.CommandBufferSize),
                        dev->wddm_context.pAllocationList,
                        static_cast<unsigned>(dev->wddm_context.AllocationListSize),
                        dev->wddm_context.pPatchLocationList,
                        static_cast<unsigned>(dev->wddm_context.PatchLocationListSize),
                        dev->wddm_context.pDmaBufferPrivateData,
                        static_cast<unsigned>(dev->wddm_context.DmaBufferPrivateDataSize));

          // Wire the command stream builder to the runtime-provided DMA buffer so all
          // command emission paths write directly into `pCommandBuffer` (no per-submit
          // std::vector allocations). This is a prerequisite for real Win7 D3D9UMDDI
          // submission plumbing.
          if (dev->wddm_context.pCommandBuffer &&
              dev->wddm_context.CommandBufferSize >= sizeof(aerogpu_cmd_stream_header)) {
            dev->cmd.set_span(dev->wddm_context.pCommandBuffer, dev->wddm_context.CommandBufferSize);
          }

          // Bind the per-submit allocation list tracker to the runtime-provided buffers
          // so allocation tracking works immediately (e.g. shared surface CreateResource
          // can reference its backing allocation before the first submit()).
          dev->alloc_list_tracker.rebind(reinterpret_cast<D3DDDI_ALLOCATIONLIST*>(dev->wddm_context.pAllocationList),
                                         dev->wddm_context.AllocationListSize,
                                         adapter->max_allocation_list_slot_id);
        }
      }
    }
  } else {
    static std::once_flag wddm_callbacks_missing_once;
    std::call_once(wddm_callbacks_missing_once, [] {
      aerogpu::logf("aerogpu-d3d9: CreateDevice missing WDDM callbacks; submissions will be stubbed\n");
    });
  }
#endif

  std::memset(pDeviceFuncs, 0, sizeof(*pDeviceFuncs));
  pDeviceFuncs->pfnDestroyDevice = device_destroy;
  pDeviceFuncs->pfnCreateResource = device_create_resource;
  pDeviceFuncs->pfnOpenResource = device_open_resource;
  pDeviceFuncs->pfnOpenResource2 = device_open_resource2;
  pDeviceFuncs->pfnDestroyResource = device_destroy_resource;
  pDeviceFuncs->pfnLock = device_lock;
  pDeviceFuncs->pfnUnlock = device_unlock;

  pDeviceFuncs->pfnSetRenderTarget = device_set_render_target;
  pDeviceFuncs->pfnSetDepthStencil = device_set_depth_stencil;
  pDeviceFuncs->pfnSetViewport = device_set_viewport;
  pDeviceFuncs->pfnSetScissorRect = device_set_scissor;
  pDeviceFuncs->pfnSetTexture = device_set_texture;
  pDeviceFuncs->pfnSetSamplerState = device_set_sampler_state;
  pDeviceFuncs->pfnSetRenderState = device_set_render_state;

  pDeviceFuncs->pfnCreateVertexDecl = device_create_vertex_decl;
  pDeviceFuncs->pfnSetVertexDecl = device_set_vertex_decl;
  pDeviceFuncs->pfnDestroyVertexDecl = device_destroy_vertex_decl;
  pDeviceFuncs->pfnSetFVF = device_set_fvf;

  pDeviceFuncs->pfnCreateShader = device_create_shader;
  pDeviceFuncs->pfnSetShader = device_set_shader;
  pDeviceFuncs->pfnDestroyShader = device_destroy_shader;
  pDeviceFuncs->pfnSetShaderConstF = device_set_shader_const_f;

  pDeviceFuncs->pfnSetStreamSource = device_set_stream_source;
  pDeviceFuncs->pfnSetIndices = device_set_indices;
  pDeviceFuncs->pfnBeginScene = device_begin_scene;
  pDeviceFuncs->pfnEndScene = device_end_scene;

  pDeviceFuncs->pfnClear = device_clear;
  pDeviceFuncs->pfnDrawPrimitive = device_draw_primitive;
  pDeviceFuncs->pfnDrawPrimitiveUP = device_draw_primitive_up;
  pDeviceFuncs->pfnDrawIndexedPrimitive = device_draw_indexed_primitive;
  pDeviceFuncs->pfnDrawPrimitive2 = device_draw_primitive2;
  pDeviceFuncs->pfnDrawIndexedPrimitive2 = device_draw_indexed_primitive2;
  pDeviceFuncs->pfnCreateSwapChain = device_create_swap_chain;
  pDeviceFuncs->pfnDestroySwapChain = device_destroy_swap_chain;
  pDeviceFuncs->pfnGetSwapChain = device_get_swap_chain;
  pDeviceFuncs->pfnSetSwapChain = device_set_swap_chain;
  pDeviceFuncs->pfnReset = device_reset;
  pDeviceFuncs->pfnResetEx = device_reset_ex;
  pDeviceFuncs->pfnCheckDeviceState = device_check_device_state;
  pDeviceFuncs->pfnWaitForVBlank = device_wait_for_vblank;
  pDeviceFuncs->pfnSetGPUThreadPriority = device_set_gpu_thread_priority;
  pDeviceFuncs->pfnGetGPUThreadPriority = device_get_gpu_thread_priority;
  pDeviceFuncs->pfnCheckResourceResidency = device_check_resource_residency;
  pDeviceFuncs->pfnQueryResourceResidency = device_query_resource_residency;
  pDeviceFuncs->pfnGetDisplayModeEx = device_get_display_mode_ex;
  pDeviceFuncs->pfnComposeRects = device_compose_rects;
  pDeviceFuncs->pfnRotateResourceIdentities = device_rotate_resource_identities;
  pDeviceFuncs->pfnPresent = device_present;
  pDeviceFuncs->pfnPresentEx = device_present_ex;
  pDeviceFuncs->pfnFlush = device_flush;
  pDeviceFuncs->pfnSetMaximumFrameLatency = device_set_maximum_frame_latency;
  pDeviceFuncs->pfnGetMaximumFrameLatency = device_get_maximum_frame_latency;
  pDeviceFuncs->pfnGetPresentStats = device_get_present_stats;
  pDeviceFuncs->pfnGetLastPresentCount = device_get_last_present_count;

  pDeviceFuncs->pfnCreateQuery = device_create_query;
  pDeviceFuncs->pfnDestroyQuery = device_destroy_query;
  pDeviceFuncs->pfnIssueQuery = device_issue_query;
  pDeviceFuncs->pfnGetQueryData = device_get_query_data;
  pDeviceFuncs->pfnGetRenderTargetData = device_get_render_target_data;
  pDeviceFuncs->pfnCopyRects = device_copy_rects;
  pDeviceFuncs->pfnWaitForIdle = device_wait_for_idle;

  pDeviceFuncs->pfnBlt = device_blt;
  pDeviceFuncs->pfnColorFill = device_color_fill;
  pDeviceFuncs->pfnUpdateSurface = device_update_surface;
  pDeviceFuncs->pfnUpdateTexture = device_update_texture;

  if (!d3d9_validate_nonnull_vtable(pDeviceFuncs, "D3D9DDI_DEVICEFUNCS")) {
    aerogpu::logf("aerogpu-d3d9: CreateDevice: device vtable contains NULL entrypoints; failing\n");
    pCreateDevice->hDevice.pDrvPrivate = nullptr;
    return trace.ret(E_FAIL);
  }

  dev.release();
  return trace.ret(S_OK);
#endif
}

HRESULT OpenAdapterCommon(const char* entrypoint,
                          UINT interface_version,
                          UINT umd_version,
                          D3DDDI_ADAPTERCALLBACKS* callbacks,
                          D3DDDI_ADAPTERCALLBACKS2* callbacks2,
                          const LUID& luid,
                          D3DDDI_HADAPTER* phAdapter,
                          D3D9DDI_ADAPTERFUNCS* pAdapterFuncs) {
  if (!entrypoint || !phAdapter || !pAdapterFuncs) {
    return E_INVALIDARG;
  }

#if defined(_WIN32)
  // Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
  // correct UMD bitness was loaded (System32 vs SysWOW64).
  static std::once_flag logged_module_path_once;
  std::call_once(logged_module_path_once, [] {
    HMODULE module = NULL;
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                               GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCSTR>(&OpenAdapterCommon),
                           &module)) {
      char path[MAX_PATH] = {};
      if (GetModuleFileNameA(module, path, static_cast<DWORD>(sizeof(path))) != 0) {
        aerogpu::logf("aerogpu-d3d9: module_path=%s\n", path);
      }
    }
  });
#endif

  if (interface_version == 0 || umd_version == 0) {
    aerogpu::logf("aerogpu-d3d9: %s invalid interface/version (%u/%u)\n",
                  entrypoint,
                  static_cast<unsigned>(interface_version),
                  static_cast<unsigned>(umd_version));
    return E_INVALIDARG;
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  // The D3D runtime passes a D3D_UMD_INTERFACE_VERSION in the OpenAdapter args.
  // Be defensive: if the runtime asks for a newer interface than the headers we
  // are compiled against, fail cleanly rather than returning a vtable that does
  // not match what the runtime expects.
  if (interface_version > D3D_UMD_INTERFACE_VERSION) {
    aerogpu::logf("aerogpu-d3d9: %s unsupported interface_version=%u (compiled=%u)\n",
                  entrypoint,
                  static_cast<unsigned>(interface_version),
                  static_cast<unsigned>(D3D_UMD_INTERFACE_VERSION));
    return E_INVALIDARG;
  }
#endif

  Adapter* adapter = acquire_adapter(luid, interface_version, umd_version, callbacks, callbacks2);
  if (!adapter) {
    return E_OUTOFMEMORY;
  }

  phAdapter->pDrvPrivate = adapter;

  std::memset(pAdapterFuncs, 0, sizeof(*pAdapterFuncs));
  pAdapterFuncs->pfnCloseAdapter = adapter_close;
  pAdapterFuncs->pfnGetCaps = adapter_get_caps;
  pAdapterFuncs->pfnCreateDevice = adapter_create_device;
  pAdapterFuncs->pfnQueryAdapterInfo = adapter_query_adapter_info;

  if (!d3d9_validate_nonnull_vtable(pAdapterFuncs, "D3D9DDI_ADAPTERFUNCS")) {
    aerogpu::logf("aerogpu-d3d9: %s: adapter vtable contains NULL entrypoints; failing\n", entrypoint);
    phAdapter->pDrvPrivate = nullptr;
    release_adapter(adapter);
    return E_FAIL;
  }

  aerogpu::logf("aerogpu-d3d9: %s Interface=%u Version=%u LUID=%08x:%08x\n",
                entrypoint,
                static_cast<unsigned>(interface_version),
                static_cast<unsigned>(umd_version),
                static_cast<unsigned>(luid.HighPart),
                static_cast<unsigned>(luid.LowPart));
  return S_OK;
}

} // namespace

uint64_t submit_locked(Device* dev, bool is_present) {
  return submit(dev, is_present);
}

aerogpu_handle_t allocate_global_handle(Adapter* adapter) {
  if (!adapter) {
    return 0;
  }

#if defined(_WIN32)
  // Protocol object handles live in a single global namespace on the host (Win7
  // KMD currently submits context_id=0), so they must be unique across the
  // entire guest (multi-process, cross-API). Allocate them from a single
  // cross-process counter shared by all UMDs (D3D9 + D3D10/11).
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";

    // Use a permissive DACL so other processes in the session can open and
    // update the counter (e.g. DWM, sandboxed apps, different integrity levels).
    HANDLE mapping =
        win32::CreateFileMappingWBestEffortLowIntegrity(
            INVALID_HANDLE_VALUE, PAGE_READWRITE, 0, sizeof(uint64_t), name);
    if (mapping) {
      void* view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(uint64_t));
      if (view) {
        g_mapping = mapping;
        g_view = view;
      } else {
        CloseHandle(mapping);
      }
    }
  }

  if (g_view) {
    auto* counter = reinterpret_cast<volatile LONG64*>(g_view);
    LONG64 token = InterlockedIncrement64(counter);
    if ((static_cast<uint64_t>(token) & 0x7FFFFFFFULL) == 0) {
      token = InterlockedIncrement64(counter);
    }
    aerogpu_handle_t handle = static_cast<aerogpu_handle_t>(static_cast<uint64_t>(token) & 0xFFFFFFFFu);
    if (handle == 0) {
      token = InterlockedIncrement64(counter);
      handle = static_cast<aerogpu_handle_t>(static_cast<uint64_t>(token) & 0xFFFFFFFFu);
    }
    return handle;
  }

  // If we fail to set up the shared counter mapping, fall back to a random
  // high-bit handle range so collisions with the shared counter (which starts
  // at 1) are vanishingly unlikely.
  static std::once_flag warn_once;
  std::call_once(warn_once, [] {
    logf("aerogpu-d3d9: global handle allocator: shared mapping unavailable; using RNG fallback\n");
  });

  for (;;) {
    const uint64_t token = adapter->share_token_allocator.allocate_share_token();
    const uint32_t low31 = static_cast<uint32_t>(token & 0x7FFFFFFFu);
    if (low31 != 0) {
      return static_cast<aerogpu_handle_t>(0x80000000u | low31);
    }
  }
#else
  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == 0) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
#endif
}
} // namespace aerogpu

// -----------------------------------------------------------------------------
// Public entrypoints
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL OpenAdapter(
    D3DDDIARG_OPENADAPTER* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapter,
                               iface_version,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0,
                               0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  // Some runtimes may call OpenAdapter/OpenAdapter2 without providing an HDC or
  // explicit LUID. Resolve a stable LUID from the primary display so the adapter
  // cache and KMD query helpers can be shared with OpenAdapterFromHdc/Luid.
  HDC hdc = GetDC(nullptr);
  if (hdc) {
    if (!aerogpu::get_luid_from_hdc(hdc, &luid)) {
      aerogpu::logf("aerogpu-d3d9: OpenAdapter failed to resolve adapter LUID from primary HDC\n");
    }
  }
#endif
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
#if defined(_WIN32)
    if (hdc) {
      ReleaseDC(nullptr, hdc);
    }
#endif
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapter",
                                                 get_interface_version(pOpenAdapter),
                                                 pOpenAdapter->Version,
                                                 pOpenAdapter->pAdapterCallbacks,
                                                 get_adapter_callbacks2(pOpenAdapter),
                                                 luid,
                                                 &pOpenAdapter->hAdapter,
                                                 adapter_funcs);
#if defined(_WIN32)
  if (SUCCEEDED(hr) && hdc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(hdc, HORZRES);
      const int h = GetDeviceCaps(hdc, VERTRES);
      const int refresh = GetDeviceCaps(hdc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }

    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(hdc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint32_t max_slot_id = 0;
      if (adapter && adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
        adapter->max_allocation_list_slot_id = max_slot_id;
        if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
          aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                        static_cast<unsigned>(max_slot_id));
        }
      }

      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
  if (hdc) {
    ReleaseDC(nullptr, hdc);
  }
#endif
  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapter2(
    D3DDDIARG_OPENADAPTER2* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapter2,
                               iface_version,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0,
                               0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  HDC hdc = GetDC(nullptr);
  if (hdc) {
    if (!aerogpu::get_luid_from_hdc(hdc, &luid)) {
      aerogpu::logf("aerogpu-d3d9: OpenAdapter2 failed to resolve adapter LUID from primary HDC\n");
    }
  }
#endif
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
#if defined(_WIN32)
    if (hdc) {
      ReleaseDC(nullptr, hdc);
    }
#endif
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapter2",
                                                 get_interface_version(pOpenAdapter),
                                                 pOpenAdapter->Version,
                                                 pOpenAdapter->pAdapterCallbacks,
                                                 get_adapter_callbacks2(pOpenAdapter),
                                                 luid,
                                                 &pOpenAdapter->hAdapter,
                                                 adapter_funcs);
#if defined(_WIN32)
  if (SUCCEEDED(hr) && hdc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(hdc, HORZRES);
      const int h = GetDeviceCaps(hdc, VERTRES);
      const int refresh = GetDeviceCaps(hdc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }

    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(hdc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint32_t max_slot_id = 0;
      if (adapter && adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
        adapter->max_allocation_list_slot_id = max_slot_id;
        if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
          aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                        static_cast<unsigned>(max_slot_id));
        }
      }

      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
  if (hdc) {
    ReleaseDC(nullptr, hdc);
  }
#endif
  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapterFromHdc(
    D3DDDIARG_OPENADAPTERFROMHDC* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapterFromHdc,
                               iface_version,
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->hDc) : 0,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  LUID luid = aerogpu::default_luid();
#if defined(_WIN32)
  if (pOpenAdapter->hDc && !aerogpu::get_luid_from_hdc(pOpenAdapter->hDc, &luid)) {
    aerogpu::logf("aerogpu-d3d9: OpenAdapterFromHdc failed to resolve adapter LUID from HDC\n");
  }
#endif
  pOpenAdapter->AdapterLuid = luid;

  aerogpu::logf("aerogpu-d3d9: OpenAdapterFromHdc hdc=%p LUID=%08x:%08x\n",
                pOpenAdapter->hDc,
                static_cast<unsigned>(luid.HighPart),
                static_cast<unsigned>(luid.LowPart));
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapterFromHdc",
                                                get_interface_version(pOpenAdapter),
                                                pOpenAdapter->Version,
                                                pOpenAdapter->pAdapterCallbacks,
                                                get_adapter_callbacks2(pOpenAdapter),
                                                luid,
                                                &pOpenAdapter->hAdapter,
                                                adapter_funcs);

#if defined(_WIN32)
  if (SUCCEEDED(hr) && pOpenAdapter->hDc) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    if (adapter) {
      const int w = GetDeviceCaps(pOpenAdapter->hDc, HORZRES);
      const int h = GetDeviceCaps(pOpenAdapter->hDc, VERTRES);
      const int refresh = GetDeviceCaps(pOpenAdapter->hDc, VREFRESH);
      if (w > 0) {
        adapter->primary_width = static_cast<uint32_t>(w);
      }
      if (h > 0) {
        adapter->primary_height = static_cast<uint32_t>(h);
      }
      if (refresh > 0) {
        adapter->primary_refresh_hz = static_cast<uint32_t>(refresh);
      }
    }
    const bool kmd_ok = adapter && adapter->kmd_query.InitFromHdc(pOpenAdapter->hDc);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint32_t max_slot_id = 0;
      if (adapter && adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
        adapter->max_allocation_list_slot_id = max_slot_id;
        if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
          aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                        static_cast<unsigned>(max_slot_id));
        }
      }

      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
#endif

  return trace.ret(hr);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapterFromLuid(
    D3DDDIARG_OPENADAPTERFROMLUID* pOpenAdapter) {
  const uint64_t iface_version =
      pOpenAdapter ? aerogpu::d3d9_trace_pack_u32_u32(get_interface_version(pOpenAdapter), pOpenAdapter->Version) : 0;
  const uint64_t luid_packed = pOpenAdapter
                                  ? aerogpu::d3d9_trace_pack_u32_u32(pOpenAdapter->AdapterLuid.LowPart,
                                                                     static_cast<uint32_t>(pOpenAdapter->AdapterLuid.HighPart))
                                  : 0;
  aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::OpenAdapterFromLuid,
                               iface_version,
                               luid_packed,
                               aerogpu::d3d9_trace_arg_ptr(pOpenAdapter),
                               pOpenAdapter ? aerogpu::d3d9_trace_arg_ptr(pOpenAdapter->pAdapterFuncs) : 0);
  if (!pOpenAdapter) {
    return trace.ret(E_INVALIDARG);
  }

  const LUID luid = pOpenAdapter->AdapterLuid;
  auto* adapter_funcs = reinterpret_cast<D3D9DDI_ADAPTERFUNCS*>(pOpenAdapter->pAdapterFuncs);
  if (!adapter_funcs) {
    return trace.ret(E_INVALIDARG);
  }

  const HRESULT hr = aerogpu::OpenAdapterCommon("OpenAdapterFromLuid",
                                                get_interface_version(pOpenAdapter),
                                                pOpenAdapter->Version,
                                                pOpenAdapter->pAdapterCallbacks,
                                                get_adapter_callbacks2(pOpenAdapter),
                                                luid,
                                                &pOpenAdapter->hAdapter,
                                                adapter_funcs);

#if defined(_WIN32)
  if (SUCCEEDED(hr)) {
    auto* adapter = aerogpu::as_adapter(pOpenAdapter->hAdapter);
    const bool kmd_ok = adapter && adapter->kmd_query.InitFromLuid(luid);
    if (adapter) {
      adapter->kmd_query_available.store(kmd_ok, std::memory_order_release);
      uint32_t vid_pn_source_id = 0;
      if (kmd_ok && adapter->kmd_query.GetVidPnSourceId(&vid_pn_source_id)) {
        adapter->vid_pn_source_id = vid_pn_source_id;
        adapter->vid_pn_source_id_valid = true;
      } else {
        adapter->vid_pn_source_id = 0;
        adapter->vid_pn_source_id_valid = false;
      }
      set_vid_pn_source_id(pOpenAdapter, adapter->vid_pn_source_id_valid ? adapter->vid_pn_source_id : 0);
    }
    if (kmd_ok) {
      uint32_t max_slot_id = 0;
      if (adapter && adapter->kmd_query.QueryMaxAllocationListSlotId(&max_slot_id)) {
        adapter->max_allocation_list_slot_id = max_slot_id;
        if (!adapter->max_allocation_list_slot_id_logged.exchange(true)) {
          aerogpu::logf("aerogpu-d3d9: KMD MaxAllocationListSlotId=%u\n",
                        static_cast<unsigned>(max_slot_id));
        }
      }

      uint64_t submitted = 0;
      uint64_t completed = 0;
      if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
        aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                      static_cast<unsigned long long>(submitted),
                      static_cast<unsigned long long>(completed));
      }

      aerogpu_umd_private_v1 priv;
      std::memset(&priv, 0, sizeof(priv));
      if (adapter->kmd_query.QueryUmdPrivate(&priv)) {
        adapter->umd_private = priv;
        adapter->umd_private_valid = true;

        char magicStr[5] = {0, 0, 0, 0, 0};
        magicStr[0] = (char)((priv.device_mmio_magic >> 0) & 0xFF);
        magicStr[1] = (char)((priv.device_mmio_magic >> 8) & 0xFF);
        magicStr[2] = (char)((priv.device_mmio_magic >> 16) & 0xFF);
        magicStr[3] = (char)((priv.device_mmio_magic >> 24) & 0xFF);

        aerogpu::logf("aerogpu-d3d9: UMDRIVERPRIVATE magic=0x%08x (%s) abi=0x%08x features=0x%llx flags=0x%08x\n",
                      priv.device_mmio_magic,
                      magicStr,
                      priv.device_abi_version_u32,
                      static_cast<unsigned long long>(priv.device_features),
                      priv.flags);
      }
    }
  }
#endif

  return trace.ret(hr);
}
