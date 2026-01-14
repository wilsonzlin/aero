// AeroGPU Windows 7 D3D11 UMD (WDK build).
//
// This translation unit is compiled only when the official Win7 D3D11 DDI headers
// (`d3d10umddi.h` / `d3d11umddi.h`) are available.
//
// Goal: provide a crash-free FL10_0-capable D3D11DDI surface that translates the
// Win7 runtime's DDIs into the shared AeroGPU command stream.

#include "../include/aerogpu_d3d10_11_umd.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include "aerogpu_d3d10_11_wdk_abi_asserts.h"

#include <d3d11.h>
#include <d3dkmthk.h>

#include <array>
#include <algorithm>
#include <atomic>
#include <cassert>
#include <cstdint>
#include <cstddef>
#include <limits>
#include <cstdio>
#include <cmath>
#include <cstring>
#include <mutex>
#include <new>
#include <tuple>
#include <type_traits>
#include <utility>
#include <vector>

#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_blend_state_validate.h"
#include "aerogpu_legacy_d3d9_format_fixup.h"
#include "aerogpu_d3d10_11_log.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_win7_abi.h"

namespace {

using namespace aerogpu::d3d10_11;
using aerogpu::shared_surface::D3d9FormatToDxgi;
using aerogpu::shared_surface::FixupLegacyPrivForOpenResource;

static void LogTexture2DPitchMismatchRateLimited(const char* label,
                                                 const Resource* res,
                                                 uint32_t subresource,
                                                 uint32_t expected_pitch,
                                                 uint32_t runtime_pitch) {
  if (!label || !res) {
    return;
  }
  if (runtime_pitch == 0 || runtime_pitch == expected_pitch) {
    return;
  }
  static std::atomic<uint32_t> g_pitch_mismatch_logs{0};
  const uint32_t n = g_pitch_mismatch_logs.fetch_add(1, std::memory_order_relaxed);
  if (n < 32) {
    AEROGPU_D3D10_11_LOG(
        "%s: Texture2D pitch mismatch: handle=%u alloc_id=%u sub=%u (mip=%u layer=%u) expected_pitch=%u runtime_pitch=%u",
        label,
        static_cast<unsigned>(res->handle),
        static_cast<unsigned>(res->backing_alloc_id),
        static_cast<unsigned>(subresource),
        static_cast<unsigned>((subresource < res->tex2d_subresources.size()) ? res->tex2d_subresources[subresource].mip_level : 0u),
        static_cast<unsigned>((subresource < res->tex2d_subresources.size()) ? res->tex2d_subresources[subresource].array_layer : 0u),
        static_cast<unsigned>(expected_pitch),
        static_cast<unsigned>(runtime_pitch));
  } else if (n == 32) {
    AEROGPU_D3D10_11_LOG("Texture2D pitch mismatch: log limit reached; suppressing further messages");
  }
}

static bool IsDeviceLive(D3D11DDI_HDEVICE hDevice) {
  return HasLiveCookie(hDevice.pDrvPrivate, kDeviceDestroyLiveCookie);
}

// Compile-time sanity: keep local checks to "member exists" only.
//
// ABI-critical size/offset conformance checks (struct layout + x86 export
// decoration) are handled separately by `aerogpu_d3d10_11_wdk_abi_asserts.h`
// when building against real WDK headers.
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICEFUNCS::pfnCreateResource)>,
              "Expected D3D11DDI_DEVICEFUNCS::pfnCreateResource");
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw)>,
              "Expected D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw");

using aerogpu::d3d10_11::AlignUpU64;
using aerogpu::d3d10_11::AlignUpU32;

struct AeroGpuD3dkmtProcs {
  decltype(&D3DKMTOpenAdapterFromHdc) pfn_open_adapter_from_hdc = nullptr;
  decltype(&D3DKMTCloseAdapter) pfn_close_adapter = nullptr;
  decltype(&D3DKMTQueryAdapterInfo) pfn_query_adapter_info = nullptr;
};

static const AeroGpuD3dkmtProcs& GetAeroGpuD3dkmtProcs() {
  static AeroGpuD3dkmtProcs procs = [] {
    AeroGpuD3dkmtProcs p{};
    HMODULE gdi32 = GetModuleHandleW(L"gdi32.dll");
    if (!gdi32) {
      gdi32 = LoadLibraryW(L"gdi32.dll");
    }
    if (!gdi32) {
      return p;
    }

    p.pfn_open_adapter_from_hdc =
        reinterpret_cast<decltype(&D3DKMTOpenAdapterFromHdc)>(GetProcAddress(gdi32, "D3DKMTOpenAdapterFromHdc"));
    p.pfn_close_adapter = reinterpret_cast<decltype(&D3DKMTCloseAdapter)>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
    p.pfn_query_adapter_info =
        reinterpret_cast<decltype(&D3DKMTQueryAdapterInfo)>(GetProcAddress(gdi32, "D3DKMTQueryAdapterInfo"));
    return p;
  }();
  return procs;
}

static void DestroyKmtAdapterHandle(Adapter* adapter) {
  if (!adapter || adapter->kmt_adapter == 0) {
    return;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (procs.pfn_close_adapter) {
    D3DKMT_CLOSEADAPTER close{};
    close.hAdapter = static_cast<D3DKMT_HANDLE>(adapter->kmt_adapter);
    (void)procs.pfn_close_adapter(&close);
  }
  adapter->kmt_adapter = 0;
}

static void InitKmtAdapterHandle(Adapter* adapter) {
  if (!adapter || adapter->kmt_adapter != 0) {
    return;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_open_adapter_from_hdc) {
    return;
  }

  wchar_t displayName[CCHDEVICENAME] = {};
  if (!GetPrimaryDisplayName(displayName)) {
    return;
  }

  HDC hdc = CreateDCW(L"DISPLAY", displayName, nullptr, nullptr);
  if (!hdc) {
    return;
  }

  D3DKMT_OPENADAPTERFROMHDC open{};
  open.hDc = hdc;
  open.hAdapter = 0;
  std::memset(&open.AdapterLuid, 0, sizeof(open.AdapterLuid));
  open.VidPnSourceId = 0;

  const NTSTATUS st = procs.pfn_open_adapter_from_hdc(&open);
  DeleteDC(hdc);
  if (!NtSuccess(st) || !open.hAdapter) {
    return;
  }

  adapter->kmt_adapter = static_cast<uint32_t>(open.hAdapter);
}

static bool QueryUmdPrivateFromKmtAdapter(D3DKMT_HANDLE hAdapter, aerogpu_umd_private_v1* out) {
  if (!out || hAdapter == 0) {
    return false;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_query_adapter_info) {
    return false;
  }

  aerogpu_umd_private_v1 blob;
  std::memset(&blob, 0, sizeof(blob));

  D3DKMT_QUERYADAPTERINFO q{};
  q.hAdapter = hAdapter;
  q.pPrivateDriverData = &blob;
  q.PrivateDriverDataSize = sizeof(blob);

  // Avoid relying on the WDK's numeric KMTQAITYPE_UMDRIVERPRIVATE constant by probing a
  // small range of values and looking for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  for (UINT type = 0; type < 256; ++type) {
    std::memset(&blob, 0, sizeof(blob));
    q.Type = static_cast<KMTQUERYADAPTERINFOTYPE>(type);

    const NTSTATUS qst = procs.pfn_query_adapter_info(&q);
    if (!NtSuccess(qst)) {
      continue;
    }

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    *out = blob;
    return true;
  }

  return false;
}

static void InitUmdPrivate(Adapter* adapter) {
  if (!adapter || adapter->umd_private_valid) {
    return;
  }

  InitKmtAdapterHandle(adapter);

  aerogpu_umd_private_v1 blob{};
  if (!QueryUmdPrivateFromKmtAdapter(static_cast<D3DKMT_HANDLE>(adapter->kmt_adapter), &blob)) {
    return;
  }

  adapter->umd_private = blob;
  adapter->umd_private_valid = true;
}

struct AeroGpuDeviceContext {
  Device* dev = nullptr;
};

template <typename T, typename = void>
struct has_member_pAdapterCallbacks : std::false_type {};
template <typename T>
struct has_member_pAdapterCallbacks<T, std::void_t<decltype(std::declval<T>().pAdapterCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDeviceCallbacks : std::false_type {};
template <typename T>
struct has_member_pDeviceCallbacks<T, std::void_t<decltype(std::declval<T>().pDeviceCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pCallbacks : std::false_type {};
template <typename T>
struct has_member_pCallbacks<T, std::void_t<decltype(std::declval<T>().pCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pUMCallbacks : std::false_type {};
template <typename T>
struct has_member_pUMCallbacks<T, std::void_t<decltype(std::declval<T>().pUMCallbacks)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pShaderCode : std::false_type {};
template <typename T>
struct has_member_pShaderCode<T, std::void_t<decltype(std::declval<T>().pShaderCode)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ShaderCodeSize : std::false_type {};
template <typename T>
struct has_member_ShaderCodeSize<T, std::void_t<decltype(std::declval<T>().ShaderCodeSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hRTDevice : std::false_type {};
template <typename T>
struct has_member_hRTDevice<T, std::void_t<decltype(std::declval<T>().hRTDevice)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pDeviceContextFuncs : std::false_type {};
template <typename T>
struct has_member_pDeviceContextFuncs<T, std::void_t<decltype(std::declval<T>().pDeviceContextFuncs)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pImmediateContextFuncs : std::false_type {};
template <typename T>
struct has_member_pImmediateContextFuncs<T, std::void_t<decltype(std::declval<T>().pImmediateContextFuncs)>> : std::true_type {};

template <typename CreateDeviceT, typename = void>
struct has_member_hImmediateContext : std::false_type {};
template <typename CreateDeviceT>
struct has_member_hImmediateContext<CreateDeviceT, std::void_t<decltype(std::declval<CreateDeviceT>().hImmediateContext)>>
    : std::true_type {};

template <typename CreateDeviceT, typename = void>
struct has_member_hDeviceContext : std::false_type {};
template <typename CreateDeviceT>
struct has_member_hDeviceContext<CreateDeviceT, std::void_t<decltype(std::declval<CreateDeviceT>().hDeviceContext)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnCalcPrivateDeviceContextSize : std::false_type {};
template <typename T>
struct has_member_pfnCalcPrivateDeviceContextSize<T, std::void_t<decltype(std::declval<T>().pfnCalcPrivateDeviceContextSize)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnStagingResourceMap : std::false_type {};
template <typename T>
struct has_member_pfnStagingResourceMap<T, std::void_t<decltype(std::declval<T>().pfnStagingResourceMap)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnStagingResourceUnmap : std::false_type {};
template <typename T>
struct has_member_pfnStagingResourceUnmap<T, std::void_t<decltype(std::declval<T>().pfnStagingResourceUnmap)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnDynamicIABufferMapDiscard : std::false_type {};
template <typename T>
struct has_member_pfnDynamicIABufferMapDiscard<T, std::void_t<decltype(std::declval<T>().pfnDynamicIABufferMapDiscard)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnDynamicIABufferMapNoOverwrite : std::false_type {};
template <typename T>
struct has_member_pfnDynamicIABufferMapNoOverwrite<T, std::void_t<decltype(std::declval<T>().pfnDynamicIABufferMapNoOverwrite)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnDynamicIABufferUnmap : std::false_type {};
template <typename T>
struct has_member_pfnDynamicIABufferUnmap<T, std::void_t<decltype(std::declval<T>().pfnDynamicIABufferUnmap)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnDynamicConstantBufferMapDiscard : std::false_type {};
template <typename T>
struct has_member_pfnDynamicConstantBufferMapDiscard<
    T,
    std::void_t<decltype(std::declval<T>().pfnDynamicConstantBufferMapDiscard)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pfnDynamicConstantBufferUnmap : std::false_type {};
template <typename T>
struct has_member_pfnDynamicConstantBufferUnmap<T, std::void_t<decltype(std::declval<T>().pfnDynamicConstantBufferUnmap)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_member_pInitialDataUP : std::false_type {};
template <typename T>
struct has_member_pInitialDataUP<T, std::void_t<decltype(std::declval<T>().pInitialDataUP)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pInitialData : std::false_type {};
template <typename T>
struct has_member_pInitialData<T, std::void_t<decltype(std::declval<T>().pInitialData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pSysMem : std::false_type {};
template <typename T>
struct has_member_pSysMem<T, std::void_t<decltype(std::declval<T>().pSysMem)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SysMemPitch : std::false_type {};
template <typename T>
struct has_member_SysMemPitch<T, std::void_t<decltype(std::declval<T>().SysMemPitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pSysMemUP : std::false_type {};
template <typename T>
struct has_member_pSysMemUP<T, std::void_t<decltype(std::declval<T>().pSysMemUP)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_RowPitch : std::false_type {};
template <typename T>
struct has_member_RowPitch<T, std::void_t<decltype(std::declval<T>().RowPitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DepthPitch : std::false_type {};
template <typename T>
struct has_member_DepthPitch<T, std::void_t<decltype(std::declval<T>().DepthPitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrcPitch : std::false_type {};
template <typename T>
struct has_member_SrcPitch<T, std::void_t<decltype(std::declval<T>().SrcPitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrcSlicePitch : std::false_type {};
template <typename T>
struct has_member_SrcSlicePitch<T, std::void_t<decltype(std::declval<T>().SrcSlicePitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SysMemSlicePitch : std::false_type {};
template <typename T>
struct has_member_SysMemSlicePitch<T, std::void_t<decltype(std::declval<T>().SysMemSlicePitch)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hAllocation : std::false_type {};
template <typename T>
struct has_member_hAllocation<T, std::void_t<decltype(std::declval<T>().hAllocation)>> : std::true_type {};

static const void* GetAdapterCallbacks(const D3D10DDIARG_OPENADAPTER* open) {
  if (!open) {
    return nullptr;
  }
  if constexpr (has_member_pAdapterCallbacks<D3D10DDIARG_OPENADAPTER>::value) {
    return open->pAdapterCallbacks;
  }
  return nullptr;
}

static const void* GetDeviceCallbacks(const D3D11DDIARG_CREATEDEVICE* cd) {
  if (!cd) {
    return nullptr;
  }
  if constexpr (has_member_pDeviceCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    if (cd->pDeviceCallbacks) {
      return cd->pDeviceCallbacks;
    }
  }
  if constexpr (has_member_pCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    if (cd->pCallbacks) {
      return cd->pCallbacks;
    }
  }
  return nullptr;
}

static const void* GetDdiCallbacks(const D3D11DDIARG_CREATEDEVICE* cd) {
  if (!cd) {
    return nullptr;
  }
  if constexpr (has_member_pUMCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    if (cd->pUMCallbacks) {
      return cd->pUMCallbacks;
    }
  }
  if constexpr (has_member_pDeviceCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    if (cd->pDeviceCallbacks) {
      return cd->pDeviceCallbacks;
    }
  }
  if constexpr (has_member_pCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    if (cd->pCallbacks) {
      return cd->pCallbacks;
    }
  }
  return nullptr;
}

static void* GetRtDevicePrivate(const D3D11DDIARG_CREATEDEVICE* cd) {
  if (!cd) {
    return nullptr;
  }
  if constexpr (has_member_hRTDevice<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->hRTDevice.pDrvPrivate;
  }
  return nullptr;
}

static D3D11DDI_DEVICECONTEXTFUNCS* GetContextFuncTable(D3D11DDIARG_CREATEDEVICE* cd) {
  if (!cd) {
    return nullptr;
  }
  if constexpr (has_member_pDeviceContextFuncs<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->pDeviceContextFuncs;
  }
  if constexpr (has_member_pImmediateContextFuncs<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->pImmediateContextFuncs;
  }
  return nullptr;
}

static D3D11DDI_HDEVICECONTEXT GetImmediateContextHandle(D3D11DDIARG_CREATEDEVICE* cd) {
  if (!cd) {
    return {};
  }
  if constexpr (has_member_hImmediateContext<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->hImmediateContext;
  }
  if constexpr (has_member_hDeviceContext<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->hDeviceContext;
  }
  return {};
}

static void SetImmediateContextHandle(D3D11DDIARG_CREATEDEVICE* cd, void* drv_private) {
  if (!cd) {
    return;
  }
  if constexpr (has_member_hImmediateContext<D3D11DDIARG_CREATEDEVICE>::value) {
    cd->hImmediateContext.pDrvPrivate = drv_private;
  } else if constexpr (has_member_hDeviceContext<D3D11DDIARG_CREATEDEVICE>::value) {
    cd->hDeviceContext.pDrvPrivate = drv_private;
  }
}

static D3D11DDI_HRTDEVICE MakeRtDeviceHandle(Device* dev) {
  D3D11DDI_HRTDEVICE h{};
  h.pDrvPrivate = dev ? dev->runtime_device : nullptr;
  return h;
}

static D3D10DDI_HRTDEVICE MakeRtDeviceHandle10(Device* dev) {
  D3D10DDI_HRTDEVICE h{};
  h.pDrvPrivate = dev ? dev->runtime_device : nullptr;
  return h;
}

static D3D11DDI_HDEVICE MakeDeviceHandle(Device* dev) {
  D3D11DDI_HDEVICE h{};
  h.pDrvPrivate = dev;
  return h;
}

template <typename Fn, typename HandleA, typename HandleB, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, HandleA handle_a, HandleB handle_b, Args&&... args);

static void SetError(Device* dev, HRESULT hr) {
  if (!HasLiveCookie(dev, kDeviceDestroyLiveCookie)) {
    return;
  }
  auto* callbacks = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  if (callbacks && callbacks->pfnSetErrorCb) {
    // Win7-era WDK headers disagree on whether pfnSetErrorCb takes HRTDEVICE or
    // HDEVICE. Prefer the HDEVICE form when that's what the signature expects.
    if constexpr (std::is_invocable_v<decltype(callbacks->pfnSetErrorCb), D3D11DDI_HDEVICE, HRESULT>) {
      callbacks->pfnSetErrorCb(MakeDeviceHandle(dev), hr);
      return;
    }
    if (dev->runtime_device) {
      CallCbMaybeHandle(callbacks->pfnSetErrorCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), hr);
    }
    return;
  }

  // Some header revisions expose `pUMCallbacks` as a bare `D3DDDI_DEVICECALLBACKS`
  // table. As a fallback, attempt to call SetErrorCb through that path.
  auto* wddm_cb = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  if (wddm_cb && wddm_cb->pfnSetErrorCb && dev->runtime_device) {
    CallCbMaybeHandle(wddm_cb->pfnSetErrorCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), hr);
  }
}

template <typename Fn, typename HandleA, typename HandleB, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, HandleA handle_a, HandleB handle_b, Args&&... args) {
  if constexpr (std::is_invocable_v<Fn, HandleA, Args...>) {
    return fn(handle_a, std::forward<Args>(args)...);
  } else if constexpr (std::is_invocable_v<Fn, HandleB, Args...>) {
    return fn(handle_b, std::forward<Args>(args)...);
  } else {
    return fn(std::forward<Args>(args)...);
  }
}

static void DestroyWddmContext(Device* dev) {
  if (!dev) {
    return;
  }
  dev->wddm_submit.Shutdown();
  dev->kmt_device = 0;
  dev->kmt_context = 0;
  dev->kmt_fence_syncobj = 0;
  dev->wddm_dma_private_data = nullptr;
  dev->wddm_dma_private_data_bytes = 0;
  dev->monitored_fence_value = nullptr;
}

static HRESULT InitWddmContext(Device* dev, void* hAdapter) {
  if (!dev) {
    return E_INVALIDARG;
  }

  auto* cb = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  if (!cb || !dev->runtime_device) {
    return E_FAIL;
  }

  const D3DKMT_HANDLE kmt_adapter_for_debug =
      (dev->adapter != nullptr) ? static_cast<D3DKMT_HANDLE>(dev->adapter->kmt_adapter) : 0;
  const HRESULT hr = dev->wddm_submit.Init(cb, hAdapter, dev->runtime_device, kmt_adapter_for_debug);
  if (FAILED(hr)) {
    DestroyWddmContext(dev);
    return hr;
  }

  dev->kmt_device = static_cast<uint32_t>(dev->wddm_submit.hDevice());
  dev->kmt_context = static_cast<uint32_t>(dev->wddm_submit.hContext());
  dev->kmt_fence_syncobj = static_cast<uint32_t>(dev->wddm_submit.hSyncObject());
  dev->wddm_dma_private_data = nullptr;
  dev->wddm_dma_private_data_bytes = 0;
  dev->monitored_fence_value = nullptr;
  if (!dev->kmt_device || !dev->kmt_context || !dev->kmt_fence_syncobj) {
    DestroyWddmContext(dev);
    return E_FAIL;
  }
  return S_OK;
}

static HRESULT WaitForFence(Device* dev, uint64_t fence_value, UINT64 timeout) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (fence_value == 0) {
    return S_OK;
  }

  atomic_max_u64(&dev->last_completed_fence, dev->wddm_submit.QueryCompletedFence());
  if (dev->last_completed_fence.load(std::memory_order_relaxed) >= fence_value) {
    return S_OK;
  }

  uint32_t timeout_ms = 0;
  if (timeout == 0ull) {
    timeout_ms = 0u;
  } else if (timeout == kAeroGpuTimeoutU64Infinite) {
    timeout_ms = kAeroGpuTimeoutMsInfinite;
  } else if (timeout >= static_cast<UINT64>(kAeroGpuTimeoutMsInfinite)) {
    timeout_ms = kAeroGpuTimeoutMsInfinite;
  } else {
    timeout_ms = static_cast<uint32_t>(timeout);
  }

  const HRESULT hr = dev->wddm_submit.WaitForFenceWithTimeout(fence_value, timeout_ms);
  if (SUCCEEDED(hr)) {
    atomic_max_u64(&dev->last_completed_fence, fence_value);
  }
  atomic_max_u64(&dev->last_completed_fence, dev->wddm_submit.QueryCompletedFence());
  return hr;
}

template <typename T, typename = void>
struct has_member_pOpenAllocationInfo : std::false_type {};
template <typename T>
struct has_member_pOpenAllocationInfo<T, std::void_t<decltype(std::declval<T>().pOpenAllocationInfo)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocationInfo : std::false_type {};
template <typename T>
struct has_member_pAllocationInfo<T, std::void_t<decltype(std::declval<T>().pAllocationInfo)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hKMResource : std::false_type {};
template <typename T>
struct has_member_hKMResource<T, std::void_t<decltype(std::declval<T>().hKMResource)>> : std::true_type {};

template <typename OpenT, bool HasOpen, bool HasAlloc>
struct OpenResourceAllocInfoAccess;

template <typename OpenT, bool HasAlloc>
struct OpenResourceAllocInfoAccess<OpenT, true, HasAlloc> {
  using AllocInfoT = std::remove_pointer_t<decltype(std::declval<OpenT>().pOpenAllocationInfo)>;
  static AllocInfoT* get(const OpenT* p) {
    return p ? p->pOpenAllocationInfo : nullptr;
  }
};

template <typename OpenT>
struct OpenResourceAllocInfoAccess<OpenT, false, true> {
  using AllocInfoT = std::remove_pointer_t<decltype(std::declval<OpenT>().pAllocationInfo)>;
  static AllocInfoT* get(const OpenT* p) {
    return p ? p->pAllocationInfo : nullptr;
  }
};

static void TrackWddmAllocForSubmitLocked(Device* dev, const Resource* res, bool write = false) {
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(dev, res, write, [&](HRESULT hr) { SetError(dev, hr); });
}

struct WddmAllocListCheckpoint {
  Device* dev = nullptr;
  size_t size = 0;
  bool oom = false;

  explicit WddmAllocListCheckpoint(Device* d) : dev(d) {
    if (!dev) {
      return;
    }
    size = dev->wddm_submit_allocation_handles.size();
    oom = dev->wddm_submit_allocation_list_oom;
  }

  void rollback() const {
    if (!dev) {
      return;
    }
    if (dev->wddm_submit_allocation_handles.size() > size) {
      dev->wddm_submit_allocation_handles.resize(size);
    }
    dev->wddm_submit_allocation_list_oom = oom;
  }
};

// Best-effort allocation-list tracking used by optional "fast path" packets.
//
// Unlike `TrackWddmAllocForSubmitLocked`, this does not set the global
// `wddm_submit_allocation_list_oom` poison flag or call SetError on OOM: callers
// must skip emitting any packet that would reference `res` if this returns false.
static bool TryTrackWddmAllocForSubmitLocked(Device* dev, const Resource* res, bool write = false) {
  if (!dev || !res) {
    return false;
  }
  if (dev->wddm_submit_allocation_list_oom) {
    return false;
  }
  if (res->backing_alloc_id == 0 || res->wddm_allocation_handle == 0) {
    return true;
  }

  const uint32_t handle = res->wddm_allocation_handle;
  for (auto& entry : dev->wddm_submit_allocation_handles) {
    if (entry.allocation_handle == handle) {
      if (write) {
        entry.write = 1;
      }
      return true;
    }
  }

  WddmSubmitAllocation entry{};
  entry.allocation_handle = handle;
  entry.write = write ? 1 : 0;
  try {
    dev->wddm_submit_allocation_handles.push_back(entry);
  } catch (...) {
    return false;
  }
  return true;
}

static void TrackBoundTargetsForSubmitLocked(Device* dev) {
  if (!dev) {
    return;
  }
  // Render targets / depth-stencil are written by Draw/Clear.
  const uint32_t count =
      std::min<uint32_t>(dev->current_rtv_count, static_cast<uint32_t>(dev->current_rtv_resources.size()));
  for (uint32_t i = 0; i < count; ++i) {
    TrackWddmAllocForSubmitLocked(dev, dev->current_rtv_resources[i], /*write=*/true);
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_dsv_resource, /*write=*/true);
}

static void TrackDrawStateLocked(Device* dev) {
  if (!dev) {
    return;
  }

  TrackBoundTargetsForSubmitLocked(dev);
  for (Resource* vb : dev->current_vb_resources) {
    TrackWddmAllocForSubmitLocked(dev, vb, /*write=*/false);
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_ib, /*write=*/false);

  for (Resource* res : dev->current_vs_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (Resource* res : dev->current_ps_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (Resource* res : dev->current_gs_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }

  for (Resource* res : dev->current_vs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (Resource* res : dev->current_ps_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (Resource* res : dev->current_gs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }

  for (Resource* res : dev->current_vs_srv_buffers) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_ps_srv_buffers) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_gs_srv_buffers) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
}

static void TrackComputeStateLocked(Device* dev) {
  if (!dev) {
    return;
  }

  for (Resource* res : dev->current_cs_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_cs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_cs_srv_buffers) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_cs_uavs) {
    // UAVs are writable in D3D11; conservatively mark them as written so the WDDM
    // allocation list can reflect write hazards correctly.
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/true);
  }
}

static bool TrackDrawStateForSubmitOrRollbackLocked(Device* dev) {
  if (!dev) {
    return false;
  }
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackDrawStateLocked(dev);
  if (dev->wddm_submit_allocation_list_oom) {
    // TrackWddmAllocForSubmitLocked already reported OOM via SetErrorCb. Roll
    // back the allocation list poison flag so unrelated commands already
    // recorded in `dev->cmd` can still be submitted safely.
    alloc_checkpoint.rollback();
    return false;
  }
  return true;
}

static bool TrackComputeStateForSubmitOrRollbackLocked(Device* dev) {
  if (!dev) {
    return false;
  }
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackComputeStateLocked(dev);
  if (dev->wddm_submit_allocation_list_oom) {
    alloc_checkpoint.rollback();
    return false;
  }
  return true;
}

static Device* DeviceFromContext(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuDeviceContext>(hCtx);
  Device* dev = ctx ? ctx->dev : nullptr;
  if (!dev) {
    return nullptr;
  }
  // Avoid touching `Device` state (including its mutex) after DestroyDevice has
  // run. DestroyDevice intentionally zeros the cookie before invoking the
  // destructor, so reading the first 4 bytes is a safe liveness check even
  // during teardown races.
  if (!HasLiveCookie(dev, kDeviceDestroyLiveCookie)) {
    return nullptr;
  }
  return dev;
}

// -----------------------------------------------------------------------------
// D3D11 WDK DDI exception barrier
// -----------------------------------------------------------------------------
//
// D3D11 DDIs are invoked via runtime-filled function tables. The runtime expects
// callbacks to be "C ABI safe": no C++ exceptions can escape into the D3D11
// runtime. Even though most hot paths avoid allocations and report OOM via
// SetErrorCb, wrap the exported DDI entrypoints defensively so unexpected C++
// exceptions (e.g. std::bad_alloc, std::system_error) cannot unwind across the
// ABI boundary.
template <typename... Args>
inline void ReportExceptionForArgs(HRESULT hr, Args... args) noexcept {
  if constexpr (sizeof...(Args) == 0) {
    return;
  } else {
    using First = std::tuple_element_t<0, std::tuple<Args...>>;
    if constexpr (std::is_same_v<std::decay_t<First>, D3D11DDI_HDEVICE>) {
      const auto tup = std::forward_as_tuple(args...);
      const auto hDevice = std::get<0>(tup);
      auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
      SetError(dev, hr);
    } else if constexpr (std::is_same_v<std::decay_t<First>, D3D11DDI_HDEVICECONTEXT>) {
      const auto tup = std::forward_as_tuple(args...);
      const auto hCtx = std::get<0>(tup);
      SetError(DeviceFromContext(hCtx), hr);
    }
  }
}

template <auto Impl>
struct aerogpu_d3d11_wdk_ddi_thunk;

template <typename Ret, typename... Args, Ret(AEROGPU_APIENTRY* Impl)(Args...)>
struct aerogpu_d3d11_wdk_ddi_thunk<Impl> {
  static Ret AEROGPU_APIENTRY thunk(Args... args) noexcept {
    try {
      if constexpr (std::is_void_v<Ret>) {
        Impl(args...);
        return;
      } else {
        return Impl(args...);
      }
    } catch (const std::bad_alloc&) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_OUTOFMEMORY;
      } else if constexpr (std::is_void_v<Ret>) {
        ReportExceptionForArgs(E_OUTOFMEMORY, args...);
        return;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        ReportExceptionForArgs(E_OUTOFMEMORY, args...);
        return sizeof(uint64_t);
      } else {
        return Ret{};
      }
    } catch (...) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_FAIL;
      } else if constexpr (std::is_void_v<Ret>) {
        ReportExceptionForArgs(E_FAIL, args...);
        return;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        ReportExceptionForArgs(E_FAIL, args...);
        return sizeof(uint64_t);
      } else {
        return Ret{};
      }
    }
  }
};

#define AEROGPU_D3D11_WDK_DDI(fn) aerogpu_d3d11_wdk_ddi_thunk<&fn>::thunk

static void ReportNotImpl(D3D11DDI_HDEVICE hDevice) {
  // Device-level void DDIs have no HRESULT return channel. Prefer to report
  // unsupported operations through SetErrorCb so the runtime can fail cleanly.
  //
  // Note: Destroy* entrypoints are overridden to use no-op stubs (see
  // `AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS`) so teardown paths do not spam
  // SetErrorCb for benign cleanup.
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
  SetError(dev, E_NOTIMPL);
}

static void ReportNotImpl(D3D11DDI_HDEVICECONTEXT hCtx) {
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

static bool EmitBindShadersCmdLocked(Device* dev,
                                     aerogpu_handle_t vs,
                                     aerogpu_handle_t ps,
                                     aerogpu_handle_t cs,
                                     aerogpu_handle_t gs) {
  if (!dev) {
    return false;
  }

  auto* cmd = dev->cmd.bind_shaders_with_gs(vs, ps, cs, gs);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  return true;
}

static bool EmitBindShadersLocked(Device* dev) {
  if (!dev) {
    return false;
  }
  return EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, dev->current_cs, dev->current_gs);
}

static HRESULT EmitUploadLocked(Device* dev, Resource* res, uint64_t offset_bytes, uint64_t size_bytes) {
  if (!dev || !res || !res->handle || size_bytes == 0) {
    return S_OK;
  }
  if (offset_bytes > static_cast<uint64_t>(SIZE_MAX) || size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }

  uint64_t upload_offset = offset_bytes;
  uint64_t upload_size = size_bytes;
  if (res->kind == ResourceKind::Buffer) {
    const uint64_t end = offset_bytes + size_bytes;
    if (end < offset_bytes) {
      return S_OK;
    }
    const uint64_t aligned_start = offset_bytes & ~3ull;
    const uint64_t aligned_end = AlignUpU64(end, 4);
    upload_offset = aligned_start;
    upload_size = aligned_end - aligned_start;
  }

  if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  const size_t off = static_cast<size_t>(upload_offset);
  const size_t sz = static_cast<size_t>(upload_size);
  if (off > res->storage.size() || sz > res->storage.size() - off) {
    // Preserve old behavior: treat out-of-bounds uploads as a no-op so callers
    // can use this helper in "best-effort" paths without forcing an error.
    return S_OK;
  }

  if (res->backing_alloc_id == 0) {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + off, sz);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return E_OUTOFMEMORY;
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = upload_offset;
    cmd->size_bytes = upload_size;
    return S_OK;
  }

  // Guest-backed resources: append RESOURCE_DIRTY_RANGE before writing into the
  // runtime allocation so OOM while recording the packet cannot desynchronize
  // the guest allocation from the host's copy.
  const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  if (!ddi || !ddi->pfnLockCb || !ddi->pfnUnlockCb || !dev->runtime_device || res->wddm_allocation_handle == 0) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock_args.SubresourceIndex = 0; }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock_args.SubResourceIndex = 0; }
  InitLockForWrite(&lock_args);

  HRESULT hr = CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock_args);
  if (FAILED(hr) || !lock_args.pData) {
    const HRESULT lock_hr = FAILED(hr) ? hr : E_FAIL;
    SetError(dev, lock_hr);
    return lock_hr;
  }

  auto unlock_allocation = [&]() -> HRESULT {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) { unlock_args.SubresourceIndex = 0; }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) { unlock_args.SubResourceIndex = 0; }
    return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock_args);
  };

  // Validate the copy plan while we hold the lock, but do not write until after
  // RESOURCE_DIRTY_RANGE is recorded successfully.
  const bool row_copy_texture2d = (res->kind == ResourceKind::Texture2D &&
                                   upload_offset == 0 &&
                                   upload_size == res->storage.size() &&
                                   res->mip_levels == 1 &&
                                   res->array_size == 1);
  uint32_t lock_pitch = 0;
  if (res->kind == ResourceKind::Texture2D) {
    __if_exists(D3DDDICB_LOCK::Pitch) {
      lock_pitch = lock_args.Pitch;
    }
  }

  uint32_t row_bytes = 0;
  uint32_t rows = 0;
  uint32_t dst_pitch = 0;
  if (row_copy_texture2d) {
    // Single-subresource Texture2D: copy row-by-row so we can use the runtime's
    // returned pitch (when present) for correct row stepping.
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      (void)unlock_allocation();
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }

    // Guest-backed textures are interpreted by the host using the protocol pitch
    // (`CREATE_TEXTURE2D.row_pitch_bytes`). Do not honor a runtime-reported pitch
    // here, otherwise we'd write rows with a stride the host does not expect.
    dst_pitch = res->row_pitch_bytes;
    if (dst_pitch < row_bytes) {
      (void)unlock_allocation();
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }
    const uint64_t needed =
        (rows == 0) ? 0ull
                    : (static_cast<uint64_t>(rows - 1) * static_cast<uint64_t>(res->row_pitch_bytes) +
                       static_cast<uint64_t>(row_bytes));
    if (needed == 0 || needed > res->storage.size()) {
      (void)unlock_allocation();
      SetError(dev, E_FAIL);
      return E_FAIL;
    }
    if (lock_pitch != 0) {
      LogTexture2DPitchMismatchRateLimited("EmitUploadLocked", res, /*subresource=*/0, res->row_pitch_bytes, lock_pitch);
    }
  }

  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  if (dev->wddm_submit_allocation_list_oom) {
    (void)unlock_allocation();
    alloc_checkpoint.rollback();
    return E_OUTOFMEMORY;
  }

  auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty) {
    (void)unlock_allocation();
    SetError(dev, E_OUTOFMEMORY);
    alloc_checkpoint.rollback();
    return E_OUTOFMEMORY;
  }
  // Note: the host validates RESOURCE_DIRTY_RANGE against the protocol-visible
  // required bytes (CREATE_TEXTURE2D layouts). Do not use the runtime's
  // SlicePitch here, which can include extra padding and exceed the protocol
  // size.
  dirty->resource_handle = res->handle;
  dirty->reserved0 = 0;
  dirty->offset_bytes = upload_offset;
  dirty->size_bytes = upload_size;

  // Only write after successfully recording the dirty-range command.
  if (row_copy_texture2d) {
    uint8_t* dst_base = static_cast<uint8_t*>(lock_args.pData);
    const uint8_t* src_base = res->storage.data();
    for (uint32_t y = 0; y < rows; ++y) {
      const size_t src_off_row = static_cast<size_t>(y) * res->row_pitch_bytes;
      const size_t dst_off_row = static_cast<size_t>(y) * dst_pitch;
      std::memcpy(dst_base + dst_off_row, src_base + src_off_row, row_bytes);
      if (dst_pitch > row_bytes) {
        std::memset(dst_base + dst_off_row + row_bytes, 0, dst_pitch - row_bytes);
      }
    }
  } else {
    // For buffers and multi-subresource Texture2D resources, treat the resource's
    // backing allocation as a linear byte array matching our `res->storage`
    // layout and copy the requested range verbatim.
    std::memcpy(static_cast<uint8_t*>(lock_args.pData) + off, res->storage.data() + off, sz);
  }

  hr = unlock_allocation();
  if (FAILED(hr)) {
    SetError(dev, hr);
    return hr;
  }
  return S_OK;
}

static void EmitDirtyRangeLocked(Device* dev, Resource* res, uint64_t offset_bytes, uint64_t size_bytes) {
  if (!dev || !res || !res->handle || size_bytes == 0) {
    return;
  }

  // RESOURCE_DIRTY_RANGE causes the host to read the guest allocation to update the host copy.
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  if (dev->wddm_submit_allocation_list_oom) {
    alloc_checkpoint.rollback();
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    alloc_checkpoint.rollback();
    return;
  }
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->offset_bytes = offset_bytes;
  cmd->size_bytes = size_bytes;
}

static Device* DeviceFromHandle(D3D11DDI_HDEVICE hDevice);
static Device* DeviceFromHandle(D3D11DDI_HDEVICECONTEXT hCtx);
template <typename T>
static Device* DeviceFromHandle(T);

inline void ReportNotImpl() {}

template <typename Handle0, typename... Rest>
inline void ReportNotImpl(Handle0 handle0, Rest...) {
  SetError(DeviceFromHandle(handle0), E_NOTIMPL);
}

static bool SetTextureLocked(Device* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  if (!dev) {
    return false;
  }
  if (!EmitSetTextureCmdLocked(dev, shader_stage, slot, texture, [&](HRESULT hr) { SetError(dev, hr); })) {
    return false;
  }

  if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    AEROGPU_D3D10_11_LOG("emit GS SetTexture slot=%u tex=%u",
                         static_cast<unsigned>(slot),
                         static_cast<unsigned>(texture));
  }
  return true;
}

static aerogpu_handle_t* ShaderResourceTableForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->vs_srvs;
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->ps_srvs;
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_srvs;
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->cs_srvs;
    default:
      return nullptr;
  }
}

static aerogpu_handle_t* SamplerTableForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->vs_samplers;
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->ps_samplers;
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->current_gs_samplers;
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->cs_samplers;
    default:
      return nullptr;
  }
}

static aerogpu_constant_buffer_binding* ConstantBufferTableForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->vs_constant_buffers;
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->ps_constant_buffers;
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_constant_buffers;
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->cs_constant_buffers;
    default:
      return nullptr;
  }
}

static aerogpu_shader_resource_buffer_binding* ShaderResourceBufferTableForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->vs_srv_buffers;
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->ps_srv_buffers;
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_srv_buffers;
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->cs_srv_buffers;
    default:
      return nullptr;
  }
}

static Resource** CurrentTextureSrvsForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->current_vs_srvs.data();
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->current_ps_srvs.data();
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->current_gs_srvs.data();
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->current_cs_srvs.data();
    default:
      return nullptr;
  }
}

static Resource** CurrentBufferSrvsForStage(Device* dev, uint32_t shader_stage) {
  if (!dev) {
    return nullptr;
  }
  switch (shader_stage) {
    case AEROGPU_SHADER_STAGE_VERTEX:
      return dev->current_vs_srv_buffers.data();
    case AEROGPU_SHADER_STAGE_PIXEL:
      return dev->current_ps_srv_buffers.data();
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->current_gs_srv_buffers.data();
    case AEROGPU_SHADER_STAGE_COMPUTE:
      return dev->current_cs_srv_buffers.data();
    default:
      return nullptr;
  }
}

static bool BindShaderResourceBuffersRangeLocked(Device* dev,
                                                 uint32_t shader_stage,
                                                 uint32_t start_slot,
                                                 uint32_t buffer_count,
                                                 const aerogpu_shader_resource_buffer_binding* bindings) {
  if (!dev || !bindings || buffer_count == 0) {
    return false;
  }
  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_shader_resource_buffers>(
      AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS, bindings, static_cast<size_t>(buffer_count) * sizeof(bindings[0]));
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->buffer_count = buffer_count;
  cmd->reserved0 = 0;

  if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    AEROGPU_D3D10_11_LOG("emit GS SetShaderResourceBuffers start=%u count=%u",
                         static_cast<unsigned>(start_slot),
                         static_cast<unsigned>(buffer_count));
  }
  return true;
}

static bool BindUnorderedAccessBuffersRangeLocked(Device* dev,
                                                  uint32_t shader_stage,
                                                  uint32_t start_slot,
                                                  uint32_t buffer_count,
                                                  const aerogpu_unordered_access_buffer_binding* bindings) {
  if (!dev || !bindings || buffer_count == 0) {
    return false;
  }
  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_unordered_access_buffers>(
      AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS, bindings, static_cast<size_t>(buffer_count) * sizeof(bindings[0]));
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->uav_count = buffer_count;
  cmd->reserved0 = 0;
  return true;
}

static void SetShaderResourceSlotLocked(Device* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  if (!dev || slot >= kMaxShaderResourceSlots) {
    return;
  }
  aerogpu_handle_t* table = ShaderResourceTableForStage(dev, shader_stage);
  if (!table) {
    return;
  }
  if (table[slot] == texture) {
    return;
  }
  if (!SetTextureLocked(dev, shader_stage, slot, texture)) {
    return;
  }
  table[slot] = texture;
}

static void UnbindResourceFromSrvsLocked(Device* dev, aerogpu_handle_t resource, const Resource* res) {
  if (!dev || (resource == 0 && !res)) {
    return;
  }
  const aerogpu_shader_resource_buffer_binding null_buf_srv{};
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if ((resource != 0 && dev->vs_srvs[slot] == resource) ||
        (res && slot < dev->current_vs_srvs.size() && ResourcesAlias(dev->current_vs_srvs[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
      if (dev->vs_srvs[slot] == 0) {
        if (slot < dev->current_vs_srvs.size()) {
          dev->current_vs_srvs[slot] = nullptr;
        }
        if (slot == 0) {
          dev->current_vs_srv0 = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->vs_srv_buffers[slot].buffer == resource) ||
        (res && slot < dev->current_vs_srv_buffers.size() && ResourcesAlias(dev->current_vs_srv_buffers[slot], res))) {
      if (BindShaderResourceBuffersRangeLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 1, &null_buf_srv)) {
        dev->vs_srv_buffers[slot] = null_buf_srv;
        if (slot < dev->current_vs_srv_buffers.size()) {
          dev->current_vs_srv_buffers[slot] = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->ps_srvs[slot] == resource) ||
        (res && slot < dev->current_ps_srvs.size() && ResourcesAlias(dev->current_ps_srvs[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
      if (dev->ps_srvs[slot] == 0) {
        if (slot < dev->current_ps_srvs.size()) {
          dev->current_ps_srvs[slot] = nullptr;
        }
        if (slot == 0) {
          dev->current_ps_srv0 = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->ps_srv_buffers[slot].buffer == resource) ||
        (res && slot < dev->current_ps_srv_buffers.size() && ResourcesAlias(dev->current_ps_srv_buffers[slot], res))) {
      if (BindShaderResourceBuffersRangeLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 1, &null_buf_srv)) {
        dev->ps_srv_buffers[slot] = null_buf_srv;
        if (slot < dev->current_ps_srv_buffers.size()) {
          dev->current_ps_srv_buffers[slot] = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->gs_srvs[slot] == resource) ||
        (res && slot < dev->current_gs_srvs.size() && ResourcesAlias(dev->current_gs_srvs[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 0);
      if (dev->gs_srvs[slot] == 0) {
        if (slot < dev->current_gs_srvs.size()) {
          dev->current_gs_srvs[slot] = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->gs_srv_buffers[slot].buffer == resource) ||
        (res && slot < dev->current_gs_srv_buffers.size() && ResourcesAlias(dev->current_gs_srv_buffers[slot], res))) {
      if (BindShaderResourceBuffersRangeLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 1, &null_buf_srv)) {
        dev->gs_srv_buffers[slot] = null_buf_srv;
        if (slot < dev->current_gs_srv_buffers.size()) {
          dev->current_gs_srv_buffers[slot] = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->cs_srvs[slot] == resource) ||
        (res && slot < dev->current_cs_srvs.size() && ResourcesAlias(dev->current_cs_srvs[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_COMPUTE, slot, 0);
      if (dev->cs_srvs[slot] == 0) {
        if (slot < dev->current_cs_srvs.size()) {
          dev->current_cs_srvs[slot] = nullptr;
        }
      }
    }
    if ((resource != 0 && dev->cs_srv_buffers[slot].buffer == resource) ||
        (res && slot < dev->current_cs_srv_buffers.size() && ResourcesAlias(dev->current_cs_srv_buffers[slot], res))) {
      if (BindShaderResourceBuffersRangeLocked(dev, AEROGPU_SHADER_STAGE_COMPUTE, slot, 1, &null_buf_srv)) {
        dev->cs_srv_buffers[slot] = null_buf_srv;
        if (slot < dev->current_cs_srv_buffers.size()) {
          dev->current_cs_srv_buffers[slot] = nullptr;
        }
      }
    }
  }
}

static void UnbindResourceFromSrvsLocked(Device* dev, const Resource* resource) {
  UnbindResourceFromSrvsLocked(dev, /*resource_handle=*/0, resource);
}

static void UnbindResourceFromSrvsLocked(Device* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  UnbindResourceFromSrvsLocked(dev, resource, nullptr);
}

static void UnbindResourceFromUavsLocked(Device* dev,
                                         aerogpu_handle_t resource,
                                         const Resource* res,
                                         uint32_t exclude_slot) {
  if (!dev || (resource == 0 && !res)) {
    return;
  }
  for (uint32_t slot = 0; slot < kMaxUavSlots; ++slot) {
    if (slot == exclude_slot) {
      continue;
    }
    if ((resource == 0 || dev->cs_uavs[slot].buffer != resource) &&
        (!res || slot >= dev->current_cs_uavs.size() || !ResourcesAlias(dev->current_cs_uavs[slot], res))) {
      continue;
    }
    aerogpu_unordered_access_buffer_binding null_uav{};
    null_uav.initial_count = kD3DUavInitialCountNoChange;
    if (BindUnorderedAccessBuffersRangeLocked(dev, AEROGPU_SHADER_STAGE_COMPUTE, slot, 1, &null_uav)) {
      dev->cs_uavs[slot] = null_uav;
      if (slot < dev->current_cs_uavs.size()) {
        dev->current_cs_uavs[slot] = nullptr;
      }
    }
  }
}

static void UnbindResourceFromUavsLocked(Device* dev, aerogpu_handle_t resource, const Resource* res) {
  UnbindResourceFromUavsLocked(dev, resource, res, /*exclude_slot=*/kMaxUavSlots);
}

static bool AppendSetRenderTargetsCmdLocked(Device* dev,
                                            uint32_t rtv_count,
                                            const std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS>& rtvs,
                                            aerogpu_handle_t dsv) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  const uint32_t count = std::min<uint32_t>(rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  cmd->color_count = count;
  cmd->depth_stencil = dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    cmd->colors[i] = (i < count) ? rtvs[i] : 0;
  }
  return true;
}

static bool UnbindResourceFromRenderTargetsLocked(Device* dev, aerogpu_handle_t resource, const Resource* res) {
  if (!dev || (resource == 0 && !res)) {
    return false;
  }

  const uint32_t count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> new_rtvs = dev->current_rtvs;
  std::array<Resource*, AEROGPU_MAX_RENDER_TARGETS> new_resources = dev->current_rtv_resources;
  aerogpu_handle_t new_dsv = dev->current_dsv;
  Resource* new_dsv_resource = dev->current_dsv_resource;

  bool changed = false;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if ((resource != 0 && new_rtvs[i] == resource) ||
        (res && ResourcesAlias(new_resources[i], res))) {
      new_rtvs[i] = 0;
      new_resources[i] = nullptr;
      changed = true;
    }
  }
  if ((resource != 0 && new_dsv == resource) ||
      (res && ResourcesAlias(new_dsv_resource, res))) {
    new_dsv = 0;
    new_dsv_resource = nullptr;
    changed = true;
  }
  if (!changed) {
    return false;
  }

  if (!AppendSetRenderTargetsCmdLocked(dev, count, new_rtvs, new_dsv)) {
    return false;
  }

  dev->current_rtvs = new_rtvs;
  dev->current_rtv_resources = new_resources;
  dev->current_dsv = new_dsv;
  dev->current_dsv_resource = new_dsv_resource;

  return true;
}

static void EmitSetRenderTargetsLocked(Device* dev) {
  if (!dev) {
    return;
  }
  if (!EmitSetRenderTargetsCmdFromStateLocked(dev)) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  // Optional bring-up logging for Win7 tracing.
  const uint32_t count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  AEROGPU_D3D10_11_LOG("SET_RENDER_TARGETS: color_count=%u depth=%u colors=[%u,%u,%u,%u,%u,%u,%u,%u]",
                       static_cast<unsigned>(count),
                       static_cast<unsigned>(dev->current_dsv),
                       static_cast<unsigned>(dev->current_rtvs[0]),
                       static_cast<unsigned>(dev->current_rtvs[1]),
                       static_cast<unsigned>(dev->current_rtvs[2]),
                       static_cast<unsigned>(dev->current_rtvs[3]),
                       static_cast<unsigned>(dev->current_rtvs[4]),
                       static_cast<unsigned>(dev->current_rtvs[5]),
                       static_cast<unsigned>(dev->current_rtvs[6]),
                       static_cast<unsigned>(dev->current_rtvs[7]));
}

static void UnbindResourceFromOutputsLocked(Device* dev, aerogpu_handle_t resource, const Resource* res) {
  if (!dev || (resource == 0 && !res)) {
    return;
  }
  // Compute UAVs are outputs too: binding a resource as an SRV must unbind any
  // aliasing UAVs.
  UnbindResourceFromUavsLocked(dev, resource, res);
  (void)UnbindResourceFromRenderTargetsLocked(dev, resource, res);
}

static void UnbindResourceFromOutputsLocked(Device* dev, const Resource* resource) {
  if (!dev || !resource) {
    return;
  }
  UnbindResourceFromOutputsLocked(dev, /*resource_handle=*/0, resource);
}

static void UnbindResourceFromConstantBuffersLocked(Device* dev, const Resource* res) {
  if (!dev || !res) {
    return;
  }

  bool oom = false;
  const aerogpu_constant_buffer_binding null_cb{};
  const aerogpu_handle_t handle = res->handle;

  auto unbind_stage = [&](uint32_t shader_stage,
                          aerogpu_constant_buffer_binding* table,
                          std::array<Resource*, kMaxConstantBufferSlots>& bound_resources) {
    if (!table) {
      return;
    }
    for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
      if ((handle != 0 && table[slot].buffer == handle) ||
          ResourcesAlias(bound_resources[slot], res)) {
        if (!oom && (table[slot].buffer != 0 ||
                     table[slot].offset_bytes != 0 ||
                     table[slot].size_bytes != 0 ||
                     table[slot].reserved0 != 0)) {
          if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                                  shader_stage,
                                                                  slot,
                                                                  /*buffer_count=*/1,
                                                                  &null_cb,
                                                                  [&](HRESULT hr) { SetError(dev, hr); })) {
            oom = true;
          }
        }
        table[slot] = null_cb;
        bound_resources[slot] = nullptr;

        // Keep the software-rasterizer CB0 caches consistent with the slot 0
        // bindings (even when the runtime relies on implicit refcounting rather
        // than explicit unbinds).
        if (slot == 0 && shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
          dev->current_vs_cb0 = nullptr;
          dev->current_vs_cb0_first_constant = 0;
          dev->current_vs_cb0_num_constants = 0;
        } else if (slot == 0 && shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
          dev->current_ps_cb0 = nullptr;
          dev->current_ps_cb0_first_constant = 0;
          dev->current_ps_cb0_num_constants = 0;
        }
      }
    }
  };

  unbind_stage(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_constant_buffers, dev->current_vs_cbs);
  unbind_stage(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_constant_buffers, dev->current_ps_cbs);
  unbind_stage(AEROGPU_SHADER_STAGE_GEOMETRY, dev->gs_constant_buffers, dev->current_gs_cbs);
  unbind_stage(AEROGPU_SHADER_STAGE_COMPUTE, dev->cs_constant_buffers, dev->current_cs_cbs);
}

static void UnbindResourceFromInputAssemblerLocked(Device* dev, const Resource* res) {
  if (!dev || !res) {
    return;
  }

  for (uint32_t slot = 0; slot < dev->current_vb_resources.size(); ++slot) {
    if (!ResourcesAlias(dev->current_vb_resources[slot], res)) {
      continue;
    }
    dev->current_vb_resources[slot] = nullptr;
    dev->current_vb_strides_bytes[slot] = 0;
    dev->current_vb_offsets_bytes[slot] = 0;
    if (slot == 0) {
      dev->current_vb = nullptr;
      dev->current_vb_stride_bytes = 0;
      dev->current_vb_offset_bytes = 0;
    }

    aerogpu_vertex_buffer_binding vb{};
    vb.buffer = 0;
    vb.stride_bytes = 0;
    vb.offset_bytes = 0;
    vb.reserved0 = 0;
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
        AEROGPU_CMD_SET_VERTEX_BUFFERS, &vb, sizeof(vb));
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
    } else {
      cmd->start_slot = slot;
      cmd->buffer_count = 1;
    }
  }

  if (ResourcesAlias(dev->current_ib, res)) {
    dev->current_ib = nullptr;
    dev->current_ib_format = kDxgiFormatUnknown;
    dev->current_ib_offset_bytes = 0;

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
    } else {
      cmd->buffer = 0;
      cmd->format = AEROGPU_INDEX_FORMAT_UINT16;
      cmd->offset_bytes = 0;
      cmd->reserved0 = 0;
    }
  }
}

template <typename TFnPtr>
struct DdiStub;

template <typename Ret, typename... Args>
struct DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, void>) {
      ReportNotImpl(args...);
      return;
    } else if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
      // Size queries must not return 0 to avoid runtimes treating the object as
      // unsupported and then dereferencing null private memory.
      return sizeof(uint64_t);
    } else {
      return Ret{};
    }
  }
};

template <typename TFnPtr>
struct DdiNoopStub;

template <typename Ret, typename... Args>
struct DdiNoopStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
      // Size queries must not return 0 to avoid runtimes treating the object as
      // unsupported and then dereferencing null private memory.
      return sizeof(uint64_t);
    } else if constexpr (std::is_same_v<Ret, void>) {
      return;
    } else {
      return Ret{};
    }
  }
};

// -------------------------------------------------------------------------------------------------
// D3D11 DDI function-table stub filling
//
// Win7 D3D11 runtimes may call a surprisingly large portion of the DDI surface
// during device creation / validation. Returning NULL function pointers in the
// device/context tables is therefore a crash risk.
//
// Strategy:
// - HRESULT-returning DDIs: return E_NOTIMPL.
// - void-returning DDIs: SetError(dev, E_NOTIMPL) and return.
// - SIZE_T-returning CalcPrivate*Size DDIs: return a small non-zero size.
//
// All assignments are guarded with `__if_exists` so this stays buildable across
// WDK header revisions / interface versions.
// -------------------------------------------------------------------------------------------------

static Device* DeviceFromHandle(D3D11DDI_HDEVICE hDevice) {
  return hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
}

static Device* DeviceFromHandle(D3D11DDI_HDEVICECONTEXT hCtx) {
  return DeviceFromContext(hCtx);
}

template <typename T>
static Device* DeviceFromHandle(T) {
  return nullptr;
}

#define AEROGPU_D3D11_DEVICEFUNCS_FIELDS(X)                                                                     \
  X(pfnDestroyDevice)                                                                                            \
  X(pfnCalcPrivateResourceSize)                                                                                  \
  X(pfnCreateResource)                                                                                            \
  X(pfnOpenResource)                                                                                              \
  X(pfnDestroyResource)                                                                                           \
  X(pfnCalcPrivateShaderResourceViewSize)                                                                         \
  X(pfnCreateShaderResourceView)                                                                                  \
  X(pfnDestroyShaderResourceView)                                                                                 \
  X(pfnCalcPrivateRenderTargetViewSize)                                                                           \
  X(pfnCreateRenderTargetView)                                                                                    \
  X(pfnDestroyRenderTargetView)                                                                                   \
  X(pfnCalcPrivateDepthStencilViewSize)                                                                           \
  X(pfnCreateDepthStencilView)                                                                                    \
  X(pfnDestroyDepthStencilView)                                                                                   \
  X(pfnCalcPrivateUnorderedAccessViewSize)                                                                        \
  X(pfnCreateUnorderedAccessView)                                                                                 \
  X(pfnDestroyUnorderedAccessView)                                                                                \
  X(pfnCalcPrivateVertexShaderSize)                                                                               \
  X(pfnCreateVertexShader)                                                                                        \
  X(pfnDestroyVertexShader)                                                                                       \
  X(pfnCalcPrivatePixelShaderSize)                                                                                \
  X(pfnCreatePixelShader)                                                                                         \
  X(pfnDestroyPixelShader)                                                                                        \
  X(pfnCalcPrivateGeometryShaderSize)                                                                             \
  X(pfnCreateGeometryShader)                                                                                      \
  X(pfnDestroyGeometryShader)                                                                                     \
  X(pfnCalcPrivateGeometryShaderWithStreamOutputSize)                                                             \
  X(pfnCreateGeometryShaderWithStreamOutput)                                                                      \
  X(pfnCalcPrivateHullShaderSize)                                                                                 \
  X(pfnCreateHullShader)                                                                                          \
  X(pfnDestroyHullShader)                                                                                         \
  X(pfnCalcPrivateDomainShaderSize)                                                                               \
  X(pfnCreateDomainShader)                                                                                        \
  X(pfnDestroyDomainShader)                                                                                       \
  X(pfnCalcPrivateComputeShaderSize)                                                                              \
  X(pfnCreateComputeShader)                                                                                       \
  X(pfnDestroyComputeShader)                                                                                      \
  X(pfnCalcPrivateElementLayoutSize)                                                                              \
  X(pfnCreateElementLayout)                                                                                       \
  X(pfnDestroyElementLayout)                                                                                      \
  X(pfnCalcPrivateSamplerSize)                                                                                    \
  X(pfnCreateSampler)                                                                                             \
  X(pfnDestroySampler)                                                                                            \
  X(pfnCalcPrivateBlendStateSize)                                                                                 \
  X(pfnCreateBlendState)                                                                                          \
  X(pfnDestroyBlendState)                                                                                         \
  X(pfnCalcPrivateRasterizerStateSize)                                                                            \
  X(pfnCreateRasterizerState)                                                                                     \
  X(pfnDestroyRasterizerState)                                                                                    \
  X(pfnCalcPrivateDepthStencilStateSize)                                                                          \
  X(pfnCreateDepthStencilState)                                                                                   \
  X(pfnDestroyDepthStencilState)                                                                                  \
  X(pfnCalcPrivateQuerySize)                                                                                      \
  X(pfnCreateQuery)                                                                                                \
  X(pfnDestroyQuery)                                                                                               \
  X(pfnCalcPrivatePredicateSize)                                                                                  \
  X(pfnCreatePredicate)                                                                                            \
  X(pfnDestroyPredicate)                                                                                           \
  X(pfnCalcPrivateCounterSize)                                                                                    \
  X(pfnCreateCounter)                                                                                              \
  X(pfnDestroyCounter)                                                                                             \
  X(pfnCalcPrivateDeferredContextSize)                                                                            \
  X(pfnCreateDeferredContext)                                                                                      \
  X(pfnDestroyDeferredContext)                                                                                     \
  X(pfnCalcPrivateCommandListSize)                                                                                \
  X(pfnCreateCommandList)                                                                                          \
  X(pfnDestroyCommandList)                                                                                         \
  X(pfnCalcPrivateClassLinkageSize)                                                                               \
  X(pfnCreateClassLinkage)                                                                                         \
  X(pfnDestroyClassLinkage)                                                                                        \
  X(pfnCalcPrivateClassInstanceSize)                                                                              \
  X(pfnCreateClassInstance)                                                                                        \
  X(pfnDestroyClassInstance)                                                                                       \
  X(pfnCheckCounterInfo)                                                                                           \
  X(pfnCheckCounter)                                                                                               \
  X(pfnGetDeviceRemovedReason)                                                                                     \
  X(pfnGetExceptionMode)                                                                                           \
  X(pfnSetExceptionMode)                                                                                           \
  X(pfnPresent)                                                                                                    \
  X(pfnRotateResourceIdentities)                                                                                   \
  X(pfnCheckDeferredContextHandleSizes)                                                                            \
  X(pfnCalcPrivateDeviceContextSize)                                                                               \
  X(pfnCreateDeviceContext)                                                                                        \
  X(pfnDestroyDeviceContext)                                                                                       \
  X(pfnCalcPrivateDeviceContextStateSize)                                                                          \
  X(pfnCreateDeviceContextState)                                                                                   \
  X(pfnDestroyDeviceContextState)

#define AEROGPU_D3D11_DEVICECONTEXTFUNCS_FIELDS(X)                                                                \
  X(pfnVsSetShader)                                                                                                \
  X(pfnVsSetConstantBuffers)                                                                                       \
  X(pfnVsSetShaderResources)                                                                                       \
  X(pfnVsSetSamplers)                                                                                              \
  X(pfnGsSetShader)                                                                                                \
  X(pfnGsSetConstantBuffers)                                                                                       \
  X(pfnGsSetShaderResources)                                                                                       \
  X(pfnGsSetSamplers)                                                                                              \
  X(pfnPsSetShader)                                                                                                \
  X(pfnPsSetConstantBuffers)                                                                                       \
  X(pfnPsSetShaderResources)                                                                                       \
  X(pfnPsSetSamplers)                                                                                              \
  X(pfnHsSetShader)                                                                                                \
  X(pfnHsSetConstantBuffers)                                                                                       \
  X(pfnHsSetShaderResources)                                                                                       \
  X(pfnHsSetSamplers)                                                                                              \
  X(pfnDsSetShader)                                                                                                \
  X(pfnDsSetConstantBuffers)                                                                                       \
  X(pfnDsSetShaderResources)                                                                                       \
  X(pfnDsSetSamplers)                                                                                              \
  X(pfnCsSetShader)                                                                                                \
  X(pfnCsSetConstantBuffers)                                                                                       \
  X(pfnCsSetShaderResources)                                                                                       \
  X(pfnCsSetSamplers)                                                                                              \
  X(pfnCsSetUnorderedAccessViews)                                                                                  \
  X(pfnIaSetInputLayout)                                                                                            \
  X(pfnIaSetVertexBuffers)                                                                                         \
  X(pfnIaSetIndexBuffer)                                                                                            \
  X(pfnIaSetTopology)                                                                                               \
  X(pfnSoSetTargets)                                                                                                \
  X(pfnSetViewports)                                                                                                \
  X(pfnSetScissorRects)                                                                                             \
  X(pfnSetRasterizerState)                                                                                          \
  X(pfnSetBlendState)                                                                                               \
  X(pfnSetDepthStencilState)                                                                                        \
  X(pfnSetRenderTargets)                                                                                            \
  X(pfnSetRenderTargetsAndUnorderedAccessViews)                                                                     \
  X(pfnSetRenderTargetsAndUnorderedAccessViews11_1)                                                                 \
  X(pfnDraw)                                                                                                        \
  X(pfnDrawIndexed)                                                                                                 \
  X(pfnDrawInstanced)                                                                                               \
  X(pfnDrawIndexedInstanced)                                                                                        \
  X(pfnDrawAuto)                                                                                                    \
  X(pfnDrawInstancedIndirect)                                                                                       \
  X(pfnDrawIndexedInstancedIndirect)                                                                                \
  X(pfnDispatch)                                                                                                    \
  X(pfnDispatchIndirect)                                                                                            \
  X(pfnStagingResourceMap)                                                                                          \
  X(pfnStagingResourceUnmap)                                                                                        \
  X(pfnDynamicIABufferMapDiscard)                                                                                   \
  X(pfnDynamicIABufferMapNoOverwrite)                                                                               \
  X(pfnDynamicIABufferUnmap)                                                                                        \
  X(pfnDynamicConstantBufferMapDiscard)                                                                             \
  X(pfnDynamicConstantBufferUnmap)                                                                                  \
  X(pfnMap)                                                                                                         \
  X(pfnUnmap)                                                                                                       \
  X(pfnUpdateSubresourceUP)                                                                                         \
  X(pfnUpdateSubresource)                                                                                           \
  X(pfnCopySubresourceRegion)                                                                                       \
  X(pfnCopyResource)                                                                                                \
  X(pfnCopyStructureCount)                                                                                           \
  X(pfnResolveSubresource)                                                                                           \
  X(pfnGenerateMips)                                                                                                \
  X(pfnSetResourceMinLOD)                                                                                             \
  X(pfnGetResourceMinLOD)                                                                                             \
  X(pfnClearRenderTargetView)                                                                                       \
  X(pfnClearUnorderedAccessViewUint)                                                                                \
  X(pfnClearUnorderedAccessViewFloat)                                                                               \
  X(pfnClearDepthStencilView)                                                                                       \
  X(pfnBegin)                                                                                                       \
  X(pfnEnd)                                                                                                         \
  X(pfnQueryGetData)                                                                                                \
  X(pfnGetData)                                                                                                      \
  X(pfnSetPredication)                                                                                               \
  X(pfnExecuteCommandList)                                                                                          \
  X(pfnFinishCommandList)                                                                                           \
  X(pfnClearState)                                                                                                  \
  X(pfnFlush)                                                                                                       \
  X(pfnPresent)                                                                                                     \
  X(pfnRotateResourceIdentities)                                                                                    \
  X(pfnDiscardResource)                                                                                              \
  X(pfnDiscardView)                                                                                                  \
  X(pfnSetMarker)                                                                                                    \
  X(pfnBeginEvent)                                                                                                   \
  X(pfnEndEvent)

// Context entrypoints that are frequently called as part of ClearState /
// unbind/reset sequences and should not spam SetErrorCb(E_NOTIMPL) when stubbed.
#define AEROGPU_D3D11_DEVICECONTEXTFUNCS_NOOP_FIELDS(X)                                                            \
  X(pfnDiscardResource)                                                                                              \
  X(pfnDiscardView)                                                                                                  \
  X(pfnSetMarker)                                                                                                    \
  X(pfnBeginEvent)                                                                                                   \
  X(pfnEndEvent)                                                                                                     \
  X(pfnSetResourceMinLOD)

// Device-level functions that should never trip the runtime error state when
// stubbed. These are primarily Destroy* entrypoints that may be called during
// cleanup/reset even after a higher-level failure.
#define AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS(X)                                                                    \
  X(pfnDestroyDevice)                                                                                                \
  X(pfnDestroyResource)                                                                                              \
  X(pfnDestroyShaderResourceView)                                                                                    \
  X(pfnDestroyRenderTargetView)                                                                                      \
  X(pfnDestroyDepthStencilView)                                                                                      \
  X(pfnDestroyUnorderedAccessView)                                                                                   \
  X(pfnDestroyVertexShader)                                                                                          \
  X(pfnDestroyPixelShader)                                                                                           \
  X(pfnDestroyGeometryShader)                                                                                        \
  X(pfnDestroyHullShader)                                                                                            \
  X(pfnDestroyDomainShader)                                                                                          \
  X(pfnDestroyComputeShader)                                                                                         \
  X(pfnDestroyClassLinkage)                                                                                          \
  X(pfnDestroyClassInstance)                                                                                         \
  X(pfnDestroyElementLayout)                                                                                         \
  X(pfnDestroySampler)                                                                                               \
  X(pfnDestroyBlendState)                                                                                            \
  X(pfnDestroyRasterizerState)                                                                                       \
  X(pfnDestroyDepthStencilState)                                                                                     \
  X(pfnDestroyQuery)                                                                                                 \
  X(pfnDestroyPredicate)                                                                                             \
  X(pfnDestroyCounter)                                                                                               \
  X(pfnDestroyDeviceContext)                                                                                         \
  X(pfnDestroyDeferredContext)                                                                                       \
  X(pfnDestroyCommandList)                                                                                           \
  X(pfnDestroyDeviceContextState)

static void InitDeviceFuncsWithStubs(D3D11DDI_DEVICEFUNCS* out) {
  if (!out) {
    return;
  }
  std::memset(out, 0, sizeof(*out));
#define AEROGPU_ASSIGN_DEVICE_STUB(field)                                                                           \
  __if_exists(D3D11DDI_DEVICEFUNCS::field) { out->field = &DdiStub<decltype(out->field)>::Call; }
  AEROGPU_D3D11_DEVICEFUNCS_FIELDS(AEROGPU_ASSIGN_DEVICE_STUB)
#undef AEROGPU_ASSIGN_DEVICE_STUB

  // Ensure benign cleanup paths never spam SetErrorCb.
#define AEROGPU_ASSIGN_DEVICE_NOOP(field)                                                                           \
  __if_exists(D3D11DDI_DEVICEFUNCS::field) { out->field = &DdiNoopStub<decltype(out->field)>::Call; }
  AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS(AEROGPU_ASSIGN_DEVICE_NOOP)
#undef AEROGPU_ASSIGN_DEVICE_NOOP
}

static void InitDeviceContextFuncsWithStubs(D3D11DDI_DEVICECONTEXTFUNCS* out) {
  if (!out) {
    return;
  }
  std::memset(out, 0, sizeof(*out));
#define AEROGPU_ASSIGN_CTX_STUB(field)                                                                              \
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::field) { out->field = &DdiStub<decltype(out->field)>::Call; }
  AEROGPU_D3D11_DEVICECONTEXTFUNCS_FIELDS(AEROGPU_ASSIGN_CTX_STUB)
#undef AEROGPU_ASSIGN_CTX_STUB

  // Avoid spamming SetErrorCb for benign ClearState/unbind sequences.
#define AEROGPU_ASSIGN_CTX_NOOP(field)                                                                              \
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::field) { out->field = &DdiNoopStub<decltype(out->field)>::Call; }
  AEROGPU_D3D11_DEVICECONTEXTFUNCS_NOOP_FIELDS(AEROGPU_ASSIGN_CTX_NOOP)
#undef AEROGPU_ASSIGN_CTX_NOOP
}
// Some DDIs (notably Present/RotateResourceIdentities) historically move between
// the device and context tables across D3D11 DDI interface versions. Bind them
// opportunistically based on whether the field exists and the pointer type
// matches.
template <typename T, typename = void>
struct HasPresent : std::false_type {};

template <typename T>
struct HasPresent<T, std::void_t<decltype(std::declval<T>().pfnPresent)>> : std::true_type {};

template <typename T, typename = void>
struct HasRotateResourceIdentities : std::false_type {};

template <typename T>
struct HasRotateResourceIdentities<T, std::void_t<decltype(std::declval<T>().pfnRotateResourceIdentities)>> : std::true_type {};

HRESULT AEROGPU_APIENTRY Present11(D3D11DDI_HDEVICECONTEXT hCtx, const D3D10DDIARG_PRESENT* pPresent);
void AEROGPU_APIENTRY RotateResourceIdentities11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE* pResources, UINT numResources);

HRESULT AEROGPU_APIENTRY Present11Device(D3D11DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent);
void AEROGPU_APIENTRY RotateResourceIdentities11Device(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRESOURCE* pResources, UINT numResources);

template <typename TFuncs>
static void BindPresentAndRotate(TFuncs* funcs) {
  if (!funcs) {
    return;
  }

  if constexpr (HasPresent<TFuncs>::value) {
    using Fn = decltype(funcs->pfnPresent);
    if constexpr (std::is_convertible_v<decltype(&Present11), Fn>) {
      funcs->pfnPresent = AEROGPU_D3D11_WDK_DDI(Present11);
    } else if constexpr (std::is_convertible_v<decltype(&Present11Device), Fn>) {
      funcs->pfnPresent = AEROGPU_D3D11_WDK_DDI(Present11Device);
    } else {
      funcs->pfnPresent = &DdiStub<Fn>::Call;
    }
  }

  if constexpr (HasRotateResourceIdentities<TFuncs>::value) {
    using Fn = decltype(funcs->pfnRotateResourceIdentities);
    if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11), Fn>) {
      funcs->pfnRotateResourceIdentities = AEROGPU_D3D11_WDK_DDI(RotateResourceIdentities11);
    } else if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11Device), Fn>) {
      funcs->pfnRotateResourceIdentities = AEROGPU_D3D11_WDK_DDI(RotateResourceIdentities11Device);
    } else {
      funcs->pfnRotateResourceIdentities = &DdiStub<Fn>::Call;
    }
  }
}

HRESULT AEROGPU_APIENTRY GetDeviceRemovedReason11(D3D11DDI_HDEVICE) {
  // The runtime expects S_OK when the device is healthy. Returning E_NOTIMPL
  // here can cause higher-level API calls like ID3D11Device::GetDeviceRemovedReason
  // to fail unexpectedly.
  return S_OK;
}

static D3D11DDI_ADAPTERFUNCS MakeStubAdapterFuncs11() {
  D3D11DDI_ADAPTERFUNCS funcs = {};
#define STUB_FIELD(field) funcs.field = &DdiStub<decltype(funcs.field)>::Call
  STUB_FIELD(pfnGetCaps);
  STUB_FIELD(pfnCalcPrivateDeviceSize);
  __if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
    STUB_FIELD(pfnCalcPrivateDeviceContextSize);
  }
  STUB_FIELD(pfnCreateDevice);
  STUB_FIELD(pfnCloseAdapter);
#undef STUB_FIELD
  assert(ValidateNoNullDdiTable("D3D11DDI_ADAPTERFUNCS (stub)", &funcs, sizeof(funcs)));
  return funcs;
}

static bool UnmapLocked(Device* dev, Resource* res) {
  if (!dev || !res) {
    return false;
  }
  if (!res->mapped) {
    return false;
  }

  const bool is_write = (res->mapped_map_type != kD3D11MapRead);
  bool dirty_emitted_on_unmap = false;
  bool dirty_failed_on_unmap = false;
  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (is_write && res->mapped_size != 0 && res->backing_alloc_id != 0) {
      // For guest-backed resources, ensure we can record RESOURCE_DIRTY_RANGE
      // before committing the CPU-written bytes into our software shadow copy.
      //
      // If we cannot record the dirty range due to OOM, roll back any command
      // buffer/alloc-list changes and restore the guest allocation contents from
      // the shadow copy, so the host and guest do not diverge.
      const auto cmd_checkpoint = dev->cmd.checkpoint();
      const WddmAllocListCheckpoint alloc_checkpoint(dev);
      TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
      if (!dev->wddm_submit_allocation_list_oom) {
        auto* dirty =
            dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (dirty) {
          dirty->resource_handle = res->handle;
          dirty->reserved0 = 0;
          dirty->offset_bytes = res->mapped_offset;
          dirty->size_bytes = res->mapped_size;
          dirty_emitted_on_unmap = true;
        }
      }
      if (!dirty_emitted_on_unmap) {
        dirty_failed_on_unmap = true;
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();

        // Best-effort rollback: restore the allocation bytes from the existing
        // shadow copy. This keeps guest memory consistent with the host-visible
        // contents even if we cannot notify the host of the CPU write.
        if (!res->storage.empty()) {
          const uint64_t off = res->mapped_offset;
          const uint64_t size = res->mapped_size;
          if (off <= static_cast<uint64_t>(SIZE_MAX) && off <= res->storage.size()) {
            const size_t off_sz = static_cast<size_t>(off);
            const size_t remaining = res->storage.size() - off_sz;
            const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
            if (copy_bytes) {
              uint8_t* dst = static_cast<uint8_t*>(res->mapped_wddm_ptr) + off_sz;
              const uint8_t* src = res->storage.data() + off_sz;
              std::memcpy(dst, src, copy_bytes);
            }
          }
        }

        SetError(dev, E_OUTOFMEMORY);
      }
    }

    if (is_write && !res->storage.empty()) {
      if (dirty_failed_on_unmap && res->backing_alloc_id != 0) {
        // We restored the allocation from the pre-map shadow copy above; keep
        // the shadow copy unchanged.
        goto UnlockMappedAllocation;
      }
      const uint8_t* src_base = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
      const uint64_t off = res->mapped_offset;
      const uint64_t size = res->mapped_size;

      if (off <= res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(off);
        const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
        if (copy_bytes) {
          if (res->kind == ResourceKind::Texture2D) {
            // Texture2D allocations are packed linearly by subresource. We lock
            // SubresourceIndex=0 and apply `mapped_offset` manually.
            if (res->mapped_subresource >= res->tex2d_subresources.size()) {
              // Fallback: best-effort linear copy.
              std::memcpy(res->storage.data() + static_cast<size_t>(off),
                          src_base + static_cast<size_t>(off),
                          copy_bytes);
            } else {
              const Texture2DSubresourceLayout& sub_layout = res->tex2d_subresources[res->mapped_subresource];
              const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
              const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, sub_layout.width);
              const uint32_t rows = sub_layout.rows_in_layout;
              // Only mip0 may report a pitch via LockCb; for other subresources we
              // rely on our packed layout pitches.
              const uint32_t src_pitch =
                  (sub_layout.mip_level == 0 && res->mapped_wddm_pitch) ? res->mapped_wddm_pitch : sub_layout.row_pitch_bytes;
              const uint32_t dst_pitch = sub_layout.row_pitch_bytes;

              const uint64_t src_needed =
                  (rows == 0) ? 0 : (static_cast<uint64_t>(rows - 1) * static_cast<uint64_t>(src_pitch) + row_bytes);
              const uint64_t dst_needed =
                  (rows == 0) ? 0 : (static_cast<uint64_t>(rows - 1) * static_cast<uint64_t>(dst_pitch) + row_bytes);

              if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 &&
                  src_pitch >= row_bytes && dst_pitch >= row_bytes &&
                  dst_needed <= static_cast<uint64_t>(remaining) &&
                  (res->mapped_wddm_slice_pitch == 0 || src_needed <= res->mapped_wddm_slice_pitch)) {
                const uint8_t* src = src_base + static_cast<size_t>(off);
                uint8_t* dst = res->storage.data() + static_cast<size_t>(off);
                for (uint32_t y = 0; y < rows; y++) {
                  uint8_t* dst_row = dst + static_cast<size_t>(y) * dst_pitch;
                  const uint8_t* src_row = src + static_cast<size_t>(y) * src_pitch;
                  std::memcpy(dst_row, src_row, row_bytes);
                  if (dst_pitch > row_bytes) {
                    std::memset(dst_row + row_bytes, 0, dst_pitch - row_bytes);
                  }
                }
              } else {
                // Fallback: best-effort linear copy.
                std::memcpy(res->storage.data() + static_cast<size_t>(off),
                            src_base + static_cast<size_t>(off),
                            copy_bytes);
              }
            }
          } else {
            std::memcpy(res->storage.data() + static_cast<size_t>(off), src_base + static_cast<size_t>(off), copy_bytes);
          }
        }
      }
    }

  UnlockMappedAllocation:
    const auto* cb = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
    const auto* cb_device = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
    if (cb && cb->pfnUnlockCb) {
      D3DDDICB_UNLOCK unlock = {};
      unlock.hAllocation = static_cast<D3DKMT_HANDLE>(res->mapped_wddm_allocation);
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock.SubResourceIndex = 0;
      }
      const HRESULT unlock_hr =
          CallCbMaybeHandle(cb->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock);
      if (FAILED(unlock_hr)) {
        SetError(dev, unlock_hr);
      }
    } else if (cb_device && cb_device->pfnUnlockCb) {
      D3DDDICB_UNLOCK unlock = {};
      unlock.hAllocation = static_cast<D3DKMT_HANDLE>(res->mapped_wddm_allocation);
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock.SubResourceIndex = 0;
      }
      const HRESULT unlock_hr = CallCbMaybeHandle(cb_device->pfnUnlockCb,
                                                  MakeRtDeviceHandle(dev),
                                                  MakeRtDeviceHandle10(dev),
                                                  &unlock);
      if (FAILED(unlock_hr)) {
        SetError(dev, unlock_hr);
      }
    }
  }

  if (is_write && res->mapped_size != 0) {
    if (res->backing_alloc_id != 0) {
      // For guest-backed resources, only report the mapped subresource region as
      // dirty. Do not expand to LockCb's SlicePitch, which describes mip0 and
      // can overlap other subresources in our packed layout.
      //
      // If we already emitted (or failed to emit) a dirty range while the
      // allocation was still mapped, do not emit another one here.
      if (!dirty_emitted_on_unmap && !dirty_failed_on_unmap) {
        EmitDirtyRangeLocked(dev, res, res->mapped_offset, res->mapped_size);
      }
    } else if (!res->storage.empty()) {
      (void)EmitUploadLocked(dev, res, res->mapped_offset, res->mapped_size);
    }
  }

  res->mapped = false;
  res->mapped_map_type = 0;
  res->mapped_map_flags = 0;
  res->mapped_subresource = 0;
  res->mapped_offset = 0;
  res->mapped_size = 0;
  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_allocation = 0;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;
  return true;
}
// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

HRESULT AEROGPU_APIENTRY GetCaps11(D3D10DDI_HADAPTER hAdapter, const D3D11DDIARG_GETCAPS* pGetCaps) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!pGetCaps) {
    return E_INVALIDARG;
  }
  if (!pGetCaps->pData || pGetCaps->DataSize == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    return S_OK;
  }

  void* data = pGetCaps->pData;
  const UINT size = pGetCaps->DataSize;
  const Adapter* adapter = hAdapter.pDrvPrivate ? FromHandle<D3D10DDI_HADAPTER, Adapter>(hAdapter) : nullptr;

#if defined(AEROGPU_D3D10_11_CAPS_LOG)
  // Emit caps queries unconditionally when AEROGPU_D3D10_11_CAPS_LOG is defined;
  // the runtime-controlled AEROGPU_D3D10_11_LOG gate is often disabled in retail
  // builds during early bring-up.
  char buf[128] = {};
  snprintf(buf,
           sizeof(buf),
           "aerogpu-d3d11: GetCaps11 type=%u size=%u\n",
           (unsigned)static_cast<uint32_t>(pGetCaps->Type),
           (unsigned)size);
  OutputDebugStringA(buf);
#endif

  auto zero_out = [&] { std::memset(data, 0, size); };

  auto log_unknown_type_once = [&](uint32_t unknown_type) {
    if (!aerogpu_d3d10_11_log_enabled()) {
      return;
    }

    // Track a common range of D3D11DDICAPS_TYPE values without any heap
    // allocations (UMD-friendly).
    static std::atomic<uint64_t> logged[4] = {}; // 256 bits.
    if (unknown_type < 256) {
      const uint32_t idx = unknown_type / 64;
      const uint64_t bit = 1ull << (unknown_type % 64);
      const uint64_t prev = logged[idx].fetch_or(bit, std::memory_order_relaxed);
      if ((prev & bit) != 0) {
        return;
      }
    }

    AEROGPU_D3D10_11_LOG("GetCaps11 unknown type=%u (size=%u) -> zero-fill + S_OK",
                         (unsigned)unknown_type,
                         (unsigned)size);
  };

  switch (pGetCaps->Type) {
    case D3D11DDICAPS_TYPE_FEATURE_LEVELS: {
      zero_out();
      static const D3D_FEATURE_LEVEL kLevels[] = {D3D_FEATURE_LEVEL_10_0};

      // Win7 D3D11 runtime generally expects "count + inline list", but some
      // header/runtime combinations treat this as a {count, pointer} struct.
      // Populate both layouts when we have enough space so we avoid mismatched
      // interpretation (in particular on 64-bit where the pointer lives at a
      // different offset than the inline list element).
      struct FeatureLevelsCapsPtr {
        UINT NumFeatureLevels;
        const D3D_FEATURE_LEVEL* pFeatureLevels;
      };

      constexpr size_t kInlineLevelsOffset = sizeof(UINT);
      constexpr size_t kPtrOffset = offsetof(FeatureLevelsCapsPtr, pFeatureLevels);

      // On 32-bit builds the pointer field overlaps the first inline element
      // (both start at offset 4), so we cannot populate both layouts. Prefer
      // the {count, pointer} layout to avoid returning a bogus pointer value
      // (e.g. 0xA000) that could crash the runtime if it expects the pointer
      // interpretation.
      if (kPtrOffset == kInlineLevelsOffset && size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      if (size >= sizeof(UINT) + sizeof(D3D_FEATURE_LEVEL)) {
        auto* out_count = reinterpret_cast<UINT*>(data);
        *out_count = 1;
        auto* out_levels = reinterpret_cast<D3D_FEATURE_LEVEL*>(out_count + 1);
        out_levels[0] = kLevels[0];
        if (size >= sizeof(FeatureLevelsCapsPtr) && kPtrOffset >= kInlineLevelsOffset + sizeof(D3D_FEATURE_LEVEL)) {
          auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
          out_ptr->pFeatureLevels = kLevels;
        }
        return S_OK;
      }

      if (size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      if (size >= sizeof(D3D_FEATURE_LEVEL)) {
        *reinterpret_cast<D3D_FEATURE_LEVEL*>(data) = kLevels[0];
        return S_OK;
      }

      return E_INVALIDARG;
    }

    // D3D11_FEATURE_* queries are routed through GetCaps on Win7. For now we
    // report everything as unsupported (all-zero output structures).
    case D3D11DDICAPS_TYPE_THREADING:
    case D3D11DDICAPS_TYPE_DOUBLES:
    case D3D11DDICAPS_TYPE_D3D11_OPTIONS:
    case D3D11DDICAPS_TYPE_ARCHITECTURE_INFO:
    case D3D11DDICAPS_TYPE_D3D9_OPTIONS:
      zero_out();
      return S_OK;

    case D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS: {
      // D3D11 feature data that gates compute shaders at feature level 10.x.
      // The public struct is `D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS` and
      // currently consists of a single BOOL field.
      zero_out();
      if (size >= sizeof(BOOL)) {
        *reinterpret_cast<BOOL*>(data) = TRUE;
      } else if (size >= sizeof(UINT)) {
        *reinterpret_cast<UINT*>(data) = 1u;
      }
      return S_OK;
    }

    case D3D11DDICAPS_TYPE_SHADER: {
      // Shader model caps for FL10_0: VS/GS/PS/CS are SM4.0; HS/DS are unsupported.
      //
      // The WDK output struct layout has been stable in practice: it begins with
      // six UINT "version tokens" matching the D3D shader bytecode token format:
      //   (program_type << 16) | (major << 4) | minor
      //
      // Be careful about overrunning DataSize: only write fields that fit.
      zero_out();

      auto write_u32 = [&](size_t offset, UINT value) {
        if (size < offset + sizeof(UINT)) {
          return;
        }
        *reinterpret_cast<UINT*>(reinterpret_cast<uint8_t*>(data) + offset) = value;
      };

      write_u32(0, DxbcShaderVersionToken(kD3DDxbcProgramTypePixel, 4, 0));
      write_u32(sizeof(UINT), DxbcShaderVersionToken(kD3DDxbcProgramTypeVertex, 4, 0));
      write_u32(sizeof(UINT) * 2, DxbcShaderVersionToken(kD3DDxbcProgramTypeGeometry, 4, 0));
      write_u32(sizeof(UINT) * 5, DxbcShaderVersionToken(kD3DDxbcProgramTypeCompute, 4, 0));
      return S_OK;
    }

    case D3D11DDICAPS_TYPE_FORMAT: {
      if (size < sizeof(DXGI_FORMAT)) {
        return E_INVALIDARG;
      }

      const auto* bytes = reinterpret_cast<const uint8_t*>(data);
      const DXGI_FORMAT format = *reinterpret_cast<const DXGI_FORMAT*>(bytes);

      zero_out();
      *reinterpret_cast<DXGI_FORMAT*>(data) = format;

      const UINT support = static_cast<UINT>(D3D11FormatSupportFlags(adapter, static_cast<uint32_t>(format)));

      auto* out_bytes = reinterpret_cast<uint8_t*>(data);
      if (size >= sizeof(DXGI_FORMAT) + sizeof(UINT)) {
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT)) = support;
      }
      if (size >= sizeof(DXGI_FORMAT) + sizeof(UINT) * 2) {
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT) + sizeof(UINT)) = 0;
      }
      return S_OK;
    }

    // D3D11_FEATURE_FORMAT_SUPPORT2 is routed through GetCaps as well. The
    // corresponding output struct is:
    //   { DXGI_FORMAT InFormat; UINT OutFormatSupport2; }
    //
    // We currently do not advertise any FormatSupport2 bits.
    case static_cast<D3D11DDICAPS_TYPE>(kD3D11DdiCapsTypeFormatSupport2): { // FORMAT_SUPPORT2
      if (size < sizeof(DXGI_FORMAT) + sizeof(UINT)) {
        return E_INVALIDARG;
      }

      const DXGI_FORMAT format = *reinterpret_cast<const DXGI_FORMAT*>(data);
      zero_out();
      *reinterpret_cast<DXGI_FORMAT*>(data) = format;

      auto* out_bytes = reinterpret_cast<uint8_t*>(data);
      *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT)) = 0;
      return S_OK;
    }

    case D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS: {
      if (size < sizeof(D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS)) {
        return E_INVALIDARG;
      }

      const auto in = *reinterpret_cast<const D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS*>(data);
      zero_out();
      auto* out = reinterpret_cast<D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS*>(data);
      out->Format = in.Format;
      out->SampleCount = in.SampleCount;
      const bool supported_format =
          AerogpuSupportsMultisampleQualityLevels(adapter, static_cast<uint32_t>(in.Format));
      out->NumQualityLevels = (in.SampleCount == 1 && supported_format) ? 1u : 0u;
      return S_OK;
    }

    default:
      // Unknown caps are treated as unsupported. Zero-fill so the runtime won't
      // read garbage, but log the type once for bring-up.
      log_unknown_type_once(static_cast<uint32_t>(pGetCaps->Type));
      zero_out();
      return S_OK;
  }
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize11(D3D10DDI_HADAPTER, const D3D11DDIARG_CREATEDEVICE*) {
  // If the runtime exposes a separate CalcPrivateDeviceContextSize hook, it
  // will allocate that memory separately.
  if constexpr (has_member_pfnCalcPrivateDeviceContextSize<D3D11DDI_ADAPTERFUNCS>::value) {
    return sizeof(Device);
  }
  return sizeof(Device) + sizeof(AeroGpuDeviceContext);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceContextSize11(D3D10DDI_HADAPTER, const D3D11DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuDeviceContext);
}

void AEROGPU_APIENTRY CloseAdapter11(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, Adapter>(hAdapter);
  DestroyKmtAdapterHandle(adapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Device DDIs (object creation)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice11(D3D11DDI_HDEVICE hDevice) {
  void* device_mem = hDevice.pDrvPrivate;
  if (!HasLiveCookie(device_mem, kDeviceDestroyLiveCookie)) {
    return;
  }
  uint32_t cookie = 0;
  std::memcpy(device_mem, &cookie, sizeof(cookie));

  auto* dev = reinterpret_cast<Device*>(device_mem);
  // The runtime may retain the immediate context object past DestroyDevice on
  // some interface versions. Null out the back-pointer so context entrypoints
  // do not dereference a freed Device (and so DeviceFromContext can short-circuit
  // without touching Device memory).
  if (dev->immediate_context) {
    auto* ctx = reinterpret_cast<AeroGpuDeviceContext*>(dev->immediate_context);
    ctx->dev = nullptr;
    dev->immediate_context = nullptr;
  }
  DestroyWddmContext(dev);
  delete const_cast<D3D11DDI_DEVICECALLBACKS*>(reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks));
  dev->runtime_callbacks = nullptr;
  dev->~Device();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERESOURCE*) {
  return sizeof(Resource);
}

HRESULT AEROGPU_APIENTRY CreateResource11(D3D11DDI_HDEVICE hDevice,
                                          const D3D11DDIARG_CREATERESOURCE* pDesc,
                                          D3D11DDI_HRESOURCE hResource,
                                          D3D11DDI_HRTRESOURCE hRTResource) {
  if (!hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the resource object so DestroyResource11 is safe even when
  // CreateResource11 fails early.
  auto* res = new (hResource.pDrvPrivate) Resource();

  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(res);
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  uint32_t sample_count = 0;
  uint32_t sample_quality = 0;
  __if_exists(D3D11DDIARG_CREATERESOURCE::SampleDesc) {
    sample_count = static_cast<uint32_t>(pDesc->SampleDesc.Count);
    sample_quality = static_cast<uint32_t>(pDesc->SampleDesc.Quality);
  }

  uint32_t cpu_access = 0;
  __if_exists(D3D11DDIARG_CREATERESOURCE::CPUAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc->CPUAccessFlags);
  }
  __if_exists(D3D11DDIARG_CREATERESOURCE::CpuAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc->CpuAccessFlags);
  }

  uint64_t resource_flags_bits = 0;
  uint32_t resource_flags_size = 0;
  __if_exists(D3D11DDIARG_CREATERESOURCE::ResourceFlags) {
    resource_flags_size = static_cast<uint32_t>(sizeof(pDesc->ResourceFlags));
    const size_t n = std::min(sizeof(resource_flags_bits), sizeof(pDesc->ResourceFlags));
    std::memcpy(&resource_flags_bits, &pDesc->ResourceFlags, n);
  }

  uint32_t primary = 0;
  __if_exists(D3D11DDIARG_CREATERESOURCE::pPrimaryDesc) {
    primary = (pDesc->pPrimaryDesc != nullptr) ? 1u : 0u;
  }

  const void* init_ptr = nullptr;
  if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
    init_ptr = pDesc->pInitialDataUP;
  } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
    init_ptr = pDesc->pInitialData;
  }

  uint32_t num_allocations = 0;
  const void* allocation_info = nullptr;
  const void* primary_desc = nullptr;
  __if_exists(D3D11DDIARG_CREATERESOURCE::NumAllocations) {
    num_allocations = static_cast<uint32_t>(pDesc->NumAllocations);
  }
  __if_exists(D3D11DDIARG_CREATERESOURCE::pAllocationInfo) {
    allocation_info = pDesc->pAllocationInfo;
  }
  __if_exists(D3D11DDIARG_CREATERESOURCE::pPrimaryDesc) {
    primary_desc = pDesc->pPrimaryDesc;
  }

  const uint32_t primary = primary_desc ? 1u : 0u;

  AEROGPU_D3D10_11_LOG(
      "trace_resources: D3D11 CreateResource dim=%u bind=0x%08X usage=%u cpu=0x%08X misc=0x%08X fmt=%u "
      "byteWidth=%u w=%u h=%u mips=%u array=%u sample=(%u,%u) rflags=0x%llX rflags_size=%u primary=%u init=%p "
      "num_alloc=%u alloc_info=%p primary_desc=%p",
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->ResourceDimension)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->BindFlags)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->Usage)),
      static_cast<unsigned>(cpu_access),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->MiscFlags)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->Format)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->ByteWidth)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->Width)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->Height)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->MipLevels)),
      static_cast<unsigned>(static_cast<uint32_t>(pDesc->ArraySize)),
      static_cast<unsigned>(sample_count),
      static_cast<unsigned>(sample_quality),
      static_cast<unsigned long long>(resource_flags_bits),
      static_cast<unsigned>(resource_flags_size),
      static_cast<unsigned>(primary),
      init_ptr,
      static_cast<unsigned>(num_allocations),
      allocation_info,
      primary_desc);
#endif

  auto* callbacks = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  if (!dev->runtime_device || !callbacks || !callbacks->pfnAllocateCb || !callbacks->pfnDeallocateCb) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  __if_exists(D3D11DDIARG_CREATERESOURCE::SampleDesc) {
    if (pDesc->SampleDesc.Count != 1 || pDesc->SampleDesc.Quality != 0) {
      return E_NOTIMPL;
    }
  }

  res->handle = AllocateGlobalHandle(dev->adapter);
  res->bind_flags = static_cast<uint32_t>(pDesc->BindFlags);
  res->misc_flags = static_cast<uint32_t>(pDesc->MiscFlags);
  res->usage = static_cast<uint32_t>(pDesc->Usage);
  uint32_t cpu_access_flags = 0;
  __if_exists(D3D11DDIARG_CREATERESOURCE::CPUAccessFlags) {
    cpu_access_flags = static_cast<uint32_t>(pDesc->CPUAccessFlags);
  }
  __if_exists(D3D11DDIARG_CREATERESOURCE::CpuAccessFlags) {
    cpu_access_flags = static_cast<uint32_t>(pDesc->CpuAccessFlags);
  }
  res->cpu_access_flags = cpu_access_flags;

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);
  bool is_primary = false;
  __if_exists(D3D11DDIARG_CREATERESOURCE::pPrimaryDesc) {
    is_primary = (pDesc->pPrimaryDesc != nullptr);
  }

  const auto deallocate_if_needed = [&]() {
    if (res->wddm.km_resource_handle == 0 && res->wddm.km_allocation_handles.empty()) {
      return;
    }

    constexpr size_t kInlineKmtAllocs = 16;
    std::array<D3DKMT_HANDLE, kInlineKmtAllocs> km_allocs_stack{};
    std::vector<D3DKMT_HANDLE> km_allocs_heap;
    D3DKMT_HANDLE* km_allocs = nullptr;
    UINT km_alloc_count = 0;

    const size_t handle_count = res->wddm.km_allocation_handles.size();
    if (handle_count != 0) {
      if (handle_count <= km_allocs_stack.size()) {
        for (size_t i = 0; i < handle_count; ++i) {
          km_allocs_stack[i] = static_cast<D3DKMT_HANDLE>(res->wddm.km_allocation_handles[i]);
        }
        km_allocs = km_allocs_stack.data();
        km_alloc_count = static_cast<UINT>(handle_count);
      } else {
        try {
          km_allocs_heap.reserve(handle_count);
          for (uint64_t h : res->wddm.km_allocation_handles) {
            km_allocs_heap.push_back(static_cast<D3DKMT_HANDLE>(h));
          }
          km_allocs = km_allocs_heap.data();
          km_alloc_count = static_cast<UINT>(km_allocs_heap.size());
        } catch (...) {
          SetError(dev, E_OUTOFMEMORY);
          km_allocs = nullptr;
          km_alloc_count = 0;
        }
      }
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = km_alloc_count;
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_alloc_count ? km_allocs : nullptr;
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_alloc_count ? km_allocs : nullptr;
    }
    CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &dealloc);

    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  };

  const auto allocate_one = [&](uint64_t size_bytes,
                                bool cpu_visible,
                                bool is_rt,
                                bool is_ds,
                                bool is_shared,
                                bool want_primary,
                                uint32_t pitch_bytes,
                                aerogpu_wddm_alloc_priv_v2* out_priv) -> HRESULT {
    if (!pDesc->pAllocationInfo) {
      return E_INVALIDARG;
    }
    if (pDesc->NumAllocations < 1) {
      return E_INVALIDARG;
    }
    if (pDesc->NumAllocations != 1) {
      return E_NOTIMPL;
    }

    if (size_bytes == 0 || size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      return E_OUTOFMEMORY;
    }

    auto* alloc_info = pDesc->pAllocationInfo;
    std::memset(alloc_info, 0, sizeof(*alloc_info));
    alloc_info[0].Size = static_cast<SIZE_T>(size_bytes);
    alloc_info[0].Alignment = 0;
    alloc_info[0].Flags.Value = 0;
    alloc_info[0].Flags.CpuVisible = cpu_visible ? 1u : 0u;
    using AllocFlagsT = decltype(alloc_info[0].Flags);
    __if_exists(AllocFlagsT::Primary) {
      alloc_info[0].Flags.Primary = want_primary ? 1u : 0u;
    }
    alloc_info[0].SupportedReadSegmentSet = 1;
    alloc_info[0].SupportedWriteSegmentSet = 1;

    uint32_t alloc_id = 0;
    do {
      alloc_id = AllocateGlobalHandle(dev->adapter) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
    } while (!alloc_id);

    aerogpu_wddm_alloc_priv_v2 priv = {};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
    priv.alloc_id = alloc_id;
    priv.flags = 0;
    if (is_shared) {
      priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED;
    }
    if (cpu_visible) {
      priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE;
    }
    if (res->usage == kD3D11UsageStaging) {
      priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING;
    }

    // The Win7 KMD owns share_token generation; provide 0 as a placeholder.
    priv.share_token = 0;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(size_bytes);
    priv.reserved0 = static_cast<aerogpu_wddm_u64>(pitch_bytes);
    priv.kind = (res->kind == ResourceKind::Buffer)
                    ? AEROGPU_WDDM_ALLOC_KIND_BUFFER
                    : (res->kind == ResourceKind::Texture2D ? AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D
                                                            : AEROGPU_WDDM_ALLOC_KIND_UNKNOWN);
    if (res->kind == ResourceKind::Texture2D) {
      priv.width = res->width;
      priv.height = res->height;
      priv.format = res->dxgi_format;
      priv.row_pitch_bytes = res->row_pitch_bytes;
    }
    priv.reserved1 = 0;

    alloc_info[0].pPrivateDriverData = &priv;
    alloc_info[0].PrivateDriverDataSize = sizeof(priv);

    D3DDDICB_ALLOCATE alloc = {};
    __if_exists(D3DDDICB_ALLOCATE::hContext) {
      alloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
    }
    alloc.hResource = hRTResource;
    alloc.NumAllocations = 1;
    alloc.pAllocationInfo = alloc_info;
    alloc.Flags.Value = 0;
    alloc.Flags.CreateResource = 1;
    if (is_shared) {
      alloc.Flags.CreateShared = 1;
    }
    __if_exists(decltype(alloc.Flags)::Primary) {
      alloc.Flags.Primary = want_primary ? 1u : 0u;
    }
    alloc.ResourceFlags.Value = 0;
    alloc.ResourceFlags.RenderTarget = is_rt ? 1u : 0u;
    alloc.ResourceFlags.ZBuffer = is_ds ? 1u : 0u;

    const HRESULT hr = CallCbMaybeHandle(callbacks->pfnAllocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &alloc);
    if (FAILED(hr)) {
      return hr;
    }

    // Consume the (potentially updated) allocation private driver data. For
    // shared allocations, the Win7 KMD fills a stable non-zero share_token.
    aerogpu_wddm_alloc_priv_v2 priv_out{};
    const bool have_priv_out = ConsumeWddmAllocPrivV2(alloc_info[0].pPrivateDriverData,
                                                      static_cast<UINT>(alloc_info[0].PrivateDriverDataSize),
                                                      &priv_out);
    if (out_priv) {
      *out_priv = priv_out;
    }
    if (have_priv_out && priv_out.alloc_id != 0) {
      alloc_id = priv_out.alloc_id;
    }
    uint64_t share_token = 0;
    bool share_token_ok = true;
    if (is_shared) {
      share_token_ok = have_priv_out &&
                       ((priv_out.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED) != 0) &&
                       (priv_out.share_token != 0);
      if (share_token_ok) {
        share_token = priv_out.share_token;
      } else {
        if (!have_priv_out) {
          static std::once_flag log_once;
          std::call_once(log_once, [] {
            AEROGPU_D3D10_11_LOG("CreateResource11: shared allocation missing/invalid private driver data");
          });
        } else {
          static std::once_flag log_once;
          std::call_once(log_once, [] {
            AEROGPU_D3D10_11_LOG("CreateResource11: shared allocation missing share_token in returned private data");
          });
        }
      }
    }

    uint64_t km_resource = 0;
    __if_exists(D3DDDICB_ALLOCATE::hKMResource) {
      km_resource = static_cast<uint64_t>(alloc.hKMResource);
    }

    uint64_t km_alloc = 0;
    using AllocationInfoT = std::remove_pointer_t<decltype(pDesc->pAllocationInfo)>;
    __if_exists(AllocationInfoT::hKMAllocation) {
      km_alloc = static_cast<uint64_t>(alloc_info[0].hKMAllocation);
    }
    __if_not_exists(AllocationInfoT::hKMAllocation) {
      __if_exists(AllocationInfoT::hAllocation) {
        km_alloc = static_cast<uint64_t>(alloc_info[0].hAllocation);
      }
    }

    if (!km_resource || !km_alloc) {
      D3DDDICB_DEALLOCATE dealloc = {};
      D3DKMT_HANDLE h = static_cast<D3DKMT_HANDLE>(km_alloc);
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
      }
      __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
        dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(km_resource);
      }
      __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
        dealloc.NumAllocations = km_alloc ? 1u : 0u;
      }
      __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
        dealloc.HandleList = km_alloc ? &h : nullptr;
      }
      __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
        dealloc.phAllocations = km_alloc ? &h : nullptr;
      }
      CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &dealloc);
      return E_FAIL;
    }

    if (is_shared && !share_token_ok) {
      // If the KMD does not return a stable token, shared surface interop cannot
      // work across processes; fail cleanly. Free the allocation handles that
      // were created by AllocateCb before returning an error.
      D3DDDICB_DEALLOCATE dealloc = {};
      D3DKMT_HANDLE h = static_cast<D3DKMT_HANDLE>(km_alloc);
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
      }
      __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
        dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(km_resource);
      }
      __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
        dealloc.NumAllocations = km_alloc ? 1u : 0u;
      }
      __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
        dealloc.HandleList = km_alloc ? &h : nullptr;
      }
      __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
        dealloc.phAllocations = km_alloc ? &h : nullptr;
      }
      CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &dealloc);
      return E_FAIL;
    }

    res->backing_alloc_id = alloc_id;
    res->backing_offset_bytes = 0;
    res->wddm.km_resource_handle = km_resource;
    res->share_token = is_shared ? share_token : 0;
    res->is_shared = is_shared;
    res->is_shared_alias = false;
    res->wddm.km_allocation_handles.clear();
    try {
      res->wddm.km_allocation_handles.push_back(km_alloc);
    } catch (...) {
      // Ensure we don't leak the just-allocated KM resource/allocation if the UMD
      // cannot record its handles due to OOM.
      D3DDDICB_DEALLOCATE dealloc = {};
      D3DKMT_HANDLE h = static_cast<D3DKMT_HANDLE>(km_alloc);
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
      }
      __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
        dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(km_resource);
      }
      __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
        dealloc.NumAllocations = km_alloc ? 1u : 0u;
      }
      __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
        dealloc.HandleList = km_alloc ? &h : nullptr;
      }
      __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
        dealloc.phAllocations = km_alloc ? &h : nullptr;
      }
      (void)CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &dealloc);
      res->wddm.km_allocation_handles.clear();
      res->wddm.km_resource_handle = 0;
      res->wddm_allocation_handle = 0;
      return E_OUTOFMEMORY;
    }
    uint32_t runtime_alloc = 0;
    if constexpr (has_member_hAllocation<AllocationInfoT>::value) {
      runtime_alloc = static_cast<uint32_t>(alloc_info[0].hAllocation);
    }
    // Prefer the runtime allocation handle (`hAllocation`) for LockCb/UnlockCb,
    // but fall back to the only handle we have if the WDK revision does not
    // expose it.
    res->wddm_allocation_handle = runtime_alloc ? runtime_alloc : static_cast<uint32_t>(km_alloc);
    return S_OK;
  };

  const auto copy_initial_bytes_to_storage = [&](const void* src, size_t bytes) -> HRESULT {
    if (!src) {
      return E_INVALIDARG;
    }
    if (bytes == 0) {
      return S_OK;
    }
    if (res->storage.empty()) {
      return E_FAIL;
    }
    if (bytes > res->storage.size()) {
      return E_INVALIDARG;
    }
    std::fill(res->storage.begin(), res->storage.end(), 0);
    std::memcpy(res->storage.data(), src, bytes);
    return S_OK;
  };

  const auto copy_initial_tex2d_subresources_to_storage = [&](auto init_data) -> HRESULT {
    if (!init_data) {
      return S_OK;
    }
    if (res->kind != ResourceKind::Texture2D) {
      return E_FAIL;
    }
    if (res->storage.empty() || res->row_pitch_bytes == 0) {
      return E_FAIL;
    }
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_NOTIMPL;
    }

    const uint64_t subresource_count_u64 =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    if (subresource_count_u64 == 0 || subresource_count_u64 > static_cast<uint64_t>(UINT32_MAX)) {
      return E_INVALIDARG;
    }
    const uint32_t subresource_count = static_cast<uint32_t>(subresource_count_u64);
    if (subresource_count > static_cast<uint32_t>(res->tex2d_subresources.size())) {
      return E_FAIL;
    }

    using ElemT = std::remove_pointer_t<decltype(init_data)>;
    static_assert(!std::is_void_v<ElemT>, "Expected typed init_data pointer");

    // Ensure padding is deterministic even if the caller supplies only tight rows.
    std::fill(res->storage.begin(), res->storage.end(), 0);

    for (uint32_t sub = 0; sub < subresource_count; ++sub) {
      const ElemT& init = init_data[sub];

      const void* sys = nullptr;
      if constexpr (has_member_pSysMem<ElemT>::value) {
        sys = init.pSysMem;
      } else if constexpr (has_member_pSysMemUP<ElemT>::value) {
        sys = init.pSysMemUP;
      } else {
        return E_NOTIMPL;
      }
      if (!sys) {
        return E_INVALIDARG;
      }

      uint32_t pitch = 0;
      if constexpr (has_member_SysMemPitch<ElemT>::value) {
        pitch = static_cast<uint32_t>(init.SysMemPitch);
      } else if constexpr (has_member_RowPitch<ElemT>::value) {
        pitch = static_cast<uint32_t>(init.RowPitch);
      } else if constexpr (has_member_SrcPitch<ElemT>::value) {
        pitch = static_cast<uint32_t>(init.SrcPitch);
      } else {
        return E_NOTIMPL;
      }

      const Texture2DSubresourceLayout& dst_layout = res->tex2d_subresources[sub];
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst_layout.width);
      if (row_bytes == 0 || dst_layout.rows_in_layout == 0) {
        return E_INVALIDARG;
      }
      if (dst_layout.row_pitch_bytes < row_bytes) {
        return E_INVALIDARG;
      }

      const uint32_t src_pitch = pitch ? pitch : row_bytes;
      if (src_pitch < row_bytes) {
        return E_INVALIDARG;
      }

      const uint8_t* src_base = static_cast<const uint8_t*>(sys);
      const size_t dst_base = static_cast<size_t>(dst_layout.offset_bytes);
      if (dst_base > res->storage.size()) {
        return E_INVALIDARG;
      }

      for (uint32_t y = 0; y < dst_layout.rows_in_layout; ++y) {
        const size_t src_off = static_cast<size_t>(y) * static_cast<size_t>(src_pitch);
        const size_t dst_off =
            dst_base + static_cast<size_t>(y) * static_cast<size_t>(dst_layout.row_pitch_bytes);
        if (dst_off + row_bytes > res->storage.size()) {
          return E_INVALIDARG;
        }
        std::memcpy(res->storage.data() + dst_off, src_base + src_off, row_bytes);
        if (dst_layout.row_pitch_bytes > row_bytes) {
          std::memset(res->storage.data() + dst_off + row_bytes, 0, dst_layout.row_pitch_bytes - row_bytes);
        }
      }
    }

    return S_OK;
  };

  const auto maybe_copy_initial_to_storage = [&](auto init_ptr) -> HRESULT {
    if (!init_ptr) {
      return S_OK;
    }

    using ElemT = std::remove_pointer_t<decltype(init_ptr)>;
    if constexpr (std::is_void_v<ElemT>) {
      if (res->kind == ResourceKind::Buffer) {
        return copy_initial_bytes_to_storage(init_ptr, static_cast<size_t>(res->size_bytes));
      }
      if (res->kind == ResourceKind::Texture2D) {
        return copy_initial_bytes_to_storage(init_ptr, res->storage.size());
      }
      return E_NOTIMPL;
    } else {
      if constexpr (!(has_member_pSysMem<ElemT>::value || has_member_pSysMemUP<ElemT>::value)) {
        return E_NOTIMPL;
      }

      if (res->kind == ResourceKind::Buffer) {
        const void* sys = nullptr;
        if constexpr (has_member_pSysMem<ElemT>::value) {
          sys = init_ptr[0].pSysMem;
        } else if constexpr (has_member_pSysMemUP<ElemT>::value) {
          sys = init_ptr[0].pSysMemUP;
        }
        if (!sys) {
          return E_INVALIDARG;
        }
        return copy_initial_bytes_to_storage(sys, static_cast<size_t>(res->size_bytes));
      }
      if (res->kind == ResourceKind::Texture2D) {
        return copy_initial_tex2d_subresources_to_storage(init_ptr);
      }
      return E_NOTIMPL;
    }
  };

  if (dim == D3D10DDIRESOURCE_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(pDesc->ByteWidth);
    res->structure_stride_bytes = 0;
    __if_exists(D3D11DDIARG_CREATERESOURCE::StructureByteStride) {
      res->structure_stride_bytes = static_cast<uint32_t>(pDesc->StructureByteStride);
    }
    const uint64_t padded_size_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    if (padded_size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    const uint64_t alloc_size = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 256);
    const bool is_staging = (res->usage == kD3D11UsageStaging);
    bool cpu_visible = is_staging || (res->cpu_access_flags != 0);
    const bool is_rt = (res->bind_flags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D11BindDepthStencil) != 0;
    bool is_shared = false;
    if (res->misc_flags & kD3D11ResourceMiscShared) {
      is_shared = true;
    }
    if (res->misc_flags & kD3D11ResourceMiscSharedKeyedMutex) {
      is_shared = true;
    }
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;
    res->is_shared = is_shared;
    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0, nullptr);
    if (FAILED(hr)) {
      SetError(dev, hr);
      ResetObject(res);
      return hr;
    }
    try {
      res->storage.resize(static_cast<size_t>(padded_size_bytes));
    } catch (...) {
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }

    if (res->usage == kD3D11UsageDynamic && !is_shared) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u alloc_id=%u size=%llu",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->backing_alloc_id),
                         static_cast<unsigned long long>(res->size_bytes));
#endif

    bool has_initial_data = false;
    HRESULT init_hr = S_OK;
    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      has_initial_data = (pDesc->pInitialDataUP != nullptr);
      init_hr = maybe_copy_initial_to_storage(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      has_initial_data = (pDesc->pInitialData != nullptr);
      init_hr = maybe_copy_initial_to_storage(pDesc->pInitialData);
    }
    if (FAILED(init_hr)) {
      deallocate_if_needed();
      ResetObject(res);
      return init_hr;
    }

    // Treat resource creation as transactional: if we fail to append any of the
    // required packets (including optional initial-data uploads or shared-surface
    // export), roll back the command stream so the host doesn't observe a
    // half-created resource.
    const auto cmd_checkpoint = dev->cmd.checkpoint();
    const size_t alloc_checkpoint = dev->wddm_submit_allocation_handles.size();
    const bool alloc_list_oom_checkpoint = dev->wddm_submit_allocation_list_oom;
    auto rollback_create = [&]() {
      dev->cmd.rollback(cmd_checkpoint);
      if (dev->wddm_submit_allocation_handles.size() > alloc_checkpoint) {
        dev->wddm_submit_allocation_handles.resize(alloc_checkpoint);
      }
      dev->wddm_submit_allocation_list_oom = alloc_list_oom_checkpoint;
    };

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      rollback_create();
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags_for_buffer(res->bind_flags);
    cmd->size_bytes = padded_size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;

    if (has_initial_data) {
      const HRESULT upload_hr = EmitUploadLocked(dev, res, 0, res->size_bytes);
      if (FAILED(upload_hr)) {
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return upload_hr;
      }
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      // The command stream references a guest allocation, but we could not
      // record it in the submission allocation list. Submitting would be unsafe
      // (the KMD cannot resolve backing_alloc_id), so fail cleanly.
      rollback_create();
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(dev, E_FAIL);
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return E_FAIL;
      }

      // Shared resources must be importable cross-process as soon as
      // CreateResource returns. Export the resource and force a submission so
      // the host observes the share_token mapping immediately (mirrors D3D9Ex
      // behavior).
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return E_OUTOFMEMORY;
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;
      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        SetError(dev, submit_hr);
        deallocate_if_needed();
        ResetObject(res);
        return submit_hr;
      }
    }
    return S_OK;
  }

  if (dim == D3D10DDIRESOURCE_TEXTURE2D) {
    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : CalcFullMipLevels(res->width, res->height);
    res->array_size = pDesc->ArraySize ? pDesc->ArraySize : 1;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(res);
      return E_NOTIMPL;
    }

    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      ResetObject(res);
      return E_NOTIMPL;
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    res->row_pitch_bytes = AlignUpU32(row_bytes, 256);

    uint64_t total_bytes = 0;
    if (!build_texture2d_subresource_layouts(aer_fmt,
                                             res->width,
                                             res->height,
                                             res->mip_levels,
                                             res->array_size,
                                              res->row_pitch_bytes,
                                              &res->tex2d_subresources,
                                              &total_bytes)) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }

    const bool is_staging = (res->usage == kD3D11UsageStaging);
    bool cpu_visible = is_staging || (res->cpu_access_flags != 0);
    const bool is_rt = (res->bind_flags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D11BindDepthStencil) != 0;
    bool is_shared = false;
    if (res->misc_flags & kD3D11ResourceMiscShared) {
      is_shared = true;
    }
    if (res->misc_flags & kD3D11ResourceMiscSharedKeyedMutex) {
      is_shared = true;
    }
    if (is_shared && (res->mip_levels != 1 || res->array_size != 1)) {
      ResetObject(res);
      return E_NOTIMPL;
    }
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;
    res->is_shared = is_shared;
    aerogpu_wddm_alloc_priv_v2 alloc_priv = {};
    HRESULT hr =
        allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared, is_primary, res->row_pitch_bytes, &alloc_priv);
    if (FAILED(hr)) {
      SetError(dev, hr);
      ResetObject(res);
      return hr;
    }

    // If the KMD returns a different pitch (via the private driver data blob),
    // update our internal + protocol-visible layout before uploading any data.
    //
    // This keeps the host's `CREATE_TEXTURE2D.row_pitch_bytes` interpretation in
    // sync with the actual guest backing memory layout and avoids silent row
    // corruption when the Win7 runtime/KMD chooses a different pitch.
    uint32_t alloc_pitch = alloc_priv.row_pitch_bytes;
    if (alloc_pitch == 0 && !AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(alloc_priv.reserved0)) {
      alloc_pitch = static_cast<uint32_t>(alloc_priv.reserved0 & 0xFFFFFFFFu);
    }
    if (alloc_pitch != 0 && alloc_pitch != res->row_pitch_bytes) {
      LogTexture2DPitchMismatchRateLimited("CreateResource11",
                                           res,
                                           /*subresource=*/0,
                                           res->row_pitch_bytes,
                                           alloc_pitch);
      if (alloc_pitch < row_bytes) {
        SetError(dev, E_INVALIDARG);
        deallocate_if_needed();
        ResetObject(res);
        return E_INVALIDARG;
      }

      std::vector<Texture2DSubresourceLayout> updated_layouts;
      uint64_t updated_total_bytes = 0;
      if (!build_texture2d_subresource_layouts(aer_fmt,
                                               res->width,
                                               res->height,
                                               res->mip_levels,
                                               res->array_size,
                                               alloc_pitch,
                                               &updated_layouts,
                                               &updated_total_bytes)) {
        SetError(dev, E_FAIL);
        deallocate_if_needed();
        ResetObject(res);
        return E_FAIL;
      }

      uint64_t backing_size = total_bytes;
      if (alloc_priv.size_bytes) {
        backing_size = static_cast<uint64_t>(alloc_priv.size_bytes);
      } else if (pDesc->pAllocationInfo) {
        // Some runtime/KMD paths update the allocation size out-of-band (via the
        // allocation info array) without updating the private allocation blob.
        // Use that as a fallback so we can accept a pitch-selected layout that
        // still fits the actual allocation size.
        backing_size = static_cast<uint64_t>(pDesc->pAllocationInfo[0].Size);
      }
      if (updated_total_bytes == 0 || updated_total_bytes > backing_size || updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(dev, E_INVALIDARG);
        deallocate_if_needed();
        ResetObject(res);
        return E_INVALIDARG;
      }

      res->row_pitch_bytes = alloc_pitch;
      res->tex2d_subresources = std::move(updated_layouts);
      total_bytes = updated_total_bytes;
    }

    // Query the runtime/KMD-selected pitch via a LockCb round-trip so our
    // protocol-visible layout matches the actual mapped allocation.
    //
    // If the reported pitch implies a larger mip0 layout than the allocation
    // size, fail cleanly rather than silently overlapping subsequent
    // subresources.
    if (dev->runtime_device && res->wddm_allocation_handle != 0) {
      const auto* wddm_cb = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
      const auto* device_cb = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
      enum class LockCbPath {
        Wddm,
        Device,
      };
      LockCbPath lock_path{};
      if (wddm_cb && wddm_cb->pfnLockCb && wddm_cb->pfnUnlockCb) {
        lock_path = LockCbPath::Wddm;
      } else if (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb) {
        lock_path = LockCbPath::Device;
      } else {
        // LockCb/UnlockCb are optional; if we cannot query, fall back to the
        // pitch we already negotiated via private allocation metadata.
        goto SkipLockPitchQuery;
      }

      {
        const auto lock_for_query = [&](D3DDDICB_LOCK* args) -> HRESULT {
          if (!args) {
            return E_INVALIDARG;
          }
          if (lock_path == LockCbPath::Wddm) {
            return CallCbMaybeHandle(wddm_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
          }
          if (lock_path == LockCbPath::Device) {
            return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
          }
          return E_NOTIMPL;
        };

        const auto unlock_query = [&](D3DDDICB_UNLOCK* args) -> HRESULT {
          if (!args) {
            return E_INVALIDARG;
          }
          if (lock_path == LockCbPath::Wddm) {
            return CallCbMaybeHandle(wddm_cb->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
          }
          if (lock_path == LockCbPath::Device) {
            return CallCbMaybeHandle(device_cb->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
          }
          return E_NOTIMPL;
        };

        D3DDDICB_LOCK lock_args = {};
        lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
        __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
          lock_args.SubresourceIndex = 0;
        }
        __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
          lock_args.SubResourceIndex = 0;
        }
        InitLockForWrite(&lock_args);

        HRESULT lock_hr = lock_for_query(&lock_args);
        if (SUCCEEDED(lock_hr) && lock_args.pData) {
          uint32_t lock_pitch = 0;
          __if_exists(D3DDDICB_LOCK::Pitch) {
            lock_pitch = lock_args.Pitch;
          }

          if (lock_pitch != 0 && lock_pitch != res->row_pitch_bytes) {
            LogTexture2DPitchMismatchRateLimited("CreateResource11",
                                                 res,
                                                 /*subresource=*/0,
                                                 res->row_pitch_bytes,
                                                 lock_pitch);

            if (lock_pitch < row_bytes) {
              D3DDDICB_UNLOCK unlock_args = {};
              unlock_args.hAllocation = lock_args.hAllocation;
              __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
                unlock_args.SubresourceIndex = 0;
              }
              __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
                unlock_args.SubResourceIndex = 0;
              }
              (void)unlock_query(&unlock_args);
              SetError(dev, E_INVALIDARG);
              deallocate_if_needed();
              ResetObject(res);
              return E_INVALIDARG;
            }

            std::vector<Texture2DSubresourceLayout> updated_layouts;
            uint64_t updated_total_bytes = 0;
            if (!build_texture2d_subresource_layouts(aer_fmt,
                                                     res->width,
                                                     res->height,
                                                     res->mip_levels,
                                                     res->array_size,
                                                     lock_pitch,
                                                     &updated_layouts,
                                                     &updated_total_bytes)) {
              D3DDDICB_UNLOCK unlock_args = {};
              unlock_args.hAllocation = lock_args.hAllocation;
              __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
                unlock_args.SubresourceIndex = 0;
              }
              __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
                unlock_args.SubResourceIndex = 0;
              }
              (void)unlock_query(&unlock_args);
              SetError(dev, E_FAIL);
              deallocate_if_needed();
              ResetObject(res);
              return E_FAIL;
            }

            uint64_t backing_size = total_bytes;
            if (alloc_priv.size_bytes != 0) {
              backing_size = static_cast<uint64_t>(alloc_priv.size_bytes);
            } else if (pDesc->pAllocationInfo) {
              backing_size = static_cast<uint64_t>(pDesc->pAllocationInfo[0].Size);
            }
            if (updated_total_bytes == 0 || updated_total_bytes > backing_size || updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
              D3DDDICB_UNLOCK unlock_args = {};
              unlock_args.hAllocation = lock_args.hAllocation;
              __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
                unlock_args.SubresourceIndex = 0;
              }
              __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
                unlock_args.SubResourceIndex = 0;
              }
              (void)unlock_query(&unlock_args);
              SetError(dev, E_INVALIDARG);
              deallocate_if_needed();
              ResetObject(res);
              return E_INVALIDARG;
            }

            res->row_pitch_bytes = lock_pitch;
            res->tex2d_subresources = std::move(updated_layouts);
            total_bytes = updated_total_bytes;
          }
        }

        if (SUCCEEDED(lock_hr)) {
          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
            unlock_args.SubresourceIndex = 0;
          }
          __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
            unlock_args.SubResourceIndex = 0;
          }
          (void)unlock_query(&unlock_args);
        }
      }
    }
  SkipLockPitchQuery:;

    try {
      res->storage.resize(static_cast<size_t>(total_bytes));
    } catch (...) {
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }

    if (res->usage == kD3D11UsageDynamic && !is_shared) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created tex2d handle=%u alloc_id=%u size=%ux%u row_pitch=%u",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->backing_alloc_id),
                         static_cast<unsigned>(res->width),
                         static_cast<unsigned>(res->height),
                         static_cast<unsigned>(res->row_pitch_bytes));
#endif

    bool has_initial_data = false;
    HRESULT init_hr = S_OK;
    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      has_initial_data = (pDesc->pInitialDataUP != nullptr);
      init_hr = maybe_copy_initial_to_storage(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      has_initial_data = (pDesc->pInitialData != nullptr);
      init_hr = maybe_copy_initial_to_storage(pDesc->pInitialData);
    }
    if (FAILED(init_hr)) {
      deallocate_if_needed();
      ResetObject(res);
      return init_hr;
    }

    // Treat CreateResource as a transaction: if any required packets fail to
    // append (OOM), roll back the command stream so the host doesn't observe a
    // partially created resource.
    const auto cmd_checkpoint = dev->cmd.checkpoint();
    const size_t alloc_checkpoint = dev->wddm_submit_allocation_handles.size();
    const bool alloc_list_oom_checkpoint = dev->wddm_submit_allocation_list_oom;
    auto rollback_create = [&]() {
      dev->cmd.rollback(cmd_checkpoint);
      if (dev->wddm_submit_allocation_handles.size() > alloc_checkpoint) {
        dev->wddm_submit_allocation_handles.resize(alloc_checkpoint);
      }
      dev->wddm_submit_allocation_list_oom = alloc_list_oom_checkpoint;
    };

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      rollback_create();
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags_for_texture(res->bind_flags);
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = res->array_size;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;

    if (has_initial_data) {
      const HRESULT upload_hr = EmitUploadLocked(dev, res, 0, res->storage.size());
      if (FAILED(upload_hr)) {
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return upload_hr;
      }
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      rollback_create();
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(dev, E_FAIL);
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return E_FAIL;
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        rollback_create();
        deallocate_if_needed();
        ResetObject(res);
        return E_OUTOFMEMORY;
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;
      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        SetError(dev, submit_hr);
        deallocate_if_needed();
        ResetObject(res);
        return submit_hr;
      }
    }
    return S_OK;
  }

  deallocate_if_needed();
  ResetObject(res);
  return E_NOTIMPL;
}

HRESULT AEROGPU_APIENTRY OpenResource11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_OPENRESOURCE* pOpenResource,
                                         D3D11DDI_HRESOURCE hResource,
                                         D3D11DDI_HRTRESOURCE) {
  if (!hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the resource so DestroyResource11 is safe even if OpenResource11 fails.
  auto* res = new (hResource.pDrvPrivate) Resource();

  if (!hDevice.pDrvPrivate || !pOpenResource) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(res);
    return E_FAIL;
  }

  const void* priv_data = nullptr;
  uint32_t priv_size = 0;
  uint32_t num_allocations = 1;
  __if_exists(D3D11DDIARG_OPENRESOURCE::NumAllocations) {
    if (pOpenResource->NumAllocations < 1) {
      return E_INVALIDARG;
    }
    num_allocations = static_cast<uint32_t>(pOpenResource->NumAllocations);
  }

  // OpenResource DDI structs vary across WDK header vintages. Some headers
  // expose the preserved private driver data at the per-allocation level; prefer
  // that when present and fall back to the top-level fields.
  __if_exists(D3D11DDIARG_OPENRESOURCE::pOpenAllocationInfo) {
    if (pOpenResource->pOpenAllocationInfo && num_allocations >= 1) {
      using OpenInfoT = std::remove_pointer_t<decltype(pOpenResource->pOpenAllocationInfo)>;
      __if_exists(OpenInfoT::pPrivateDriverData) {
        priv_data = pOpenResource->pOpenAllocationInfo[0].pPrivateDriverData;
      }
      __if_exists(OpenInfoT::PrivateDriverDataSize) {
        priv_size = static_cast<uint32_t>(pOpenResource->pOpenAllocationInfo[0].PrivateDriverDataSize);
      }
    }
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::pPrivateDriverData) {
    if (!priv_data) {
      priv_data = pOpenResource->pPrivateDriverData;
    }
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::PrivateDriverDataSize) {
    if (priv_size == 0) {
      priv_size = static_cast<uint32_t>(pOpenResource->PrivateDriverDataSize);
    }
  }

  if (num_allocations != 1) {
    return E_NOTIMPL;
  }

  if (!priv_data || priv_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return E_INVALIDARG;
  }

  aerogpu_wddm_alloc_priv_v2 priv{};
  if (!ConsumeWddmAllocPrivV2(priv_data, static_cast<UINT>(priv_size), &priv)) {
    return E_INVALIDARG;
  }
  if (!FixupLegacyPrivForOpenResource(&priv)) {
    return E_INVALIDARG;
  }
  if ((priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) == 0 || priv.share_token == 0 || priv.alloc_id == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  res->handle = AllocateGlobalHandle(dev->adapter);
  res->backing_alloc_id = static_cast<uint32_t>(priv.alloc_id);
  res->backing_offset_bytes = 0;
  res->wddm_allocation_handle = 0;
  res->share_token = static_cast<uint64_t>(priv.share_token);
  res->is_shared = true;
  res->is_shared_alias = true;

  __if_exists(D3D11DDIARG_OPENRESOURCE::BindFlags) {
    res->bind_flags = static_cast<uint32_t>(pOpenResource->BindFlags);
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::MiscFlags) {
    res->misc_flags = static_cast<uint32_t>(pOpenResource->MiscFlags);
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::Usage) {
    res->usage = static_cast<uint32_t>(pOpenResource->Usage);
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::CPUAccessFlags) {
    res->cpu_access_flags = static_cast<uint32_t>(pOpenResource->CPUAccessFlags);
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::CpuAccessFlags) {
    res->cpu_access_flags = static_cast<uint32_t>(pOpenResource->CpuAccessFlags);
  }

  __if_exists(D3D11DDIARG_OPENRESOURCE::hKMResource) {
    res->wddm.km_resource_handle = static_cast<uint64_t>(pOpenResource->hKMResource);
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::hKMAllocation) {
    try {
      res->wddm.km_allocation_handles.push_back(static_cast<uint64_t>(pOpenResource->hKMAllocation));
    } catch (...) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::hAllocation) {
    const uint64_t h = static_cast<uint64_t>(pOpenResource->hAllocation);
    if (h != 0) {
      res->wddm_allocation_handle = static_cast<uint32_t>(h);
      if (res->wddm.km_allocation_handles.empty()) {
        try {
          res->wddm.km_allocation_handles.push_back(h);
        } catch (...) {
          ResetObject(res);
          return E_OUTOFMEMORY;
        }
      }
    }
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::phAllocations) {
    __if_exists(D3D11DDIARG_OPENRESOURCE::NumAllocations) {
      if (pOpenResource->phAllocations && pOpenResource->NumAllocations) {
        const uint64_t h = static_cast<uint64_t>(pOpenResource->phAllocations[0]);
        if (h != 0) {
          res->wddm_allocation_handle = static_cast<uint32_t>(h);
          if (res->wddm.km_allocation_handles.empty()) {
            try {
              res->wddm.km_allocation_handles.push_back(h);
            } catch (...) {
              ResetObject(res);
              return E_OUTOFMEMORY;
            }
          }
        }
      }
    }
  }

  // Fall back to per-allocation handles when top-level members are absent.
  __if_exists(D3D11DDIARG_OPENRESOURCE::pOpenAllocationInfo) {
    if (pOpenResource->pOpenAllocationInfo && num_allocations >= 1) {
      uint64_t km_alloc = 0;
      uint32_t runtime_alloc = 0;
      using OpenInfoT = std::remove_pointer_t<decltype(pOpenResource->pOpenAllocationInfo)>;
      __if_exists(OpenInfoT::hKMAllocation) {
        km_alloc = static_cast<uint64_t>(pOpenResource->pOpenAllocationInfo[0].hKMAllocation);
      }
      __if_not_exists(OpenInfoT::hKMAllocation) {
        __if_exists(OpenInfoT::hAllocation) {
          km_alloc = static_cast<uint64_t>(pOpenResource->pOpenAllocationInfo[0].hAllocation);
        }
      }
      __if_exists(OpenInfoT::hAllocation) {
        runtime_alloc = static_cast<uint32_t>(pOpenResource->pOpenAllocationInfo[0].hAllocation);
      }
      if (res->wddm_allocation_handle == 0 && (runtime_alloc != 0 || km_alloc != 0)) {
        res->wddm_allocation_handle = runtime_alloc ? runtime_alloc : static_cast<uint32_t>(km_alloc);
      }
      if (km_alloc != 0 &&
          std::find(res->wddm.km_allocation_handles.begin(), res->wddm.km_allocation_handles.end(), km_alloc) ==
              res->wddm.km_allocation_handles.end()) {
        try {
          res->wddm.km_allocation_handles.push_back(km_alloc);
        } catch (...) {
          ResetObject(res);
          return E_OUTOFMEMORY;
        }
      }
    }
  }

  if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(priv.size_bytes);
    res->structure_stride_bytes = 0;
  } else if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(priv.format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    res->kind = ResourceKind::Texture2D;
    res->width = static_cast<uint32_t>(priv.width);
    res->height = static_cast<uint32_t>(priv.height);
    res->mip_levels = 1;
    res->array_size = 1;
    res->dxgi_format = static_cast<uint32_t>(priv.format);
    res->row_pitch_bytes = static_cast<uint32_t>(priv.row_pitch_bytes);
    if (res->row_pitch_bytes == 0 && res->width != 0) {
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      if (row_bytes == 0) {
        ResetObject(res);
        return E_INVALIDARG;
      }
      res->row_pitch_bytes = AlignUpU32(row_bytes, 256);
    }

    uint64_t total_bytes = 0;
    if (!build_texture2d_subresource_layouts(aer_fmt,
                                             res->width,
                                             res->height,
                                             res->mip_levels,
                                             res->array_size,
                                              res->row_pitch_bytes,
                                              &res->tex2d_subresources,
                                              &total_bytes)) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    try {
      res->storage.resize(static_cast<size_t>(total_bytes));
    } catch (...) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
  } else {
    ResetObject(res);
    return E_INVALIDARG;
  }

  auto* import_cmd =
      dev->cmd.append_fixed<aerogpu_cmd_import_shared_surface>(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!import_cmd) {
    ResetObject(res);
    return E_OUTOFMEMORY;
  }
  import_cmd->out_resource_handle = res->handle;
  import_cmd->reserved0 = 0;
  import_cmd->share_token = res->share_token;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyResource11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRESOURCE hResource) {
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(res);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(res);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (res->mapped) {
    (void)UnmapLocked(dev, res);
  }

  // Be conservative and scrub bindings before emitting the host-side destroy.
  // The runtime generally unbinds resources prior to destruction, but stale
  // bindings can occur during error paths. Additionally, shared/aliased resources
  // may appear as distinct Resource objects while referring to the same backing
  // allocation; treat those as aliasing for the purposes of cleanup.
  UnbindResourceFromSrvsLocked(dev, res->handle, res);
  UnbindResourceFromOutputsLocked(dev, res->handle, res);
  UnbindResourceFromConstantBuffersLocked(dev, res);
  UnbindResourceFromInputAssemblerLocked(dev, res);

  // Best-effort safety net: if any unbind command emission failed (OOM), some of
  // the above helpers may leave cached pointers intact. Ensure we never keep a
  // dangling `Resource*` to memory we're about to destroy.
  //
  // Note: this does not guarantee the host state was updated (OOM may have
  // prevented command emission), but it prevents UMD-side use-after-free on later
  // state tracking.
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (slot < dev->current_vs_srvs.size() && dev->current_vs_srvs[slot] == res) {
      dev->current_vs_srvs[slot] = nullptr;
      dev->vs_srvs[slot] = 0;
      if (slot == 0) {
        dev->current_vs_srv0 = nullptr;
      }
    }
    if (slot < dev->current_ps_srvs.size() && dev->current_ps_srvs[slot] == res) {
      dev->current_ps_srvs[slot] = nullptr;
      dev->ps_srvs[slot] = 0;
      if (slot == 0) {
        dev->current_ps_srv0 = nullptr;
      }
    }
    if (slot < dev->current_gs_srvs.size() && dev->current_gs_srvs[slot] == res) {
      dev->current_gs_srvs[slot] = nullptr;
      dev->gs_srvs[slot] = 0;
    }
    if (slot < dev->current_cs_srvs.size() && dev->current_cs_srvs[slot] == res) {
      dev->current_cs_srvs[slot] = nullptr;
      dev->cs_srvs[slot] = 0;
    }

    if (slot < dev->current_vs_srv_buffers.size() && dev->current_vs_srv_buffers[slot] == res) {
      dev->current_vs_srv_buffers[slot] = nullptr;
      dev->vs_srv_buffers[slot] = {};
    }
    if (slot < dev->current_ps_srv_buffers.size() && dev->current_ps_srv_buffers[slot] == res) {
      dev->current_ps_srv_buffers[slot] = nullptr;
      dev->ps_srv_buffers[slot] = {};
    }
    if (slot < dev->current_gs_srv_buffers.size() && dev->current_gs_srv_buffers[slot] == res) {
      dev->current_gs_srv_buffers[slot] = nullptr;
      dev->gs_srv_buffers[slot] = {};
    }
    if (slot < dev->current_cs_srv_buffers.size() && dev->current_cs_srv_buffers[slot] == res) {
      dev->current_cs_srv_buffers[slot] = nullptr;
      dev->cs_srv_buffers[slot] = {};
    }
  }
  for (uint32_t slot = 0; slot < kMaxUavSlots; ++slot) {
    if (slot < dev->current_cs_uavs.size() && dev->current_cs_uavs[slot] == res) {
      dev->current_cs_uavs[slot] = nullptr;
      aerogpu_unordered_access_buffer_binding null_uav{};
      null_uav.initial_count = kD3DUavInitialCountNoChange;
      dev->cs_uavs[slot] = null_uav;
    }
  }

  // Render targets / depth-stencil (outputs). These cached pointers are used by
  // draw-state tracking and the bring-up software renderer; never allow them to
  // dangle past resource destruction even if unbind command emission failed
  // earlier (e.g. due to OOM).
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if ((res->handle != 0 && dev->current_rtvs[i] == res->handle) ||
        ResourcesAlias(dev->current_rtv_resources[i], res)) {
      dev->current_rtvs[i] = 0;
      dev->current_rtv_resources[i] = nullptr;
    }
  }
  if ((res->handle != 0 && dev->current_dsv == res->handle) ||
      ResourcesAlias(dev->current_dsv_resource, res)) {
    dev->current_dsv = 0;
    dev->current_dsv_resource = nullptr;
  }

  if (res->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
    } else {
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
    }
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  if (is_guest_backed && !dev->cmd.empty()) {
    // Flush before releasing the WDDM allocation so submissions that referenced
    // backing_alloc_id can still build an alloc_table from this allocation.
    HRESULT submit_hr = S_OK;
    submit_locked(dev, /*want_present=*/false, &submit_hr);
    if (FAILED(submit_hr)) {
      SetError(dev, submit_hr);
    }
  }

  auto* callbacks = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  if (callbacks && callbacks->pfnDeallocateCb && dev->runtime_device &&
      (res->wddm.km_resource_handle != 0 || !res->wddm.km_allocation_handles.empty())) {
    constexpr size_t kInlineKmtAllocs = 16;
    std::array<D3DKMT_HANDLE, kInlineKmtAllocs> km_allocs_stack{};
    std::vector<D3DKMT_HANDLE> km_allocs_heap;
    D3DKMT_HANDLE* km_allocs = nullptr;
    UINT km_alloc_count = 0;

    const size_t handle_count = res->wddm.km_allocation_handles.size();
    if (handle_count != 0) {
      if (handle_count <= km_allocs_stack.size()) {
        for (size_t i = 0; i < handle_count; ++i) {
          km_allocs_stack[i] = static_cast<D3DKMT_HANDLE>(res->wddm.km_allocation_handles[i]);
        }
        km_allocs = km_allocs_stack.data();
        km_alloc_count = static_cast<UINT>(handle_count);
      } else {
        try {
          km_allocs_heap.reserve(handle_count);
          for (uint64_t h : res->wddm.km_allocation_handles) {
            km_allocs_heap.push_back(static_cast<D3DKMT_HANDLE>(h));
          }
          km_allocs = km_allocs_heap.data();
          km_alloc_count = static_cast<UINT>(km_allocs_heap.size());
        } catch (...) {
          SetError(dev, E_OUTOFMEMORY);
          km_allocs = nullptr;
          km_alloc_count = 0;
        }
      }
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = km_alloc_count;
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_alloc_count ? km_allocs : nullptr;
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_alloc_count ? km_allocs : nullptr;
    }
    const HRESULT hr = CallCbMaybeHandle(callbacks->pfnDeallocateCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &dealloc);
    if (FAILED(hr)) {
      SetError(dev, hr);
    }
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  }
  dev->pending_staging_writes.erase(
      std::remove(dev->pending_staging_writes.begin(), dev->pending_staging_writes.end(), res),
      dev->pending_staging_writes.end());
  ResetObject(res);
}

// Views

static bool D3dViewFormatCompatible(const Device* dev, const Resource* res, uint32_t view_dxgi_format) {
  if (!dev || !res) {
    return false;
  }
  // DXGI_FORMAT_UNKNOWN means "use the resource's format".
  if (view_dxgi_format == kDxgiFormatUnknown) {
    return true;
  }

  const uint32_t res_aer = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
  const uint32_t view_aer = dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
  if (res_aer == AEROGPU_FORMAT_INVALID || view_aer == AEROGPU_FORMAT_INVALID) {
    return false;
  }
  return res_aer == view_aer;
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRenderTargetViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(RenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRenderTargetView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                                  D3D11DDI_HRENDERTARGETVIEW hView,
                                                  D3D11DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyRenderTargetView11 is safe even
  // if we reject the descriptor.
  auto* rtv = new (hView.pDrvPrivate) RenderTargetView();
  rtv->texture = 0;
  rtv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }

  D3D11DDI_HRESOURCE hRes{};
  __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
    __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hRes);
  if (!res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t view_fmt = kDxgiFormatUnknown;
  __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Format) {
    view_fmt = static_cast<uint32_t>(pDesc->Format);
  }
  if (!D3dViewFormatCompatible(dev, res, view_fmt)) {
    AEROGPU_D3D10_11_LOG(
        "CreateRenderTargetView11: reject unsupported RTV format (view_fmt=%u res_fmt=%u handle=%u)",
        static_cast<unsigned>(view_fmt),
        static_cast<unsigned>(res->dxgi_format),
        static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      return E_NOTIMPL;
    }
  } else if (res->array_size > 1) {
    // Array resources must provide an explicit view dimension so we can extract
    // slice ranges from the descriptor union.
    return E_NOTIMPL;
  }

  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
  __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::MipSlice) {
    mip_slice = static_cast<uint32_t>(pDesc->MipSlice);
    have_mip_slice = true;
  }
  __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::MipSlice) {
    if (view_is_array) {
      __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2DArray.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2DArray.MipSlice);
          have_mip_slice = true;
        }
      }
    } else {
      __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2D) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2D.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2D) {
        __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Texture2D) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2D.MipSlice);
          have_mip_slice = true;
        }
      }
    }
  }

  if (!have_mip_slice) {
    return E_NOTIMPL;
  }

  if (mip_slice >= res->mip_levels) {
    return E_INVALIDARG;
  }

  uint32_t first_slice = 0;
  uint32_t slice_count = res->array_size;
  bool have_slice_range = !view_is_array;
  if (view_is_array) {
    __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      slice_count = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D11DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
  }

  if (!have_slice_range) {
    return E_NOTIMPL;
  }

  slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

  if (first_slice >= res->array_size || slice_count == 0 || first_slice + slice_count > res->array_size) {
    return E_INVALIDARG;
  }

  const uint32_t view_dxgi_format = (view_fmt != kDxgiFormatUnknown) ? view_fmt : res->dxgi_format;
  const bool format_reinterpret = (view_fmt != kDxgiFormatUnknown) && (view_fmt != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || mip_slice != 0 || first_slice != 0 || slice_count != res->array_size;
  const bool supports_views = SupportsTextureViews(dev);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }
  rtv->resource = res;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(rtv);
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(rtv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = first_slice;
    cmd->array_layer_count = slice_count;
    cmd->reserved0 = 0;

    rtv->texture = view_handle;
  }

  return S_OK;
}

void AEROGPU_APIENTRY DestroyRenderTargetView11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRENDERTARGETVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (SupportsTextureViews(dev) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (view) {
    view->~RenderTargetView();
    new (view) RenderTargetView();
  }
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(DepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                                  D3D11DDI_HDEPTHSTENCILVIEW hView,
                                                  D3D11DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyDepthStencilView11 is safe even
  // if we reject the descriptor.
  auto* dsv = new (hView.pDrvPrivate) DepthStencilView();
  dsv->texture = 0;
  dsv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }

  D3D11DDI_HRESOURCE hRes{};
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hRes);
  if (!res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t view_fmt = kDxgiFormatUnknown;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Format) {
    view_fmt = static_cast<uint32_t>(pDesc->Format);
  }
  if (!D3dViewFormatCompatible(dev, res, view_fmt)) {
    AEROGPU_D3D10_11_LOG(
        "CreateDepthStencilView11: reject unsupported DSV format (view_fmt=%u res_fmt=%u handle=%u)",
        static_cast<unsigned>(view_fmt),
        static_cast<unsigned>(res->dxgi_format),
        static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      return E_NOTIMPL;
    }
  } else if (res->array_size > 1) {
    return E_NOTIMPL;
  }

  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::MipSlice) {
    mip_slice = static_cast<uint32_t>(pDesc->MipSlice);
    have_mip_slice = true;
  }
  __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::MipSlice) {
    if (view_is_array) {
      __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2DArray.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2DArray.MipSlice);
          have_mip_slice = true;
        }
      }
    } else {
      __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2D) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2D.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2D) {
        __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Texture2D) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2D.MipSlice);
          have_mip_slice = true;
        }
      }
    }
  }

  if (!have_mip_slice) {
    return E_NOTIMPL;
  }

  if (mip_slice >= res->mip_levels) {
    return E_INVALIDARG;
  }

  uint32_t flags = 0;
  bool have_flags = false;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Flags) {
    flags = static_cast<uint32_t>(pDesc->Flags);
    have_flags = true;
  }
  if (have_flags && flags != 0) {
    AEROGPU_D3D10_11_LOG(
        "CreateDepthStencilView11: reject unsupported DSV flags=0x%x (handle=%u)",
        static_cast<unsigned>(flags),
        static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  uint32_t first_slice = 0;
  uint32_t slice_count = res->array_size;
  bool have_slice_range = !view_is_array;
  if (view_is_array) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      slice_count = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
  }

  if (!have_slice_range) {
    return E_NOTIMPL;
  }

  slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

  if (first_slice >= res->array_size || slice_count == 0 || first_slice + slice_count > res->array_size) {
    return E_INVALIDARG;
  }

  const uint32_t view_dxgi_format = (view_fmt != kDxgiFormatUnknown) ? view_fmt : res->dxgi_format;
  const bool format_reinterpret = (view_fmt != kDxgiFormatUnknown) && (view_fmt != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || mip_slice != 0 || first_slice != 0 || slice_count != res->array_size;
  const bool supports_views = SupportsTextureViews(dev);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }
  dsv->resource = res;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(dsv);
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(dsv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = first_slice;
    cmd->array_layer_count = slice_count;
    cmd->reserved0 = 0;

    dsv->texture = view_handle;
  }

  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilView11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HDEPTHSTENCILVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (SupportsTextureViews(dev) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (view) {
    view->~DepthStencilView();
    new (view) DepthStencilView();
  }
}

struct ShaderResourceView {
  enum class Kind : uint32_t {
    Texture2D = 0,
    Buffer = 1,
  };
  Kind kind = Kind::Texture2D;
  aerogpu_handle_t texture = 0;
  aerogpu_shader_resource_buffer_binding buffer{};
  Resource* resource = nullptr;
};

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(ShaderResourceView);
}

static uint32_t BytesPerElementForDxgiFormat(uint32_t dxgi_format) {
  switch (static_cast<DXGI_FORMAT>(dxgi_format)) {
    case DXGI_FORMAT_R32G32B32A32_FLOAT:
    case DXGI_FORMAT_R32G32B32A32_UINT:
    case DXGI_FORMAT_R32G32B32A32_SINT:
      return 16;
    case DXGI_FORMAT_R32G32B32_FLOAT:
    case DXGI_FORMAT_R32G32B32_UINT:
    case DXGI_FORMAT_R32G32B32_SINT:
      return 12;
    case DXGI_FORMAT_R32G32_FLOAT:
    case DXGI_FORMAT_R32G32_UINT:
    case DXGI_FORMAT_R32G32_SINT:
      return 8;
    case DXGI_FORMAT_R32_FLOAT:
    case DXGI_FORMAT_R32_UINT:
    case DXGI_FORMAT_R32_SINT:
      return 4;
    case DXGI_FORMAT_R16G16_FLOAT:
    case DXGI_FORMAT_R16G16_UNORM:
    case DXGI_FORMAT_R16G16_UINT:
    case DXGI_FORMAT_R16G16_SNORM:
    case DXGI_FORMAT_R16G16_SINT:
      return 4;
    case DXGI_FORMAT_R16_FLOAT:
    case DXGI_FORMAT_R16_UNORM:
    case DXGI_FORMAT_R16_UINT:
    case DXGI_FORMAT_R16_SNORM:
    case DXGI_FORMAT_R16_SINT:
      return 2;
    case DXGI_FORMAT_R8_UNORM:
    case DXGI_FORMAT_R8_UINT:
    case DXGI_FORMAT_R8_SNORM:
    case DXGI_FORMAT_R8_SINT:
      return 1;
    default:
      return 0;
  }
}

template <typename T, typename = void>
struct has_member_SrvDescFormat : std::false_type {};
template <typename T>
struct has_member_SrvDescFormat<T, std::void_t<decltype(std::declval<T>().Format)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrvDescViewDimension : std::false_type {};
template <typename T>
struct has_member_SrvDescViewDimension<T, std::void_t<decltype(std::declval<T>().ViewDimension)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrvDescBuffer : std::false_type {};
template <typename T>
struct has_member_SrvDescBuffer<T, std::void_t<decltype(std::declval<T>().Buffer)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrvDescBufferEx : std::false_type {};
template <typename T>
struct has_member_SrvDescBufferEx<T, std::void_t<decltype(std::declval<T>().BufferEx)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrvDescTexture2D : std::false_type {};
template <typename T>
struct has_member_SrvDescTexture2D<T, std::void_t<decltype(std::declval<T>().Texture2D)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrvDescTexture2DArray : std::false_type {};
template <typename T>
struct has_member_SrvDescTexture2DArray<T, std::void_t<decltype(std::declval<T>().Texture2DArray)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BufferViewFirstElement : std::false_type {};
template <typename T>
struct has_member_BufferViewFirstElement<T, std::void_t<decltype(std::declval<T>().FirstElement)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BufferViewNumElements : std::false_type {};
template <typename T>
struct has_member_BufferViewNumElements<T, std::void_t<decltype(std::declval<T>().NumElements)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BufferViewFlags : std::false_type {};
template <typename T>
struct has_member_BufferViewFlags<T, std::void_t<decltype(std::declval<T>().Flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ViewMostDetailedMip : std::false_type {};
template <typename T>
struct has_member_ViewMostDetailedMip<T, std::void_t<decltype(std::declval<T>().MostDetailedMip)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ViewMipLevels : std::false_type {};
template <typename T>
struct has_member_ViewMipLevels<T, std::void_t<decltype(std::declval<T>().MipLevels)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ViewFirstArraySlice : std::false_type {};
template <typename T>
struct has_member_ViewFirstArraySlice<T, std::void_t<decltype(std::declval<T>().FirstArraySlice)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ViewArraySize : std::false_type {};
template <typename T>
struct has_member_ViewArraySize<T, std::void_t<decltype(std::declval<T>().ArraySize)>> : std::true_type {};

HRESULT AEROGPU_APIENTRY CreateShaderResourceView11(D3D11DDI_HDEVICE hDevice,
                                                    const D3D11DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                    D3D11DDI_HSHADERRESOURCEVIEW hView,
                                                    D3D11DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyShaderResourceView11 is safe even
  // if we reject the descriptor.
  auto* srv = new (hView.pDrvPrivate) ShaderResourceView();
  srv->kind = ShaderResourceView::Kind::Texture2D;
  srv->texture = 0;
  srv->buffer = {};
  srv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }

  D3D11DDI_HRESOURCE hRes{};
  __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hRes);
  if (!res) {
    return E_INVALIDARG;
  }

  if (res->kind == ResourceKind::Texture2D) {
    std::lock_guard<std::mutex> lock(dev->mutex);

    uint32_t view_fmt = kDxgiFormatUnknown;

    uint32_t most_detailed_mip = 0;
    uint32_t mip_levels = 0;
    bool have_mip_range = false;
    uint32_t first_array_slice = 0;
    uint32_t array_size = 0;
    bool have_array_range = false;

    bool used_desc = false;
    __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Desc) {
      used_desc = true;
      using DescT = std::remove_reference_t<decltype(pDesc->Desc)>;
      if constexpr (has_member_SrvDescFormat<DescT>::value) {
        view_fmt = static_cast<uint32_t>(pDesc->Desc.Format);
      }
      if constexpr (has_member_SrvDescViewDimension<DescT>::value) {
        const uint32_t dim = static_cast<uint32_t>(pDesc->Desc.ViewDimension);
        if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_TEXTURE2D)) {
          if constexpr (has_member_SrvDescTexture2D<DescT>::value) {
            using TexT = std::remove_reference_t<decltype(pDesc->Desc.Texture2D)>;
            if constexpr (has_member_ViewMostDetailedMip<TexT>::value && has_member_ViewMipLevels<TexT>::value) {
              most_detailed_mip = static_cast<uint32_t>(pDesc->Desc.Texture2D.MostDetailedMip);
              mip_levels = static_cast<uint32_t>(pDesc->Desc.Texture2D.MipLevels);
              have_mip_range = true;
            }
          }
        } else if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_TEXTURE2DARRAY)) {
          if constexpr (has_member_SrvDescTexture2DArray<DescT>::value) {
            using TexT = std::remove_reference_t<decltype(pDesc->Desc.Texture2DArray)>;
            if constexpr (has_member_ViewMostDetailedMip<TexT>::value && has_member_ViewMipLevels<TexT>::value) {
              most_detailed_mip = static_cast<uint32_t>(pDesc->Desc.Texture2DArray.MostDetailedMip);
              mip_levels = static_cast<uint32_t>(pDesc->Desc.Texture2DArray.MipLevels);
              have_mip_range = true;
            }
            if constexpr (has_member_ViewFirstArraySlice<TexT>::value && has_member_ViewArraySize<TexT>::value) {
              first_array_slice = static_cast<uint32_t>(pDesc->Desc.Texture2DArray.FirstArraySlice);
              array_size = static_cast<uint32_t>(pDesc->Desc.Texture2DArray.ArraySize);
              have_array_range = true;
            }
          }
        } else {
          AEROGPU_D3D10_11_LOG(
              "CreateShaderResourceView11: reject unsupported SRV view_dim=%u (handle=%u)",
              static_cast<unsigned>(dim),
              static_cast<unsigned>(res->handle));
          return E_NOTIMPL;
        }
      }
    }

    if (!used_desc) {
      __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Format) {
        view_fmt = static_cast<uint32_t>(pDesc->Format);
      }
    }

    if (!D3dViewFormatCompatible(dev, res, view_fmt)) {
      AEROGPU_D3D10_11_LOG(
          "CreateShaderResourceView11: reject unsupported SRV format (view_fmt=%u res_fmt=%u handle=%u)",
          static_cast<unsigned>(view_fmt),
          static_cast<unsigned>(res->dxgi_format),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }

    uint32_t view_dim = 0;
    bool have_dim = false;

    if (used_desc) {
      __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Desc) {
        using DescT = std::remove_reference_t<decltype(pDesc->Desc)>;
        if constexpr (has_member_SrvDescViewDimension<DescT>::value) {
          view_dim = static_cast<uint32_t>(pDesc->Desc.ViewDimension);
          have_dim = true;
        }
      }
      if (have_dim) {
        const uint32_t dim = view_dim;
        if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_TEXTURE2D)) {
          if (res->array_size != 1) {
            AEROGPU_D3D10_11_LOG(
                "CreateShaderResourceView11: reject non-array SRV for array texture (array=%u handle=%u)",
                static_cast<unsigned>(res->array_size),
                static_cast<unsigned>(res->handle));
            return E_NOTIMPL;
          }
        } else if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_TEXTURE2DARRAY)) {
          // Full-array view only.
          uint32_t effective_array_size = array_size;
          if (effective_array_size == 0 || effective_array_size == kD3DUintAll) {
            effective_array_size = res->array_size;
          }
          if (have_array_range) {
            if (first_array_slice >= res->array_size) {
              return E_INVALIDARG;
            }
            if (effective_array_size > res->array_size - first_array_slice) {
              return E_INVALIDARG;
            }
            if (first_array_slice != 0 || effective_array_size != res->array_size) {
              AEROGPU_D3D10_11_LOG(
                  "CreateShaderResourceView11: reject unsupported SRV array range (first=%u size=%u res_array=%u handle=%u)",
                  static_cast<unsigned>(first_array_slice),
                  static_cast<unsigned>(effective_array_size),
                  static_cast<unsigned>(res->array_size),
                  static_cast<unsigned>(res->handle));
              return E_NOTIMPL;
            }
          } else if (res->array_size != 1) {
            // No array selector available: conservatively reject multi-slice resources.
            AEROGPU_D3D10_11_LOG(
                "CreateShaderResourceView11: reject array SRV without array selector (array=%u handle=%u)",
                static_cast<unsigned>(res->array_size),
                static_cast<unsigned>(res->handle));
            return E_NOTIMPL;
          }
        } else {
          AEROGPU_D3D10_11_LOG(
              "CreateShaderResourceView11: reject unsupported SRV view_dim=%u (handle=%u)",
              static_cast<unsigned>(dim),
              static_cast<unsigned>(res->handle));
          return E_NOTIMPL;
        }
      } else if (res->array_size != 1) {
        // If the runtime does not provide a view-dimension discriminator, we
        // cannot safely determine whether the SRV is a Texture2D vs
        // Texture2DArray view. Conservatively reject array resources to avoid
        // silently binding the wrong subresource range.
        AEROGPU_D3D10_11_LOG(
            "CreateShaderResourceView11: reject SRV for array texture without view dimension (array=%u handle=%u)",
            static_cast<unsigned>(res->array_size),
            static_cast<unsigned>(res->handle));
        return E_NOTIMPL;
      }
    } else {
      __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
        view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
        have_dim = true;
      }
      __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
        __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::ViewDimension) {
          view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
          have_dim = true;
        }
      }
    }

    bool view_is_array = false;
    if (have_dim) {
      if (D3dViewDimensionIsTexture2D(view_dim)) {
        if (res->array_size != 1) {
          AEROGPU_D3D10_11_LOG(
              "CreateShaderResourceView11: reject non-array SRV for array texture (array=%u handle=%u)",
              static_cast<unsigned>(res->array_size),
              static_cast<unsigned>(res->handle));
          return E_NOTIMPL;
        }
        view_is_array = false;
      } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
        view_is_array = true;
      } else {
        return E_NOTIMPL;
      }
    } else if (res->array_size > 1) {
      return E_NOTIMPL;
    }

    if (!used_desc) {
      __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->MipLevels);
        have_mip_range = true;
      }
      __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
        __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2D.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Tex2D.MipLevels);
          have_mip_range = true;
        }
        __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
          __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Texture2D) {
            most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2D.MostDetailedMip);
            mip_levels = static_cast<uint32_t>(pDesc->Texture2D.MipLevels);
            have_mip_range = true;
          }
        }
      }
    }

    if (!have_mip_range) {
      return E_NOTIMPL;
    }

    uint32_t mip_count = D3dViewCountToRemaining(most_detailed_mip, mip_levels, res->mip_levels);

    if (most_detailed_mip >= res->mip_levels ||
        mip_count == 0 ||
        most_detailed_mip + mip_count > res->mip_levels) {
      return E_INVALIDARG;
    }

    uint32_t first_slice = 0;
    uint32_t slice_count = res->array_size;
    bool have_slice_range = !view_is_array;
    if (view_is_array) {
      __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
        first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
        __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
          have_slice_range = true;
        }
        __if_not_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
          __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
            first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
            slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
            have_slice_range = true;
          }
        }
      }
    }

    if (!have_slice_range) {
      return E_NOTIMPL;
    }

    slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

    if (first_slice >= res->array_size ||
        slice_count == 0 ||
        first_slice + slice_count > res->array_size) {
      return E_INVALIDARG;
    }

    const uint32_t view_dxgi_format = (view_fmt != kDxgiFormatUnknown) ? view_fmt : res->dxgi_format;
    const bool format_reinterpret = (view_fmt != kDxgiFormatUnknown) && (view_fmt != res->dxgi_format);
    const bool non_trivial =
        format_reinterpret ||
        most_detailed_mip != 0 ||
        mip_count != res->mip_levels ||
        first_slice != 0 ||
        slice_count != res->array_size;
    const bool supports_views = SupportsTextureViews(dev);
    if (non_trivial && !supports_views) {
      return E_NOTIMPL;
    }
    srv->kind = ShaderResourceView::Kind::Texture2D;
    srv->texture = 0;
    srv->buffer = {};
    srv->resource = res;

    if (non_trivial && supports_views) {
      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
      if (aer_fmt == AEROGPU_FORMAT_INVALID) {
        ResetObject(srv);
        return E_NOTIMPL;
      }

      const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
      if (!cmd) {
        ResetObject(srv);
        return E_OUTOFMEMORY;
      }
      cmd->view_handle = view_handle;
      cmd->texture_handle = res->handle;
      cmd->format = aer_fmt;
      cmd->base_mip_level = most_detailed_mip;
      cmd->mip_level_count = mip_count;
      cmd->base_array_layer = first_slice;
      cmd->array_layer_count = slice_count;
      cmd->reserved0 = 0;

      srv->texture = view_handle;
    }

    return S_OK;
  }

  if (res->kind == ResourceKind::Buffer) {
    aerogpu_shader_resource_buffer_binding binding{};
    binding.buffer = res->handle;
    binding.offset_bytes = 0;
    binding.size_bytes = 0; // "remaining bytes"
    binding.reserved0 = 0;

    uint64_t first_element = 0;
    uint64_t num_elements = 0;
    uint32_t view_format = kDxgiFormatUnknown;
    uint32_t bufferex_flags = 0;

    __if_exists(D3D11DDIARG_CREATESHADERRESOURCEVIEW::Desc) {
      // Best-effort decode of Buffer/BufferEx view ranges. If any fields are
      // missing in a given WDK vintage, fall back to whole-buffer binding.
      using DescT = std::remove_reference_t<decltype(pDesc->Desc)>;
      if constexpr (has_member_SrvDescFormat<DescT>::value) {
        view_format = static_cast<uint32_t>(pDesc->Desc.Format);
      }
      if constexpr (has_member_SrvDescViewDimension<DescT>::value) {
        const uint32_t dim = static_cast<uint32_t>(pDesc->Desc.ViewDimension);
        if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_BUFFER)) {
          if constexpr (has_member_SrvDescBuffer<DescT>::value) {
            using BufT = std::remove_reference_t<decltype(pDesc->Desc.Buffer)>;
            if constexpr (has_member_BufferViewFirstElement<BufT>::value && has_member_BufferViewNumElements<BufT>::value) {
              first_element = static_cast<uint64_t>(pDesc->Desc.Buffer.FirstElement);
              num_elements = static_cast<uint64_t>(pDesc->Desc.Buffer.NumElements);
            }
          }
        } else if (dim == static_cast<uint32_t>(D3D11_SRV_DIMENSION_BUFFEREX)) {
          if constexpr (has_member_SrvDescBufferEx<DescT>::value) {
            using BufT = std::remove_reference_t<decltype(pDesc->Desc.BufferEx)>;
            if constexpr (has_member_BufferViewFirstElement<BufT>::value && has_member_BufferViewNumElements<BufT>::value) {
              first_element = static_cast<uint64_t>(pDesc->Desc.BufferEx.FirstElement);
              num_elements = static_cast<uint64_t>(pDesc->Desc.BufferEx.NumElements);
            }
            if constexpr (has_member_BufferViewFlags<BufT>::value) {
              bufferex_flags = static_cast<uint32_t>(pDesc->Desc.BufferEx.Flags);
            }
          }
        }
      }
    }

    uint32_t elem_bytes = 0;
    if (bufferex_flags & static_cast<uint32_t>(D3D11_BUFFEREX_SRV_FLAG_RAW)) {
      elem_bytes = 4;
    }
    if (elem_bytes == 0 && view_format != kDxgiFormatUnknown) {
      elem_bytes = BytesPerElementForDxgiFormat(view_format);
    }
    if (elem_bytes == 0 && res->structure_stride_bytes != 0) {
      elem_bytes = res->structure_stride_bytes;
    }
    if (elem_bytes == 0) {
      elem_bytes = 4;
    }

    const uint64_t off_bytes = first_element * static_cast<uint64_t>(elem_bytes);
    const uint64_t sz_bytes = num_elements * static_cast<uint64_t>(elem_bytes);
    uint64_t clamped_off = std::min<uint64_t>(off_bytes, res->size_bytes);
    uint64_t clamped_sz = sz_bytes;
    if (clamped_sz != 0 && clamped_sz > res->size_bytes - clamped_off) {
      clamped_sz = res->size_bytes - clamped_off;
    }

    binding.offset_bytes = ClampU64ToU32(clamped_off);
    binding.size_bytes = ClampU64ToU32(clamped_sz);

    srv->resource = res;
    srv->kind = ShaderResourceView::Kind::Buffer;
    srv->texture = 0;
    srv->buffer = binding;
    return S_OK;
  }

  // Texture3D / TextureCube / etc are not supported by the bring-up UMD yet.
  return E_NOTIMPL;
}

void AEROGPU_APIENTRY DestroyShaderResourceView11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, Device>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (SupportsTextureViews(dev) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (view) {
    view->~ShaderResourceView();
    new (view) ShaderResourceView();
  }
}

struct UnorderedAccessView {
  aerogpu_unordered_access_buffer_binding buffer{};
  Resource* resource = nullptr;
};

SIZE_T AEROGPU_APIENTRY CalcPrivateUnorderedAccessViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEUNORDEREDACCESSVIEW*) {
  return sizeof(UnorderedAccessView);
}

HRESULT AEROGPU_APIENTRY CreateUnorderedAccessView11(D3D11DDI_HDEVICE hDevice,
                                                     const D3D11DDIARG_CREATEUNORDEREDACCESSVIEW* pDesc,
                                                     D3D11DDI_HUNORDEREDACCESSVIEW hView,
                                                     D3D11DDI_HRTUNORDEREDACCESSVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyUnorderedAccessView11 is safe even
  // if we reject the descriptor.
  auto* uav = new (hView.pDrvPrivate) UnorderedAccessView();
  uav->buffer = {};
  uav->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  D3D11DDI_HRESOURCE hRes{};
  __if_exists(D3D11DDIARG_CREATEUNORDEREDACCESSVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D11DDIARG_CREATEUNORDEREDACCESSVIEW::hDrvResource) {
    __if_exists(D3D11DDIARG_CREATEUNORDEREDACCESSVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hRes);
  if (!res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer) {
    return E_NOTIMPL;
  }

  uav->resource = res;
  uav->buffer.buffer = res->handle;
  uav->buffer.offset_bytes = 0;
  uav->buffer.size_bytes = 0;
  uav->buffer.initial_count = kD3DUavInitialCountNoChange;

  uint64_t first_element = 0;
  uint64_t num_elements = 0;
  uint32_t view_format = kDxgiFormatUnknown;
  uint32_t buffer_flags = 0;

  __if_exists(D3D11DDIARG_CREATEUNORDEREDACCESSVIEW::Desc) {
    using DescT = std::remove_reference_t<decltype(pDesc->Desc)>;
    if constexpr (has_member_SrvDescFormat<DescT>::value) {
      view_format = static_cast<uint32_t>(pDesc->Desc.Format);
    }
    if constexpr (has_member_SrvDescViewDimension<DescT>::value) {
      const uint32_t dim = static_cast<uint32_t>(pDesc->Desc.ViewDimension);
      if (dim == static_cast<uint32_t>(D3D11_UAV_DIMENSION_BUFFER)) {
        if constexpr (has_member_SrvDescBuffer<DescT>::value) {
          using BufT = std::remove_reference_t<decltype(pDesc->Desc.Buffer)>;
          if constexpr (has_member_BufferViewFirstElement<BufT>::value && has_member_BufferViewNumElements<BufT>::value) {
            first_element = static_cast<uint64_t>(pDesc->Desc.Buffer.FirstElement);
            num_elements = static_cast<uint64_t>(pDesc->Desc.Buffer.NumElements);
          }
          if constexpr (has_member_BufferViewFlags<BufT>::value) {
            buffer_flags = static_cast<uint32_t>(pDesc->Desc.Buffer.Flags);
          }
        }
      }
    }
  }

  uint32_t elem_bytes = 0;
  if (buffer_flags & static_cast<uint32_t>(D3D11_BUFFER_UAV_FLAG_RAW)) {
    elem_bytes = 4;
  }
  if (elem_bytes == 0 && view_format != kDxgiFormatUnknown) {
    elem_bytes = BytesPerElementForDxgiFormat(view_format);
  }
  if (elem_bytes == 0 && res->structure_stride_bytes != 0) {
    elem_bytes = res->structure_stride_bytes;
  }
  if (elem_bytes == 0) {
    elem_bytes = 4;
  }

  const uint64_t off_bytes = first_element * static_cast<uint64_t>(elem_bytes);
  const uint64_t sz_bytes = num_elements * static_cast<uint64_t>(elem_bytes);
  uint64_t clamped_off = std::min<uint64_t>(off_bytes, res->size_bytes);
  uint64_t clamped_sz = sz_bytes;
  if (clamped_sz != 0 && clamped_sz > res->size_bytes - clamped_off) {
    clamped_sz = res->size_bytes - clamped_off;
  }

  uav->buffer.offset_bytes = ClampU64ToU32(clamped_off);
  uav->buffer.size_bytes = ClampU64ToU32(clamped_sz);

  return S_OK;
}

void AEROGPU_APIENTRY DestroyUnorderedAccessView11(D3D11DDI_HDEVICE, D3D11DDI_HUNORDEREDACCESSVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D11DDI_HUNORDEREDACCESSVIEW, UnorderedAccessView>(hView);
  view->~UnorderedAccessView();
  new (view) UnorderedAccessView();
}
struct Sampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_LINEAR;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
};

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESAMPLER*) {
  return sizeof(Sampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_CREATESAMPLER* pDesc,
                                         D3D11DDI_HSAMPLER hSampler,
                                         D3D11DDI_HRTSAMPLER) {
  if (!hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the sampler so DestroySampler11 is safe even if we reject
  // the descriptor early.
  auto* sampler = new (hSampler.pDrvPrivate) Sampler();

  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(sampler);
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  sampler->handle = AllocateGlobalHandle(dev->adapter);
  if (!sampler->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still
    // probe Destroy* after a failed Create*.
    ResetObject(sampler);
    return E_FAIL;
  }

  InitSamplerFromCreateSamplerArg(sampler, pDesc);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    // Avoid leaving a stale non-zero handle in pDrvPrivate memory if the runtime
    // probes Destroy after a failed Create.
    ResetObject(sampler);
    SetError(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->sampler_handle = sampler->handle;
  cmd->filter = sampler->filter;
  cmd->address_u = sampler->address_u;
  cmd->address_v = sampler->address_v;
  cmd->address_w = sampler->address_w;
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HSAMPLER hSampler) {
  auto* sampler = FromHandle<D3D11DDI_HSAMPLER, Sampler>(hSampler);
  if (!sampler) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sampler);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sampler);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (sampler->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_sampler>(AEROGPU_CMD_DESTROY_SAMPLER);
    if (cmd) {
      cmd->sampler_handle = sampler->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(dev, E_OUTOFMEMORY);
    }
  }
  ResetObject(sampler);
}

// Shaders

static HRESULT CreateShaderCommon(D3D11DDI_HDEVICE hDevice,
                                  const void* pCode,
                                  SIZE_T code_size,
                                  Shader* out,
                                  uint32_t stage) {
  if (!hDevice.pDrvPrivate || !out || !pCode || code_size == 0) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  out->handle = AllocateGlobalHandle(dev->adapter);
  if (!out->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still
    // probe Destroy* after a failed Create*, and double-destruction would be
    // unsafe.
    ResetObject(out);
    return E_FAIL;
  }
  out->stage = stage;
  try {
    out->dxbc.resize(static_cast<size_t>(code_size));
  } catch (...) {
    // Ensure teardown paths do not emit DESTROY_SHADER for a handle that never
    // made it into the command stream (some runtimes may probe Destroy after a
    // failed Create).
    ResetObject(out);
    return E_OUTOFMEMORY;
  }
  std::memcpy(out->dxbc.data(), pCode, static_cast<size_t>(code_size));
  out->forced_ndc_z_valid = false;
  out->forced_ndc_z = 0.0f;
  if (stage == AEROGPU_SHADER_STAGE_VERTEX) {
    const uint32_t neg_half_bits = f32_bits(-0.5f);
    const size_t token_count = out->dxbc.size() / sizeof(uint32_t);
    for (size_t i = 0; i < token_count; ++i) {
      uint32_t token = 0;
      std::memcpy(&token, out->dxbc.data() + i * sizeof(uint32_t), sizeof(uint32_t));
      if (token == neg_half_bits) {
        out->forced_ndc_z_valid = true;
        out->forced_ndc_z = -0.5f;
        break;
      }
    }
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, out->dxbc.data(), out->dxbc.size());
  if (!cmd) {
    ResetObject(out);
    SetError(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->shader_handle = out->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(out->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

static void DestroyShaderCommon(Device* dev, Shader* sh) {
  if (!dev || !sh) {
    return;
  }
  if (sh->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    if (cmd) {
      cmd->shader_handle = sh->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(dev, E_OUTOFMEMORY);
    }
  }
  ResetObject(sh);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateVertexShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEVERTEXSHADER*) {
  return sizeof(Shader);
}

HRESULT AEROGPU_APIENTRY CreateVertexShader11(D3D11DDI_HDEVICE hDevice,
                                              const D3D11DDIARG_CREATEVERTEXSHADER* pDesc,
                                              D3D11DDI_HVERTEXSHADER hShader,
                                              D3D11DDI_HRTVERTEXSHADER) {
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* sh = new (hShader.pDrvPrivate) Shader();
  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_VERTEX);
}

void AEROGPU_APIENTRY DestroyVertexShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HVERTEXSHADER hShader) {
  auto* sh = FromHandle<D3D11DDI_HVERTEXSHADER, Shader>(hShader);
  if (!sh) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sh);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DestroyShaderCommon(dev, sh);
}

SIZE_T AEROGPU_APIENTRY CalcPrivatePixelShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEPIXELSHADER*) {
  return sizeof(Shader);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader11(D3D11DDI_HDEVICE hDevice,
                                             const D3D11DDIARG_CREATEPIXELSHADER* pDesc,
                                             D3D11DDI_HPIXELSHADER hShader,
                                             D3D11DDI_HRTPIXELSHADER) {
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* sh = new (hShader.pDrvPrivate) Shader();
  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_PIXEL);
}

void AEROGPU_APIENTRY DestroyPixelShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HPIXELSHADER hShader) {
  auto* sh = FromHandle<D3D11DDI_HPIXELSHADER, Shader>(hShader);
  if (!sh) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sh);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DestroyShaderCommon(dev, sh);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateGeometryShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEGEOMETRYSHADER*) {
  return sizeof(Shader);
}

template <typename FnPtr>
struct CalcPrivateGeometryShaderWithStreamOutputSizeImpl;

template <typename Ret, typename... Args>
struct CalcPrivateGeometryShaderWithStreamOutputSizeImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args...) {
    return static_cast<Ret>(sizeof(Shader));
  }
};

template <typename FnPtr>
struct CreateGeometryShaderWithStreamOutputImpl;

template <typename Ret, typename... Args>
struct CreateGeometryShaderWithStreamOutputImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    D3D11DDI_HDEVICE hDevice{};
    D3D11DDI_HGEOMETRYSHADER hShader{};
    const void* shader_code = nullptr;
    SIZE_T shader_code_size = 0;

    auto capture = [&](auto v) {
      using T = std::decay_t<decltype(v)>;
      if constexpr (std::is_same_v<T, D3D11DDI_HDEVICE>) {
        hDevice = v;
      } else if constexpr (std::is_same_v<T, D3D11DDI_HGEOMETRYSHADER>) {
        hShader = v;
      } else if constexpr (std::is_pointer_v<T>) {
        using Pointee = std::remove_pointer_t<T>;
        if constexpr (has_member_pShaderCode<Pointee>::value && has_member_ShaderCodeSize<Pointee>::value) {
          if (v) {
            shader_code = v->pShaderCode;
            shader_code_size = static_cast<SIZE_T>(v->ShaderCodeSize);
          }
        }
      }
    };
    (capture(args), ...);

    if (!hShader.pDrvPrivate) {
      return E_INVALIDARG;
    }
    auto* sh = new (hShader.pDrvPrivate) Shader();
    if (!hDevice.pDrvPrivate || !shader_code || shader_code_size == 0) {
      return E_INVALIDARG;
    }

    auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
    if (!dev) {
      ResetObject(sh);
      return E_FAIL;
    }

    std::lock_guard<std::mutex> lock(dev->mutex);

    return CreateShaderCommon(hDevice, shader_code, shader_code_size, sh, AEROGPU_SHADER_STAGE_GEOMETRY);
  }
};

HRESULT AEROGPU_APIENTRY CreateGeometryShader11(D3D11DDI_HDEVICE hDevice,
                                                const D3D11DDIARG_CREATEGEOMETRYSHADER* pDesc,
                                                D3D11DDI_HGEOMETRYSHADER hShader,
                                                D3D11DDI_HRTGEOMETRYSHADER) {
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* sh = new (hShader.pDrvPrivate) Shader();
  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_GEOMETRY);
}

void AEROGPU_APIENTRY DestroyGeometryShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HGEOMETRYSHADER hShader) {
  auto* sh = FromHandle<D3D11DDI_HGEOMETRYSHADER, Shader>(hShader);
  if (!sh) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sh);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DestroyShaderCommon(dev, sh);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateComputeShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATECOMPUTESHADER*) {
  return sizeof(Shader);
}

HRESULT AEROGPU_APIENTRY CreateComputeShader11(D3D11DDI_HDEVICE hDevice,
                                               const D3D11DDIARG_CREATECOMPUTESHADER* pDesc,
                                               D3D11DDI_HCOMPUTESHADER hShader,
                                               D3D11DDI_HRTCOMPUTESHADER) {
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* sh = new (hShader.pDrvPrivate) Shader();
  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_COMPUTE);
}

void AEROGPU_APIENTRY DestroyComputeShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HCOMPUTESHADER hShader) {
  auto* sh = FromHandle<D3D11DDI_HCOMPUTESHADER, Shader>(hShader);
  if (!sh) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sh);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DestroyShaderCommon(dev, sh);
}

// Input layout / element layout

SIZE_T AEROGPU_APIENTRY CalcPrivateElementLayoutSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEELEMENTLAYOUT*) {
  return sizeof(InputLayout);
}

HRESULT AEROGPU_APIENTRY CreateElementLayout11(D3D11DDI_HDEVICE hDevice,
                                               const D3D11DDIARG_CREATEELEMENTLAYOUT* pDesc,
                                               D3D11DDI_HELEMENTLAYOUT hLayout,
                                               D3D11DDI_HRTELEMENTLAYOUT) {
  if (!hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the layout object so DestroyElementLayout11 is safe even if
  // CreateElementLayout11 fails early.
  auto* layout = new (hLayout.pDrvPrivate) InputLayout();

  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(layout);
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  layout->handle = AllocateGlobalHandle(dev->adapter);
  if (!layout->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still
    // probe Destroy* after a failed Create*.
    ResetObject(layout);
    return E_FAIL;
  }

  const UINT elem_count = pDesc->NumElements;
  if (!pDesc->pVertexElements || elem_count == 0) {
    ResetObject(layout);
    return E_INVALIDARG;
  }

  const size_t header_size = sizeof(aerogpu_input_layout_blob_header);
  const size_t elem_size = sizeof(aerogpu_input_layout_element_dxgi);
  if (elem_count > (SIZE_MAX - header_size) / elem_size) {
    ResetObject(layout);
    return E_OUTOFMEMORY;
  }

  const size_t blob_size = header_size + static_cast<size_t>(elem_count) * elem_size;
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    ResetObject(layout);
    return E_OUTOFMEMORY;
  }

  auto* hdr = reinterpret_cast<aerogpu_input_layout_blob_header*>(layout->blob.data());
  hdr->magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
  hdr->version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
  hdr->element_count = elem_count;
  hdr->reserved0 = 0;

  auto* elems = reinterpret_cast<aerogpu_input_layout_element_dxgi*>(layout->blob.data() + header_size);
  for (UINT i = 0; i < elem_count; i++) {
    const auto& e = pDesc->pVertexElements[i];
    elems[i].semantic_name_hash = HashSemanticName(e.SemanticName);
    elems[i].semantic_index = e.SemanticIndex;
    elems[i].dxgi_format = static_cast<uint32_t>(e.Format);
    elems[i].input_slot = e.InputSlot;
    elems[i].aligned_byte_offset = e.AlignedByteOffset;
    elems[i].input_slot_class = e.InputSlotClass;
    elems[i].instance_data_step_rate = e.InstanceDataStepRate;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, layout->blob.data(), layout->blob.size());
  if (!cmd) {
    ResetObject(layout);
    SetError(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;

  return S_OK;
}

void AEROGPU_APIENTRY DestroyElementLayout11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HELEMENTLAYOUT hLayout) {
  auto* layout = FromHandle<D3D11DDI_HELEMENTLAYOUT, InputLayout>(hLayout);
  if (!layout) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(layout);
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    ResetObject(layout);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (layout->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    if (cmd) {
      cmd->input_layout_handle = layout->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(dev, E_OUTOFMEMORY);
    }
  }
  ResetObject(layout);
}

// Fixed-function state objects (accepted and bindable; conservative encoding).

static bool IsSupportedD3D11BlendFactor(uint32_t factor) {
  uint32_t out = 0;
  return D3dBlendFactorToAerogpu(factor, &out);
}

static bool IsSupportedD3D11BlendOp(uint32_t blend_op) {
  uint32_t out = 0;
  return D3dBlendOpToAerogpu(blend_op, &out);
}

template <typename RtBlendDescT>
static bool D3D11RtBlendDescEquivalent(const RtBlendDescT& a, const RtBlendDescT& b) {
  if (a.BlendEnable != b.BlendEnable) {
    return false;
  }
  if (a.RenderTargetWriteMask != b.RenderTargetWriteMask) {
    return false;
  }
  // Blend factors/ops are ignored when blending is disabled, so avoid rejecting
  // state solely due to differences in unused fields.
  if (!a.BlendEnable) {
    return true;
  }
  return a.SrcBlend == b.SrcBlend &&
         a.DestBlend == b.DestBlend &&
         a.BlendOp == b.BlendOp &&
         a.SrcBlendAlpha == b.SrcBlendAlpha &&
         a.DestBlendAlpha == b.DestBlendAlpha &&
         a.BlendOpAlpha == b.BlendOpAlpha;
}

template <typename RtBlendDescT>
static bool D3D11RtBlendDescRepresentableByAeroGpu(const RtBlendDescT& rt) {
  // Protocol only supports 4 bits of write mask.
  if ((static_cast<uint32_t>(rt.RenderTargetWriteMask) & ~kD3DColorWriteMaskAll) != 0) {
    return false;
  }
  if (!rt.BlendEnable) {
    // When BlendEnable=FALSE, blend factors/ops are ignored by the pipeline.
    // Do not reject states solely due to unsupported factors in this case.
    return true;
  }
  return IsSupportedD3D11BlendFactor(static_cast<uint32_t>(rt.SrcBlend)) &&
         IsSupportedD3D11BlendFactor(static_cast<uint32_t>(rt.DestBlend)) &&
         IsSupportedD3D11BlendFactor(static_cast<uint32_t>(rt.SrcBlendAlpha)) &&
         IsSupportedD3D11BlendFactor(static_cast<uint32_t>(rt.DestBlendAlpha)) &&
         IsSupportedD3D11BlendOp(static_cast<uint32_t>(rt.BlendOp)) &&
         IsSupportedD3D11BlendOp(static_cast<uint32_t>(rt.BlendOpAlpha));
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEBLENDSTATE*) {
  return sizeof(BlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState11(D3D11DDI_HDEVICE hDevice,
                                            const D3D11DDIARG_CREATEBLENDSTATE* pDesc,
                                            D3D11DDI_HBLENDSTATE hState,
                                            D3D11DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* state = new (hState.pDrvPrivate) BlendState();
  const auto set_defaults = [&]() {
    state->blend_enable = 0;
    state->src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
    state->dest_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
    state->blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
    state->src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
    state->dest_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
    state->blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
    state->render_target_write_mask = kD3DColorWriteMaskAll;
  };
  const auto fail = [&](HRESULT hr) -> HRESULT {
    // The runtime does not necessarily call DestroyBlendState on failed creates.
    // Ensure we run the destructor so future additions to BlendState (handles,
    // allocations, etc.) don't leak on error paths.
    state->~BlendState();
    new (state) BlendState();
    set_defaults();
    return hr;
  };
  set_defaults();

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATEBLENDSTATE::RenderTarget) {
    __if_exists(D3D11DDIARG_CREATEBLENDSTATE::AlphaToCoverageEnable) {
      if (pDesc->AlphaToCoverageEnable) {
        return fail(E_NOTIMPL);
      }
    }
    bool independent = false;
    __if_exists(D3D11DDIARG_CREATEBLENDSTATE::IndependentBlendEnable) {
      independent = pDesc->IndependentBlendEnable ? true : false;
    }
    const auto& rt0 = pDesc->RenderTarget[0];
    if (independent) {
      for (UINT i = 1; i < D3D11_SIMULTANEOUS_RENDER_TARGET_COUNT; ++i) {
        if (!D3D11RtBlendDescEquivalent(pDesc->RenderTarget[i], rt0)) {
          return fail(E_NOTIMPL);
        }
      }
    }
    if (!D3D11RtBlendDescRepresentableByAeroGpu(rt0)) {
      return fail(E_NOTIMPL);
    }
    state->blend_enable = rt0.BlendEnable ? 1u : 0u;
    state->src_blend = static_cast<uint32_t>(rt0.SrcBlend);
    state->dest_blend = static_cast<uint32_t>(rt0.DestBlend);
    state->blend_op = static_cast<uint32_t>(rt0.BlendOp);
    state->src_blend_alpha = static_cast<uint32_t>(rt0.SrcBlendAlpha);
    state->dest_blend_alpha = static_cast<uint32_t>(rt0.DestBlendAlpha);
    state->blend_op_alpha = static_cast<uint32_t>(rt0.BlendOpAlpha);
    state->render_target_write_mask = static_cast<uint32_t>(rt0.RenderTargetWriteMask);
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEBLENDSTATE::BlendDesc) {
      // Copy so `decltype(desc)` is a value type (required for MSVC __if_exists
      // member probes on some WDK vintages).
      const auto desc = pDesc->BlendDesc;
      __if_exists(decltype(desc)::AlphaToCoverageEnable) {
        if (desc.AlphaToCoverageEnable) {
          return fail(E_NOTIMPL);
        }
      }
      const bool independent = desc.IndependentBlendEnable ? true : false;
      const auto& rt0 = desc.RenderTarget[0];
      if (independent) {
        for (UINT i = 1; i < D3D11_SIMULTANEOUS_RENDER_TARGET_COUNT; ++i) {
          if (!D3D11RtBlendDescEquivalent(desc.RenderTarget[i], rt0)) {
            return fail(E_NOTIMPL);
          }
        }
      }
      if (!D3D11RtBlendDescRepresentableByAeroGpu(rt0)) {
        return fail(E_NOTIMPL);
      }
      state->blend_enable = rt0.BlendEnable ? 1u : 0u;
      state->src_blend = static_cast<uint32_t>(rt0.SrcBlend);
      state->dest_blend = static_cast<uint32_t>(rt0.DestBlend);
      state->blend_op = static_cast<uint32_t>(rt0.BlendOp);
      state->src_blend_alpha = static_cast<uint32_t>(rt0.SrcBlendAlpha);
      state->dest_blend_alpha = static_cast<uint32_t>(rt0.DestBlendAlpha);
      state->blend_op_alpha = static_cast<uint32_t>(rt0.BlendOpAlpha);
      state->render_target_write_mask = static_cast<uint32_t>(rt0.RenderTargetWriteMask);
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEBLENDSTATE::Desc) {
      // Copy so `decltype(desc)` is a value type (required for MSVC __if_exists
      // member probes on some WDK vintages).
      const auto desc = pDesc->Desc;
      __if_exists(decltype(desc)::AlphaToCoverageEnable) {
        if (desc.AlphaToCoverageEnable) {
          return fail(E_NOTIMPL);
        }
      }
      const bool independent = desc.IndependentBlendEnable ? true : false;
      const auto& rt0 = desc.RenderTarget[0];
      if (independent) {
        for (UINT i = 1; i < D3D11_SIMULTANEOUS_RENDER_TARGET_COUNT; ++i) {
          if (!D3D11RtBlendDescEquivalent(desc.RenderTarget[i], rt0)) {
            return fail(E_NOTIMPL);
          }
        }
      }
      if (!D3D11RtBlendDescRepresentableByAeroGpu(rt0)) {
        return fail(E_NOTIMPL);
      }
      state->blend_enable = rt0.BlendEnable ? 1u : 0u;
      state->src_blend = static_cast<uint32_t>(rt0.SrcBlend);
      state->dest_blend = static_cast<uint32_t>(rt0.DestBlend);
      state->blend_op = static_cast<uint32_t>(rt0.BlendOp);
      state->src_blend_alpha = static_cast<uint32_t>(rt0.SrcBlendAlpha);
      state->dest_blend_alpha = static_cast<uint32_t>(rt0.DestBlendAlpha);
      state->blend_op_alpha = static_cast<uint32_t>(rt0.BlendOpAlpha);
      state->render_target_write_mask = static_cast<uint32_t>(rt0.RenderTargetWriteMask);
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEBLENDSTATE::pBlendDesc) {
      if (pDesc->pBlendDesc) {
        // Copy so `decltype(desc)` is a value type (required for MSVC __if_exists
        // member probes on some WDK vintages).
        const auto desc = *pDesc->pBlendDesc;
        __if_exists(decltype(desc)::AlphaToCoverageEnable) {
          if (desc.AlphaToCoverageEnable) {
            return fail(E_NOTIMPL);
          }
        }
        const bool independent = desc.IndependentBlendEnable ? true : false;
        const auto& rt0 = desc.RenderTarget[0];
        if (independent) {
          for (UINT i = 1; i < D3D11_SIMULTANEOUS_RENDER_TARGET_COUNT; ++i) {
            if (!D3D11RtBlendDescEquivalent(desc.RenderTarget[i], rt0)) {
              return fail(E_NOTIMPL);
            }
          }
        }
        if (!D3D11RtBlendDescRepresentableByAeroGpu(rt0)) {
          return fail(E_NOTIMPL);
        }
        state->blend_enable = rt0.BlendEnable ? 1u : 0u;
        state->src_blend = static_cast<uint32_t>(rt0.SrcBlend);
        state->dest_blend = static_cast<uint32_t>(rt0.DestBlend);
        state->blend_op = static_cast<uint32_t>(rt0.BlendOp);
        state->src_blend_alpha = static_cast<uint32_t>(rt0.SrcBlendAlpha);
        state->dest_blend_alpha = static_cast<uint32_t>(rt0.DestBlendAlpha);
        state->blend_op_alpha = static_cast<uint32_t>(rt0.BlendOpAlpha);
        state->render_target_write_mask = static_cast<uint32_t>(rt0.RenderTargetWriteMask);
        filled = true;
      }
    }
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState11(D3D11DDI_HDEVICE, D3D11DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* state = FromHandle<D3D11DDI_HBLENDSTATE, BlendState>(hState);
  state->~BlendState();
  new (state) BlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERASTERIZERSTATE*) {
  return sizeof(RasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState11(D3D11DDI_HDEVICE hDevice,
                                                 const D3D11DDIARG_CREATERASTERIZERSTATE* pDesc,
                                                 D3D11DDI_HRASTERIZERSTATE hState,
                                                 D3D11DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* state = new (hState.pDrvPrivate) RasterizerState();
  state->fill_mode = static_cast<uint32_t>(D3D11_FILL_SOLID);
  state->cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
  state->front_ccw = 0u;
  state->scissor_enable = 0u;
  state->depth_bias = 0;
  state->depth_clip_enable = 1u;

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::CullMode) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::FillMode) {
      state->fill_mode = static_cast<uint32_t>(pDesc->FillMode);
    }
    state->cull_mode = static_cast<uint32_t>(pDesc->CullMode);
    state->front_ccw = pDesc->FrontCounterClockwise ? 1u : 0u;
    state->scissor_enable = pDesc->ScissorEnable ? 1u : 0u;
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::DepthBias) {
      state->depth_bias = static_cast<int32_t>(pDesc->DepthBias);
    }
    state->depth_clip_enable = pDesc->DepthClipEnable ? 1u : 0u;
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::RasterizerDesc) {
      const auto& desc = pDesc->RasterizerDesc;
      state->fill_mode = static_cast<uint32_t>(desc.FillMode);
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_bias = static_cast<int32_t>(desc.DepthBias);
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::Desc) {
      const auto& desc = pDesc->Desc;
      state->fill_mode = static_cast<uint32_t>(desc.FillMode);
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_bias = static_cast<int32_t>(desc.DepthBias);
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::pRasterizerDesc) {
      if (pDesc->pRasterizerDesc) {
        const auto& desc = *pDesc->pRasterizerDesc;
        state->fill_mode = static_cast<uint32_t>(desc.FillMode);
        state->cull_mode = static_cast<uint32_t>(desc.CullMode);
        state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
        state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
        state->depth_bias = static_cast<int32_t>(desc.DepthBias);
        state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
        filled = true;
      }
    }
  }
  switch (static_cast<D3D11_FILL_MODE>(state->fill_mode)) {
    case D3D11_FILL_SOLID:
    case D3D11_FILL_WIREFRAME:
      break;
    default:
      state->~RasterizerState();
      new (state) RasterizerState();
      state->fill_mode = static_cast<uint32_t>(D3D11_FILL_SOLID);
      state->cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
      state->front_ccw = 0u;
      state->scissor_enable = 0u;
      state->depth_bias = 0;
      state->depth_clip_enable = 1u;
      return E_NOTIMPL;
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState11(D3D11DDI_HDEVICE, D3D11DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* state = FromHandle<D3D11DDI_HRASTERIZERSTATE, RasterizerState>(hState);
  state->~RasterizerState();
  new (state) RasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEDEPTHSTENCILSTATE*) {
  return sizeof(DepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState11(D3D11DDI_HDEVICE hDevice,
                                                   const D3D11DDIARG_CREATEDEPTHSTENCILSTATE* pDesc,
                                                   D3D11DDI_HDEPTHSTENCILSTATE hState,
                                                   D3D11DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* state = new (hState.pDrvPrivate) DepthStencilState();
  // Defaults matching the D3D11 default depth state.
  state->depth_enable = 1u;
  state->depth_write_mask = static_cast<uint32_t>(D3D11_DEPTH_WRITE_MASK_ALL);
  state->depth_func = static_cast<uint32_t>(D3D11_COMPARISON_LESS);
  state->stencil_enable = 0u;
  state->stencil_read_mask = kD3DStencilMaskAll;
  state->stencil_write_mask = kD3DStencilMaskAll;

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::DepthEnable) {
    state->depth_enable = pDesc->DepthEnable ? 1u : 0u;
    state->depth_write_mask = static_cast<uint32_t>(pDesc->DepthWriteMask);
    state->depth_func = static_cast<uint32_t>(pDesc->DepthFunc);
    state->stencil_enable = pDesc->StencilEnable ? 1u : 0u;
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::StencilReadMask) {
      state->stencil_read_mask = static_cast<uint8_t>(pDesc->StencilReadMask);
    }
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::StencilWriteMask) {
      state->stencil_write_mask = static_cast<uint8_t>(pDesc->StencilWriteMask);
    }
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::DepthStencilDesc) {
      // Copy the descriptor so `decltype(desc)` is a value type (not a reference),
      // which is required for `__if_exists(decltype(desc)::...)` member probes on MSVC.
      const auto desc = pDesc->DepthStencilDesc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      __if_exists(decltype(desc)::StencilReadMask) {
        state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
      }
      __if_exists(decltype(desc)::StencilWriteMask) {
        state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
      }
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::Desc) {
      // Copy the descriptor so `decltype(desc)` is a value type (not a reference),
      // which is required for `__if_exists(decltype(desc)::...)` member probes on MSVC.
      const auto desc = pDesc->Desc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      __if_exists(decltype(desc)::StencilReadMask) {
        state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
      }
      __if_exists(decltype(desc)::StencilWriteMask) {
        state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
      }
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::pDepthStencilDesc) {
      if (pDesc->pDepthStencilDesc) {
        // Copy the descriptor so `decltype(desc)` is a value type (not a reference),
        // which is required for `__if_exists(decltype(desc)::...)` member probes on MSVC.
        const auto desc = *pDesc->pDepthStencilDesc;
        state->depth_enable = desc.DepthEnable ? 1u : 0u;
        state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
        state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
        state->stencil_enable = desc.StencilEnable ? 1u : 0u;
        __if_exists(decltype(desc)::StencilReadMask) {
          state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
        }
        __if_exists(decltype(desc)::StencilWriteMask) {
          state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
        }
        filled = true;
      }
    }
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState11(D3D11DDI_HDEVICE, D3D11DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* state = FromHandle<D3D11DDI_HDEPTHSTENCILSTATE, DepthStencilState>(hState);
  state->~DepthStencilState();
  new (state) DepthStencilState();
}

// -------------------------------------------------------------------------------------------------
// Immediate context DDIs (binding + draws)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY IaSetInputLayout11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HELEMENTLAYOUT hLayout) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  InputLayout* layout = hLayout.pDrvPrivate ? FromHandle<D3D11DDI_HELEMENTLAYOUT, InputLayout>(hLayout) : nullptr;
  const aerogpu_handle_t handle = layout ? layout->handle : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  dev->current_input_layout_obj = layout;
  dev->current_input_layout = handle;

  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetVertexBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                           UINT StartSlot,
                                           UINT NumBuffers,
                                           const D3D11DDI_HRESOURCE* phBuffers,
                                           const UINT* pStrides,
                                           const UINT* pOffsets) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Like D3D10, some runtime paths use NumBuffers==0 as shorthand for unbinding
  // vertex buffers from StartSlot..end of the slot range.
  UINT bind_count = NumBuffers;
  if (bind_count != 0) {
    if (!phBuffers || !pStrides || !pOffsets) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (StartSlot >= kD3D11IaVertexInputResourceSlotCount) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (bind_count > (kD3D11IaVertexInputResourceSlotCount - StartSlot)) {
      SetError(dev, E_INVALIDARG);
      return;
    }
  } else {
    if (StartSlot > kD3D11IaVertexInputResourceSlotCount) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (StartSlot == kD3D11IaVertexInputResourceSlotCount) {
      return;
    }
    bind_count = kD3D11IaVertexInputResourceSlotCount - StartSlot;
  }

  std::array<aerogpu_vertex_buffer_binding, kD3D11IaVertexInputResourceSlotCount> bindings{};
  std::array<Resource*, kD3D11IaVertexInputResourceSlotCount> new_resources{};
  std::array<uint32_t, kD3D11IaVertexInputResourceSlotCount> new_strides{};
  std::array<uint32_t, kD3D11IaVertexInputResourceSlotCount> new_offsets{};
  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);

    aerogpu_vertex_buffer_binding b{};
    Resource* vb_res = nullptr;
    if (NumBuffers != 0) {
      vb_res = phBuffers[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[i]) : nullptr;
      if (vb_res && vb_res->kind != ResourceKind::Buffer) {
        SetError(dev, E_INVALIDARG);
        return;
      }
      b.buffer = vb_res ? vb_res->handle : 0;
      b.stride_bytes = pStrides[i];
      b.offset_bytes = pOffsets[i];
    } else {
      b.buffer = 0;
      b.stride_bytes = 0;
      b.offset_bytes = 0;
    }
    b.reserved0 = 0;
    bindings[i] = b;
    new_resources[i] = vb_res;
    new_strides[i] = b.stride_bytes;
    new_offsets[i] = b.offset_bytes;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, bindings.data(), static_cast<size_t>(bind_count) * sizeof(bindings[0]));
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->start_slot = StartSlot;
  cmd->buffer_count = bind_count;

  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);
    if (slot < dev->current_vb_resources.size()) {
      dev->current_vb_resources[slot] = new_resources[i];
      dev->current_vb_strides_bytes[slot] = new_strides[i];
      dev->current_vb_offsets_bytes[slot] = new_offsets[i];
    }
    if (slot == 0) {
      dev->current_vb = new_resources[i];
      dev->current_vb_stride_bytes = new_strides[i];
      dev->current_vb_offset_bytes = new_offsets[i];
    }
  }
}

void AEROGPU_APIENTRY IaSetIndexBuffer11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hBuffer, DXGI_FORMAT format, UINT offset) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  Resource* ib = hBuffer.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hBuffer) : nullptr;
  if (ib && ib->kind != ResourceKind::Buffer) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  uint32_t offset_bytes = offset;
  const uint32_t dxgi_format = static_cast<uint32_t>(format);
  uint32_t stored_dxgi_format = kDxgiFormatUnknown;
  uint32_t aerogpu_format = AEROGPU_INDEX_FORMAT_UINT16;
  if (ib) {
    if (dxgi_format != kDxgiFormatR16Uint && dxgi_format != kDxgiFormatR32Uint) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    const uint32_t alignment = (dxgi_format == kDxgiFormatR32Uint) ? 4u : 2u;
    if ((offset_bytes % alignment) != 0) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    stored_dxgi_format = dxgi_format;
    aerogpu_format = dxgi_index_format_to_aerogpu(dxgi_format);
  } else {
    // D3D11 requires Format=UNKNOWN and Offset=0 when unbinding the index buffer.
    // Be permissive and treat all NULL-buffer bindings as an unbind regardless of
    // the format/offset values the runtime passes.
    offset_bytes = 0;
    stored_dxgi_format = kDxgiFormatUnknown;
    aerogpu_format = AEROGPU_INDEX_FORMAT_UINT16;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  dev->current_ib = ib;
  dev->current_ib_format = stored_dxgi_format;
  dev->current_ib_offset_bytes = offset_bytes;

  cmd->buffer = ib ? ib->handle : 0;
  cmd->format = aerogpu_format;
  cmd->offset_bytes = offset_bytes;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetTopology11(D3D11DDI_HDEVICECONTEXT hCtx, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint32_t topo = static_cast<uint32_t>(topology);
  (void)SetPrimitiveTopologyLocked(dev, topo, [&](HRESULT hr) { SetError(dev, hr); });
}

void AEROGPU_APIENTRY VsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                    D3D11DDI_HVERTEXSHADER hShader,
                                    const D3D11DDI_HCLASSINSTANCE*,
                                    UINT) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  Shader* sh = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HVERTEXSHADER, Shader>(hShader) : nullptr;
  const aerogpu_handle_t new_vs = sh ? sh->handle : 0;
  const bool new_forced_z_valid = sh ? sh->forced_ndc_z_valid : false;
  const float new_forced_z = (sh && sh->forced_ndc_z_valid) ? sh->forced_ndc_z : 0.0f;

  if (!EmitBindShadersCmdLocked(dev, new_vs, dev->current_ps, dev->current_cs, dev->current_gs)) {
    return;
  }

  dev->current_vs = new_vs;
  dev->current_vs_forced_z_valid = new_forced_z_valid;
  dev->current_vs_forced_z = new_forced_z_valid ? new_forced_z : 0.0f;
}

void AEROGPU_APIENTRY PsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                    D3D11DDI_HPIXELSHADER hShader,
                                    const D3D11DDI_HCLASSINSTANCE*,
                                    UINT) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const aerogpu_handle_t new_ps =
      hShader.pDrvPrivate ? FromHandle<D3D11DDI_HPIXELSHADER, Shader>(hShader)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, new_ps, dev->current_cs, dev->current_gs)) {
    return;
  }
  dev->current_ps = new_ps;
}

void AEROGPU_APIENTRY GsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                     D3D11DDI_HGEOMETRYSHADER hShader,
                                     const D3D11DDI_HCLASSINSTANCE*,
                                     UINT) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  const aerogpu_handle_t new_gs =
      hShader.pDrvPrivate ? FromHandle<D3D11DDI_HGEOMETRYSHADER, Shader>(hShader)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, dev->current_cs, new_gs)) {
    return;
  }
  dev->current_gs = new_gs;
}

static void SetConstantBuffers11Locked(Device* dev,
                                       uint32_t shader_stage,
                                       UINT start_slot,
                                       UINT buffer_count,
                                       const D3D11DDI_HRESOURCE* phBuffers,
                                       const UINT* pFirstConstant,
                                       const UINT* pNumConstants) {
  if (!dev || buffer_count == 0) {
    return;
  }
  if (start_slot >= kMaxConstantBufferSlots) {
    return;
  }
  if (start_slot + buffer_count > kMaxConstantBufferSlots) {
    buffer_count = kMaxConstantBufferSlots - start_slot;
  }

  aerogpu_constant_buffer_binding* table = ConstantBufferTableForStage(dev, shader_stage);
  if (!table) {
    return;
  }

  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> bindings{};
  std::array<Resource*, kMaxConstantBufferSlots> resources{};
  Resource** bound_resources = nullptr;
  if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
    bound_resources = dev->current_vs_cbs.data();
  } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
    bound_resources = dev->current_ps_cbs.data();
  } else if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    bound_resources = dev->current_gs_cbs.data();
  } else if (shader_stage == AEROGPU_SHADER_STAGE_COMPUTE) {
    bound_resources = dev->current_cs_cbs.data();
  }
  bool changed = false;
  for (UINT i = 0; i < buffer_count; i++) {
    aerogpu_constant_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.reserved0 = 0;

    Resource* buf = (phBuffers && phBuffers[i].pDrvPrivate) ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[i]) : nullptr;
    Resource* buf_res = nullptr;
    if (buf && buf->kind == ResourceKind::Buffer) {
      buf_res = buf;
      uint64_t offset_bytes = 0;
      uint64_t size_bytes = buf->size_bytes;
      if (pFirstConstant && pNumConstants) {
        offset_bytes = static_cast<uint64_t>(pFirstConstant[i]) * 16ull;
        size_bytes = static_cast<uint64_t>(pNumConstants[i]) * 16ull;
        if (size_bytes == 0) {
          size_bytes = buf->size_bytes;
        }
      }

      if (offset_bytes > buf->size_bytes) {
        offset_bytes = buf->size_bytes;
      }
      if (size_bytes > buf->size_bytes - offset_bytes) {
        size_bytes = buf->size_bytes - offset_bytes;
      }

      b.buffer = buf->handle;
      b.offset_bytes = ClampU64ToU32(offset_bytes);
      b.size_bytes = ClampU64ToU32(size_bytes);
    }

    bindings[i] = b;
    resources[i] = buf_res;
    if (!changed) {
      const aerogpu_constant_buffer_binding& current = table[start_slot + i];
      changed = current.buffer != b.buffer || current.offset_bytes != b.offset_bytes || current.size_bytes != b.size_bytes ||
                current.reserved0 != b.reserved0;
    }
  }

  if (!changed) {
    return;
  }

  if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                          shader_stage,
                                                          static_cast<uint32_t>(start_slot),
                                                          static_cast<uint32_t>(buffer_count),
                                                          bindings.data(),
                                                          [&](HRESULT hr) { SetError(dev, hr); })) {
    return;
  }

  if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    AEROGPU_D3D10_11_LOG("emit GS SetConstantBuffers start=%u count=%u",
                         static_cast<unsigned>(start_slot),
                         static_cast<unsigned>(buffer_count));
  }

  for (UINT i = 0; i < buffer_count; i++) {
    table[start_slot + i] = bindings[i];
    if (bound_resources) {
      bound_resources[start_slot + i] = resources[i];
    }
  }
}

void AEROGPU_APIENTRY VsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                              const UINT* pFirstConstant,
                                              const UINT* pNumConstants) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetConstantBuffers11Locked(dev, AEROGPU_SHADER_STAGE_VERTEX, StartSlot, NumBuffers, phBuffers, pFirstConstant, pNumConstants);
  if (StartSlot == 0 && NumBuffers >= 1) {
    Resource* buf = (phBuffers && phBuffers[0].pDrvPrivate) ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[0]) : nullptr;
    const aerogpu_handle_t expected = (buf && buf->kind == ResourceKind::Buffer) ? buf->handle : 0;
    if (dev->vs_constant_buffers[0].buffer == expected) {
      dev->current_vs_cb0 = expected ? buf : nullptr;
      dev->current_vs_cb0_first_constant = pFirstConstant ? pFirstConstant[0] : 0;
      dev->current_vs_cb0_num_constants = pNumConstants ? pNumConstants[0] : 0;
    }
  }
}

void AEROGPU_APIENTRY PsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                              const UINT* pFirstConstant,
                                              const UINT* pNumConstants) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetConstantBuffers11Locked(dev, AEROGPU_SHADER_STAGE_PIXEL, StartSlot, NumBuffers, phBuffers, pFirstConstant, pNumConstants);
  if (StartSlot == 0 && NumBuffers >= 1) {
    Resource* buf = (phBuffers && phBuffers[0].pDrvPrivate) ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[0]) : nullptr;
    const aerogpu_handle_t expected = (buf && buf->kind == ResourceKind::Buffer) ? buf->handle : 0;
    if (dev->ps_constant_buffers[0].buffer == expected) {
      dev->current_ps_cb0 = expected ? buf : nullptr;
      dev->current_ps_cb0_first_constant = pFirstConstant ? pFirstConstant[0] : 0;
      dev->current_ps_cb0_num_constants = pNumConstants ? pNumConstants[0] : 0;
    }
  }
}
void AEROGPU_APIENTRY GsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT* pFirstConstant,
                                             const UINT* pNumConstants) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetConstantBuffers11Locked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, StartSlot, NumBuffers, phBuffers, pFirstConstant, pNumConstants);
}

template <typename FnPtr>
struct SoSetTargetsThunk;

template <typename... Args>
struct SoSetTargetsThunk<void(AEROGPU_APIENTRY*)(Args...)> {
  static void AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
  }
};

// Stream-output is unsupported for bring-up. Treat unbind (all-null handles) as
// a no-op but report E_NOTIMPL if an app attempts to bind real targets.
template <typename TargetsPtr, typename... Tail>
struct SoSetTargetsThunk<void(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT, UINT, TargetsPtr, Tail...)> {
  static void AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumTargets, TargetsPtr phTargets, Tail... tail) {
    ((void)tail, ...);
    if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phTargets, NumTargets)) {
      return;
    }
    SetError(DeviceFromContext(hCtx), E_NOTIMPL);
  }
};

template <typename FnPtr>
struct SetPredicationThunk;

template <typename... Args>
struct SetPredicationThunk<void(AEROGPU_APIENTRY*)(Args...)> {
  static void AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
  }
};

// Predication is optional. Treat clearing/unbinding as a no-op but report
// E_NOTIMPL when a non-null predicate is set.
template <typename PredicateHandle, typename... Tail>
struct SetPredicationThunk<void(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT, PredicateHandle, Tail...)> {
  static void AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx, PredicateHandle hPredicate, Tail... tail) {
    ((void)tail, ...);
    if (!hCtx.pDrvPrivate || !hPredicate.pDrvPrivate) {
      return;
    }
    SetError(DeviceFromContext(hCtx), E_NOTIMPL);
  }
};

// Tessellation stages are unsupported in the current FL10_0 bring-up
// implementation. These entrypoints must behave like no-ops when
// clearing/unbinding (runtime ClearState), but should still report E_NOTIMPL when
// an app attempts to bind real state.
void AEROGPU_APIENTRY HsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                    D3D11DDI_HHULLSHADER hShader,
                                    const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HDOMAINSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                    D3D11DDI_HCOMPUTESHADER hShader,
                                    const D3D11DDI_HCLASSINSTANCE*,
                                    UINT) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  Shader* sh = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HCOMPUTESHADER, Shader>(hShader) : nullptr;
  const aerogpu_handle_t new_cs = sh ? sh->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, new_cs, dev->current_gs)) {
    return;
  }
  dev->current_cs = new_cs;
}

void AEROGPU_APIENTRY CsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT* pFirstConstant,
                                             const UINT* pNumConstants) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetConstantBuffers11Locked(dev,
                             AEROGPU_SHADER_STAGE_COMPUTE,
                             StartSlot,
                             NumBuffers,
                             phBuffers,
                             pFirstConstant,
                              pNumConstants);
}

static void SetShaderResources11Locked(Device* dev,
                                       uint32_t shader_stage,
                                       UINT start_slot,
                                       UINT view_count,
                                       const D3D11DDI_HSHADERRESOURCEVIEW* phViews);

static void SetSamplers11Locked(Device* dev,
                                uint32_t shader_stage,
                                UINT start_slot,
                                UINT sampler_count,
                                const D3D11DDI_HSAMPLER* phSamplers);

void AEROGPU_APIENTRY CsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || NumViews == 0) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetShaderResources11Locked(dev, AEROGPU_SHADER_STAGE_COMPUTE, StartSlot, NumViews, phViews);
}

void AEROGPU_APIENTRY CsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT StartSlot,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplers11Locked(dev, AEROGPU_SHADER_STAGE_COMPUTE, StartSlot, NumSamplers, phSamplers);
}

void AEROGPU_APIENTRY CsSetUnorderedAccessViews11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                   UINT StartSlot,
                                                   UINT NumUavs,
                                                   const D3D11DDI_HUNORDEREDACCESSVIEW* phUavs,
                                                   const UINT* pUAVInitialCounts) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || NumUavs == 0) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (StartSlot >= kMaxUavSlots) {
    return;
  }
  if (StartSlot + NumUavs > kMaxUavSlots) {
    NumUavs = kMaxUavSlots - StartSlot;
  }

  std::array<aerogpu_unordered_access_buffer_binding, kMaxUavSlots> bindings{};
  std::array<Resource*, kMaxUavSlots> resources{};
  bool changed = false;

  for (UINT i = 0; i < NumUavs; i++) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);
    aerogpu_unordered_access_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.initial_count = kD3DUavInitialCountNoChange;

    Resource* res = nullptr;
    if (phUavs && phUavs[i].pDrvPrivate) {
      auto* view = FromHandle<D3D11DDI_HUNORDEREDACCESSVIEW, UnorderedAccessView>(phUavs[i]);
      if (view) {
        res = view->resource;
        b = view->buffer;
        b.buffer = res ? res->handle : b.buffer;
      }
    }
    // D3D11 ignores initial counts for null UAV bindings. Preserve the sentinel
    // kD3DUavInitialCountNoChange in that case so the command stream does not carry a
    // potentially uninitialized app-provided value.
    if (pUAVInitialCounts && b.buffer) {
      b.initial_count = pUAVInitialCounts[i];
    }

    if (b.buffer) {
      // D3D11 hazards: unbind from SRVs and other outputs when binding as UAV.
      UnbindResourceFromSrvsLocked(dev, b.buffer, res);
      (void)UnbindResourceFromRenderTargetsLocked(dev, b.buffer, res);
      UnbindResourceFromUavsLocked(dev, b.buffer, res, slot);
    }

    bindings[i] = b;
    resources[i] = res;
    if (!changed) {
      const aerogpu_unordered_access_buffer_binding& cur = dev->cs_uavs[slot];
      changed = cur.buffer != b.buffer || cur.offset_bytes != b.offset_bytes || cur.size_bytes != b.size_bytes ||
                cur.initial_count != b.initial_count;
    }
  }

  if (!changed) {
    return;
  }

  if (!BindUnorderedAccessBuffersRangeLocked(dev,
                                             AEROGPU_SHADER_STAGE_COMPUTE,
                                             static_cast<uint32_t>(StartSlot),
                                             static_cast<uint32_t>(NumUavs),
                                             bindings.data())) {
    return;
  }

  for (UINT i = 0; i < NumUavs; i++) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);
    dev->cs_uavs[slot] = bindings[i];
    if (slot < dev->current_cs_uavs.size()) {
      dev->current_cs_uavs[slot] = resources[i];
    }
  }
}

static void SetShaderResources11Locked(Device* dev,
                                       uint32_t shader_stage,
                                       UINT start_slot,
                                       UINT view_count,
                                       const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!dev || view_count == 0) {
    return;
  }
  if (start_slot >= kMaxShaderResourceSlots) {
    return;
  }
  if (start_slot + view_count > kMaxShaderResourceSlots) {
    view_count = kMaxShaderResourceSlots - start_slot;
  }

  aerogpu_handle_t* tex_table = ShaderResourceTableForStage(dev, shader_stage);
  aerogpu_shader_resource_buffer_binding* buf_table = ShaderResourceBufferTableForStage(dev, shader_stage);
  Resource** bound_tex_resources = CurrentTextureSrvsForStage(dev, shader_stage);
  Resource** bound_buf_resources = CurrentBufferSrvsForStage(dev, shader_stage);

  std::array<aerogpu_shader_resource_buffer_binding, kMaxShaderResourceSlots> buf_bindings{};
  std::array<Resource*, kMaxShaderResourceSlots> buf_resources{};
  bool buf_changed = false;

  for (UINT i = 0; i < view_count; i++) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);

    aerogpu_handle_t tex = 0;
    Resource* tex_res = nullptr;
    aerogpu_shader_resource_buffer_binding buf{};
    buf.buffer = 0;
    buf.offset_bytes = 0;
    buf.size_bytes = 0;
    buf.reserved0 = 0;
    Resource* buf_res = nullptr;

    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(phViews[i]);
      if (view) {
        if (view->kind == ShaderResourceView::Kind::Texture2D) {
          tex_res = view->resource;
          // `view->texture` is a protocol texture view handle when non-zero. When
          // it is 0, this SRV is trivial (full-resource) and should bind the
          // underlying resource handle, which can change via RotateResourceIdentities.
          tex = view->texture ? view->texture : (tex_res ? tex_res->handle : 0);
        } else if (view->kind == ShaderResourceView::Kind::Buffer) {
          buf_res = view->resource;
          buf = view->buffer;
          if (buf_res) {
            buf.buffer = buf_res->handle;
          }
        }
      }
    }

    const aerogpu_handle_t bind_handle = tex ? tex : buf.buffer;
    if (bind_handle) {
      // D3D11 hazard rule: a resource cannot be simultaneously bound for output
      // (RTV/DSV/UAV) and as an SRV. Consider aliasing resources (e.g. via shared
      // handles) by passing the underlying Resource pointer when available.
      UnbindResourceFromOutputsLocked(dev, bind_handle, tex ? tex_res : buf_res);
    }

    // Update texture SRV slot (including clearing any previous texture binding
    // when binding a buffer SRV).
    SetShaderResourceSlotLocked(dev, shader_stage, slot, tex);
    if (tex_table && tex_table[slot] == tex) {
      if (slot < kAeroGpuD3D11MaxSrvSlots && bound_tex_resources) {
        bound_tex_resources[slot] = tex_res;
      }
      if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX && slot == 0) {
        dev->current_vs_srv0 = tex_res;
      } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL && slot == 0) {
        dev->current_ps_srv0 = tex_res;
      }
    }

    buf_bindings[i] = buf;
    buf_resources[i] = buf_res;
    if (!buf_changed && buf_table) {
      const aerogpu_shader_resource_buffer_binding& cur = buf_table[slot];
      buf_changed = cur.buffer != buf.buffer || cur.offset_bytes != buf.offset_bytes || cur.size_bytes != buf.size_bytes ||
                    cur.reserved0 != buf.reserved0;
    }
  }

  if (!buf_table || !buf_changed) {
    return;
  }

  if (!BindShaderResourceBuffersRangeLocked(dev,
                                            shader_stage,
                                            static_cast<uint32_t>(start_slot),
                                            static_cast<uint32_t>(view_count),
                                            buf_bindings.data())) {
    return;
  }

  for (UINT i = 0; i < view_count; i++) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);
    buf_table[slot] = buf_bindings[i];
    if (slot < kAeroGpuD3D11MaxSrvSlots && bound_buf_resources) {
      bound_buf_resources[slot] = buf_resources[i];
    }
  }
}

void AEROGPU_APIENTRY VsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                UINT StartSlot,
                                                UINT NumViews,
                                                const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || NumViews == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetShaderResources11Locked(dev, AEROGPU_SHADER_STAGE_VERTEX, StartSlot, NumViews, phViews);
}

void AEROGPU_APIENTRY PsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                UINT StartSlot,
                                                UINT NumViews,
                                                const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || NumViews == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetShaderResources11Locked(dev, AEROGPU_SHADER_STAGE_PIXEL, StartSlot, NumViews, phViews);
}

void AEROGPU_APIENTRY GsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || NumViews == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetShaderResources11Locked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, StartSlot, NumViews, phViews);
}

static void SetSamplers11Locked(Device* dev,
                                uint32_t shader_stage,
                                UINT start_slot,
                                UINT sampler_count,
                                const D3D11DDI_HSAMPLER* phSamplers) {
  if (!dev || sampler_count == 0) {
    return;
  }
  if (start_slot >= kMaxSamplerSlots) {
    return;
  }
  if (start_slot + sampler_count > kMaxSamplerSlots) {
    sampler_count = kMaxSamplerSlots - start_slot;
  }

  aerogpu_handle_t* table = SamplerTableForStage(dev, shader_stage);
  if (!table) {
    return;
  }

  std::array<aerogpu_handle_t, kMaxSamplerSlots> handles{};
  bool changed = false;
  bool slot0_touched = false;
  uint32_t slot0_addr_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t slot0_addr_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;

  for (UINT i = 0; i < sampler_count; i++) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);
    aerogpu_handle_t handle = 0;
    uint32_t addr_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
    uint32_t addr_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
    if (phSamplers && phSamplers[i].pDrvPrivate) {
      auto* sampler = FromHandle<D3D11DDI_HSAMPLER, Sampler>(phSamplers[i]);
      if (sampler) {
        handle = sampler->handle;
        addr_u = sampler->address_u;
        addr_v = sampler->address_v;
      }
    }

    handles[i] = handle;
    if (!changed) {
      changed = table[slot] != handle;
    }
    if (slot == 0) {
      slot0_touched = true;
      slot0_addr_u = addr_u;
      slot0_addr_v = addr_v;
    }
  }

  if (!changed) {
    return;
  }

  if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                   shader_stage,
                                                   static_cast<uint32_t>(start_slot),
                                                   static_cast<uint32_t>(sampler_count),
                                                   handles.data(),
                                                   [&](HRESULT hr) { SetError(dev, hr); })) {
    return;
  }

  if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    AEROGPU_D3D10_11_LOG("emit GS SetSamplers start=%u count=%u",
                         static_cast<unsigned>(start_slot),
                         static_cast<unsigned>(sampler_count));
  }

  for (UINT i = 0; i < sampler_count; i++) {
    table[start_slot + i] = handles[i];
  }
  if (slot0_touched) {
    if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
      dev->current_vs_sampler0_address_u = slot0_addr_u;
      dev->current_vs_sampler0_address_v = slot0_addr_v;
    } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
      dev->current_ps_sampler0_address_u = slot0_addr_u;
      dev->current_ps_sampler0_address_v = slot0_addr_v;
    }
  }
}

void AEROGPU_APIENTRY VsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT StartSlot,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplers11Locked(dev, AEROGPU_SHADER_STAGE_VERTEX, StartSlot, NumSamplers, phSamplers);
}

void AEROGPU_APIENTRY PsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT StartSlot,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplers11Locked(dev, AEROGPU_SHADER_STAGE_PIXEL, StartSlot, NumSamplers, phSamplers);
}
void AEROGPU_APIENTRY GsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT StartSlot,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplers11Locked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, StartSlot, NumSamplers, phSamplers);
}

void AEROGPU_APIENTRY SetViewports11(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumViewports, const D3D10_DDI_VIEWPORT* pViewports) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  validate_and_emit_viewports_locked(dev,
                                    static_cast<uint32_t>(NumViewports),
                                    pViewports,
                                    [&](HRESULT hr) { SetError(dev, hr); });
}

void AEROGPU_APIENTRY SetScissorRects11(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumRects, const D3D10_DDI_RECT* pRects) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  validate_and_emit_scissor_rects_locked(dev,
                                         static_cast<uint32_t>(NumRects),
                                         pRects,
                                         [&](HRESULT hr) { SetError(dev, hr); });
}
static bool EmitRasterizerStateLocked(Device* dev, const RasterizerState* rs) {
  if (!dev) {
    return false;
  }

  uint32_t fill_mode = static_cast<uint32_t>(D3D11_FILL_SOLID);
  uint32_t cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  int32_t depth_bias = 0;
  uint32_t depth_clip_enable = 1u;
  if (rs) {
    fill_mode = rs->fill_mode;
    cull_mode = rs->cull_mode;
    front_ccw = rs->front_ccw;
    scissor_enable = rs->scissor_enable;
    depth_bias = rs->depth_bias;
    depth_clip_enable = rs->depth_clip_enable;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }

  cmd->state.fill_mode = D3DFillModeToAerogpu(fill_mode);
  if (fill_mode != static_cast<uint32_t>(D3D11_FILL_SOLID) &&
      fill_mode != static_cast<uint32_t>(D3D11_FILL_WIREFRAME)) {
    static std::once_flag once;
    std::call_once(once, [=] {
      AEROGPU_D3D10_11_LOG("EmitRasterizerStateLocked: unsupported fill_mode=%u (falling back to SOLID)",
                           (unsigned)fill_mode);
    });
  }
  cmd->state.cull_mode = D3DCullModeToAerogpu(cull_mode);
  cmd->state.front_ccw = front_ccw ? 1u : 0u;
  cmd->state.scissor_enable = scissor_enable ? 1u : 0u;
  cmd->state.depth_bias = depth_bias;
  cmd->state.flags = depth_clip_enable ? AEROGPU_RASTERIZER_FLAG_NONE
                                       : AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;
  return true;
}

static bool EmitBlendStateLocked(Device* dev, const BlendState* bs, const float blend_factor[4], uint32_t sample_mask) {
  if (!dev) {
    return false;
  }

  uint32_t blend_enable = 0u;
  uint32_t src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t write_mask = kD3DColorWriteMaskAll;
  if (bs) {
    blend_enable = bs->blend_enable;
    write_mask = bs->render_target_write_mask;
    if (blend_enable) {
      src_blend = bs->src_blend;
      dst_blend = bs->dest_blend;
      blend_op = bs->blend_op;
      src_blend_alpha = bs->src_blend_alpha;
      dst_blend_alpha = bs->dest_blend_alpha;
      blend_op_alpha = bs->blend_op_alpha;
    }
  }

  if (blend_enable) {
    if (!IsSupportedD3D11BlendFactor(src_blend) ||
        !IsSupportedD3D11BlendFactor(dst_blend) ||
        !IsSupportedD3D11BlendFactor(src_blend_alpha) ||
        !IsSupportedD3D11BlendFactor(dst_blend_alpha) ||
        !IsSupportedD3D11BlendOp(blend_op) ||
        !IsSupportedD3D11BlendOp(blend_op_alpha)) {
      // Avoid silent incorrect blending: if a non-representable blend state slips
      // through (e.g. due to header drift), flag the device error state once per
      // bind and disable blending for this emission.
      SetError(dev, E_NOTIMPL);
      blend_enable = 0u;
      src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
      dst_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
      blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
      src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
      dst_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
      blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }

  cmd->state.enable = blend_enable ? 1u : 0u;
  cmd->state.src_factor = D3dBlendFactorToAerogpuOr(src_blend, AEROGPU_BLEND_ONE);
  cmd->state.dst_factor = D3dBlendFactorToAerogpuOr(dst_blend, AEROGPU_BLEND_ZERO);
  cmd->state.blend_op = D3dBlendOpToAerogpuOr(blend_op, AEROGPU_BLEND_OP_ADD);
  cmd->state.color_write_mask = static_cast<uint8_t>(write_mask & kD3DColorWriteMaskAll);
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
  cmd->state.reserved0[2] = 0;

  cmd->state.src_factor_alpha = D3dBlendFactorToAerogpuOr(src_blend_alpha, cmd->state.src_factor);
  cmd->state.dst_factor_alpha = D3dBlendFactorToAerogpuOr(dst_blend_alpha, cmd->state.dst_factor);
  cmd->state.blend_op_alpha = D3dBlendOpToAerogpuOr(blend_op_alpha, cmd->state.blend_op);

  const float* bf = blend_factor ? blend_factor : dev->current_blend_factor;
  cmd->state.blend_constant_rgba_f32[0] = f32_bits(bf[0]);
  cmd->state.blend_constant_rgba_f32[1] = f32_bits(bf[1]);
  cmd->state.blend_constant_rgba_f32[2] = f32_bits(bf[2]);
  cmd->state.blend_constant_rgba_f32[3] = f32_bits(bf[3]);
  cmd->state.sample_mask = sample_mask;
  return true;
}

static bool EmitDepthStencilStateLocked(Device* dev, const DepthStencilState* dss) {
  if (!dev) {
    return false;
  }
  if (!EmitDepthStencilStateCmdLocked(dev, dss)) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  return true;
}

void AEROGPU_APIENTRY SetRasterizerState11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRASTERIZERSTATE hState) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  RasterizerState* new_rs =
      hState.pDrvPrivate ? FromHandle<D3D11DDI_HRASTERIZERSTATE, RasterizerState>(hState) : nullptr;
  if (!EmitRasterizerStateLocked(dev, new_rs)) {
    return;
  }
  dev->current_rs = new_rs;
}

void AEROGPU_APIENTRY SetBlendState11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      D3D11DDI_HBLENDSTATE hState,
                                      const FLOAT blend_factor[4],
                                      UINT sample_mask) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  BlendState* new_bs = hState.pDrvPrivate ? FromHandle<D3D11DDI_HBLENDSTATE, BlendState>(hState) : nullptr;
  float new_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  if (blend_factor) {
    std::memcpy(new_blend_factor, blend_factor, sizeof(new_blend_factor));
  }
  const uint32_t new_sample_mask = sample_mask;

  if (!EmitBlendStateLocked(dev, new_bs, new_blend_factor, new_sample_mask)) {
    return;
  }

  dev->current_bs = new_bs;
  std::memcpy(dev->current_blend_factor, new_blend_factor, sizeof(dev->current_blend_factor));
  dev->current_sample_mask = new_sample_mask;
}
void AEROGPU_APIENTRY SetDepthStencilState11(D3D11DDI_HDEVICECONTEXT hCtx,
                                               D3D11DDI_HDEPTHSTENCILSTATE hState,
                                               UINT stencil_ref) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DepthStencilState* new_dss =
      hState.pDrvPrivate ? FromHandle<D3D11DDI_HDEPTHSTENCILSTATE, DepthStencilState>(hState) : nullptr;
  if (!EmitDepthStencilStateLocked(dev, new_dss)) {
    return;
  }
  dev->current_dss = new_dss;
  dev->current_stencil_ref = stencil_ref;
}

void AEROGPU_APIENTRY ClearState11(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Unbind texture SRVs explicitly (no range command in the protocol yet).
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (dev->vs_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
    }
    if (dev->vs_srvs[slot] == 0) {
      if (slot < dev->current_vs_srvs.size()) {
        dev->current_vs_srvs[slot] = nullptr;
      }
      if (slot == 0) {
        dev->current_vs_srv0 = nullptr;
      }
    }
    if (dev->ps_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
    }
    if (dev->ps_srvs[slot] == 0) {
      if (slot < dev->current_ps_srvs.size()) {
        dev->current_ps_srvs[slot] = nullptr;
      }
      if (slot == 0) {
        dev->current_ps_srv0 = nullptr;
      }
    }
    if (dev->gs_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 0);
    }
    if (dev->gs_srvs[slot] == 0) {
      if (slot < dev->current_gs_srvs.size()) {
        dev->current_gs_srvs[slot] = nullptr;
      }
    }
    if (dev->cs_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_COMPUTE, slot, 0);
    }
    if (dev->cs_srvs[slot] == 0) {
      if (slot < dev->current_cs_srvs.size()) {
        dev->current_cs_srvs[slot] = nullptr;
      }
    }
  }

  // Unbind constant buffers, samplers, and buffer SRVs using range commands.
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> null_cbs{};
  auto emit_null_cbs = [&](uint32_t stage) -> bool {
    if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                            stage,
                                                            /*start_slot=*/0,
                                                            static_cast<uint32_t>(null_cbs.size()),
                                                            null_cbs.data(),
                                                            [&](HRESULT hr) { SetError(dev, hr); })) {
      return false;
    }
    if (stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
      AEROGPU_D3D10_11_LOG("emit GS ClearState: null constant buffers");
    }
    return true;
  };
  if (!emit_null_cbs(AEROGPU_SHADER_STAGE_VERTEX)) {
    return;
  }
  std::memset(dev->vs_constant_buffers, 0, sizeof(dev->vs_constant_buffers));
  dev->current_vs_cbs.fill(nullptr);
  dev->current_vs_cb0 = nullptr;
  dev->current_vs_cb0_first_constant = 0;
  dev->current_vs_cb0_num_constants = 0;

  if (!emit_null_cbs(AEROGPU_SHADER_STAGE_PIXEL)) {
    return;
  }
  std::memset(dev->ps_constant_buffers, 0, sizeof(dev->ps_constant_buffers));
  dev->current_ps_cbs.fill(nullptr);
  dev->current_ps_cb0 = nullptr;
  dev->current_ps_cb0_first_constant = 0;
  dev->current_ps_cb0_num_constants = 0;

  if (!emit_null_cbs(AEROGPU_SHADER_STAGE_GEOMETRY)) {
    return;
  }
  std::memset(dev->gs_constant_buffers, 0, sizeof(dev->gs_constant_buffers));
  dev->current_gs_cbs.fill(nullptr);

  if (!emit_null_cbs(AEROGPU_SHADER_STAGE_COMPUTE)) {
    return;
  }
  std::memset(dev->cs_constant_buffers, 0, sizeof(dev->cs_constant_buffers));
  dev->current_cs_cbs.fill(nullptr);

  std::array<aerogpu_handle_t, kMaxSamplerSlots> null_samplers{};
  auto emit_null_samplers = [&](uint32_t stage) -> bool {
    if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                     stage,
                                                     /*start_slot=*/0,
                                                     static_cast<uint32_t>(null_samplers.size()),
                                                     null_samplers.data(),
                                                     [&](HRESULT hr) { SetError(dev, hr); })) {
      return false;
    }
    if (stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
      AEROGPU_D3D10_11_LOG("emit GS ClearState: null samplers");
    }
    return true;
  };
  if (!emit_null_samplers(AEROGPU_SHADER_STAGE_VERTEX)) {
    return;
  }
  std::memset(dev->vs_samplers, 0, sizeof(dev->vs_samplers));
  dev->current_vs_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_vs_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;

  if (!emit_null_samplers(AEROGPU_SHADER_STAGE_PIXEL)) {
    return;
  }
  std::memset(dev->ps_samplers, 0, sizeof(dev->ps_samplers));
  dev->current_ps_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_ps_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;

  if (!emit_null_samplers(AEROGPU_SHADER_STAGE_GEOMETRY)) {
    return;
  }
  std::memset(dev->current_gs_samplers, 0, sizeof(dev->current_gs_samplers));

  if (!emit_null_samplers(AEROGPU_SHADER_STAGE_COMPUTE)) {
    return;
  }
  std::memset(dev->cs_samplers, 0, sizeof(dev->cs_samplers));

  std::array<aerogpu_shader_resource_buffer_binding, kMaxShaderResourceSlots> null_buf_srvs{};
  auto emit_null_buf_srvs = [&](uint32_t stage) -> bool {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_shader_resource_buffers>(
        AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS, null_buf_srvs.data(), null_buf_srvs.size() * sizeof(null_buf_srvs[0]));
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return false;
    }
    cmd->shader_stage = stage;
    cmd->start_slot = 0;
    cmd->buffer_count = kMaxShaderResourceSlots;
    cmd->reserved0 = 0;
    if (stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
      AEROGPU_D3D10_11_LOG("emit GS ClearState: null SRV buffers");
    }
    return true;
  };
  if (!emit_null_buf_srvs(AEROGPU_SHADER_STAGE_VERTEX)) {
    return;
  }
  std::memset(dev->vs_srv_buffers, 0, sizeof(dev->vs_srv_buffers));
  dev->current_vs_srv_buffers.fill(nullptr);

  if (!emit_null_buf_srvs(AEROGPU_SHADER_STAGE_PIXEL)) {
    return;
  }
  std::memset(dev->ps_srv_buffers, 0, sizeof(dev->ps_srv_buffers));
  dev->current_ps_srv_buffers.fill(nullptr);

  if (!emit_null_buf_srvs(AEROGPU_SHADER_STAGE_GEOMETRY)) {
    return;
  }
  std::memset(dev->gs_srv_buffers, 0, sizeof(dev->gs_srv_buffers));
  dev->current_gs_srv_buffers.fill(nullptr);

  if (!emit_null_buf_srvs(AEROGPU_SHADER_STAGE_COMPUTE)) {
    return;
  }
  std::memset(dev->cs_srv_buffers, 0, sizeof(dev->cs_srv_buffers));
  dev->current_cs_srv_buffers.fill(nullptr);

  std::array<aerogpu_unordered_access_buffer_binding, kMaxUavSlots> null_uavs{};
  for (auto& b : null_uavs) {
    b.initial_count = kD3DUavInitialCountNoChange;
  }
  auto* uav_cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_unordered_access_buffers>(
      AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS, null_uavs.data(), null_uavs.size() * sizeof(null_uavs[0]));
  if (!uav_cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  uav_cmd->shader_stage = AEROGPU_SHADER_STAGE_COMPUTE;
  uav_cmd->start_slot = 0;
  uav_cmd->uav_count = kMaxUavSlots;
  uav_cmd->reserved0 = 0;
  for (uint32_t i = 0; i < kMaxUavSlots; ++i) {
    dev->cs_uavs[i] = null_uavs[i];
  }
  dev->current_cs_uavs.fill(nullptr);

  // Reset input-assembler state to D3D11 defaults.
  //
  // ClearState is required to reset *all* pipeline state. If we only update the
  // UMD-side tracked state without emitting the corresponding commands, the
  // host-side command executor can continue using stale input layout / VB / IB
  // bindings across ClearState.
  auto* il_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!il_cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  il_cmd->input_layout_handle = 0;
  il_cmd->reserved0 = 0;
  dev->current_input_layout = 0;
  dev->current_input_layout_obj = nullptr;

  const uint32_t default_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  auto* topo_cmd =
      dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!topo_cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  topo_cmd->topology = default_topology;
  topo_cmd->reserved0 = 0;
  dev->current_topology = default_topology;

  std::array<aerogpu_vertex_buffer_binding, kD3D11IaVertexInputResourceSlotCount> vb_zeros{};
  auto* vb_cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, vb_zeros.data(), vb_zeros.size() * sizeof(vb_zeros[0]));
  if (!vb_cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  vb_cmd->start_slot = 0;
  vb_cmd->buffer_count = static_cast<uint32_t>(vb_zeros.size());
  dev->current_vb_resources.fill(nullptr);
  dev->current_vb_strides_bytes.fill(0);
  dev->current_vb_offsets_bytes.fill(0);
  dev->current_vb = nullptr;
  dev->current_vb_stride_bytes = 0;
  dev->current_vb_offset_bytes = 0;

  auto* ib_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!ib_cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  ib_cmd->buffer = 0;
  ib_cmd->format = AEROGPU_INDEX_FORMAT_UINT16;
  ib_cmd->offset_bytes = 0;
  ib_cmd->reserved0 = 0;
  dev->current_ib = nullptr;
  dev->current_ib_format = kDxgiFormatUnknown;
  dev->current_ib_offset_bytes = 0;

  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> rtv_zeros{};
  if (!AppendSetRenderTargetsCmdLocked(dev, /*rtv_count=*/0, rtv_zeros, /*dsv=*/0)) {
    return;
  }
  dev->current_rtv_count = 0;
  dev->current_rtvs.fill(0);
  dev->current_rtv_resources.fill(nullptr);
  dev->current_dsv = 0;
  dev->current_dsv_resource = nullptr;

  const float default_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  const uint32_t default_sample_mask = kD3DSampleMaskAll;
  if (!EmitBlendStateLocked(dev, nullptr, default_blend_factor, default_sample_mask)) {
    return;
  }
  dev->current_bs = nullptr;
  std::memcpy(dev->current_blend_factor, default_blend_factor, sizeof(dev->current_blend_factor));
  dev->current_sample_mask = default_sample_mask;

  if (!EmitDepthStencilStateLocked(dev, nullptr)) {
    return;
  }
  dev->current_dss = nullptr;
  dev->current_stencil_ref = 0;

  if (!EmitRasterizerStateLocked(dev, nullptr)) {
    return;
  }
  dev->current_rs = nullptr;

  if (!EmitBindShadersCmdLocked(dev, /*vs=*/0, /*ps=*/0, /*cs=*/0, /*gs=*/0)) {
    return;
  }
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_cs = 0;
  dev->current_gs = 0;
  dev->current_vs_forced_z_valid = false;
  dev->current_vs_forced_z = 0.0f;

  // Reset viewport/scissor state as part of ClearState. The AeroGPU protocol
  // uses a degenerate (0x0) viewport/scissor to encode "use default".
  validate_and_emit_viewports_locked(dev,
                                    /*num_viewports=*/0,
                                    static_cast<const D3D10_DDI_VIEWPORT*>(nullptr),
                                    [&](HRESULT hr) { SetError(dev, hr); });
  validate_and_emit_scissor_rects_locked(dev,
                                         /*num_rects=*/0,
                                         static_cast<const D3D10_DDI_RECT*>(nullptr),
                                         [&](HRESULT hr) { SetError(dev, hr); });
}

void AEROGPU_APIENTRY SetRenderTargets11(D3D11DDI_HDEVICECONTEXT hCtx,
                                            UINT NumViews,
                                            const D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                            D3D11DDI_HDEPTHSTENCILVIEW hDsv) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> new_rtvs{};
  std::array<Resource*, AEROGPU_MAX_RENDER_TARGETS> new_rtv_resources{};
  const uint32_t new_rtv_count = std::min<uint32_t>(NumViews, AEROGPU_MAX_RENDER_TARGETS);
  for (uint32_t i = 0; i < new_rtv_count; ++i) {
    const RenderTargetView* view = (phRtvs && phRtvs[i].pDrvPrivate) ? FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(phRtvs[i])
                                                                     : nullptr;
    Resource* res = view ? view->resource : nullptr;
    new_rtv_resources[i] = res;
    new_rtvs[i] = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
  }

  aerogpu_handle_t new_dsv = 0;
  Resource* new_dsv_resource = nullptr;
  if (hDsv.pDrvPrivate) {
    const DepthStencilView* dsv = FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hDsv);
    new_dsv_resource = dsv ? dsv->resource : nullptr;
    new_dsv = dsv ? (dsv->texture ? dsv->texture : (new_dsv_resource ? new_dsv_resource->handle : 0)) : 0;
  }

  // Auto-unbind SRVs/UAVs that alias the newly bound render targets/depth buffer.
  for (uint32_t i = 0; i < new_rtv_count; ++i) {
    UnbindResourceFromSrvsLocked(dev, new_rtvs[i], new_rtv_resources[i]);
    UnbindResourceFromUavsLocked(dev, new_rtvs[i], new_rtv_resources[i]);
  }
  UnbindResourceFromSrvsLocked(dev, new_dsv, new_dsv_resource);
  UnbindResourceFromUavsLocked(dev, new_dsv, new_dsv_resource);

  if (!AppendSetRenderTargetsCmdLocked(dev, new_rtv_count, new_rtvs, new_dsv)) {
    return;
  }

  dev->current_rtv_count = new_rtv_count;
  dev->current_rtvs = new_rtvs;
  dev->current_rtv_resources = new_rtv_resources;
  dev->current_dsv = new_dsv;
  dev->current_dsv_resource = new_dsv_resource;

  AEROGPU_D3D10_11_LOG("SET_RENDER_TARGETS: color_count=%u depth=%u colors=[%u,%u,%u,%u,%u,%u,%u,%u]",
                       static_cast<unsigned>(new_rtv_count),
                       static_cast<unsigned>(new_dsv),
                       static_cast<unsigned>(new_rtvs[0]),
                       static_cast<unsigned>(new_rtvs[1]),
                       static_cast<unsigned>(new_rtvs[2]),
                       static_cast<unsigned>(new_rtvs[3]),
                       static_cast<unsigned>(new_rtvs[4]),
                       static_cast<unsigned>(new_rtvs[5]),
                       static_cast<unsigned>(new_rtvs[6]),
                       static_cast<unsigned>(new_rtvs[7]));
}

// D3D11 exposes OMSetRenderTargetsAndUnorderedAccessViews which may map to
// interface-version-specific DDIs. For bring-up, wire any such entrypoints back
// to our simple RTV/DSV binder.
//
// UAV binding is unsupported at FL10_0. Treat unbinding (all-null UAVs) as
// benign (ClearState-friendly), but report E_NOTIMPL when an app attempts to
// bind real UAV state.
template <typename FnPtr>
struct SetRenderTargetsAndUavsThunk;

template <typename... Args>
struct SetRenderTargetsAndUavsThunk<void(AEROGPU_APIENTRY*)(Args...)> {
  static void AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
  }
};

template <typename... Tail>
struct SetRenderTargetsAndUavsThunk<
    void(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT,
                            UINT,
                            const D3D11DDI_HRENDERTARGETVIEW*,
                            D3D11DDI_HDEPTHSTENCILVIEW,
                            Tail...)> {
  static void AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx,
                                    UINT NumViews,
                                    const D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                    D3D11DDI_HDEPTHSTENCILVIEW hDsv,
                                    Tail... tail) {
    SetRenderTargets11(hCtx, NumViews, phRtvs, hDsv);

    if constexpr (sizeof...(Tail) >= 3) {
      using TailTuple = std::tuple<Tail...>;
      using CountT = std::tuple_element_t<1, TailTuple>;
      using UavPtrT = std::tuple_element_t<2, TailTuple>;
      if constexpr ((std::is_integral_v<std::decay_t<CountT>> || std::is_enum_v<std::decay_t<CountT>>) &&
                    std::is_pointer_v<std::decay_t<UavPtrT>> &&
                    std::is_same_v<std::remove_cv_t<std::remove_pointer_t<std::decay_t<UavPtrT>>>, D3D11DDI_HUNORDEREDACCESSVIEW>) {
        auto tail_tup = std::forward_as_tuple(tail...);
        const UINT num_uavs = static_cast<UINT>(std::get<1>(tail_tup));
        const auto* ph_uavs = std::get<2>(tail_tup);
        if (AnyNonNullHandles(ph_uavs, num_uavs)) {
          SetError(DeviceFromContext(hCtx), E_NOTIMPL);
        }
      }
    }

    ((void)tail, ...);
  }
};

template <typename... Tail>
struct SetRenderTargetsAndUavsThunk<
    void(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT,
                            UINT,
                            D3D11DDI_HRENDERTARGETVIEW*,
                            D3D11DDI_HDEPTHSTENCILVIEW,
                            Tail...)> {
  static void AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx,
                                    UINT NumViews,
                                    D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                    D3D11DDI_HDEPTHSTENCILVIEW hDsv,
                                    Tail... tail) {
    SetRenderTargets11(hCtx, NumViews, phRtvs, hDsv);

    if constexpr (sizeof...(Tail) >= 3) {
      using TailTuple = std::tuple<Tail...>;
      using CountT = std::tuple_element_t<1, TailTuple>;
      using UavPtrT = std::tuple_element_t<2, TailTuple>;
      if constexpr ((std::is_integral_v<std::decay_t<CountT>> || std::is_enum_v<std::decay_t<CountT>>) &&
                    std::is_pointer_v<std::decay_t<UavPtrT>> &&
                    std::is_same_v<std::remove_cv_t<std::remove_pointer_t<std::decay_t<UavPtrT>>>, D3D11DDI_HUNORDEREDACCESSVIEW>) {
        auto tail_tup = std::forward_as_tuple(tail...);
        const UINT num_uavs = static_cast<UINT>(std::get<1>(tail_tup));
        const auto* ph_uavs = std::get<2>(tail_tup);
        if (AnyNonNullHandles(ph_uavs, num_uavs)) {
          SetError(DeviceFromContext(hCtx), E_NOTIMPL);
        }
      }
    }

    ((void)tail, ...);
  }
};

static uint8_t U8FromFloat01(float v) {
  if (std::isnan(v)) {
    v = 0.0f;
  }
  v = std::clamp(v, 0.0f, 1.0f);
  const long rounded = std::lround(v * 255.0f);
  if (rounded < 0) {
    return 0;
  }
  if (rounded > 255) {
    return 255;
  }
  return static_cast<uint8_t>(rounded);
}

static uint32_t UnormFromFloat01(float v, uint32_t max) {
  if (std::isnan(v)) {
    v = 0.0f;
  }
  v = std::clamp(v, 0.0f, 1.0f);
  const long rounded = std::lround(v * static_cast<float>(max));
  if (rounded < 0) {
    return 0;
  }
  if (static_cast<uint64_t>(rounded) > static_cast<uint64_t>(max)) {
    return max;
  }
  return static_cast<uint32_t>(rounded);
}

static void SoftwareClearTexture2D(Resource* rt, const FLOAT rgba[4]) {
  if (!rt || rt->kind != ResourceKind::Texture2D || rt->width == 0 || rt->height == 0 || rt->row_pitch_bytes == 0) {
    return;
  }
  if (rt->storage.size() < static_cast<size_t>(rt->row_pitch_bytes) * rt->height) {
    return;
  }

  const uint8_t r = U8FromFloat01(rgba[0]);
  const uint8_t g = U8FromFloat01(rgba[1]);
  const uint8_t b = U8FromFloat01(rgba[2]);
  const uint8_t a = U8FromFloat01(rgba[3]);

  uint32_t bytes_per_pixel = 0;
  bool is_16bpp = false;
  uint8_t px[4] = {0, 0, 0, 0};
  uint16_t px16 = 0;
  switch (rt->dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Typeless:
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8UnormSrgb:
    case kDxgiFormatB8G8R8X8Typeless:
      px[0] = b;
      px[1] = g;
      px[2] = r;
      px[3] = a;
      bytes_per_pixel = 4;
      break;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatR8G8B8A8Typeless:
      px[0] = r;
      px[1] = g;
      px[2] = b;
      px[3] = a;
      bytes_per_pixel = 4;
      break;
    case kDxgiFormatB5G6R5Unorm: {
      const uint16_t r5 = static_cast<uint16_t>(UnormFromFloat01(rgba[0], 31));
      const uint16_t g6 = static_cast<uint16_t>(UnormFromFloat01(rgba[1], 63));
      const uint16_t b5 = static_cast<uint16_t>(UnormFromFloat01(rgba[2], 31));
      px16 = static_cast<uint16_t>((r5 << 11) | (g6 << 5) | b5);
      bytes_per_pixel = 2;
      is_16bpp = true;
      break;
    }
    case kDxgiFormatB5G5R5A1Unorm: {
      const uint16_t r5 = static_cast<uint16_t>(UnormFromFloat01(rgba[0], 31));
      const uint16_t g5 = static_cast<uint16_t>(UnormFromFloat01(rgba[1], 31));
      const uint16_t b5 = static_cast<uint16_t>(UnormFromFloat01(rgba[2], 31));
      const uint16_t a1 = static_cast<uint16_t>(UnormFromFloat01(rgba[3], 1));
      px16 = static_cast<uint16_t>((a1 << 15) | (r5 << 10) | (g5 << 5) | b5);
      bytes_per_pixel = 2;
      is_16bpp = true;
      break;
    }
    default:
      return;
  }

  if (bytes_per_pixel == 0 || rt->row_pitch_bytes < rt->width * bytes_per_pixel) {
    return;
  }

  for (uint32_t y = 0; y < rt->height; y++) {
    uint8_t* row = rt->storage.data() + static_cast<size_t>(y) * rt->row_pitch_bytes;
    for (uint32_t x = 0; x < rt->width; x++) {
      if (is_16bpp) {
        std::memcpy(row + static_cast<size_t>(x) * 2, &px16, sizeof(px16));
      } else {
        std::memcpy(row + static_cast<size_t>(x) * 4, px, sizeof(px));
      }
    }
  }
}

static float Clamp01(float v) {
  if (std::isnan(v)) {
    return 0.0f;
  }
  return std::clamp(v, 0.0f, 1.0f);
}

static void SoftwareClearDepthTexture2D(Resource* ds, float depth) {
  if (!ds || ds->kind != ResourceKind::Texture2D || ds->width == 0 || ds->height == 0 || ds->row_pitch_bytes == 0) {
    return;
  }
  if (!(ds->dxgi_format == kDxgiFormatD24UnormS8Uint || ds->dxgi_format == kDxgiFormatD32Float)) {
    return;
  }
  if (ds->row_pitch_bytes < ds->width * sizeof(uint32_t)) {
    return;
  }
  if (ds->storage.size() < static_cast<size_t>(ds->row_pitch_bytes) * ds->height) {
    return;
  }

  const uint32_t bits = f32_bits(Clamp01(depth));
  for (uint32_t y = 0; y < ds->height; y++) {
    uint8_t* row = ds->storage.data() + static_cast<size_t>(y) * ds->row_pitch_bytes;
    for (uint32_t x = 0; x < ds->width; x++) {
      std::memcpy(row + static_cast<size_t>(x) * sizeof(uint32_t), &bits, sizeof(bits));
    }
  }
}

static bool DepthCompare(uint32_t func, float src, float dst) {
  if (std::isnan(src) || std::isnan(dst)) {
    return false;
  }
  switch (func) {
    case D3D11_COMPARISON_NEVER:
      return false;
    case D3D11_COMPARISON_LESS:
      return src < dst;
    case D3D11_COMPARISON_EQUAL:
      return src == dst;
    case D3D11_COMPARISON_LESS_EQUAL:
      return src <= dst;
    case D3D11_COMPARISON_GREATER:
      return src > dst;
    case D3D11_COMPARISON_NOT_EQUAL:
      return src != dst;
    case D3D11_COMPARISON_GREATER_EQUAL:
      return src >= dst;
    case D3D11_COMPARISON_ALWAYS:
      return true;
    default:
      return src < dst;
  }
}

static float EdgeFn(float ax, float ay, float bx, float by, float px, float py) {
  return (px - ax) * (by - ay) - (py - ay) * (bx - ax);
}

static uint32_t DxgiFormatSizeBytes(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR32G32Float:
      return 8;
    case kDxgiFormatR32G32B32Float:
      return 12;
    case kDxgiFormatR32G32B32A32Float:
      return 16;
    default:
      return 0;
  }
}

struct ValidationInputLayout {
  bool has_position = false;
  uint32_t position_offset = 0;
  uint32_t position_format = 0;

  bool has_color = false;
  uint32_t color_offset = 0;

  bool has_texcoord0 = false;
  uint32_t texcoord0_offset = 0;
};

static bool DecodeInputLayout(const InputLayout* layout, ValidationInputLayout* out) {
  if (!layout || !out) {
    return false;
  }
  *out = {};

  if (layout->blob.size() < sizeof(aerogpu_input_layout_blob_header)) {
    return false;
  }
  aerogpu_input_layout_blob_header header{};
  std::memcpy(&header, layout->blob.data(), sizeof(header));
  if (header.magic != AEROGPU_INPUT_LAYOUT_BLOB_MAGIC || header.version != AEROGPU_INPUT_LAYOUT_BLOB_VERSION) {
    return false;
  }
  const size_t elems_bytes = static_cast<size_t>(header.element_count) * sizeof(aerogpu_input_layout_element_dxgi);
  if (layout->blob.size() < sizeof(header) + elems_bytes) {
    return false;
  }

  const uint32_t kPosHash = HashSemanticName("POSITION");
  const uint32_t kColorHash = HashSemanticName("COLOR");
  const uint32_t kTexHash = HashSemanticName("TEXCOORD");

  uint32_t running_offset[16] = {};
  const uint8_t* p = layout->blob.data() + sizeof(header);
  for (uint32_t i = 0; i < header.element_count; i++) {
    aerogpu_input_layout_element_dxgi e{};
    std::memcpy(&e, p + static_cast<size_t>(i) * sizeof(e), sizeof(e));

    if (e.input_slot >= 16) {
      continue;
    }
    if (e.input_slot_class != 0) {
      // Instance data not supported by the software validator.
      continue;
    }

    uint32_t offset = e.aligned_byte_offset;
    if (offset == kD3DAppendAlignedElement) {
      offset = running_offset[e.input_slot];
    }
    const uint32_t size_bytes = DxgiFormatSizeBytes(e.dxgi_format);
    if (size_bytes != 0) {
      running_offset[e.input_slot] = offset + size_bytes;
    }

    // Validation renderer only supports slot 0.
    if (e.input_slot != 0) {
      continue;
    }

    if (e.semantic_name_hash == kPosHash && e.semantic_index == 0 &&
        (e.dxgi_format == kDxgiFormatR32G32Float || e.dxgi_format == kDxgiFormatR32G32B32Float)) {
      out->has_position = true;
      out->position_offset = offset;
      out->position_format = e.dxgi_format;
    } else if (e.semantic_name_hash == kColorHash && e.semantic_index == 0 && e.dxgi_format == kDxgiFormatR32G32B32A32Float) {
      out->has_color = true;
      out->color_offset = offset;
    } else if (e.semantic_name_hash == kTexHash && e.semantic_index == 0 && e.dxgi_format == kDxgiFormatR32G32Float) {
      out->has_texcoord0 = true;
      out->texcoord0_offset = offset;
    }
  }

  return out->has_position;
}

struct SoftwareVtx {
  float x = 0.0f;
  float y = 0.0f;
  float z = 0.0f;
  float a[4] = {};
};

static bool ReadFloat4FromCbBinding(Resource* cb,
                                   const aerogpu_constant_buffer_binding& binding,
                                   uint32_t offset_within_binding_bytes,
                                   float out_rgba[4]) {
  if (!cb || !out_rgba) {
    return false;
  }
  if (cb->kind != ResourceKind::Buffer) {
    return false;
  }

  const uint64_t binding_offset = binding.offset_bytes;
  uint64_t binding_size = binding.size_bytes;
  if (binding_size == 0) {
    binding_size = binding_offset < cb->size_bytes ? (cb->size_bytes - binding_offset) : 0;
  }

  constexpr uint64_t kFloat4Bytes = sizeof(float) * 4ull;
  const uint64_t read_off = binding_offset + static_cast<uint64_t>(offset_within_binding_bytes);
  const uint64_t end_off = read_off + kFloat4Bytes;
  if (static_cast<uint64_t>(offset_within_binding_bytes) + kFloat4Bytes > binding_size) {
    return false;
  }
  if (end_off > cb->storage.size()) {
    return false;
  }

  std::memcpy(out_rgba, cb->storage.data() + static_cast<size_t>(read_off), kFloat4Bytes);
  return true;
}

static bool FetchSoftwareVtx(const Device* dev,
                             const ValidationInputLayout& layout,
                             uint32_t vertex_index,
                             bool want_color,
                             bool want_uv,
                             SoftwareVtx* out) {
  if (!dev || !out) {
    return false;
  }
  const Resource* vb = dev->current_vb;
  if (!vb || vb->kind != ResourceKind::Buffer) {
    return false;
  }
  if (!layout.has_position) {
    return false;
  }

  const uint32_t stride = dev->current_vb_stride_bytes;
  const uint32_t base_off = dev->current_vb_offset_bytes;
  const uint64_t byte_off = static_cast<uint64_t>(base_off) + static_cast<uint64_t>(vertex_index) * stride;

  auto read = [&](uint32_t off, void* dst, size_t bytes) -> bool {
    const uint64_t o = byte_off + off;
    if (o > vb->storage.size() || bytes > vb->storage.size() - static_cast<size_t>(o)) {
      return false;
    }
    std::memcpy(dst, vb->storage.data() + static_cast<size_t>(o), bytes);
    return true;
  };

  *out = {};

  if (layout.position_format == kDxgiFormatR32G32Float) {
    float xy[2] = {};
    if (!read(layout.position_offset, xy, sizeof(xy))) {
      return false;
    }
    out->x = xy[0];
    out->y = xy[1];
    out->z = dev->current_vs_forced_z_valid ? dev->current_vs_forced_z : 0.0f;
  } else if (layout.position_format == kDxgiFormatR32G32B32Float) {
    float xyz[3] = {};
    if (!read(layout.position_offset, xyz, sizeof(xyz))) {
      return false;
    }
    out->x = xyz[0];
    out->y = xyz[1];
    out->z = xyz[2];
  } else {
    return false;
  }

  if (want_color && layout.has_color) {
    (void)read(layout.color_offset, out->a, sizeof(float) * 4);
  } else if (want_uv && layout.has_texcoord0) {
    (void)read(layout.texcoord0_offset, out->a, sizeof(float) * 2);
    out->a[2] = 0.0f;
    out->a[3] = 0.0f;
  }
  return true;
}

static bool ReadConstantColor(Device* dev, float out_rgba[4]) {
  if (!dev || !out_rgba) {
    return false;
  }

  float vs_color[4] = {};
  bool has_vs_color = false;
  {
    const aerogpu_constant_buffer_binding& vs_cb0_binding = dev->vs_constant_buffers[0];
    Resource* vs_cb0 = dev->current_vs_cb0;
    if (vs_cb0 && vs_cb0_binding.buffer != 0 && ReadFloat4FromCbBinding(vs_cb0, vs_cb0_binding, 0, vs_color)) {
      has_vs_color = true;
    }
  }

  float ps_color0[4] = {};
  bool has_ps_color0 = false;
  {
    const aerogpu_constant_buffer_binding& ps_cb0_binding = dev->ps_constant_buffers[0];
    Resource* ps_cb0 = dev->current_ps_cb0;
    if (ps_cb0 && ps_cb0_binding.buffer != 0 && ReadFloat4FromCbBinding(ps_cb0, ps_cb0_binding, 0, ps_color0)) {
      has_ps_color0 = true;
    }
  }

  if (!has_vs_color) {
    if (!has_ps_color0) {
      return false;
    }
    for (int i = 0; i < 4; ++i) {
      out_rgba[i] = Clamp01(ps_color0[i]);
    }
    return true;
  }

  float ps_mul[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  const aerogpu_constant_buffer_binding& ps_cb0_binding = dev->ps_constant_buffers[0];
  Resource* ps_cb0 = dev->current_ps_cb0;
  if (ps_cb0 && ps_cb0_binding.buffer != 0) {
    uint64_t ps_binding_size = ps_cb0_binding.size_bytes;
    if (ps_binding_size == 0) {
      ps_binding_size =
          ps_cb0_binding.offset_bytes < ps_cb0->size_bytes ? (ps_cb0->size_bytes - ps_cb0_binding.offset_bytes) : 0;
    }
    const uint32_t ps_mul_off = ps_binding_size >= 32 ? 16 : 0;
    float tmp[4] = {};
    if (ReadFloat4FromCbBinding(ps_cb0, ps_cb0_binding, ps_mul_off, tmp)) {
      std::memcpy(ps_mul, tmp, sizeof(ps_mul));
    }
  }

  for (int i = 0; i < 4; ++i) {
    out_rgba[i] = Clamp01(vs_color[i] * ps_mul[i]);
  }
  return true;
}

static float ApplySamplerAddress(float coord, uint32_t mode) {
  if (std::isnan(coord)) {
    coord = 0.0f;
  }
  switch (mode) {
    case AEROGPU_SAMPLER_ADDRESS_REPEAT:
    case D3D11_TEXTURE_ADDRESS_WRAP: {
      coord = coord - std::floor(coord);
      if (coord < 0.0f) {
        coord += 1.0f;
      }
      return coord;
    }
    case AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT:
    case D3D11_TEXTURE_ADDRESS_MIRROR: {
      if (!std::isfinite(coord)) {
        coord = 0.0f;
      }
      const float floored = std::floor(coord);
      float frac = coord - floored;
      if (frac < 0.0f) {
        frac += 1.0f;
      }
      const int64_t whole = static_cast<int64_t>(floored);
      if (whole & 1) {
        frac = 1.0f - frac;
      }
      return frac;
    }
    case AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE:
    case D3D11_TEXTURE_ADDRESS_CLAMP:
    default:
      return std::clamp(coord, 0.0f, 1.0f);
  }
}

static bool SampleTexturePoint(Resource* tex, float u, float v, uint32_t addr_u, uint32_t addr_v, float out_rgba[4]) {
  if (!tex || !out_rgba || tex->kind != ResourceKind::Texture2D || tex->width == 0 || tex->height == 0 ||
      tex->row_pitch_bytes == 0) {
    return false;
  }
  if (tex->storage.size() < static_cast<size_t>(tex->row_pitch_bytes) * static_cast<size_t>(tex->height)) {
    return false;
  }
  if (!(tex->dxgi_format == kDxgiFormatB8G8R8A8Unorm || tex->dxgi_format == kDxgiFormatB8G8R8A8UnormSrgb ||
        tex->dxgi_format == kDxgiFormatB8G8R8A8Typeless || tex->dxgi_format == kDxgiFormatB8G8R8X8Unorm ||
        tex->dxgi_format == kDxgiFormatB8G8R8X8UnormSrgb || tex->dxgi_format == kDxgiFormatB8G8R8X8Typeless ||
        tex->dxgi_format == kDxgiFormatR8G8B8A8Unorm || tex->dxgi_format == kDxgiFormatR8G8B8A8UnormSrgb ||
        tex->dxgi_format == kDxgiFormatR8G8B8A8Typeless)) {
    return false;
  }

  u = ApplySamplerAddress(u, addr_u);
  v = ApplySamplerAddress(v, addr_v);

  int x = static_cast<int>(u * static_cast<float>(tex->width));
  int y = static_cast<int>(v * static_cast<float>(tex->height));
  x = std::clamp(x, 0, static_cast<int>(tex->width) - 1);
  y = std::clamp(y, 0, static_cast<int>(tex->height) - 1);

  const size_t off = static_cast<size_t>(y) * tex->row_pitch_bytes + static_cast<size_t>(x) * 4;
  if (off + 4 > tex->storage.size()) {
    return false;
  }

  uint8_t r = 0, g = 0, b = 0, a = 255;
  switch (tex->dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Typeless:
      b = tex->storage[off + 0];
      g = tex->storage[off + 1];
      r = tex->storage[off + 2];
      a = tex->storage[off + 3];
      break;
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8UnormSrgb:
    case kDxgiFormatB8G8R8X8Typeless:
      b = tex->storage[off + 0];
      g = tex->storage[off + 1];
      r = tex->storage[off + 2];
      a = 255;
      break;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatR8G8B8A8Typeless:
      r = tex->storage[off + 0];
      g = tex->storage[off + 1];
      b = tex->storage[off + 2];
      a = tex->storage[off + 3];
      break;
    default:
      return false;
  }

  constexpr float inv255 = 1.0f / 255.0f;
  out_rgba[0] = static_cast<float>(r) * inv255;
  out_rgba[1] = static_cast<float>(g) * inv255;
  out_rgba[2] = static_cast<float>(b) * inv255;
  out_rgba[3] = static_cast<float>(a) * inv255;
  return true;
}

static void SoftwareRasterTriangle(Device* dev,
                                   Resource* rt,
                                   const SoftwareVtx& v0,
                                   const SoftwareVtx& v1,
                                   const SoftwareVtx& v2,
                                   bool has_color,
                                   bool has_uv,
                                   const float constant_rgba[4],
                                   Resource* tex,
                                   uint32_t sampler_addr_u,
                                   uint32_t sampler_addr_v) {
  if (!dev || !rt) {
    return;
  }
  if (rt->kind != ResourceKind::Texture2D || rt->width == 0 || rt->height == 0 || rt->row_pitch_bytes == 0) {
    return;
  }
  if (rt->storage.size() < static_cast<size_t>(rt->row_pitch_bytes) * static_cast<size_t>(rt->height)) {
    return;
  }

  const float vp_x = dev->viewport_width > 0.0f ? dev->viewport_x : 0.0f;
  const float vp_y = dev->viewport_height > 0.0f ? dev->viewport_y : 0.0f;
  const float vp_w = dev->viewport_width > 0.0f ? dev->viewport_width : static_cast<float>(rt->width);
  const float vp_h = dev->viewport_height > 0.0f ? dev->viewport_height : static_cast<float>(rt->height);
  if (vp_w <= 0.0f || vp_h <= 0.0f) {
    return;
  }

  uint32_t cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  uint32_t depth_clip_enable = 1u;
  if (const RasterizerState* rs = dev->current_rs) {
    cull_mode = rs->cull_mode;
    front_ccw = rs->front_ccw;
    scissor_enable = rs->scissor_enable;
    depth_clip_enable = rs->depth_clip_enable;
  }

  if (depth_clip_enable != 0u) {
    if (std::isnan(v0.z) || std::isnan(v1.z) || std::isnan(v2.z)) {
      return;
    }
    const bool all_below = (v0.z < 0.0f) && (v1.z < 0.0f) && (v2.z < 0.0f);
    const bool all_above = (v0.z > 1.0f) && (v1.z > 1.0f) && (v2.z > 1.0f);
    if (all_below || all_above) {
      return;
    }
  }

  const uint32_t sample_mask = dev->current_sample_mask;
  if ((sample_mask & 1u) == 0u) {
    return;
  }

  const auto to_screen = [&](const SoftwareVtx& v, float* out_x, float* out_y) {
    const float ndc_x = v.x;
    const float ndc_y = v.y;
    *out_x = vp_x + (ndc_x + 1.0f) * 0.5f * vp_w;
    *out_y = vp_y + (1.0f - ndc_y) * 0.5f * vp_h;
  };

  float x0 = 0, y0 = 0, x1 = 0, y1 = 0, x2 = 0, y2 = 0;
  to_screen(v0, &x0, &y0);
  to_screen(v1, &x1, &y1);
  to_screen(v2, &x2, &y2);

  const float area = EdgeFn(x0, y0, x1, y1, x2, y2);
  if (area == 0.0f) {
    return;
  }

  if (cull_mode != static_cast<uint32_t>(D3D11_CULL_NONE)) {
    const bool tri_ccw = area > 0.0f;
    const bool front = (front_ccw != 0u) ? tri_ccw : !tri_ccw;
    if (cull_mode == static_cast<uint32_t>(D3D11_CULL_BACK) && !front) {
      return;
    }
    if (cull_mode == static_cast<uint32_t>(D3D11_CULL_FRONT) && front) {
      return;
    }
  }

  const float min_xf = std::min({x0, x1, x2});
  const float max_xf = std::max({x0, x1, x2});
  const float min_yf = std::min({y0, y1, y2});
  const float max_yf = std::max({y0, y1, y2});

  int min_x = static_cast<int>(std::floor(min_xf));
  int max_x = static_cast<int>(std::ceil(max_xf));
  int min_y = static_cast<int>(std::floor(min_yf));
  int max_y = static_cast<int>(std::ceil(max_yf));

  min_x = std::max(min_x, 0);
  min_y = std::max(min_y, 0);
  max_x = std::min(max_x, static_cast<int>(rt->width) - 1);
  max_y = std::min(max_y, static_cast<int>(rt->height) - 1);

  if (scissor_enable != 0u && dev->scissor_valid) {
    const int sc_left = std::clamp(dev->scissor_left, 0, static_cast<int>(rt->width));
    const int sc_top = std::clamp(dev->scissor_top, 0, static_cast<int>(rt->height));
    const int sc_right = std::clamp(dev->scissor_right, sc_left, static_cast<int>(rt->width));
    const int sc_bottom = std::clamp(dev->scissor_bottom, sc_top, static_cast<int>(rt->height));
    min_x = std::max(min_x, sc_left);
    min_y = std::max(min_y, sc_top);
    max_x = std::min(max_x, sc_right - 1);
    max_y = std::min(max_y, sc_bottom - 1);
  }
  if (min_x > max_x || min_y > max_y) {
    return;
  }

  const float inv_area = 1.0f / area;

  Resource* ds = dev->current_dsv_resource;
  const DepthStencilState* dss = dev->current_dss;

  uint32_t depth_enable = 1u;
  uint32_t depth_write = 1u;
  uint32_t depth_func = static_cast<uint32_t>(D3D11_COMPARISON_LESS);
  if (dss) {
    depth_enable = dss->depth_enable;
    depth_write = dss->depth_write_mask;
    depth_func = dss->depth_func;
  }

  bool do_depth = depth_enable != 0;
  if (!do_depth || !ds || ds->kind != ResourceKind::Texture2D || ds->width != rt->width || ds->height != rt->height ||
      ds->row_pitch_bytes == 0 ||
      !(ds->dxgi_format == kDxgiFormatD24UnormS8Uint || ds->dxgi_format == kDxgiFormatD32Float) ||
      ds->row_pitch_bytes < ds->width * sizeof(uint32_t) ||
      ds->storage.size() < static_cast<size_t>(ds->row_pitch_bytes) * static_cast<size_t>(ds->height)) {
    do_depth = false;
  }

  const float vp_min_z = dev->viewport_min_depth;
  const float vp_max_z = dev->viewport_max_depth;
  const float z0 = vp_min_z + Clamp01(v0.z) * (vp_max_z - vp_min_z);
  const float z1 = vp_min_z + Clamp01(v1.z) * (vp_max_z - vp_min_z);
  const float z2 = vp_min_z + Clamp01(v2.z) * (vp_max_z - vp_min_z);

  uint32_t blend_enable = 0u;
  uint32_t src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t write_mask = kD3DColorWriteMaskAll;
  if (const BlendState* bs = dev->current_bs) {
    blend_enable = bs->blend_enable;
    src_blend = bs->src_blend;
    dst_blend = bs->dest_blend;
    blend_op = bs->blend_op;
    src_blend_alpha = bs->src_blend_alpha;
    dst_blend_alpha = bs->dest_blend_alpha;
    blend_op_alpha = bs->blend_op_alpha;
    write_mask = bs->render_target_write_mask;
  }

  const float* blend_factor = dev->current_blend_factor;
  const auto factor_value = [&](uint32_t factor, const float src_rgba[4], const float dst_rgba[4], int chan) -> float {
    switch (static_cast<D3D11_BLEND>(factor)) {
      case D3D11_BLEND_ZERO:
        return 0.0f;
      case D3D11_BLEND_ONE:
        return 1.0f;
      case D3D11_BLEND_SRC_ALPHA:
        return Clamp01(src_rgba[3]);
      case D3D11_BLEND_INV_SRC_ALPHA:
        return 1.0f - Clamp01(src_rgba[3]);
      case D3D11_BLEND_DEST_ALPHA:
        return Clamp01(dst_rgba[3]);
      case D3D11_BLEND_INV_DEST_ALPHA:
        return 1.0f - Clamp01(dst_rgba[3]);
      case D3D11_BLEND_BLEND_FACTOR:
        return Clamp01(blend_factor ? blend_factor[chan] : 1.0f);
      case D3D11_BLEND_INV_BLEND_FACTOR:
        return 1.0f - Clamp01(blend_factor ? blend_factor[chan] : 1.0f);
      default:
        return 1.0f;
    }
  };

  const auto blend_apply = [&](uint32_t op, float src_term, float dst_term) -> float {
    switch (static_cast<D3D11_BLEND_OP>(op)) {
      case D3D11_BLEND_OP_ADD:
        return src_term + dst_term;
      case D3D11_BLEND_OP_SUBTRACT:
        return src_term - dst_term;
      case D3D11_BLEND_OP_REV_SUBTRACT:
        return dst_term - src_term;
      case D3D11_BLEND_OP_MIN:
        return std::min(src_term, dst_term);
      case D3D11_BLEND_OP_MAX:
        return std::max(src_term, dst_term);
      default:
        return src_term + dst_term;
    }
  };

  for (int y = min_y; y <= max_y; y++) {
    uint8_t* row = rt->storage.data() + static_cast<size_t>(y) * rt->row_pitch_bytes;
    for (int x = min_x; x <= max_x; x++) {
      const float px = static_cast<float>(x) + 0.5f;
      const float py = static_cast<float>(y) + 0.5f;

      const float w0 = EdgeFn(x1, y1, x2, y2, px, py);
      const float w1 = EdgeFn(x2, y2, x0, y0, px, py);
      const float w2 = EdgeFn(x0, y0, x1, y1, px, py);

      if (area > 0.0f) {
        if (w0 < 0.0f || w1 < 0.0f || w2 < 0.0f) {
          continue;
        }
      } else {
        if (w0 > 0.0f || w1 > 0.0f || w2 > 0.0f) {
          continue;
        }
      }

      const float b0 = w0 * inv_area;
      const float b1 = w1 * inv_area;
      const float b2 = w2 * inv_area;

      const float depth = b0 * z0 + b1 * z1 + b2 * z2;
      if (do_depth) {
        const size_t ds_off = static_cast<size_t>(y) * ds->row_pitch_bytes + static_cast<size_t>(x) * sizeof(uint32_t);
        if (ds_off + sizeof(uint32_t) > ds->storage.size()) {
          continue;
        }
        float dst_depth = 0.0f;
        std::memcpy(&dst_depth, ds->storage.data() + ds_off, sizeof(float));
        if (!DepthCompare(depth_func, depth, dst_depth)) {
          continue;
        }
        if (depth_write != 0) {
          std::memcpy(ds->storage.data() + ds_off, &depth, sizeof(float));
        }
      }

      float out_rgba[4] = {};
      if (has_color) {
        for (int i = 0; i < 4; i++) {
          out_rgba[i] = b0 * v0.a[i] + b1 * v1.a[i] + b2 * v2.a[i];
        }
      } else if (has_uv) {
        const float u = b0 * v0.a[0] + b1 * v1.a[0] + b2 * v2.a[0];
        const float v = b0 * v0.a[1] + b1 * v1.a[1] + b2 * v2.a[1];
        if (!SampleTexturePoint(tex, u, v, sampler_addr_u, sampler_addr_v, out_rgba)) {
          continue;
        }
      } else if (constant_rgba) {
        std::memcpy(out_rgba, constant_rgba, sizeof(out_rgba));
      }

      float src_rgba[4] = {Clamp01(out_rgba[0]), Clamp01(out_rgba[1]), Clamp01(out_rgba[2]), Clamp01(out_rgba[3])};
      uint8_t* dst = row + static_cast<size_t>(x) * 4;

      uint8_t dst_u8[4] = {};
      switch (rt->dxgi_format) {
        case kDxgiFormatB8G8R8A8Unorm:
        case kDxgiFormatB8G8R8A8UnormSrgb:
        case kDxgiFormatB8G8R8A8Typeless:
        case kDxgiFormatB8G8R8X8Unorm:
        case kDxgiFormatB8G8R8X8UnormSrgb:
        case kDxgiFormatB8G8R8X8Typeless:
          dst_u8[0] = dst[2];
          dst_u8[1] = dst[1];
          dst_u8[2] = dst[0];
          dst_u8[3] = dst[3];
          break;
        case kDxgiFormatR8G8B8A8Unorm:
        case kDxgiFormatR8G8B8A8UnormSrgb:
        case kDxgiFormatR8G8B8A8Typeless:
          dst_u8[0] = dst[0];
          dst_u8[1] = dst[1];
          dst_u8[2] = dst[2];
          dst_u8[3] = dst[3];
          break;
        default:
          break;
      }

      constexpr float inv255 = 1.0f / 255.0f;
      float dst_rgba[4] = {static_cast<float>(dst_u8[0]) * inv255,
                           static_cast<float>(dst_u8[1]) * inv255,
                           static_cast<float>(dst_u8[2]) * inv255,
                           static_cast<float>(dst_u8[3]) * inv255};

      float blended_rgba[4] = {};
      if (blend_enable != 0u) {
        for (int chan = 0; chan < 3; ++chan) {
          const float sf = factor_value(src_blend, src_rgba, dst_rgba, chan);
          const float df = factor_value(dst_blend, src_rgba, dst_rgba, chan);
          blended_rgba[chan] = blend_apply(blend_op, src_rgba[chan] * sf, dst_rgba[chan] * df);
        }
        const float sf_a = factor_value(src_blend_alpha, src_rgba, dst_rgba, 3);
        const float df_a = factor_value(dst_blend_alpha, src_rgba, dst_rgba, 3);
        blended_rgba[3] = blend_apply(blend_op_alpha, src_rgba[3] * sf_a, dst_rgba[3] * df_a);
      } else {
        std::memcpy(blended_rgba, src_rgba, sizeof(blended_rgba));
      }

      uint8_t out_u8[4] = {U8FromFloat01(blended_rgba[0]),
                           U8FromFloat01(blended_rgba[1]),
                           U8FromFloat01(blended_rgba[2]),
                           U8FromFloat01(blended_rgba[3])};
      if ((write_mask & 0x1u) == 0u) {
        out_u8[0] = dst_u8[0];
      }
      if ((write_mask & 0x2u) == 0u) {
        out_u8[1] = dst_u8[1];
      }
      if ((write_mask & 0x4u) == 0u) {
        out_u8[2] = dst_u8[2];
      }
      if ((write_mask & 0x8u) == 0u) {
        out_u8[3] = dst_u8[3];
      }

      switch (rt->dxgi_format) {
        case kDxgiFormatB8G8R8A8Unorm:
        case kDxgiFormatB8G8R8A8UnormSrgb:
        case kDxgiFormatB8G8R8A8Typeless:
        case kDxgiFormatB8G8R8X8Unorm:
        case kDxgiFormatB8G8R8X8UnormSrgb:
        case kDxgiFormatB8G8R8X8Typeless:
          dst[0] = out_u8[2];
          dst[1] = out_u8[1];
          dst[2] = out_u8[0];
          dst[3] = out_u8[3];
          break;
        case kDxgiFormatR8G8B8A8Unorm:
        case kDxgiFormatR8G8B8A8UnormSrgb:
        case kDxgiFormatR8G8B8A8Typeless:
          dst[0] = out_u8[0];
          dst[1] = out_u8[1];
          dst[2] = out_u8[2];
          dst[3] = out_u8[3];
          break;
        default:
          break;
      }
    }
  }
}

static void SoftwareDrawTriangleList(Device* dev, UINT vertex_count, UINT first_vertex) {
  if (!dev) {
    return;
  }
  Resource* rt = (dev->current_rtv_count != 0) ? dev->current_rtv_resources[0] : nullptr;
  Resource* vb = dev->current_vb;
  if (!rt || !vb || rt->kind != ResourceKind::Texture2D || vb->kind != ResourceKind::Buffer) {
    return;
  }
  if (rt->width == 0 || rt->height == 0 || rt->row_pitch_bytes == 0) {
    return;
  }
  if (rt->storage.size() < static_cast<size_t>(rt->row_pitch_bytes) * static_cast<size_t>(rt->height)) {
    return;
  }
  if (!(rt->dxgi_format == kDxgiFormatB8G8R8A8Unorm || rt->dxgi_format == kDxgiFormatB8G8R8A8UnormSrgb ||
        rt->dxgi_format == kDxgiFormatB8G8R8A8Typeless || rt->dxgi_format == kDxgiFormatB8G8R8X8Unorm ||
        rt->dxgi_format == kDxgiFormatB8G8R8X8UnormSrgb || rt->dxgi_format == kDxgiFormatB8G8R8X8Typeless ||
        rt->dxgi_format == kDxgiFormatR8G8B8A8Unorm || rt->dxgi_format == kDxgiFormatR8G8B8A8UnormSrgb ||
        rt->dxgi_format == kDxgiFormatR8G8B8A8Typeless)) {
    return;
  }
  if (dev->current_topology != AEROGPU_TOPOLOGY_TRIANGLELIST) {
    return;
  }
  if (vertex_count < 3) {
    return;
  }

  ValidationInputLayout layout{};
  if (!DecodeInputLayout(dev->current_input_layout_obj, &layout)) {
    return;
  }

  Resource* tex = dev->current_ps_srv0 ? dev->current_ps_srv0 : dev->current_vs_srv0;
  const bool has_uv = layout.has_texcoord0 && tex;
  const bool has_color = (!has_uv) && layout.has_color;
  float constant_rgba[4] = {};
  uint32_t sampler_addr_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t sampler_addr_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  if (has_uv) {
    if (tex == dev->current_ps_srv0) {
      sampler_addr_u = dev->current_ps_sampler0_address_u;
      sampler_addr_v = dev->current_ps_sampler0_address_v;
    } else {
      sampler_addr_u = dev->current_vs_sampler0_address_u;
      sampler_addr_v = dev->current_vs_sampler0_address_v;
    }
  } else if (!has_color) {
    if (!ReadConstantColor(dev, constant_rgba)) {
      return;
    }
  }

  const uint32_t tri_count = vertex_count / 3;
  for (uint32_t tri = 0; tri < tri_count; ++tri) {
    const uint32_t idx0 = first_vertex + tri * 3 + 0;
    const uint32_t idx1 = first_vertex + tri * 3 + 1;
    const uint32_t idx2 = first_vertex + tri * 3 + 2;

    SoftwareVtx v0{};
    SoftwareVtx v1{};
    SoftwareVtx v2{};
    if (!FetchSoftwareVtx(dev, layout, idx0, has_color, has_uv, &v0) ||
        !FetchSoftwareVtx(dev, layout, idx1, has_color, has_uv, &v1) ||
        !FetchSoftwareVtx(dev, layout, idx2, has_color, has_uv, &v2)) {
      continue;
    }

    SoftwareRasterTriangle(dev,
                           rt,
                           v0,
                           v1,
                           v2,
                           has_color,
                           has_uv,
                           has_color || has_uv ? nullptr : constant_rgba,
                           tex,
                           sampler_addr_u,
                           sampler_addr_v);
  }
}

static void SoftwareDrawIndexedTriangleList(Device* dev, UINT index_count, UINT first_index, INT base_vertex) {
  if (!dev) {
    return;
  }
  Resource* rt = (dev->current_rtv_count != 0) ? dev->current_rtv_resources[0] : nullptr;
  Resource* vb = dev->current_vb;
  Resource* ib = dev->current_ib;
  if (!rt || !vb || !ib || rt->kind != ResourceKind::Texture2D || vb->kind != ResourceKind::Buffer ||
      ib->kind != ResourceKind::Buffer) {
    return;
  }
  if (rt->width == 0 || rt->height == 0 || rt->row_pitch_bytes == 0) {
    return;
  }
  if (rt->storage.size() < static_cast<size_t>(rt->row_pitch_bytes) * static_cast<size_t>(rt->height)) {
    return;
  }
  if (!(rt->dxgi_format == kDxgiFormatB8G8R8A8Unorm || rt->dxgi_format == kDxgiFormatB8G8R8A8UnormSrgb ||
        rt->dxgi_format == kDxgiFormatB8G8R8A8Typeless || rt->dxgi_format == kDxgiFormatB8G8R8X8Unorm ||
        rt->dxgi_format == kDxgiFormatB8G8R8X8UnormSrgb || rt->dxgi_format == kDxgiFormatB8G8R8X8Typeless ||
        rt->dxgi_format == kDxgiFormatR8G8B8A8Unorm || rt->dxgi_format == kDxgiFormatR8G8B8A8UnormSrgb ||
        rt->dxgi_format == kDxgiFormatR8G8B8A8Typeless)) {
    return;
  }
  if (dev->current_topology != AEROGPU_TOPOLOGY_TRIANGLELIST) {
    return;
  }
  if (index_count < 3) {
    return;
  }

  ValidationInputLayout layout{};
  if (!DecodeInputLayout(dev->current_input_layout_obj, &layout)) {
    return;
  }

  Resource* tex = dev->current_ps_srv0 ? dev->current_ps_srv0 : dev->current_vs_srv0;
  const bool has_uv = layout.has_texcoord0 && tex;
  const bool has_color = (!has_uv) && layout.has_color;
  float constant_rgba[4] = {};
  uint32_t sampler_addr_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t sampler_addr_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  if (has_uv) {
    if (tex == dev->current_ps_srv0) {
      sampler_addr_u = dev->current_ps_sampler0_address_u;
      sampler_addr_v = dev->current_ps_sampler0_address_v;
    } else {
      sampler_addr_u = dev->current_vs_sampler0_address_u;
      sampler_addr_v = dev->current_vs_sampler0_address_v;
    }
  } else if (!has_color) {
    if (!ReadConstantColor(dev, constant_rgba)) {
      return;
    }
  }

  size_t index_size = 0;
  if (dev->current_ib_format == kDxgiFormatR16Uint) {
    index_size = 2;
  } else if (dev->current_ib_format == kDxgiFormatR32Uint) {
    index_size = 4;
  } else {
    return;
  }

  const uint64_t indices_off = static_cast<uint64_t>(dev->current_ib_offset_bytes) +
                               static_cast<uint64_t>(first_index) * static_cast<uint64_t>(index_size);
  if (indices_off >= ib->storage.size()) {
    return;
  }

  auto read_index = [&](uint32_t idx, uint32_t* out) -> bool {
    if (!out) {
      return false;
    }
    const uint64_t byte_off = indices_off + static_cast<uint64_t>(idx) * static_cast<uint64_t>(index_size);
    if (byte_off + index_size > ib->storage.size()) {
      return false;
    }
    const uint8_t* p = ib->storage.data() + static_cast<size_t>(byte_off);
    if (index_size == 2) {
      uint16_t v = 0;
      std::memcpy(&v, p, sizeof(v));
      *out = v;
      return true;
    }
    if (index_size == 4) {
      uint32_t v = 0;
      std::memcpy(&v, p, sizeof(v));
      *out = v;
      return true;
    }
    return false;
  };

  const uint32_t tri_count = index_count / 3;
  for (uint32_t tri = 0; tri < tri_count; tri++) {
    uint32_t i0 = 0, i1 = 0, i2 = 0;
    if (!read_index(tri * 3 + 0, &i0) || !read_index(tri * 3 + 1, &i1) || !read_index(tri * 3 + 2, &i2)) {
      return;
    }

    const int64_t v0_idx = static_cast<int64_t>(i0) + static_cast<int64_t>(base_vertex);
    const int64_t v1_idx = static_cast<int64_t>(i1) + static_cast<int64_t>(base_vertex);
    const int64_t v2_idx = static_cast<int64_t>(i2) + static_cast<int64_t>(base_vertex);
    if (v0_idx < 0 || v1_idx < 0 || v2_idx < 0) {
      continue;
    }

    SoftwareVtx v0{};
    SoftwareVtx v1{};
    SoftwareVtx v2{};
    if (!FetchSoftwareVtx(dev, layout, static_cast<uint32_t>(v0_idx), has_color, has_uv, &v0) ||
        !FetchSoftwareVtx(dev, layout, static_cast<uint32_t>(v1_idx), has_color, has_uv, &v1) ||
        !FetchSoftwareVtx(dev, layout, static_cast<uint32_t>(v2_idx), has_color, has_uv, &v2)) {
      continue;
    }

    SoftwareRasterTriangle(dev,
                           rt,
                           v0,
                           v1,
                           v2,
                           has_color,
                           has_uv,
                           has_color || has_uv ? nullptr : constant_rgba,
                           tex,
                           sampler_addr_u,
                           sampler_addr_v);
  }
}

void AEROGPU_APIENTRY ClearRenderTargetView11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HRENDERTARGETVIEW hRtv,
                                              const FLOAT rgba[4]) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !rgba) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  Resource* rt = nullptr;
  if (hRtv.pDrvPrivate) {
    auto* view = FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(hRtv);
    rt = view ? view->resource : nullptr;
  }
  if (!rt) {
    rt = (dev->current_rtv_count != 0) ? dev->current_rtv_resources[0] : nullptr;
  }
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackBoundTargetsForSubmitLocked(dev);
  if (dev->wddm_submit_allocation_list_oom) {
    alloc_checkpoint.rollback();
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  SoftwareClearTexture2D(rt, rgba);
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(rgba[0]);
  cmd->color_rgba_f32[1] = f32_bits(rgba[1]);
  cmd->color_rgba_f32[2] = f32_bits(rgba[2]);
  cmd->color_rgba_f32[3] = f32_bits(rgba[3]);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;
}

void AEROGPU_APIENTRY ClearDepthStencilView11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HDEPTHSTENCILVIEW hDsv,
                                              UINT flags,
                                              FLOAT depth,
                                              UINT8 stencil) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  Resource* ds = nullptr;
  if (hDsv.pDrvPrivate) {
    auto* view = FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hDsv);
    ds = view ? view->resource : nullptr;
  }
  if (!ds) {
    ds = dev->current_dsv_resource;
  }

  uint32_t aer_flags = 0;
  if (flags & 0x1u) {
    aer_flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (flags & 0x2u) {
    aer_flags |= AEROGPU_CLEAR_STENCIL;
  }

  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackBoundTargetsForSubmitLocked(dev);
  if (dev->wddm_submit_allocation_list_oom) {
    alloc_checkpoint.rollback();
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  if (flags & 0x1u) {
    SoftwareClearDepthTexture2D(ds, depth);
  }
  cmd->flags = aer_flags;
  cmd->color_rgba_f32[0] = 0;
  cmd->color_rgba_f32[1] = 0;
  cmd->color_rgba_f32[2] = 0;
  cmd->color_rgba_f32[3] = 0;
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
}

static void ClearUavBufferLocked(Device* dev, const UnorderedAccessView* uav, const uint32_t pattern_u32[4]) {
  if (!dev || !uav || !uav->resource || !pattern_u32) {
    return;
  }
  auto* res = uav->resource;
  if (res->kind != ResourceKind::Buffer) {
    SetError(dev, E_NOTIMPL);
    return;
  }

  const uint64_t off = static_cast<uint64_t>(uav->buffer.offset_bytes);
  uint64_t size = static_cast<uint64_t>(uav->buffer.size_bytes);
  if (off > res->size_bytes) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (size == 0 || size > res->size_bytes - off) {
    size = res->size_bytes - off;
  }
  if (size == 0) {
    return;
  }

  if (off > res->storage.size() || size > res->storage.size() - static_cast<size_t>(off)) {
    SetError(dev, E_FAIL);
    return;
  }

  const uint64_t end = off + size;
  if (end < off) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  const uint64_t upload_offset = off & ~3ull;
  const uint64_t upload_end = AlignUpU64(end, 4);
  if (upload_end < upload_offset) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  const uint64_t upload_size = upload_end - upload_offset;
  if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  const size_t upload_off = static_cast<size_t>(upload_offset);
  const size_t upload_sz = static_cast<size_t>(upload_size);
  if (upload_off > res->storage.size() || upload_sz > res->storage.size() - upload_off) {
    SetError(dev, E_FAIL);
    return;
  }

  // D3D11's ClearUnorderedAccessView* for buffers is defined in terms of a 4x32-bit
  // pattern. For structured/raw buffers, this is effectively a 16-byte repeating
  // pattern; for typed buffers, the driver may interpret the components based on
  // the view format. For bring-up, use the repeated 16-byte pattern.
  uint8_t pattern_bytes[16];
  std::memcpy(pattern_bytes, pattern_u32, sizeof(pattern_bytes));

  // Clearing a UAV writes into the resource; enforce the D3D11 hazard rule by
  // unbinding any aliasing SRVs (typically already handled by UAV binding).
  UnbindResourceFromSrvsLocked(dev, res->handle, res);

  if (res->backing_alloc_id == 0) {
    // Host-owned resource: upload an aligned byte range. The protocol requires
    // UPLOAD_RESOURCE offsets/sizes to be 4-byte aligned for buffers.
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + upload_off, upload_sz);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return;
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = upload_offset;
    cmd->size_bytes = upload_size;

    // Patch the copied upload payload in-place to reflect the clear without
    // allocating a separate staging buffer. This keeps the UMD shadow copy
    // unmodified if the command append fails (OOM).
    uint8_t* upload_payload = reinterpret_cast<uint8_t*>(cmd) + sizeof(*cmd);
    uint8_t* upload_dst = upload_payload + static_cast<size_t>(off - upload_offset);
    for (uint64_t i = 0; i < size; i += sizeof(pattern_bytes)) {
      const size_t n = static_cast<size_t>(std::min<uint64_t>(sizeof(pattern_bytes), size - i));
      std::memcpy(upload_dst + static_cast<size_t>(i), pattern_bytes, n);
    }

    // Commit to the software shadow copy after successfully appending the
    // upload packet.
    uint8_t* dst = res->storage.data() + static_cast<size_t>(off);
    for (uint64_t i = 0; i < size; i += sizeof(pattern_bytes)) {
      const size_t n = static_cast<size_t>(std::min<uint64_t>(sizeof(pattern_bytes), size - i));
      std::memcpy(dst + static_cast<size_t>(i), pattern_bytes, n);
    }
    return;
  }

  const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  const auto* device_cb = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  const bool has_lock_unlock =
      (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) || (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb);
  if (!has_lock_unlock || !dev->runtime_device || res->wddm_allocation_handle == 0) {
    SetError(dev, E_FAIL);
    return;
  }

  auto lock_for_write = [&](D3DDDICB_LOCK* lock_args) -> HRESULT {
    if (!lock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnLockCb) {
      return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    if (device_cb && device_cb->pfnLockCb) {
      return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    return E_NOTIMPL;
  };

  auto unlock = [&](D3DDDICB_UNLOCK* unlock_args) -> HRESULT {
    if (!unlock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnUnlockCb) {
      return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), unlock_args);
    }
    if (device_cb && device_cb->pfnUnlockCb) {
      return CallCbMaybeHandle(device_cb->pfnUnlockCb,
                               MakeRtDeviceHandle(dev),
                               MakeRtDeviceHandle10(dev),
                               unlock_args);
    }
    return E_NOTIMPL;
  };

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock_args.SubresourceIndex = 0;
  }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
    lock_args.SubResourceIndex = 0;
  }
  InitLockForWrite(&lock_args);

  HRESULT hr = lock_for_write(&lock_args);
  if (FAILED(hr)) {
    SetError(dev, hr);
    return;
  }
  if (!lock_args.pData) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    SetError(dev, E_FAIL);
    return;
  }

  // RESOURCE_DIRTY_RANGE causes the host to read the guest allocation to update
  // the host copy.
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  if (dev->wddm_submit_allocation_list_oom) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    alloc_checkpoint.rollback();
    return;
  }
  auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    SetError(dev, E_OUTOFMEMORY);
    alloc_checkpoint.rollback();
    return;
  }
  dirty->resource_handle = res->handle;
  dirty->reserved0 = 0;
  dirty->offset_bytes = upload_offset;
  dirty->size_bytes = upload_size;

  // Fill the guest allocation with the cleared bytes (plus any required
  // alignment prefix/suffix) after successfully appending the dirty-range
  // command, so OOM cannot partially update the resource.
  uint8_t* alloc_bytes = static_cast<uint8_t*>(lock_args.pData);
  const size_t pre = static_cast<size_t>(off - upload_offset);
  const size_t post = static_cast<size_t>(upload_end - end);
  if (pre) {
    std::memcpy(alloc_bytes + upload_off, res->storage.data() + upload_off, pre);
  }
  uint8_t* alloc_dst = alloc_bytes + static_cast<size_t>(off);
  for (uint64_t i = 0; i < size; i += sizeof(pattern_bytes)) {
    const size_t n = static_cast<size_t>(std::min<uint64_t>(sizeof(pattern_bytes), size - i));
    std::memcpy(alloc_dst + static_cast<size_t>(i), pattern_bytes, n);
  }
  if (post) {
    std::memcpy(alloc_bytes + static_cast<size_t>(end),
                res->storage.data() + static_cast<size_t>(end),
                post);
  }

  // Commit to the software shadow copy.
  uint8_t* dst = res->storage.data() + static_cast<size_t>(off);
  for (uint64_t i = 0; i < size; i += sizeof(pattern_bytes)) {
    const size_t n = static_cast<size_t>(std::min<uint64_t>(sizeof(pattern_bytes), size - i));
    std::memcpy(dst + static_cast<size_t>(i), pattern_bytes, n);
  }

  D3DDDICB_UNLOCK unlock_args = {};
  unlock_args.hAllocation = lock_args.hAllocation;
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
    unlock_args.SubresourceIndex = 0;
  }
  __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
    unlock_args.SubResourceIndex = 0;
  }
  hr = unlock(&unlock_args);
  if (FAILED(hr)) {
    SetError(dev, hr);
    return;
  }
}

void AEROGPU_APIENTRY ClearUnorderedAccessViewUint11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                     D3D11DDI_HUNORDEREDACCESSVIEW hUav,
                                                     const UINT values[4]) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !values) {
    return;
  }
  if (!hUav.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  auto* uav = FromHandle<D3D11DDI_HUNORDEREDACCESSVIEW, UnorderedAccessView>(hUav);
  if (!uav || !uav->resource) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint32_t pattern_u32[4] = {values[0], values[1], values[2], values[3]};
  ClearUavBufferLocked(dev, uav, pattern_u32);
}

void AEROGPU_APIENTRY ClearUnorderedAccessViewFloat11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                      D3D11DDI_HUNORDEREDACCESSVIEW hUav,
                                                      const FLOAT values[4]) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !values) {
    return;
  }
  if (!hUav.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  auto* uav = FromHandle<D3D11DDI_HUNORDEREDACCESSVIEW, UnorderedAccessView>(hUav);
  if (!uav || !uav->resource) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint32_t pattern_u32[4] = {f32_bits(values[0]), f32_bits(values[1]), f32_bits(values[2]), f32_bits(values[3])};
  ClearUavBufferLocked(dev, uav, pattern_u32);
}

void AEROGPU_APIENTRY Draw11(D3D11DDI_HDEVICECONTEXT hCtx, UINT VertexCount, UINT StartVertexLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (VertexCount == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  SoftwareDrawTriangleList(dev, VertexCount, StartVertexLocation);
  cmd->vertex_count = VertexCount;
  cmd->instance_count = 1;
  cmd->first_vertex = StartVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawInstanced11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT VertexCountPerInstance,
                                      UINT InstanceCount,
                                      UINT StartVertexLocation,
                                      UINT StartInstanceLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (VertexCountPerInstance == 0 || InstanceCount == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawTriangleList(dev, VertexCountPerInstance, StartVertexLocation);
  cmd->vertex_count = VertexCountPerInstance;
  cmd->instance_count = InstanceCount;
  cmd->first_vertex = StartVertexLocation;
  cmd->first_instance = StartInstanceLocation;
}

void AEROGPU_APIENTRY DrawIndexed11(D3D11DDI_HDEVICECONTEXT hCtx, UINT IndexCount, UINT StartIndexLocation, INT BaseVertexLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (IndexCount == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  SoftwareDrawIndexedTriangleList(dev, IndexCount, StartIndexLocation, BaseVertexLocation);
  cmd->index_count = IndexCount;
  cmd->instance_count = 1;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexedInstanced11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT IndexCountPerInstance,
                                             UINT InstanceCount,
                                             UINT StartIndexLocation,
                                             INT BaseVertexLocation,
                                             UINT StartInstanceLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (IndexCountPerInstance == 0 || InstanceCount == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawIndexedTriangleList(dev, IndexCountPerInstance, StartIndexLocation, BaseVertexLocation);
  cmd->index_count = IndexCountPerInstance;
  cmd->instance_count = InstanceCount;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = StartInstanceLocation;
}

void AEROGPU_APIENTRY DrawInstancedIndirect11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HRESOURCE hBufferForArgs,
                                              UINT AlignedByteOffsetForArgs) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hBufferForArgs.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  auto* buf = FromHandle<D3D11DDI_HRESOURCE, Resource>(hBufferForArgs);
  if (!buf || buf->kind != ResourceKind::Buffer) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if ((AlignedByteOffsetForArgs & 3u) != 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint64_t off = static_cast<uint64_t>(AlignedByteOffsetForArgs);
  if (off > buf->size_bytes || buf->size_bytes - off < 16) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (buf->storage.size() < off + 16) {
    SetError(dev, E_FAIL);
    return;
  }

  uint32_t vertex_count_per_instance = 0;
  uint32_t instance_count = 0;
  uint32_t start_vertex = 0;
  uint32_t start_instance = 0;
  std::memcpy(&vertex_count_per_instance, buf->storage.data() + static_cast<size_t>(off) + 0, sizeof(vertex_count_per_instance));
  std::memcpy(&instance_count, buf->storage.data() + static_cast<size_t>(off) + 4, sizeof(instance_count));
  std::memcpy(&start_vertex, buf->storage.data() + static_cast<size_t>(off) + 8, sizeof(start_vertex));
  std::memcpy(&start_instance, buf->storage.data() + static_cast<size_t>(off) + 12, sizeof(start_instance));
  if (vertex_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawTriangleList(dev, vertex_count_per_instance, start_vertex);
  cmd->vertex_count = vertex_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = start_instance;
}

void AEROGPU_APIENTRY DrawIndexedInstancedIndirect11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                     D3D11DDI_HRESOURCE hBufferForArgs,
                                                     UINT AlignedByteOffsetForArgs) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hBufferForArgs.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  auto* buf = FromHandle<D3D11DDI_HRESOURCE, Resource>(hBufferForArgs);
  if (!buf || buf->kind != ResourceKind::Buffer) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if ((AlignedByteOffsetForArgs & 3u) != 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint64_t off = static_cast<uint64_t>(AlignedByteOffsetForArgs);
  if (off > buf->size_bytes || buf->size_bytes - off < 20) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (buf->storage.size() < off + 20) {
    SetError(dev, E_FAIL);
    return;
  }

  uint32_t index_count_per_instance = 0;
  uint32_t instance_count = 0;
  uint32_t start_index = 0;
  int32_t base_vertex = 0;
  uint32_t start_instance = 0;
  std::memcpy(&index_count_per_instance, buf->storage.data() + static_cast<size_t>(off) + 0, sizeof(index_count_per_instance));
  std::memcpy(&instance_count, buf->storage.data() + static_cast<size_t>(off) + 4, sizeof(instance_count));
  std::memcpy(&start_index, buf->storage.data() + static_cast<size_t>(off) + 8, sizeof(start_index));
  std::memcpy(&base_vertex, buf->storage.data() + static_cast<size_t>(off) + 12, sizeof(base_vertex));
  std::memcpy(&start_instance, buf->storage.data() + static_cast<size_t>(off) + 16, sizeof(start_instance));
  if (index_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  if (!TrackDrawStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawIndexedTriangleList(dev, index_count_per_instance, start_index, base_vertex);
  cmd->index_count = index_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = start_instance;
}

void AEROGPU_APIENTRY Dispatch11(D3D11DDI_HDEVICECONTEXT hCtx,
                                 UINT ThreadGroupCountX,
                                 UINT ThreadGroupCountY,
                                 UINT ThreadGroupCountZ) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (ThreadGroupCountX == 0 || ThreadGroupCountY == 0 || ThreadGroupCountZ == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!TrackComputeStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_dispatch>(AEROGPU_CMD_DISPATCH);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->group_count_x = ThreadGroupCountX;
  cmd->group_count_y = ThreadGroupCountY;
  cmd->group_count_z = ThreadGroupCountZ;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY DispatchIndirect11(D3D11DDI_HDEVICECONTEXT hCtx,
                                         D3D11DDI_HRESOURCE hBufferForArgs,
                                         UINT AlignedByteOffsetForArgs) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hBufferForArgs.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  auto* buf = FromHandle<D3D11DDI_HRESOURCE, Resource>(hBufferForArgs);
  if (!buf || buf->kind != ResourceKind::Buffer) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if ((AlignedByteOffsetForArgs & 3u) != 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint64_t off = static_cast<uint64_t>(AlignedByteOffsetForArgs);
  if (off > buf->size_bytes || buf->size_bytes - off < 12) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (buf->storage.size() < off + 12) {
    SetError(dev, E_FAIL);
    return;
  }

  uint32_t group_count_x = 0;
  uint32_t group_count_y = 0;
  uint32_t group_count_z = 0;
  std::memcpy(&group_count_x, buf->storage.data() + static_cast<size_t>(off) + 0, sizeof(group_count_x));
  std::memcpy(&group_count_y, buf->storage.data() + static_cast<size_t>(off) + 4, sizeof(group_count_y));
  std::memcpy(&group_count_z, buf->storage.data() + static_cast<size_t>(off) + 8, sizeof(group_count_z));

  if (group_count_x == 0 || group_count_y == 0 || group_count_z == 0) {
    return;
  }

  if (!TrackComputeStateForSubmitOrRollbackLocked(dev)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_dispatch>(AEROGPU_CMD_DISPATCH);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->group_count_x = group_count_x;
  cmd->group_count_y = group_count_y;
  cmd->group_count_z = group_count_z;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY CopySubresourceRegion11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                D3D11DDI_HRESOURCE hDstResource,
                                                UINT dst_subresource,
                                                UINT dst_x,
                                               UINT dst_y,
                                               UINT dst_z,
                                               D3D11DDI_HRESOURCE hSrcResource,
                                               UINT src_subresource,
                                               const D3D10_DDI_BOX* pSrcBox);

void AEROGPU_APIENTRY CopyResource11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hDstResource, D3D11DDI_HRESOURCE hSrcResource) {
  // In the AeroGPU bring-up path, CopyResource is equivalent to a CopySubresourceRegion
  // with subresource 0, dst offsets (0,0,0), and no source box.
  CopySubresourceRegion11(hCtx, hDstResource, 0, 0, 0, 0, hSrcResource, 0, nullptr);
}

void AEROGPU_APIENTRY CopyStructureCount11(D3D11DDI_HDEVICECONTEXT hCtx,
                                           D3D11DDI_HRESOURCE hDstBuffer,
                                           UINT DstAlignedByteOffset,
                                           D3D11DDI_HUNORDEREDACCESSVIEW hSrcView) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hDstBuffer.pDrvPrivate || !hSrcView.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if ((DstAlignedByteOffset & 3u) != 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  auto* dst = FromHandle<D3D11DDI_HRESOURCE, Resource>(hDstBuffer);
  auto* src = FromHandle<D3D11DDI_HUNORDEREDACCESSVIEW, UnorderedAccessView>(hSrcView);
  if (!dst || !src || !src->resource) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (dst->kind != ResourceKind::Buffer || src->resource->kind != ResourceKind::Buffer) {
    SetError(dev, E_NOTIMPL);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint64_t off = static_cast<uint64_t>(DstAlignedByteOffset);
  if (off > dst->size_bytes || dst->size_bytes - off < 4) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (dst->storage.size() < off + 4) {
    SetError(dev, E_FAIL);
    return;
  }

  // The bring-up implementation does not track UAV counters. Best-effort:
  // if the UAV is currently bound and has a known initial_count, forward that;
  // otherwise write 0.
  //
  // Writing into the destination buffer is an output hazard; unbind any aliasing
  // SRVs to preserve D3D11's "no SRV+output simultaneously" rule.
  UnbindResourceFromSrvsLocked(dev, dst->handle, dst);
  uint32_t count = 0;
  for (uint32_t slot = 0; slot < kMaxUavSlots; ++slot) {
    if (!ResourcesAlias(dev->current_cs_uavs[slot], src->resource)) {
      continue;
    }
    const uint32_t init = dev->cs_uavs[slot].initial_count;
    if (init != kD3DUavInitialCountNoChange) {
      count = init;
    }
    break;
  }

  if (dst->backing_alloc_id == 0) {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, &count, sizeof(count));
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return;
    }
    cmd->resource_handle = dst->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = off;
    cmd->size_bytes = sizeof(count);
    std::memcpy(dst->storage.data() + static_cast<size_t>(off), &count, sizeof(count));
    return;
  }

  const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  const auto* device_cb = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  const bool has_lock_unlock =
      (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) || (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb);
  if (!has_lock_unlock || !dev->runtime_device || dst->wddm_allocation_handle == 0) {
    SetError(dev, E_FAIL);
    return;
  }

  auto lock_for_write = [&](D3DDDICB_LOCK* lock_args) -> HRESULT {
    if (!lock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnLockCb) {
      return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    if (device_cb && device_cb->pfnLockCb) {
      return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    return E_NOTIMPL;
  };

  auto unlock = [&](D3DDDICB_UNLOCK* unlock_args) -> HRESULT {
    if (!unlock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnUnlockCb) {
      return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), unlock_args);
    }
    if (device_cb && device_cb->pfnUnlockCb) {
      return CallCbMaybeHandle(device_cb->pfnUnlockCb,
                               MakeRtDeviceHandle(dev),
                               MakeRtDeviceHandle10(dev),
                               unlock_args);
    }
    return E_NOTIMPL;
  };

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(dst->wddm_allocation_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock_args.SubresourceIndex = 0;
  }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
    lock_args.SubResourceIndex = 0;
  }
  InitLockForWrite(&lock_args);

  HRESULT hr = lock_for_write(&lock_args);
  if (FAILED(hr)) {
    SetError(dev, hr);
    return;
  }
  if (!lock_args.pData) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    SetError(dev, E_FAIL);
    return;
  }

  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  TrackWddmAllocForSubmitLocked(dev, dst, /*write=*/false);
  if (dev->wddm_submit_allocation_list_oom) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    alloc_checkpoint.rollback();
    return;
  }
  auto* dirty_cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty_cmd) {
    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    (void)unlock(&unlock_args);
    SetError(dev, E_OUTOFMEMORY);
    alloc_checkpoint.rollback();
    return;
  }
  dirty_cmd->resource_handle = dst->handle;
  dirty_cmd->reserved0 = 0;
  dirty_cmd->offset_bytes = off;
  dirty_cmd->size_bytes = sizeof(count);

  std::memcpy(static_cast<uint8_t*>(lock_args.pData) + static_cast<size_t>(off), &count, sizeof(count));
  std::memcpy(dst->storage.data() + static_cast<size_t>(off), &count, sizeof(count));

  D3DDDICB_UNLOCK unlock_args = {};
  unlock_args.hAllocation = lock_args.hAllocation;
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
    unlock_args.SubresourceIndex = 0;
  }
  __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
    unlock_args.SubResourceIndex = 0;
  }
  hr = unlock(&unlock_args);
  if (FAILED(hr)) {
    SetError(dev, hr);
    return;
  }
}

void AEROGPU_APIENTRY CopySubresourceRegion11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             D3D11DDI_HRESOURCE hDstResource,
                                             UINT dst_subresource,
                                             UINT dst_x,
                                             UINT dst_y,
                                             UINT dst_z,
                                             D3D11DDI_HRESOURCE hSrcResource,
                                             UINT src_subresource,
                                             const D3D10_DDI_BOX* pSrcBox) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  auto* dst = hDstResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hDstResource) : nullptr;
  auto* src = hSrcResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hSrcResource) : nullptr;
  if (!dst || !src) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dst->kind == ResourceKind::Buffer && src->kind == ResourceKind::Buffer) {
    if (dst_subresource != 0 || src_subresource != 0 || dst_z != 0) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (dst_y != 0) {
      SetError(dev, E_NOTIMPL);
      return;
    }

    const uint64_t src_left = pSrcBox ? static_cast<uint64_t>(pSrcBox->left) : 0;
    const uint64_t src_right = pSrcBox ? static_cast<uint64_t>(pSrcBox->right) : src->size_bytes;
    const uint64_t dst_off = static_cast<uint64_t>(dst_x);

    if (src_right < src_left) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint64_t max_src = (src_left < src->size_bytes) ? (src->size_bytes - src_left) : 0;
    const uint64_t requested = src_right - src_left;
    const uint64_t max_dst = (dst_off < dst->size_bytes) ? (dst->size_bytes - dst_off) : 0;
    const uint64_t bytes = std::min(std::min(requested, max_src), max_dst);

    if (bytes && dst->storage.size() >= dst_off + bytes && src->storage.size() >= src_left + bytes) {
      if (dst->backing_alloc_id == 0) {
        // Host-owned buffers: upload the post-copy bytes (aligned to 4) before
        // mutating the shadow copy, so an OOM during command emission doesn't
        // desynchronize the UMD from the host.
        const uint64_t end = dst_off + bytes;
        const uint64_t upload_offset = dst_off & ~3ull;
        const uint64_t upload_end = AlignUpU64(end, 4);
        if (upload_end < upload_offset) {
          SetError(dev, E_INVALIDARG);
          return;
        }
        const uint64_t upload_size = upload_end - upload_offset;
        if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
          SetError(dev, E_OUTOFMEMORY);
          return;
        }
        const size_t upload_off = static_cast<size_t>(upload_offset);
        const size_t upload_sz = static_cast<size_t>(upload_size);
        if (upload_off > dst->storage.size() || upload_sz > dst->storage.size() - upload_off) {
          return;
        }

        // Fast path: aligned transfer can upload directly from the source
        // buffer bytes.
        const bool is_aligned_upload = (upload_offset == dst_off) && (upload_size == bytes);
        std::vector<uint8_t> upload_payload;
        const void* upload_data = nullptr;
        size_t upload_data_bytes = 0;
        if (is_aligned_upload) {
          upload_data = src->storage.data() + static_cast<size_t>(src_left);
          upload_data_bytes = static_cast<size_t>(bytes);
        } else {
          try {
            upload_payload.resize(upload_sz);
          } catch (...) {
            SetError(dev, E_OUTOFMEMORY);
            return;
          }
          if (upload_sz) {
            std::memcpy(upload_payload.data(), dst->storage.data() + upload_off, upload_sz);
          }
          std::memcpy(upload_payload.data() + static_cast<size_t>(dst_off - upload_offset),
                      src->storage.data() + static_cast<size_t>(src_left),
                      static_cast<size_t>(bytes));
          upload_data = upload_payload.data();
          upload_data_bytes = upload_payload.size();
        }

        auto* upload_cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
            AEROGPU_CMD_UPLOAD_RESOURCE, upload_data, upload_data_bytes);
        if (!upload_cmd) {
          SetError(dev, E_OUTOFMEMORY);
          return;
        }
        upload_cmd->resource_handle = dst->handle;
        upload_cmd->reserved0 = 0;
        upload_cmd->offset_bytes = upload_offset;
        upload_cmd->size_bytes = upload_size;

        std::memmove(dst->storage.data() + static_cast<size_t>(dst_off),
                     src->storage.data() + static_cast<size_t>(src_left),
                     static_cast<size_t>(bytes));
      } else {
        // Guest-backed buffers: append RESOURCE_DIRTY_RANGE before writing into
        // the runtime allocation to avoid drift on OOM.
        const uint64_t end = dst_off + bytes;
        const uint64_t upload_offset = dst_off & ~3ull;
        const uint64_t upload_end = AlignUpU64(end, 4);
        if (upload_end < upload_offset) {
          SetError(dev, E_INVALIDARG);
          return;
        }
        const uint64_t upload_size = upload_end - upload_offset;
        if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
          SetError(dev, E_OUTOFMEMORY);
          return;
        }
        const size_t upload_off = static_cast<size_t>(upload_offset);
        const size_t upload_sz = static_cast<size_t>(upload_size);
        if (upload_off > dst->storage.size() || upload_sz > dst->storage.size() - upload_off) {
          return;
        }

        const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
        const auto* device_cb = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
        const bool has_lock_unlock = (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) ||
                                     (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb);
        if (!has_lock_unlock || !dev->runtime_device || dst->wddm_allocation_handle == 0) {
          SetError(dev, E_FAIL);
          return;
        }

        auto lock_for_write = [&](D3DDDICB_LOCK* lock_args) -> HRESULT {
          if (!lock_args || !dev->runtime_device) {
            return E_NOTIMPL;
          }
          if (ddi && ddi->pfnLockCb) {
            return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
          }
          if (device_cb && device_cb->pfnLockCb) {
            return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
          }
          return E_NOTIMPL;
        };

        auto unlock = [&](D3DDDICB_UNLOCK* unlock_args) -> HRESULT {
          if (!unlock_args || !dev->runtime_device) {
            return E_NOTIMPL;
          }
          if (ddi && ddi->pfnUnlockCb) {
            return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), unlock_args);
          }
          if (device_cb && device_cb->pfnUnlockCb) {
            return CallCbMaybeHandle(device_cb->pfnUnlockCb,
                                     MakeRtDeviceHandle(dev),
                                     MakeRtDeviceHandle10(dev),
                                     unlock_args);
          }
          return E_NOTIMPL;
        };

        D3DDDICB_LOCK lock_args = {};
        lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(dst->wddm_allocation_handle);
        __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
          lock_args.SubresourceIndex = 0;
        }
        __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
          lock_args.SubResourceIndex = 0;
        }
        InitLockForWrite(&lock_args);

        HRESULT hr = lock_for_write(&lock_args);
        if (FAILED(hr)) {
          SetError(dev, hr);
          return;
        }
        if (!lock_args.pData) {
          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
            unlock_args.SubresourceIndex = 0;
          }
          __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
            unlock_args.SubResourceIndex = 0;
          }
          (void)unlock(&unlock_args);
          SetError(dev, E_FAIL);
          return;
        }

        const WddmAllocListCheckpoint alloc_checkpoint(dev);
        TrackWddmAllocForSubmitLocked(dev, dst, /*write=*/false);
        if (dev->wddm_submit_allocation_list_oom) {
          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
            unlock_args.SubresourceIndex = 0;
          }
          __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
            unlock_args.SubResourceIndex = 0;
          }
          (void)unlock(&unlock_args);
          alloc_checkpoint.rollback();
          return;
        }
        auto* dirty_cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!dirty_cmd) {
          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
            unlock_args.SubresourceIndex = 0;
          }
          __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
            unlock_args.SubResourceIndex = 0;
          }
          (void)unlock(&unlock_args);
          SetError(dev, E_OUTOFMEMORY);
          alloc_checkpoint.rollback();
          return;
        }
        dirty_cmd->resource_handle = dst->handle;
        dirty_cmd->reserved0 = 0;
        dirty_cmd->offset_bytes = upload_offset;
        dirty_cmd->size_bytes = upload_size;

        uint8_t* dst_bytes = static_cast<uint8_t*>(lock_args.pData);
        const size_t pre = static_cast<size_t>(dst_off - upload_offset);
        const size_t post = static_cast<size_t>(upload_end - end);
        if (pre) {
          std::memcpy(dst_bytes + static_cast<size_t>(upload_offset),
                      dst->storage.data() + static_cast<size_t>(upload_offset),
                      pre);
        }
        std::memcpy(dst_bytes + static_cast<size_t>(dst_off),
                    src->storage.data() + static_cast<size_t>(src_left),
                    static_cast<size_t>(bytes));
        if (post) {
          std::memcpy(dst_bytes + static_cast<size_t>(end),
                      dst->storage.data() + static_cast<size_t>(end),
                      post);
        }

        std::memmove(dst->storage.data() + static_cast<size_t>(dst_off),
                     src->storage.data() + static_cast<size_t>(src_left),
                     static_cast<size_t>(bytes));

        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
          unlock_args.SubresourceIndex = 0;
        }
        __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
          unlock_args.SubResourceIndex = 0;
        }
        hr = unlock(&unlock_args);
        if (FAILED(hr)) {
          SetError(dev, hr);
          return;
        }
      }
    }
    else {
      // Internal invariant violated (storage doesn't match declared buffer size).
      // Preserve old behavior: attempt an upload (may no-op due to bounds) but
      // keep the shadow copy unchanged.
      (void)EmitUploadLocked(dev, dst, dst_off, bytes);
    }

    const bool transfer_aligned = (((dst_off | src_left | bytes) & 3ull) == 0);
    const bool same_buffer = (dst->handle == src->handle);
    if (!SupportsTransfer(dev) || !transfer_aligned || same_buffer) {
      return;
    }

    // COPY_BUFFER is a best-effort optimization; if we cannot track allocations
    // for submission (OOM), skip it without poisoning the current command buffer.
    if (!TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false) ||
        !TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true)) {
      return;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      // The COPY_BUFFER packet is an optimization; CPU copy + upload already ran.
      return;
    }
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = dst_off;
    cmd->src_offset_bytes = src_left;
    cmd->size_bytes = bytes;
    uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
    if (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0) {
      copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
    }
    cmd->flags = copy_flags;
    cmd->reserved0 = 0;
    TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { SetError(dev, hr); });
    return;
  }

  if (dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
    if (dst_z != 0) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (dst->dxgi_format != src->dxgi_format) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint64_t dst_count_u64 =
        static_cast<uint64_t>(dst->mip_levels) * static_cast<uint64_t>(dst->array_size);
    const uint64_t src_count_u64 =
        static_cast<uint64_t>(src->mip_levels) * static_cast<uint64_t>(src->array_size);
    if (dst_count_u64 == 0 || src_count_u64 == 0 ||
        dst_count_u64 > static_cast<uint64_t>(UINT32_MAX) ||
        src_count_u64 > static_cast<uint64_t>(UINT32_MAX)) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (dst_subresource >= static_cast<UINT>(dst_count_u64) || dst_subresource >= dst->tex2d_subresources.size() ||
        src_subresource >= static_cast<UINT>(src_count_u64) || src_subresource >= src->tex2d_subresources.size()) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const Texture2DSubresourceLayout dst_sub_layout = dst->tex2d_subresources[dst_subresource];
    const Texture2DSubresourceLayout src_sub_layout = src->tex2d_subresources[src_subresource];

    const uint32_t src_left = pSrcBox ? static_cast<uint32_t>(pSrcBox->left) : 0;
    const uint32_t src_top = pSrcBox ? static_cast<uint32_t>(pSrcBox->top) : 0;
    const uint32_t src_right = pSrcBox ? static_cast<uint32_t>(pSrcBox->right) : src_sub_layout.width;
    const uint32_t src_bottom = pSrcBox ? static_cast<uint32_t>(pSrcBox->bottom) : src_sub_layout.height;

    if (pSrcBox) {
      // Only support 2D boxes for Texture2D copies.
      if (pSrcBox->front != 0 || pSrcBox->back != 1) {
        SetError(dev, E_NOTIMPL);
        return;
      }
    }

    if (src_right < src_left || src_bottom < src_top) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    if (src_right > src_sub_layout.width || src_bottom > src_sub_layout.height) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width =
        std::min(src_right - src_left, dst_sub_layout.width > dst_x ? (dst_sub_layout.width - dst_x) : 0u);
    const uint32_t copy_height =
        std::min(src_bottom - src_top, dst_sub_layout.height > dst_y ? (dst_sub_layout.height - dst_y) : 0u);

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      SetError(dev, E_NOTIMPL);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      SetError(dev, E_NOTIMPL);
      return;
    }

    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aer_fmt);
    const uint32_t dst_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst_sub_layout.width);
    const uint32_t src_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, src_sub_layout.width);
    const uint32_t dst_rows_total = dst_sub_layout.rows_in_layout;
    const uint32_t src_rows_total = src_sub_layout.rows_in_layout;
    if (!layout.valid || dst_min_row == 0 || src_min_row == 0 || dst_rows_total == 0 || src_rows_total == 0 ||
        dst_sub_layout.row_pitch_bytes < dst_min_row || src_sub_layout.row_pitch_bytes < src_min_row) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t src_copy_right = src_left + copy_width;
    const uint32_t src_copy_bottom = src_top + copy_height;
    const uint32_t dst_copy_right = dst_x + copy_width;
    const uint32_t dst_copy_bottom = dst_y + copy_height;
    if (src_copy_right < src_left || src_copy_bottom < src_top || dst_copy_right < dst_x || dst_copy_bottom < dst_y) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    if (layout.block_width > 1 || layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((src_left % layout.block_width) != 0 || (src_top % layout.block_height) != 0 ||
          (dst_x % layout.block_width) != 0 || (dst_y % layout.block_height) != 0 ||
          !aligned_or_edge(src_copy_right, layout.block_width, src_sub_layout.width) ||
          !aligned_or_edge(src_copy_bottom, layout.block_height, src_sub_layout.height) ||
          !aligned_or_edge(dst_copy_right, layout.block_width, dst_sub_layout.width) ||
          !aligned_or_edge(dst_copy_bottom, layout.block_height, dst_sub_layout.height)) {
        SetError(dev, E_INVALIDARG);
        return;
      }
    }

    const uint32_t src_block_left = src_left / layout.block_width;
    const uint32_t src_block_top = src_top / layout.block_height;
    const uint32_t dst_block_left = dst_x / layout.block_width;
    const uint32_t dst_block_top = dst_y / layout.block_height;
    const uint32_t src_block_right = aerogpu_div_round_up_u32(src_copy_right, layout.block_width);
    const uint32_t src_block_bottom = aerogpu_div_round_up_u32(src_copy_bottom, layout.block_height);
    const uint32_t dst_block_right = aerogpu_div_round_up_u32(dst_copy_right, layout.block_width);
    const uint32_t dst_block_bottom = aerogpu_div_round_up_u32(dst_copy_bottom, layout.block_height);
    if (src_block_right < src_block_left || src_block_bottom < src_block_top ||
        dst_block_right < dst_block_left || dst_block_bottom < dst_block_top) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width_blocks = std::min(src_block_right - src_block_left, dst_block_right - dst_block_left);
    const uint32_t copy_height_blocks = std::min(src_block_bottom - src_block_top, dst_block_bottom - dst_block_top);
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > SIZE_MAX || row_bytes_u64 > UINT32_MAX) {
      return;
    }
    const size_t row_bytes = static_cast<size_t>(row_bytes_u64);

    const uint64_t dst_row_needed = static_cast<uint64_t>(dst_block_left) * static_cast<uint64_t>(layout.bytes_per_block) +
                                    static_cast<uint64_t>(row_bytes);
    const uint64_t src_row_needed = static_cast<uint64_t>(src_block_left) * static_cast<uint64_t>(layout.bytes_per_block) +
                                    static_cast<uint64_t>(row_bytes);

    const bool can_cpu_copy =
        row_bytes && copy_height_blocks && dst_row_needed <= dst_sub_layout.row_pitch_bytes &&
        src_row_needed <= src_sub_layout.row_pitch_bytes &&
        dst_block_top + copy_height_blocks <= dst_rows_total && src_block_top + copy_height_blocks <= src_rows_total;

    // When transfer opcodes are available, rely on COPY_TEXTURE2D for the host-side copy and only update
    // the CPU shadow after the command has been successfully appended. This avoids UMD/host drift if we
    // hit OOM while recording the packet.
    if (SupportsTransfer(dev)) {
      const WddmAllocListCheckpoint alloc_checkpoint(dev);
      TrackWddmAllocForSubmitLocked(dev, src, /*write=*/false);
      TrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true);
      if (dev->wddm_submit_allocation_list_oom) {
        alloc_checkpoint.rollback();
        return;
      }
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
      if (!cmd) {
        // Preserve old behavior: COPY_TEXTURE2D is best-effort. Avoid mutating the shadow copy
        // unless we successfully record the packet.
        return;
      }
      cmd->dst_texture = dst->handle;
      cmd->src_texture = src->handle;
      cmd->dst_mip_level = dst_sub_layout.mip_level;
      cmd->dst_array_layer = dst_sub_layout.array_layer;
      cmd->src_mip_level = src_sub_layout.mip_level;
      cmd->src_array_layer = src_sub_layout.array_layer;
      cmd->dst_x = dst_x;
      cmd->dst_y = dst_y;
      cmd->src_x = src_left;
      cmd->src_y = src_top;
      cmd->width = copy_width;
      cmd->height = copy_height;
      uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
      if (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0) {
        copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
      }
      cmd->flags = copy_flags;
      cmd->reserved0 = 0;
      TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { SetError(dev, hr); });

      if (can_cpu_copy) {
        for (uint32_t y = 0; y < copy_height_blocks; y++) {
          const size_t dst_off =
              static_cast<size_t>(dst_sub_layout.offset_bytes) +
              static_cast<size_t>(dst_block_top + y) * dst_sub_layout.row_pitch_bytes +
              static_cast<size_t>(dst_block_left) * layout.bytes_per_block;
          const size_t src_off =
              static_cast<size_t>(src_sub_layout.offset_bytes) +
              static_cast<size_t>(src_block_top + y) * src_sub_layout.row_pitch_bytes +
              static_cast<size_t>(src_block_left) * layout.bytes_per_block;
          if (dst_off + row_bytes <= dst->storage.size() && src_off + row_bytes <= src->storage.size()) {
            std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
          }
        }
      }
      return;
    }

    if (!can_cpu_copy) {
      return;
    }

    // No transfer backend: implement the copy by patching the destination backing store (UPLOAD_RESOURCE for
    // host-owned textures, RESOURCE_DIRTY_RANGE + guest allocation writes for guest-backed textures).
    // Append the corresponding packet before mutating `dst->storage` / the allocation so OOM doesn't desynchronize
    // the UMD shadow from the host.

    if (dst->backing_alloc_id == 0) {
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        const size_t dst_off =
            static_cast<size_t>(dst_sub_layout.offset_bytes) +
            static_cast<size_t>(dst_block_top + y) * dst_sub_layout.row_pitch_bytes +
            static_cast<size_t>(dst_block_left) * layout.bytes_per_block;
        const size_t src_off =
            static_cast<size_t>(src_sub_layout.offset_bytes) +
            static_cast<size_t>(src_block_top + y) * src_sub_layout.row_pitch_bytes +
            static_cast<size_t>(src_block_left) * layout.bytes_per_block;
        if (dst_off + row_bytes > dst->storage.size() || src_off + row_bytes > src->storage.size()) {
          continue;
        }

        auto* upload_cmd =
            dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(AEROGPU_CMD_UPLOAD_RESOURCE,
                                                                      src->storage.data() + src_off,
                                                                      row_bytes);
        if (!upload_cmd) {
          SetError(dev, E_OUTOFMEMORY);
          return;
        }
        upload_cmd->resource_handle = dst->handle;
        upload_cmd->reserved0 = 0;
        upload_cmd->offset_bytes = static_cast<uint64_t>(dst_off);
        upload_cmd->size_bytes = static_cast<uint64_t>(row_bytes);

        std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
      }
      return;
    }

    const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
    const auto* device_cb = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
    const bool has_lock_unlock =
        (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) || (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb);
    if (!has_lock_unlock || !dev->runtime_device || dst->wddm_allocation_handle == 0) {
      SetError(dev, E_FAIL);
      return;
    }

    auto lock_for_write = [&](D3DDDICB_LOCK* lock_args) -> HRESULT {
      if (!lock_args || !dev->runtime_device) {
        return E_NOTIMPL;
      }
      if (ddi && ddi->pfnLockCb) {
        return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
      }
      if (device_cb && device_cb->pfnLockCb) {
        return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
      }
      return E_NOTIMPL;
    };

    auto unlock = [&](D3DDDICB_UNLOCK* unlock_args) -> HRESULT {
      if (!unlock_args || !dev->runtime_device) {
        return E_NOTIMPL;
      }
      if (ddi && ddi->pfnUnlockCb) {
        return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), unlock_args);
      }
      if (device_cb && device_cb->pfnUnlockCb) {
        return CallCbMaybeHandle(device_cb->pfnUnlockCb,
                                 MakeRtDeviceHandle(dev),
                                 MakeRtDeviceHandle10(dev),
                                 unlock_args);
      }
      return E_NOTIMPL;
    };

    D3DDDICB_LOCK lock_args = {};
    lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(dst->wddm_allocation_handle);
    __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
      lock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
      lock_args.SubResourceIndex = 0;
    }
    InitLockForWrite(&lock_args);

    HRESULT hr = lock_for_write(&lock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }
    if (!lock_args.pData) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_FAIL);
      return;
    }

    const WddmAllocListCheckpoint alloc_checkpoint(dev);
    TrackWddmAllocForSubmitLocked(dev, dst, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      alloc_checkpoint.rollback();
      return;
    }

    uint8_t* dst_wddm_bytes = static_cast<uint8_t*>(lock_args.pData);
    for (uint32_t y = 0; y < copy_height_blocks; y++) {
      const size_t dst_off =
          static_cast<size_t>(dst_sub_layout.offset_bytes) +
          static_cast<size_t>(dst_block_top + y) * dst_sub_layout.row_pitch_bytes +
          static_cast<size_t>(dst_block_left) * layout.bytes_per_block;
      const size_t src_off =
          static_cast<size_t>(src_sub_layout.offset_bytes) +
          static_cast<size_t>(src_block_top + y) * src_sub_layout.row_pitch_bytes +
          static_cast<size_t>(src_block_left) * layout.bytes_per_block;
      if (dst_off + row_bytes > dst->storage.size() || src_off + row_bytes > src->storage.size()) {
        continue;
      }

      auto* dirty_cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!dirty_cmd) {
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
          unlock_args.SubresourceIndex = 0;
        }
        __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
          unlock_args.SubResourceIndex = 0;
        }
        (void)unlock(&unlock_args);
        SetError(dev, E_OUTOFMEMORY);
        return;
      }
      dirty_cmd->resource_handle = dst->handle;
      dirty_cmd->reserved0 = 0;
      dirty_cmd->offset_bytes = static_cast<uint64_t>(dst_off);
      dirty_cmd->size_bytes = static_cast<uint64_t>(row_bytes);

      std::memcpy(dst_wddm_bytes + dst_off, src->storage.data() + src_off, row_bytes);
      std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
    }

    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    hr = unlock(&unlock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }
    return;
  }

  SetError(dev, E_NOTIMPL);
}

static HRESULT MapLocked11(Device* dev,
                           Resource* res,
                           UINT subresource,
                           D3D11_DDI_MAP map_type,
                           UINT map_flags,
                           D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  if (!dev || !res || !pMapped) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return E_INVALIDARG;
  }
  if (res->mapped) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  Texture2DSubresourceLayout sub_layout{};
  if (res->kind == ResourceKind::Texture2D) {
    const uint64_t count_u64 =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    if (count_u64 == 0 || count_u64 > static_cast<uint64_t>(UINT32_MAX)) {
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }
    if (subresource >= static_cast<UINT>(count_u64) || subresource >= res->tex2d_subresources.size()) {
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }
    sub_layout = res->tex2d_subresources[subresource];
  } else if (subresource != 0) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  if ((map_flags & ~static_cast<UINT>(kD3D11MapFlagDoNotWait)) != 0) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  const uint32_t map_u32 = static_cast<uint32_t>(map_type);
  bool want_read = false;
  bool want_write = false;
  switch (map_u32) {
    case kD3D11MapRead:
      want_read = true;
      break;
    case kD3D11MapWrite:
    case kD3D11MapWriteDiscard:
    case kD3D11MapWriteNoOverwrite:
      want_write = true;
      break;
    case kD3D11MapReadWrite:
      want_read = true;
      want_write = true;
      break;
    default:
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
  }

  // Enforce the D3D11 Map/Usage rules (see docs/graphics/win7-d3d11-map-unmap.md).
  switch (res->usage) {
    case kD3D11UsageDynamic:
      if (map_u32 != kD3D11MapWriteDiscard && map_u32 != kD3D11MapWriteNoOverwrite) {
        SetError(dev, E_INVALIDARG);
        return E_INVALIDARG;
      }
      break;
    case kD3D11UsageStaging: {
      const uint32_t access_mask = kD3D11CpuAccessRead | kD3D11CpuAccessWrite;
      const uint32_t access = res->cpu_access_flags & access_mask;
      if (access == kD3D11CpuAccessRead) {
        if (map_u32 != kD3D11MapRead) {
          SetError(dev, E_INVALIDARG);
          return E_INVALIDARG;
        }
      } else if (access == kD3D11CpuAccessWrite) {
        if (map_u32 != kD3D11MapWrite) {
          SetError(dev, E_INVALIDARG);
          return E_INVALIDARG;
        }
      } else if (access == access_mask) {
        if (map_u32 != kD3D11MapRead && map_u32 != kD3D11MapWrite && map_u32 != kD3D11MapReadWrite) {
          SetError(dev, E_INVALIDARG);
          return E_INVALIDARG;
        }
      } else {
        SetError(dev, E_INVALIDARG);
        return E_INVALIDARG;
      }
      break;
    }
    default:
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
  }

  if (want_read && !(res->cpu_access_flags & kD3D11CpuAccessRead)) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if (want_write && !(res->cpu_access_flags & kD3D11CpuAccessWrite)) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  // Win7 readback path: the runtime expects Map(READ) on staging resources to
  // block (or return DXGI_ERROR_WAS_STILL_DRAWING for DO_NOT_WAIT) until the GPU
  // has finished writing the staging allocation.
  if (want_read && res->usage == kD3D11UsageStaging) {
    // Make sure any pending work is actually submitted so we have a fence to
    // wait on.
    if (!dev->cmd.empty()) {
      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        SetError(dev, submit_hr);
        return submit_hr;
      }
    }

    const uint64_t fence = res->last_gpu_write_fence;
    if (fence != 0) {
      const bool do_not_wait = (map_flags & kD3D11MapFlagDoNotWait) != 0;
      const UINT64 timeout = do_not_wait ? 0ull : kAeroGpuTimeoutU64Infinite;
      const HRESULT wait_hr = WaitForFence(dev, fence, timeout);
      if (wait_hr == kDxgiErrorWasStillDrawing || (do_not_wait && wait_hr == kHrPending)) {
        return kDxgiErrorWasStillDrawing;
      }
      if (FAILED(wait_hr)) {
        SetError(dev, wait_hr);
        return wait_hr;
      }
    }
  }

  if (map_u32 == kD3D11MapWriteDiscard) {
    if (res->kind == ResourceKind::Buffer) {
      // Approximate DISCARD renaming by allocating a fresh CPU backing store.
      try {
        res->storage.assign(res->storage.size(), 0);
      } catch (...) {
        SetError(dev, E_OUTOFMEMORY);
        return E_OUTOFMEMORY;
      }
    } else if (res->kind == ResourceKind::Texture2D) {
      // Discard the mapped subresource region (contents are undefined).
      if (sub_layout.size_bytes && sub_layout.offset_bytes <= res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(sub_layout.offset_bytes);
        const size_t clear_bytes = static_cast<size_t>(std::min<uint64_t>(sub_layout.size_bytes, remaining));
        std::memset(res->storage.data() + static_cast<size_t>(sub_layout.offset_bytes), 0, clear_bytes);
      }
    }
  }

  const bool allow_storage_map = (res->backing_alloc_id == 0) && !(want_read && res->usage == kD3D11UsageStaging);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    uint64_t mapped_off = 0;
    uint64_t mapped_size = res->storage.size();
    if (res->kind == ResourceKind::Texture2D) {
      mapped_off = sub_layout.offset_bytes;
      mapped_size = sub_layout.size_bytes;
    }

    const uint64_t storage_size = static_cast<uint64_t>(res->storage.size());
    if (mapped_off > storage_size || mapped_size > storage_size - mapped_off) {
      SetError(dev, E_FAIL);
      return E_FAIL;
    }

    pMapped->pData = (res->storage.empty() || mapped_off >= res->storage.size())
                         ? nullptr
                         : (res->storage.data() + static_cast<size_t>(mapped_off));
    if (res->kind == ResourceKind::Texture2D) {
      pMapped->RowPitch = sub_layout.row_pitch_bytes;
      pMapped->DepthPitch = static_cast<UINT>(sub_layout.size_bytes);
    } else if (res->kind == ResourceKind::Buffer) {
      // D3D11[DDI] defines RowPitch/DepthPitch only for texture resources. For
      // buffers the fields are undefined; returning the buffer size can confuse
      // callers that treat a non-zero pitch as "texture-like" memory.
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
    } else {
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
    }

    res->mapped = true;
    res->mapped_map_type = map_u32;
    res->mapped_map_flags = map_flags;
    res->mapped_subresource = subresource;
    res->mapped_offset = mapped_off;
    res->mapped_size = mapped_size;
    return S_OK;
  };

  const auto* cb = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  const auto* cb_device = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  enum class LockCbPath {
    Wddm,
    Device,
  };
  LockCbPath lock_path{};
  if (cb && cb->pfnLockCb && cb->pfnUnlockCb) {
    lock_path = LockCbPath::Wddm;
  } else if (cb_device && cb_device->pfnLockCb && cb_device->pfnUnlockCb) {
    lock_path = LockCbPath::Device;
  } else {
    if (allow_storage_map) {
      return map_storage();
    }
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  uint64_t alloc_handle = 0;
  if (res->wddm_allocation_handle != 0) {
    alloc_handle = static_cast<uint64_t>(res->wddm_allocation_handle);
  } else if (!res->wddm.km_allocation_handles.empty()) {
    alloc_handle = res->wddm.km_allocation_handles[0];
  }

  if (!alloc_handle) {
    if (allow_storage_map) {
      return map_storage();
    }
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_allocation = 0;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;

  D3DDDICB_LOCK lock = {};
  lock.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
  const UINT lock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : subresource;
  InitLockArgsForMap(&lock, lock_subresource, map_u32, map_flags);

  HRESULT lock_hr = E_FAIL;
  if (lock_path == LockCbPath::Wddm) {
    lock_hr = CallCbMaybeHandle(cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock);
  } else {
    lock_hr = CallCbMaybeHandle(cb_device->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock);
  }
  const bool do_not_wait = (map_flags & kD3D11MapFlagDoNotWait) != 0;
  if (lock_hr == kDxgiErrorWasStillDrawing ||
      (do_not_wait && (lock_hr == kHrPending || lock_hr == kHrWaitTimeout || lock_hr == kHrErrorTimeout ||
                       lock_hr == kHrNtStatusTimeout ||
                       lock_hr == kHrNtStatusGraphicsGpuBusy))) {
    if (allow_storage_map && !want_read) {
      return map_storage();
    }
    return kDxgiErrorWasStillDrawing;
  }
  if (FAILED(lock_hr)) {
    if (allow_storage_map) {
      return map_storage();
    }
    SetError(dev, lock_hr);
    return lock_hr;
  }
  if (!lock.pData) {
    D3DDDICB_UNLOCK unlock = {};
    unlock.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
    InitUnlockArgsForMap(&unlock, lock_subresource);
    if (lock_path == LockCbPath::Wddm) {
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock);
    } else {
      (void)CallCbMaybeHandle(cb_device->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock);
    }
    if (allow_storage_map) {
      return map_storage();
    }
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  const auto unlock_locked_allocation = [&]() {
    D3DDDICB_UNLOCK unlock = {};
    unlock.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
    InitUnlockArgsForMap(&unlock, lock_subresource);
    if (lock_path == LockCbPath::Wddm) {
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock);
    } else {
      (void)CallCbMaybeHandle(cb_device->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock);
    }
  };

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  const uint64_t mapped_off = (res->kind == ResourceKind::Texture2D) ? sub_layout.offset_bytes : 0;
  const uint64_t mapped_size = (res->kind == ResourceKind::Texture2D)
                                   ? sub_layout.size_bytes
                                   : (res->kind == ResourceKind::Buffer ? res->size_bytes : res->storage.size());
  if (res->kind == ResourceKind::Texture2D) {
    if (mapped_size != 0 && mapped_off > (std::numeric_limits<uint64_t>::max() - mapped_size)) {
      unlock_locked_allocation();
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }
    if (!res->storage.empty()) {
      const uint64_t total = static_cast<uint64_t>(res->storage.size());
      if (mapped_off > total || mapped_size > total - mapped_off) {
        unlock_locked_allocation();
        SetError(dev, E_INVALIDARG);
        return E_INVALIDARG;
      }
    }
  }

  // For Texture2D, LockCb may return a pitch that differs from our assumed
  // `Texture2DSubresourceLayout::row_pitch_bytes`. On Win7, we lock
  // SubresourceIndex=0 and use `offset_bytes` to reach other subresources, so the
  // LockCb pitch is only meaningful for the mip0 layout rule (mip>0 is tight in
  // the AeroGPU protocol).
  uint32_t mapped_row_pitch = 0;
  uint32_t mapped_slice_pitch = 0;
  uint32_t tex_row_bytes = 0;
  uint32_t tex_rows = 0;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t expected_pitch = sub_layout.row_pitch_bytes;
    const bool use_lock_pitch = (sub_layout.mip_level == 0);

    if (use_lock_pitch) {
      uint32_t lock_row_pitch = 0;
      uint32_t lock_slice_pitch = 0;
      __if_exists(D3DDDICB_LOCK::Pitch) {
        lock_row_pitch = lock.Pitch;
      }
      __if_exists(D3DDDICB_LOCK::SlicePitch) {
        lock_slice_pitch = lock.SlicePitch;
      }
      if (lock_row_pitch != 0) {
        LogTexture2DPitchMismatchRateLimited("MapLocked11", res, subresource, expected_pitch, lock_row_pitch);
      }

      // Guest-backed resources are interpreted by the host using the protocol
      // pitch (CREATE_TEXTURE2D.row_pitch_bytes). Do not propagate a runtime
      // pitch to the D3D runtime for guest-backed textures as that would cause
      // apps to write with a different stride than the host expects.
      if (!is_guest_backed) {
        mapped_row_pitch = lock_row_pitch;
        mapped_slice_pitch = lock_slice_pitch;
      }
    }

    const uint32_t effective_row_pitch = mapped_row_pitch ? mapped_row_pitch : expected_pitch;
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    tex_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, sub_layout.width);
    tex_rows = sub_layout.rows_in_layout;
    if (tex_row_bytes == 0 || tex_rows == 0 || expected_pitch < tex_row_bytes) {
      unlock_locked_allocation();
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }
    // Fail cleanly if the runtime reports a pitch that cannot fit the texel row.
    if (mapped_row_pitch != 0 && mapped_row_pitch < tex_row_bytes) {
      unlock_locked_allocation();
      SetError(dev, E_INVALIDARG);
      return E_INVALIDARG;
    }

    if (mapped_slice_pitch == 0) {
      const uint64_t slice_pitch_u64 =
          static_cast<uint64_t>(effective_row_pitch) * static_cast<uint64_t>(tex_rows);
      if (slice_pitch_u64 == 0 || slice_pitch_u64 > UINT32_MAX) {
        unlock_locked_allocation();
        SetError(dev, E_INVALIDARG);
        return E_INVALIDARG;
      }
      mapped_slice_pitch = static_cast<uint32_t>(slice_pitch_u64);
    }
  }
  uint8_t* mapped_ptr = static_cast<uint8_t*>(lock.pData);
  if (res->kind == ResourceKind::Texture2D) {
    // Validate offset math before applying it.
    if (mapped_off > static_cast<uint64_t>(SIZE_MAX)) {
      unlock_locked_allocation();
      if (allow_storage_map) {
        return map_storage();
      }
      SetError(dev, E_FAIL);
      return E_FAIL;
    }
    if (mapped_off != 0) {
      mapped_ptr = mapped_ptr + static_cast<size_t>(mapped_off);
    }
  }

  // Keep the software-backed shadow copy (`res->storage`) in sync with the
  // runtime allocation pointer we hand back to the D3D runtime.
  if (!res->storage.empty()) {
    if (map_u32 == kD3D11MapWriteDiscard) {
      // Discard contents are undefined; clear for deterministic tests.
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t dst_pitch = (mapped_row_pitch != 0) ? mapped_row_pitch : sub_layout.row_pitch_bytes;
        if (tex_row_bytes != 0 && tex_rows != 0 && dst_pitch >= tex_row_bytes) {
          for (uint32_t y = 0; y < tex_rows; ++y) {
            const size_t dst_off_row = static_cast<size_t>(y) * dst_pitch;
            std::memset(mapped_ptr + dst_off_row, 0, dst_pitch);
          }
        } else {
          const size_t clear_bytes = static_cast<size_t>(
              std::min<uint64_t>(mapped_size, static_cast<uint64_t>(res->storage.size())));
          if (clear_bytes) {
            std::memset(mapped_ptr, 0, clear_bytes);
          }
        }
      } else {
        std::memset(lock.pData, 0, res->storage.size());
      }
    } else if (!is_guest_backed && res->kind == ResourceKind::Texture2D) {
      const uint32_t src_pitch = sub_layout.row_pitch_bytes;
      const uint32_t dst_pitch = (mapped_row_pitch != 0) ? mapped_row_pitch : sub_layout.row_pitch_bytes;
      const uint8_t* src_bytes = res->storage.data();
      uint8_t* dst_bytes = static_cast<uint8_t*>(lock.pData);
      if (tex_row_bytes != 0 && tex_rows != 0 && src_pitch >= tex_row_bytes && dst_pitch >= tex_row_bytes &&
          mapped_off <= res->storage.size()) {
        for (uint32_t y = 0; y < tex_rows; ++y) {
          const uint64_t src_off_u64 =
              mapped_off + static_cast<uint64_t>(y) * static_cast<uint64_t>(src_pitch);
          if (src_off_u64 > res->storage.size() || tex_row_bytes > res->storage.size() - static_cast<size_t>(src_off_u64)) {
            break;
          }
          const size_t src_off = static_cast<size_t>(src_off_u64);
          const size_t dst_off = static_cast<size_t>(mapped_off) + static_cast<size_t>(y) * dst_pitch;
          std::memcpy(dst_bytes + dst_off, src_bytes + src_off, tex_row_bytes);
          if (dst_pitch > tex_row_bytes) {
            std::memset(dst_bytes + dst_off + tex_row_bytes, 0, dst_pitch - tex_row_bytes);
          }
        }
      }
    } else if (!is_guest_backed) {
      std::memcpy(lock.pData, res->storage.data(), res->storage.size());
    } else if (want_read || (want_write && res->usage == kD3D11UsageStaging)) {
      // Guest-backed resources are updated by writing directly into the backing
      // allocation (and emitting RESOURCE_DIRTY_RANGE). Avoid overwriting the
      // runtime allocation contents with shadow storage; instead refresh the
      // shadow copy for Map() calls that need existing contents (READ or staging
      // WRITE paths that may be followed by an OOM rollback on Unmap).
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t src_pitch = (mapped_row_pitch != 0) ? mapped_row_pitch : sub_layout.row_pitch_bytes;
        const uint32_t dst_pitch = sub_layout.row_pitch_bytes;
        if (tex_row_bytes != 0 && tex_rows != 0 && src_pitch >= tex_row_bytes && dst_pitch >= tex_row_bytes &&
            mapped_off <= res->storage.size()) {
          const uint8_t* src_bytes = static_cast<const uint8_t*>(lock.pData);
          uint8_t* dst_bytes = res->storage.data();
          for (uint32_t y = 0; y < tex_rows; ++y) {
            const uint64_t dst_off_u64 =
                mapped_off + static_cast<uint64_t>(y) * static_cast<uint64_t>(dst_pitch);
            if (dst_off_u64 > res->storage.size() || tex_row_bytes > res->storage.size() - static_cast<size_t>(dst_off_u64)) {
              break;
            }
            const size_t dst_off = static_cast<size_t>(dst_off_u64);
            const size_t src_off = static_cast<size_t>(mapped_off) + static_cast<size_t>(y) * src_pitch;
            std::memcpy(dst_bytes + dst_off, src_bytes + src_off, tex_row_bytes);
            if (dst_pitch > tex_row_bytes) {
              std::memset(dst_bytes + dst_off + tex_row_bytes, 0, dst_pitch - tex_row_bytes);
            }
          }
        }
      } else {
        std::memcpy(res->storage.data(), lock.pData, res->storage.size());
      }
    }
  }

  pMapped->pData = mapped_ptr;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t row_pitch = (mapped_row_pitch != 0) ? mapped_row_pitch : sub_layout.row_pitch_bytes;
    pMapped->RowPitch = row_pitch;
    pMapped->DepthPitch = mapped_slice_pitch ? mapped_slice_pitch
                                             : static_cast<UINT>(row_pitch) * static_cast<UINT>(sub_layout.rows_in_layout);
  } else {
    // Undefined for buffers/other resources; keep deterministic zeroes for spec-friendly behavior.
    pMapped->RowPitch = 0;
    pMapped->DepthPitch = 0;
  }

  res->mapped_wddm_ptr = lock.pData;
  res->mapped_wddm_allocation = alloc_handle;
  res->mapped_wddm_pitch = mapped_row_pitch;
  res->mapped_wddm_slice_pitch = mapped_slice_pitch;

  res->mapped = true;
  res->mapped_map_type = map_u32;
  res->mapped_map_flags = map_flags;
  res->mapped_subresource = subresource;
  res->mapped_offset = mapped_off;
  res->mapped_size = mapped_size;
  return S_OK;
}

static HRESULT MapCore11(D3D11DDI_HDEVICECONTEXT hCtx,
                         D3D11DDI_HRESOURCE hResource,
                         UINT subresource,
                         D3D11_DDI_MAP map_type,
                         UINT map_flags,
                         D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate || !pMapped) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return MapLocked11(dev, res, subresource, map_type, map_flags, pMapped);
}

HRESULT AEROGPU_APIENTRY Map11(D3D11DDI_HDEVICECONTEXT hCtx,
                               D3D11DDI_HRESOURCE hResource,
                               UINT subresource,
                               D3D11_DDI_MAP map_type,
                               UINT map_flags,
                               D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnMap subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));
  return MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY Map11Void(D3D11DDI_HDEVICECONTEXT hCtx,
                                D3D11DDI_HRESOURCE hResource,
                                UINT subresource,
                                D3D11_DDI_MAP map_type,
                                UINT map_flags,
                                D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnMap(void) subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));
  const HRESULT hr = MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
  // When the runtime negotiates a void-returning Map entrypoint, errors are
  // reported exclusively through SetErrorCb. Preserve DO_NOT_WAIT semantics by
  // mapping DXGI_ERROR_WAS_STILL_DRAWING into the error callback.
  if (hr == kDxgiErrorWasStillDrawing) {
    SetError(DeviceFromContext(hCtx), hr);
  }
}

void AEROGPU_APIENTRY Unmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, UINT subresource) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hResource.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (res->mapped && subresource != res->mapped_subresource) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!UnmapLocked(dev, res)) {
    SetError(dev, E_INVALIDARG);
  }
}

static HRESULT DynamicBufferMapCore11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      D3D11DDI_HRESOURCE hResource,
                                      uint32_t bind_mask,
                                      uint32_t map_u32,
                                      void** ppData) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate || !ppData) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if ((res->bind_flags & bind_mask) == 0) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  D3D11DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = MapLocked11(dev,
                                 res,
                                 /*subresource=*/0,
                                 static_cast<D3D11_DDI_MAP>(map_u32),
                                 /*map_flags=*/0,
                                 &mapped);
  if (FAILED(hr)) {
    return hr;
  }
  *ppData = mapped.pData;
  return S_OK;
}

HRESULT AEROGPU_APIENTRY StagingResourceMap11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HRESOURCE hResource,
                                              UINT subresource,
                                              D3D11_DDI_MAP map_type,
                                              UINT map_flags,
                                              D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnStagingResourceMap subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if (res->usage != kD3D11UsageStaging) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  return MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY StagingResourceMap11Void(D3D11DDI_HDEVICECONTEXT hCtx,
                                               D3D11DDI_HRESOURCE hResource,
                                               UINT subresource,
                                               D3D11_DDI_MAP map_type,
                                               UINT map_flags,
                                               D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  const HRESULT hr = StagingResourceMap11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
  if (hr == kDxgiErrorWasStillDrawing) {
    SetError(DeviceFromContext(hCtx), hr);
  }
}

void AEROGPU_APIENTRY StagingResourceUnmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, UINT subresource) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hResource.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (res->usage != kD3D11UsageStaging) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (res->mapped && subresource != res->mapped_subresource) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!UnmapLocked(dev, res)) {
    SetError(dev, E_INVALIDARG);
  }
}

HRESULT AEROGPU_APIENTRY DynamicIABufferMapDiscard11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();
  return DynamicBufferMapCore11(hCtx,
                                hResource,
                                kD3D11BindVertexBuffer | kD3D11BindIndexBuffer,
                                kD3D11MapWriteDiscard,
                                ppData);
}

void AEROGPU_APIENTRY DynamicIABufferMapDiscard11Void(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  (void)DynamicIABufferMapDiscard11(hCtx, hResource, ppData);
}

HRESULT AEROGPU_APIENTRY DynamicIABufferMapNoOverwrite11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();
  return DynamicBufferMapCore11(hCtx,
                               hResource,
                               kD3D11BindVertexBuffer | kD3D11BindIndexBuffer,
                               kD3D11MapWriteNoOverwrite,
                               ppData);
}

void AEROGPU_APIENTRY DynamicIABufferMapNoOverwrite11Void(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  (void)DynamicIABufferMapNoOverwrite11(hCtx, hResource, ppData);
}

void AEROGPU_APIENTRY DynamicIABufferUnmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hResource.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (res->kind != ResourceKind::Buffer || (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) == 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!UnmapLocked(dev, res)) {
    SetError(dev, E_INVALIDARG);
  }
}

HRESULT AEROGPU_APIENTRY DynamicConstantBufferMapDiscard11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();
  return DynamicBufferMapCore11(hCtx, hResource, kD3D11BindConstantBuffer, kD3D11MapWriteDiscard, ppData);
}

void AEROGPU_APIENTRY DynamicConstantBufferMapDiscard11Void(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  (void)DynamicConstantBufferMapDiscard11(hCtx, hResource, ppData);
}

void AEROGPU_APIENTRY DynamicConstantBufferUnmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  if (!hResource.pDrvPrivate) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  if (res->kind != ResourceKind::Buffer || (res->bind_flags & kD3D11BindConstantBuffer) == 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!UnmapLocked(dev, res)) {
    SetError(dev, E_INVALIDARG);
  }
}

void AEROGPU_APIENTRY UpdateSubresourceUP11(D3D11DDI_HDEVICECONTEXT hCtx,
                                            D3D11DDI_HRESOURCE hDstResource,
                                            UINT dst_subresource,
                                            const D3D10_DDI_BOX* pDstBox,
                                            const void* pSysMem,
                                            UINT src_pitch,
                                            UINT /*src_slice_pitch*/) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hDstResource.pDrvPrivate || !pSysMem) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hDstResource);
  if (!res) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  Texture2DSubresourceLayout dst_sub_layout{};
  if (res->kind == ResourceKind::Texture2D) {
    const uint64_t count_u64 =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    if (count_u64 == 0 || count_u64 > static_cast<uint64_t>(UINT32_MAX)) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (dst_subresource >= static_cast<UINT>(count_u64) || dst_subresource >= res->tex2d_subresources.size()) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    dst_sub_layout = res->tex2d_subresources[dst_subresource];
  } else if (dst_subresource != 0) {
    SetError(dev, E_INVALIDARG);
    return;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  const D3DDDI_DEVICECALLBACKS* ddi =
      is_guest_backed ? reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks) : nullptr;
  const auto* device_cb =
      is_guest_backed ? reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks) : nullptr;

  const auto lock_for_write = [&](D3DDDICB_LOCK* lock_args) -> HRESULT {
    if (!lock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnLockCb) {
      return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    if (device_cb && device_cb->pfnLockCb) {
      return CallCbMaybeHandle(device_cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), lock_args);
    }
    return E_NOTIMPL;
  };

  const auto unlock = [&](D3DDDICB_UNLOCK* unlock_args) -> HRESULT {
    if (!unlock_args || !dev->runtime_device) {
      return E_NOTIMPL;
    }
    if (ddi && ddi->pfnUnlockCb) {
      return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), unlock_args);
    }
    if (device_cb && device_cb->pfnUnlockCb) {
      return CallCbMaybeHandle(device_cb->pfnUnlockCb,
                               MakeRtDeviceHandle(dev),
                               MakeRtDeviceHandle10(dev),
                               unlock_args);
    }
    return E_NOTIMPL;
  };

  if (is_guest_backed) {
    const bool has_lock_unlock =
        (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) || (device_cb && device_cb->pfnLockCb && device_cb->pfnUnlockCb);
    if (!has_lock_unlock) {
      SetError(dev, E_NOTIMPL);
      return;
    }
    if (res->wddm_allocation_handle == 0) {
      SetError(dev, E_NOTIMPL);
      return;
    }
  }

  if (res->kind == ResourceKind::Buffer) {
    uint64_t dst_off = 0;
    uint64_t bytes = res->size_bytes;
    if (pDstBox) {
      if (pDstBox->right < pDstBox->left || pDstBox->top != 0 || pDstBox->bottom != 1 || pDstBox->front != 0 ||
          pDstBox->back != 1) {
        SetError(dev, E_INVALIDARG);
        return;
      }
      dst_off = static_cast<uint64_t>(pDstBox->left);
      bytes = static_cast<uint64_t>(pDstBox->right - pDstBox->left);
    }
    if (dst_off > res->size_bytes || bytes > res->size_bytes - dst_off) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (res->storage.size() < dst_off + bytes) {
      SetError(dev, E_FAIL);
      return;
    }
    if (bytes == 0) {
      return;
    }

    if (!is_guest_backed) {
      // For buffer uploads, the protocol requires the emitted UPLOAD_RESOURCE
      // range to be 4-byte aligned. Use a staging buffer for the aligned range
      // and only commit to the shadow `res->storage` after we successfully
      // append the upload command (avoids UMD/host drift on OOM).
      const uint64_t end = dst_off + bytes;
      const uint64_t upload_offset = dst_off & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      if (upload_end < upload_offset) {
        SetError(dev, E_INVALIDARG);
        return;
      }
      const uint64_t upload_size = upload_end - upload_offset;
      if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(dev, E_OUTOFMEMORY);
        return;
      }
      const size_t upload_off = static_cast<size_t>(upload_offset);
      const size_t upload_sz = static_cast<size_t>(upload_size);
      if (upload_off > res->storage.size() || upload_sz > res->storage.size() - upload_off) {
        SetError(dev, E_FAIL);
        return;
      }

      // Fast path: the update range is already 4-byte aligned, so the upload
      // payload can be taken directly from `pSysMem`.
      const bool is_aligned_upload = (upload_offset == dst_off) && (upload_size == bytes);
      std::vector<uint8_t> upload_payload;
      const void* upload_data = nullptr;
      size_t upload_data_bytes = 0;
      if (is_aligned_upload) {
        upload_data = pSysMem;
        upload_data_bytes = static_cast<size_t>(bytes);
      } else {
        try {
          upload_payload.resize(upload_sz);
        } catch (...) {
          SetError(dev, E_OUTOFMEMORY);
          return;
        }
        if (upload_sz) {
          std::memcpy(upload_payload.data(), res->storage.data() + upload_off, upload_sz);
        }
        std::memcpy(upload_payload.data() + static_cast<size_t>(dst_off - upload_offset),
                    pSysMem,
                    static_cast<size_t>(bytes));
        upload_data = upload_payload.data();
        upload_data_bytes = upload_payload.size();
      }

      auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, upload_data, upload_data_bytes);
      if (!cmd) {
        SetError(dev, E_OUTOFMEMORY);
        return;
      }
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = upload_offset;
      cmd->size_bytes = upload_size;

      std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pSysMem, static_cast<size_t>(bytes));
      return;
    }

    D3DDDICB_LOCK lock_args = {};
    lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
    __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
      lock_args.SubresourceIndex = dst_subresource;
    }
    __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
      lock_args.SubResourceIndex = dst_subresource;
    }
    InitLockForWrite(&lock_args);

    HRESULT hr = lock_for_write(&lock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }

    if (!lock_args.pData) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = dst_subresource;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = dst_subresource;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_FAIL);
      return;
    }

    // Only commit the write to both the runtime allocation and the shadow copy
    // if we can successfully append the corresponding dirty-range command.
    const WddmAllocListCheckpoint alloc_checkpoint(dev);
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = dst_subresource;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = dst_subresource;
      }
      (void)unlock(&unlock_args);
      alloc_checkpoint.rollback();
      return;
    }
    auto* dirty_cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!dirty_cmd) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = dst_subresource;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = dst_subresource;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_OUTOFMEMORY);
      alloc_checkpoint.rollback();
      return;
    }
    dirty_cmd->resource_handle = res->handle;
    dirty_cmd->reserved0 = 0;
    dirty_cmd->offset_bytes = dst_off;
    dirty_cmd->size_bytes = bytes;

    std::memcpy(static_cast<uint8_t*>(lock_args.pData) + static_cast<size_t>(dst_off),
                pSysMem,
                static_cast<size_t>(bytes));
    std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pSysMem, static_cast<size_t>(bytes));

    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = dst_subresource;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = dst_subresource;
    }

    hr = unlock(&unlock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(pSysMem);
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aer_fmt);
    const uint32_t mip_w = dst_sub_layout.width;
    const uint32_t mip_h = dst_sub_layout.height;
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, mip_w);
    if (!layout.valid || min_row_bytes == 0 || dst_sub_layout.row_pitch_bytes < min_row_bytes) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    uint32_t left = 0;
    uint32_t top = 0;
    uint32_t right = mip_w;
    uint32_t bottom = mip_h;
    if (pDstBox) {
      if (pDstBox->right < pDstBox->left || pDstBox->bottom < pDstBox->top || pDstBox->front != 0 ||
          pDstBox->back != 1) {
        SetError(dev, E_INVALIDARG);
        return;
      }
      left = pDstBox->left;
      top = pDstBox->top;
      right = pDstBox->right;
      bottom = pDstBox->bottom;
    }
    if (right > mip_w || bottom > mip_h) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    if (layout.block_width > 1 || layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((left % layout.block_width) != 0 || (top % layout.block_height) != 0 ||
          !aligned_or_edge(right, layout.block_width, mip_w) ||
          !aligned_or_edge(bottom, layout.block_height, mip_h)) {
        SetError(dev, E_INVALIDARG);
        return;
      }
    }

    const uint32_t block_left = left / layout.block_width;
    const uint32_t block_top = top / layout.block_height;
    const uint32_t block_right = aerogpu_div_round_up_u32(right, layout.block_width);
    const uint32_t block_bottom = aerogpu_div_round_up_u32(bottom, layout.block_height);
    if (block_right < block_left || block_bottom < block_top) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width_blocks = block_right - block_left;
    const uint32_t copy_height_blocks = block_bottom - block_top;
    const uint64_t row_bytes_u64 = static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > UINT32_MAX || copy_height_blocks == 0) {
      return;
    }
    const uint32_t row_bytes = static_cast<uint32_t>(row_bytes_u64);

    const uint32_t pitch = src_pitch ? src_pitch : row_bytes;
    if (pitch < row_bytes) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const bool full_row_update = (left == 0) && (right == mip_w);
    const bool full_subresource_update = full_row_update && (top == 0) && (bottom == mip_h);
    if (block_left > UINT32_MAX / layout.bytes_per_block) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    const uint64_t row_needed =
        static_cast<uint64_t>(block_left) * static_cast<uint64_t>(layout.bytes_per_block) + static_cast<uint64_t>(row_bytes);
    if (row_needed > dst_sub_layout.row_pitch_bytes) {
      SetError(dev, E_INVALIDARG);
      return;
    }
    if (block_top + copy_height_blocks > dst_sub_layout.rows_in_layout) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    if (!is_guest_backed) {
      // Host-owned textures: build the UPLOAD_RESOURCE packet before mutating
      // the shadow copy, so OOM during command emission doesn't desynchronize
      // the UMD from the host.
      uint64_t upload_offset = dst_sub_layout.offset_bytes;
      uint64_t upload_size = dst_sub_layout.size_bytes;
      if (!full_subresource_update) {
        // Host-owned texture uploads must be row-aligned for the host executor.
        // Upload the affected row range (full rows) so we do not clobber unrelated
        // rows of the subresource.
        const uint64_t row_pitch_u64 = static_cast<uint64_t>(dst_sub_layout.row_pitch_bytes);
        const uint64_t row_start_bytes = static_cast<uint64_t>(block_top) * row_pitch_u64;
        upload_offset = dst_sub_layout.offset_bytes + row_start_bytes;
        upload_size = static_cast<uint64_t>(copy_height_blocks) * row_pitch_u64;
        if ((block_top != 0 && row_start_bytes / row_pitch_u64 != block_top) ||
            upload_offset < dst_sub_layout.offset_bytes ||
            upload_size / row_pitch_u64 != copy_height_blocks) {
          SetError(dev, E_INVALIDARG);
          return;
        }
      }

      if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(dev, E_OUTOFMEMORY);
        return;
      }
      const size_t upload_off = static_cast<size_t>(upload_offset);
      const size_t upload_sz = static_cast<size_t>(upload_size);
      if (upload_off > res->storage.size() || upload_sz > res->storage.size() - upload_off) {
        SetError(dev, E_FAIL);
        return;
      }

      auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + upload_off, upload_sz);
      if (!cmd) {
        SetError(dev, E_OUTOFMEMORY);
        return;
      }
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = upload_offset;
      cmd->size_bytes = upload_size;

      uint8_t* upload_payload = reinterpret_cast<uint8_t*>(cmd) + sizeof(*cmd);
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        const size_t dst_off =
            static_cast<size_t>(dst_sub_layout.offset_bytes) +
            static_cast<size_t>(block_top + y) * dst_sub_layout.row_pitch_bytes +
            static_cast<size_t>(block_left) * layout.bytes_per_block;
        const size_t src_off = static_cast<size_t>(y) * pitch;
        if (dst_off + row_bytes > res->storage.size() || dst_off < upload_off) {
          SetError(dev, E_FAIL);
          return;
        }
        const size_t payload_off = dst_off - upload_off;
        std::memcpy(upload_payload + payload_off, src_bytes + src_off, row_bytes);
        if (full_row_update && dst_sub_layout.row_pitch_bytes > row_bytes) {
          std::memset(upload_payload + payload_off + row_bytes, 0, dst_sub_layout.row_pitch_bytes - row_bytes);
        }
      }

      // Commit to the shadow copy only after we successfully emitted the upload
      // packet.
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        const size_t dst_off =
            static_cast<size_t>(dst_sub_layout.offset_bytes) +
            static_cast<size_t>(block_top + y) * dst_sub_layout.row_pitch_bytes +
            static_cast<size_t>(block_left) * layout.bytes_per_block;
        const size_t src_off = static_cast<size_t>(y) * pitch;
        std::memcpy(res->storage.data() + dst_off, src_bytes + src_off, row_bytes);
        if (full_row_update && dst_sub_layout.row_pitch_bytes > row_bytes) {
          std::memset(res->storage.data() + dst_off + row_bytes, 0, dst_sub_layout.row_pitch_bytes - row_bytes);
        }
      }
      return;
    }

    // Guest-backed texture: lock the runtime allocation and emit a dirty-range
    // command before writing into the allocation/shadow to avoid drift on OOM.
    D3DDDICB_LOCK lock_args = {};
    lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
    __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
      lock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
      lock_args.SubResourceIndex = 0;
    }
    InitLockForWrite(&lock_args);

    HRESULT hr = lock_for_write(&lock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }
    if (!lock_args.pData) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_FAIL);
      return;
    }

    if (dst_sub_layout.offset_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_FAIL);
      return;
    }

    uint32_t lock_pitch = 0;
    __if_exists(D3DDDICB_LOCK::Pitch) {
      lock_pitch = lock_args.Pitch;
    }
    const bool use_lock_pitch = (dst_sub_layout.mip_level == 0);
    if (use_lock_pitch && lock_pitch != 0) {
      LogTexture2DPitchMismatchRateLimited("UpdateSubresourceUP11",
                                           res,
                                           dst_subresource,
                                           dst_sub_layout.row_pitch_bytes,
                                           lock_pitch);
    }

    const WddmAllocListCheckpoint alloc_checkpoint(dev);
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      alloc_checkpoint.rollback();
      return;
    }
    auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!dirty) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = 0;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = 0;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_OUTOFMEMORY);
      alloc_checkpoint.rollback();
      return;
    }
    dirty->resource_handle = res->handle;
    dirty->reserved0 = 0;
    dirty->offset_bytes = dst_sub_layout.offset_bytes;
    dirty->size_bytes = dst_sub_layout.size_bytes;

    const uint32_t wddm_pitch = dst_sub_layout.row_pitch_bytes;
    uint8_t* wddm_base =
        static_cast<uint8_t*>(lock_args.pData) + static_cast<size_t>(dst_sub_layout.offset_bytes);

    for (uint32_t y = 0; y < copy_height_blocks; y++) {
      const size_t dst_off_storage =
          static_cast<size_t>(dst_sub_layout.offset_bytes) +
          static_cast<size_t>(block_top + y) * dst_sub_layout.row_pitch_bytes +
          static_cast<size_t>(block_left) * layout.bytes_per_block;
      const size_t dst_off_wddm =
          static_cast<size_t>(block_top + y) * wddm_pitch + static_cast<size_t>(block_left) * layout.bytes_per_block;
      const size_t src_off = static_cast<size_t>(y) * pitch;
      if (dst_off_storage + row_bytes > res->storage.size()) {
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
          unlock_args.SubresourceIndex = 0;
        }
        __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
          unlock_args.SubResourceIndex = 0;
        }
        (void)unlock(&unlock_args);
        SetError(dev, E_FAIL);
        return;
      }
      std::memcpy(res->storage.data() + dst_off_storage, src_bytes + src_off, row_bytes);
      std::memcpy(wddm_base + dst_off_wddm, src_bytes + src_off, row_bytes);
      if (full_row_update && dst_sub_layout.row_pitch_bytes > row_bytes) {
        std::memset(res->storage.data() + dst_off_storage + row_bytes, 0, dst_sub_layout.row_pitch_bytes - row_bytes);
        std::memset(wddm_base + dst_off_wddm + row_bytes, 0, wddm_pitch - row_bytes);
      }
    }

    D3DDDICB_UNLOCK unlock_args = {};
    unlock_args.hAllocation = lock_args.hAllocation;
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock_args.SubresourceIndex = 0;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock_args.SubResourceIndex = 0;
    }
    hr = unlock(&unlock_args);
    if (FAILED(hr)) {
      SetError(dev, hr);
      return;
    }
    return;
  }

  SetError(dev, E_NOTIMPL);
}

void AEROGPU_APIENTRY UpdateSubresourceUP11Args(D3D11DDI_HDEVICECONTEXT hCtx, const D3D11DDIARG_UPDATESUBRESOURCEUP* pArgs) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pArgs) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return;
  }

  const void* pSysMem = nullptr;
  if constexpr (has_member_pSysMemUP<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pSysMem = pArgs->pSysMemUP;
  } else if constexpr (has_member_pSysMem<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pSysMem = pArgs->pSysMem;
  }

  UINT pitch = 0;
  if constexpr (has_member_SrcPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->SrcPitch;
  } else if constexpr (has_member_RowPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->RowPitch;
  } else if constexpr (has_member_SysMemPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->SysMemPitch;
  }

  UINT slice_pitch = 0;
  if constexpr (has_member_SrcSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->SrcSlicePitch;
  } else if constexpr (has_member_DepthPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->DepthPitch;
  } else if constexpr (has_member_SysMemSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->SysMemSlicePitch;
  }

  UpdateSubresourceUP11(hCtx, pArgs->hDstResource, pArgs->DstSubresource, pArgs->pDstBox, pSysMem, pitch, slice_pitch);
}

void AEROGPU_APIENTRY UpdateSubresourceUP11ArgsAndSysMem(D3D11DDI_HDEVICECONTEXT hCtx,
                                                        const D3D11DDIARG_UPDATESUBRESOURCEUP* pArgs,
                                                        const void* pSysMem) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pArgs) {
    if (dev) {
      SetError(dev, E_INVALIDARG);
    }
    return;
  }

  UINT pitch = 0;
  if constexpr (has_member_SrcPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->SrcPitch;
  } else if constexpr (has_member_RowPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->RowPitch;
  } else if constexpr (has_member_SysMemPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    pitch = pArgs->SysMemPitch;
  }

  UINT slice_pitch = 0;
  if constexpr (has_member_SrcSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->SrcSlicePitch;
  } else if constexpr (has_member_DepthPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->DepthPitch;
  } else if constexpr (has_member_SysMemSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
    slice_pitch = pArgs->SysMemSlicePitch;
  }

  UpdateSubresourceUP11(hCtx, pArgs->hDstResource, pArgs->DstSubresource, pArgs->pDstBox, pSysMem, pitch, slice_pitch);
}

void AEROGPU_APIENTRY Flush11(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH)) {
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  HRESULT hr = S_OK;
  submit_locked(dev, /*want_present=*/false, &hr);
  if (FAILED(hr)) {
    SetError(dev, hr);
  }
}

HRESULT AEROGPU_APIENTRY Present11(D3D11DDI_HDEVICECONTEXT hCtx, const D3D10DDIARG_PRESENT* pPresent) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pPresent) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto present_once = [&]() -> HRESULT {
    const auto cmd_checkpoint = dev->cmd.checkpoint();
    const WddmAllocListCheckpoint alloc_checkpoint(dev);
    auto rollback = [&]() {
      dev->cmd.rollback(cmd_checkpoint);
      alloc_checkpoint.rollback();
    };

    D3D10DDI_HRESOURCE hsrc = {};
    __if_exists(D3D10DDIARG_PRESENT::hSrcResource) {
      hsrc = pPresent->hSrcResource;
    }
    __if_exists(D3D10DDIARG_PRESENT::hRenderTarget) {
      hsrc = pPresent->hRenderTarget;
    }
    __if_exists(D3D10DDIARG_PRESENT::hResource) {
      hsrc = pPresent->hResource;
    }
    __if_exists(D3D10DDIARG_PRESENT::hSurface) {
      hsrc = pPresent->hSurface;
    }

    Resource* src_res = hsrc.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, Resource>(hsrc) : nullptr;
    TrackWddmAllocForSubmitLocked(dev, src_res, /*write=*/false);
    if (dev->wddm_submit_allocation_list_oom) {
      rollback();
      return E_OUTOFMEMORY;
    }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    aerogpu_handle_t src_handle = 0;
    src_handle = src_res ? src_res->handle : 0;
    AEROGPU_D3D10_11_LOG("trace_resources: D3D11 Present sync=%u src_handle=%u",
                         static_cast<unsigned>(pPresent->SyncInterval),
                         static_cast<unsigned>(src_handle));
#endif

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
    if (!cmd) {
      rollback();
      return E_OUTOFMEMORY;
    }
    cmd->scanout_id = 0;
    bool vsync = (pPresent->SyncInterval != 0);
    if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
      vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
    }
    cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

    HRESULT hr = S_OK;
    submit_locked(dev, /*want_present=*/true, &hr);
    return FAILED(hr) ? hr : S_OK;
  };

  HRESULT hr = present_once();
  if (hr != E_OUTOFMEMORY) {
    return hr;
  }

  // If Present failed due to OOM while tracking allocations or appending the
  // packet, try to submit the already-recorded command buffer without present
  // (so the host stays in sync with the software shadow), then retry a minimal
  // Present submission.
  HRESULT flush_hr = S_OK;
  submit_locked(dev, /*want_present=*/false, &flush_hr);
  if (FAILED(flush_hr)) {
    return flush_hr;
  }
  return present_once();
}

void AEROGPU_APIENTRY RotateResourceIdentities11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE* pResources, UINT numResources) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pResources || numResources < 2) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  AEROGPU_D3D10_11_LOG("trace_resources: D3D11 RotateResourceIdentities count=%u",
                       static_cast<unsigned>(numResources));
  for (UINT i = 0; i < numResources; i++) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  + slot[%u]=%u",
                          static_cast<unsigned>(i),
                          static_cast<unsigned>(handle));
  }
#endif

  std::vector<Resource*> resources;
  try {
    resources.reserve(numResources);
  } catch (...) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  for (UINT i = 0; i < numResources; ++i) {
    auto* res = pResources[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[i]) : nullptr;
    if (!res || res->mapped) {
      return;
    }
    if (std::find(resources.begin(), resources.end(), res) != resources.end()) {
      // Reject duplicates: RotateResourceIdentities expects distinct resources.
      return;
    }
    // Shared resources have stable identities (`share_token`); rotating them is
    // likely to break EXPORT/IMPORT semantics across processes.
    if (res->is_shared || res->is_shared_alias || res->share_token != 0) {
      return;
    }
    resources.push_back(res);
  }

  const Resource* ref = resources[0];
  if (!ref || ref->kind != ResourceKind::Texture2D || !(ref->bind_flags & kD3D11BindRenderTarget)) {
    return;
  }
  for (UINT i = 1; i < numResources; ++i) {
    const Resource* r = resources[i];
    if (!r || r->kind != ResourceKind::Texture2D || !(r->bind_flags & kD3D11BindRenderTarget) ||
        r->width != ref->width || r->height != ref->height || r->dxgi_format != ref->dxgi_format ||
        r->mip_levels != ref->mip_levels || r->array_size != ref->array_size) {
      return;
    }
  }

  // Treat RotateResourceIdentities as a transaction: if rebinding packets cannot
  // be appended (OOM), roll back the command stream and undo the rotation so the
  // runtime-visible state remains unchanged.
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const uint32_t prev_rtv_count = dev->current_rtv_count;
  const std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> prev_rtvs = dev->current_rtvs;
  const aerogpu_handle_t prev_dsv = dev->current_dsv;
  aerogpu_handle_t prev_vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_gs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_cs_srvs[kMaxShaderResourceSlots] = {};
  std::memcpy(prev_vs_srvs, dev->vs_srvs, sizeof(prev_vs_srvs));
  std::memcpy(prev_ps_srvs, dev->ps_srvs, sizeof(prev_ps_srvs));
  std::memcpy(prev_gs_srvs, dev->gs_srvs, sizeof(prev_gs_srvs));
  std::memcpy(prev_cs_srvs, dev->cs_srvs, sizeof(prev_cs_srvs));

  struct ResourceIdentity {
    aerogpu_handle_t handle = 0;
    uint32_t backing_alloc_id = 0;
    uint32_t backing_offset_bytes = 0;
    uint32_t wddm_allocation_handle = 0;
    Resource::WddmIdentity wddm;
    std::vector<Texture2DSubresourceLayout> tex2d_subresources;
    std::vector<uint8_t> storage;
    uint64_t last_gpu_write_fence = 0;
    bool mapped = false;
    uint32_t mapped_map_type = 0;
    uint32_t mapped_map_flags = 0;
    uint32_t mapped_subresource = 0;
    uint64_t mapped_offset = 0;
    uint64_t mapped_size = 0;
  };

  auto take_identity = [](Resource* res) -> ResourceIdentity {
    ResourceIdentity id{};
    if (!res) {
      return id;
    }
    id.handle = res->handle;
    id.backing_alloc_id = res->backing_alloc_id;
    id.backing_offset_bytes = res->backing_offset_bytes;
    id.wddm_allocation_handle = res->wddm_allocation_handle;
    id.wddm = std::move(res->wddm);
    id.tex2d_subresources = std::move(res->tex2d_subresources);
    id.storage = std::move(res->storage);
    id.last_gpu_write_fence = res->last_gpu_write_fence;
    id.mapped = res->mapped;
    id.mapped_map_type = res->mapped_map_type;
    id.mapped_map_flags = res->mapped_map_flags;
    id.mapped_subresource = res->mapped_subresource;
    id.mapped_offset = res->mapped_offset;
    id.mapped_size = res->mapped_size;
    return id;
  };

  auto put_identity = [](Resource* res, ResourceIdentity&& id) {
    if (!res) {
      return;
    }
    res->handle = id.handle;
    res->backing_alloc_id = id.backing_alloc_id;
    res->backing_offset_bytes = id.backing_offset_bytes;
    res->wddm_allocation_handle = id.wddm_allocation_handle;
    res->wddm = std::move(id.wddm);
    res->tex2d_subresources = std::move(id.tex2d_subresources);
    res->storage = std::move(id.storage);
    res->last_gpu_write_fence = id.last_gpu_write_fence;
    res->mapped = id.mapped;
    res->mapped_map_type = id.mapped_map_type;
    res->mapped_map_flags = id.mapped_map_flags;
    res->mapped_subresource = id.mapped_subresource;
    res->mapped_offset = id.mapped_offset;
    res->mapped_size = id.mapped_size;
  };

  auto rollback_rotation = [&](bool report_oom) {
    dev->cmd.rollback(cmd_checkpoint);

    // Undo the rotation (rotate right by one).
    ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
    for (UINT i = numResources - 1; i > 0; --i) {
      put_identity(resources[i], take_identity(resources[i - 1]));
    }
    put_identity(resources[0], std::move(undo_saved));

    dev->current_rtv_count = prev_rtv_count;
    dev->current_rtvs = prev_rtvs;
    dev->current_dsv = prev_dsv;
    std::memcpy(dev->vs_srvs, prev_vs_srvs, sizeof(prev_vs_srvs));
    std::memcpy(dev->ps_srvs, prev_ps_srvs, sizeof(prev_ps_srvs));
    std::memcpy(dev->gs_srvs, prev_gs_srvs, sizeof(prev_gs_srvs));
    std::memcpy(dev->cs_srvs, prev_cs_srvs, sizeof(prev_cs_srvs));

    if (report_oom) {
      SetError(dev, E_OUTOFMEMORY);
    }
  };

  // Capture the pre-rotation AeroGPU handles so we can remap bound handle slots
  // (which store raw protocol handles, not resource pointers).
  std::vector<aerogpu_handle_t> old_handles;
  try {
    old_handles.reserve(resources.size());
  } catch (...) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  for (auto* res : resources) {
    old_handles.push_back(res ? res->handle : 0);
  }

  ResourceIdentity saved = take_identity(resources[0]);
  for (UINT i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  auto remap_handle = [&](aerogpu_handle_t handle) -> aerogpu_handle_t {
    if (!handle) {
      return handle;
    }
    for (size_t i = 0; i < old_handles.size(); ++i) {
      if (old_handles[i] == handle) {
        return resources[i] ? resources[i]->handle : 0;
      }
    }
    return handle;
  };

  // If any bound outputs were rotated (e.g. swapchain backbuffer), re-emit the
  // OM binding with the new protocol handles.
  bool outputs_need_rebind = false;
  const uint32_t bound_rtv_count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> new_rtvs = dev->current_rtvs;
  for (uint32_t i = 0; i < bound_rtv_count; ++i) {
    if (dev->current_rtv_resources[i] &&
        std::find(resources.begin(), resources.end(), dev->current_rtv_resources[i]) != resources.end()) {
      outputs_need_rebind = true;
    }
    new_rtvs[i] = remap_handle(new_rtvs[i]);
  }
  aerogpu_handle_t new_dsv = remap_handle(dev->current_dsv);
  if (dev->current_dsv_resource &&
      std::find(resources.begin(), resources.end(), dev->current_dsv_resource) != resources.end()) {
    outputs_need_rebind = true;
  }

  if (outputs_need_rebind) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!cmd) {
      rollback_rotation(/*report_oom=*/true);
      return;
    }

    // Update the cached handles only after we've successfully appended the
    // rebind packet. If we fail to append (OOM), we roll back the rotation and
    // must keep the previous handles intact.
    dev->current_rtvs = new_rtvs;
    dev->current_dsv = new_dsv;
 
    cmd->color_count = bound_rtv_count;
    cmd->depth_stencil = new_dsv;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      cmd->colors[i] = (i < bound_rtv_count) ? new_rtvs[i] : 0;
    }

    // Bring-up logging: swapchains may rebind RT state via RotateResourceIdentities.
    AEROGPU_D3D10_11_LOG("SET_RENDER_TARGETS (rotate): color_count=%u depth=%u colors=[%u,%u,%u,%u,%u,%u,%u,%u]",
                         static_cast<unsigned>(bound_rtv_count),
                         static_cast<unsigned>(new_dsv),
                         static_cast<unsigned>(cmd->colors[0]),
                         static_cast<unsigned>(cmd->colors[1]),
                         static_cast<unsigned>(cmd->colors[2]),
                         static_cast<unsigned>(cmd->colors[3]),
                         static_cast<unsigned>(cmd->colors[4]),
                         static_cast<unsigned>(cmd->colors[5]),
                         static_cast<unsigned>(cmd->colors[6]),
                         static_cast<unsigned>(cmd->colors[7]));
  }

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    const aerogpu_handle_t new_vs = remap_handle(dev->vs_srvs[slot]);
    if (new_vs != dev->vs_srvs[slot]) {
      if (!SetTextureLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, new_vs)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->vs_srvs[slot] = new_vs;
    }
    const aerogpu_handle_t new_ps = remap_handle(dev->ps_srvs[slot]);
    if (new_ps != dev->ps_srvs[slot]) {
      if (!SetTextureLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, new_ps)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->ps_srvs[slot] = new_ps;
    }
  }

  for (uint32_t slot = 0; slot < dev->current_cs_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_cs_srvs[slot])) {
      continue;
    }
    const aerogpu_handle_t new_cs = dev->current_cs_srvs[slot] ? dev->current_cs_srvs[slot]->handle : 0;
    if (new_cs != dev->cs_srvs[slot]) {
      if (!SetTextureLocked(dev, AEROGPU_SHADER_STAGE_COMPUTE, slot, new_cs)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->cs_srvs[slot] = new_cs;
    }
  }

  for (uint32_t slot = 0; slot < dev->current_gs_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_gs_srvs[slot])) {
      continue;
    }
    const aerogpu_handle_t new_gs = dev->current_gs_srvs[slot] ? dev->current_gs_srvs[slot]->handle : 0;
    if (new_gs != dev->gs_srvs[slot]) {
      if (!SetTextureLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, new_gs)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->gs_srvs[slot] = new_gs;
    }
  }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  for (UINT i = 0; i < numResources; i++) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  -> slot[%u]=%u",
                         static_cast<unsigned>(i),
                         static_cast<unsigned>(handle));
  }
#endif
}

HRESULT AEROGPU_APIENTRY Present11Device(D3D11DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->immediate_context) {
    return E_FAIL;
  }
  D3D11DDI_HDEVICECONTEXT hCtx = {};
  hCtx.pDrvPrivate = dev->immediate_context;
  return Present11(hCtx, pPresent);
}

void AEROGPU_APIENTRY RotateResourceIdentities11Device(D3D11DDI_HDEVICE hDevice,
                                                      D3D11DDI_HRESOURCE* pResources,
                                                      UINT numResources) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->immediate_context) {
    return;
  }
  D3D11DDI_HDEVICECONTEXT hCtx = {};
  hCtx.pDrvPrivate = dev->immediate_context;
  RotateResourceIdentities11(hCtx, pResources, numResources);
}

// -------------------------------------------------------------------------------------------------
// Device creation
// -------------------------------------------------------------------------------------------------

HRESULT AEROGPU_APIENTRY CreateDevice11(D3D10DDI_HADAPTER hAdapter, D3D11DDIARG_CREATEDEVICE* pCreateDevice) {
  if (!hAdapter.pDrvPrivate || !pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, Adapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }
  // Make sure the adapter open negotiated a DDI version that matches the table
  // layouts this binary was compiled against.
#if defined(D3D11DDI_SUPPORTED)
  constexpr UINT supported_version = D3D11DDI_SUPPORTED;
#else
  constexpr UINT supported_version = D3D11DDI_INTERFACE_VERSION;
#endif
  if (adapter->d3d11_ddi_version != supported_version) {
    return E_NOINTERFACE;
  }

  auto* ctx_funcs = GetContextFuncTable(pCreateDevice);
  if (!ctx_funcs) {
    return E_INVALIDARG;
  }

  void* ctx_mem = GetImmediateContextHandle(pCreateDevice).pDrvPrivate;
  if (!ctx_mem) {
    // Interface versions without CalcPrivateDeviceContextSize expect the driver
    // to carve out context storage from the device allocation.
    ctx_mem = reinterpret_cast<uint8_t*>(pCreateDevice->hDevice.pDrvPrivate) + sizeof(Device);
    SetImmediateContextHandle(pCreateDevice, ctx_mem);
  }

  auto* dev = new (pCreateDevice->hDevice.pDrvPrivate) Device();
  dev->adapter = adapter;
  const auto* callbacks_in = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(GetDeviceCallbacks(pCreateDevice));
  if (!callbacks_in) {
    dev->~Device();
    return E_INVALIDARG;
  }
  auto* callbacks_copy = new (std::nothrow) D3D11DDI_DEVICECALLBACKS();
  if (!callbacks_copy) {
    dev->~Device();
    return E_OUTOFMEMORY;
  }
  *callbacks_copy = *callbacks_in;
  dev->runtime_callbacks = callbacks_copy;
  dev->runtime_ddi_callbacks = GetDdiCallbacks(pCreateDevice);
  dev->runtime_device = GetRtDevicePrivate(pCreateDevice);

  auto* ctx = new (ctx_mem) AeroGpuDeviceContext();
  ctx->dev = dev;
  dev->immediate_context = ctx;

  HRESULT wddm_hr = InitWddmContext(dev, hAdapter.pDrvPrivate);
  if (FAILED(wddm_hr) || !dev->kmt_context || !dev->kmt_fence_syncobj) {
    DestroyWddmContext(dev);
    ctx->~AeroGpuDeviceContext();
    delete callbacks_copy;
    dev->runtime_callbacks = nullptr;
    dev->~Device();
    return FAILED(wddm_hr) ? wddm_hr : E_FAIL;
  }

  // Win7 runtimes are known to call a surprisingly large chunk of the D3D11 DDI
  // surface (even for simple triangle samples). Start from fully-stubbed
  // defaults so we never leave NULL function pointers behind.
  InitDeviceFuncsWithStubs(pCreateDevice->pDeviceFuncs);
  InitDeviceContextFuncsWithStubs(ctx_funcs);

  // Device funcs.
  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = AEROGPU_D3D11_WDK_DDI(DestroyDevice11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateResourceSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateResource = AEROGPU_D3D11_WDK_DDI(CreateResource11);
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnOpenResource) {
    pCreateDevice->pDeviceFuncs->pfnOpenResource = AEROGPU_D3D11_WDK_DDI(OpenResource11);
  }
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = AEROGPU_D3D11_WDK_DDI(DestroyResource11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateRenderTargetViewSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = AEROGPU_D3D11_WDK_DDI(CreateRenderTargetView11);
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = AEROGPU_D3D11_WDK_DDI(DestroyRenderTargetView11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateDepthStencilViewSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = AEROGPU_D3D11_WDK_DDI(CreateDepthStencilView11);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = AEROGPU_D3D11_WDK_DDI(DestroyDepthStencilView11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateUnorderedAccessViewSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateUnorderedAccessViewSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateUnorderedAccessView = AEROGPU_D3D11_WDK_DDI(CreateUnorderedAccessView11);
  pCreateDevice->pDeviceFuncs->pfnDestroyUnorderedAccessView = AEROGPU_D3D11_WDK_DDI(DestroyUnorderedAccessView11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateShaderResourceViewSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = AEROGPU_D3D11_WDK_DDI(CreateShaderResourceView11);
  pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = AEROGPU_D3D11_WDK_DDI(DestroyShaderResourceView11);

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCalcPrivateUnorderedAccessViewSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateUnorderedAccessViewSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateUnorderedAccessViewSize11);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCreateUnorderedAccessView) {
    pCreateDevice->pDeviceFuncs->pfnCreateUnorderedAccessView = AEROGPU_D3D11_WDK_DDI(CreateUnorderedAccessView11);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnDestroyUnorderedAccessView) {
    pCreateDevice->pDeviceFuncs->pfnDestroyUnorderedAccessView = AEROGPU_D3D11_WDK_DDI(DestroyUnorderedAccessView11);
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateVertexShaderSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = AEROGPU_D3D11_WDK_DDI(CreateVertexShader11);
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = AEROGPU_D3D11_WDK_DDI(DestroyVertexShader11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = AEROGPU_D3D11_WDK_DDI(CalcPrivatePixelShaderSize11);
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = AEROGPU_D3D11_WDK_DDI(CreatePixelShader11);
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = AEROGPU_D3D11_WDK_DDI(DestroyPixelShader11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateGeometryShaderSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader = AEROGPU_D3D11_WDK_DDI(CreateGeometryShader11);
  pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader = AEROGPU_D3D11_WDK_DDI(DestroyGeometryShader11);

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderWithStreamOutputSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        AEROGPU_D3D11_WDK_DDI(CalcPrivateGeometryShaderWithStreamOutputSizeImpl<
                              decltype(pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCreateGeometryShaderWithStreamOutput) {
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput =
        AEROGPU_D3D11_WDK_DDI(CreateGeometryShaderWithStreamOutputImpl<
                              decltype(pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput)>::Call);
  }

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCalcPrivateComputeShaderSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateComputeShaderSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateComputeShaderSize11);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCreateComputeShader) {
    pCreateDevice->pDeviceFuncs->pfnCreateComputeShader = AEROGPU_D3D11_WDK_DDI(CreateComputeShader11);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnDestroyComputeShader) {
    pCreateDevice->pDeviceFuncs->pfnDestroyComputeShader = AEROGPU_D3D11_WDK_DDI(DestroyComputeShader11);
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateElementLayoutSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = AEROGPU_D3D11_WDK_DDI(CreateElementLayout11);
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = AEROGPU_D3D11_WDK_DDI(DestroyElementLayout11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateSamplerSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateSampler = AEROGPU_D3D11_WDK_DDI(CreateSampler11);
  pCreateDevice->pDeviceFuncs->pfnDestroySampler = AEROGPU_D3D11_WDK_DDI(DestroySampler11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateBlendStateSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = AEROGPU_D3D11_WDK_DDI(CreateBlendState11);
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = AEROGPU_D3D11_WDK_DDI(DestroyBlendState11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateRasterizerStateSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = AEROGPU_D3D11_WDK_DDI(CreateRasterizerState11);
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = AEROGPU_D3D11_WDK_DDI(DestroyRasterizerState11);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateDepthStencilStateSize11);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = AEROGPU_D3D11_WDK_DDI(CreateDepthStencilState11);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = AEROGPU_D3D11_WDK_DDI(DestroyDepthStencilState11);

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnGetDeviceRemovedReason) {
    pCreateDevice->pDeviceFuncs->pfnGetDeviceRemovedReason = AEROGPU_D3D11_WDK_DDI(GetDeviceRemovedReason11);
  }

  BindPresentAndRotate(pCreateDevice->pDeviceFuncs);

  // Immediate context funcs.
  ctx_funcs->pfnIaSetInputLayout = AEROGPU_D3D11_WDK_DDI(IaSetInputLayout11);
  ctx_funcs->pfnIaSetVertexBuffers = AEROGPU_D3D11_WDK_DDI(IaSetVertexBuffers11);
  ctx_funcs->pfnIaSetIndexBuffer = AEROGPU_D3D11_WDK_DDI(IaSetIndexBuffer11);
  ctx_funcs->pfnIaSetTopology = AEROGPU_D3D11_WDK_DDI(IaSetTopology11);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSoSetTargets) {
    using Fn = decltype(ctx_funcs->pfnSoSetTargets);
    ctx_funcs->pfnSoSetTargets = AEROGPU_D3D11_WDK_DDI(SoSetTargetsThunk<Fn>::Impl);
  }

  ctx_funcs->pfnVsSetShader = AEROGPU_D3D11_WDK_DDI(VsSetShader11);
  ctx_funcs->pfnVsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(VsSetConstantBuffers11);
  ctx_funcs->pfnVsSetShaderResources = AEROGPU_D3D11_WDK_DDI(VsSetShaderResources11);
  ctx_funcs->pfnVsSetSamplers = AEROGPU_D3D11_WDK_DDI(VsSetSamplers11);

  ctx_funcs->pfnPsSetShader = AEROGPU_D3D11_WDK_DDI(PsSetShader11);
  ctx_funcs->pfnPsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(PsSetConstantBuffers11);
  ctx_funcs->pfnPsSetShaderResources = AEROGPU_D3D11_WDK_DDI(PsSetShaderResources11);
  ctx_funcs->pfnPsSetSamplers = AEROGPU_D3D11_WDK_DDI(PsSetSamplers11);

  ctx_funcs->pfnGsSetShader = AEROGPU_D3D11_WDK_DDI(GsSetShader11);
  ctx_funcs->pfnGsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(GsSetConstantBuffers11);
  ctx_funcs->pfnGsSetShaderResources = AEROGPU_D3D11_WDK_DDI(GsSetShaderResources11);
  ctx_funcs->pfnGsSetSamplers = AEROGPU_D3D11_WDK_DDI(GsSetSamplers11);

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShader) { ctx_funcs->pfnHsSetShader = AEROGPU_D3D11_WDK_DDI(HsSetShader11); }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetConstantBuffers) {
    ctx_funcs->pfnHsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(HsSetConstantBuffers11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShaderResources) {
    ctx_funcs->pfnHsSetShaderResources = AEROGPU_D3D11_WDK_DDI(HsSetShaderResources11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetSamplers) { ctx_funcs->pfnHsSetSamplers = AEROGPU_D3D11_WDK_DDI(HsSetSamplers11); }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShader) { ctx_funcs->pfnDsSetShader = AEROGPU_D3D11_WDK_DDI(DsSetShader11); }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetConstantBuffers) {
    ctx_funcs->pfnDsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(DsSetConstantBuffers11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShaderResources) {
    ctx_funcs->pfnDsSetShaderResources = AEROGPU_D3D11_WDK_DDI(DsSetShaderResources11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetSamplers) { ctx_funcs->pfnDsSetSamplers = AEROGPU_D3D11_WDK_DDI(DsSetSamplers11); }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShader) { ctx_funcs->pfnCsSetShader = AEROGPU_D3D11_WDK_DDI(CsSetShader11); }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetConstantBuffers) {
    ctx_funcs->pfnCsSetConstantBuffers = AEROGPU_D3D11_WDK_DDI(CsSetConstantBuffers11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShaderResources) {
    ctx_funcs->pfnCsSetShaderResources = AEROGPU_D3D11_WDK_DDI(CsSetShaderResources11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetSamplers) { ctx_funcs->pfnCsSetSamplers = AEROGPU_D3D11_WDK_DDI(CsSetSamplers11); }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetUnorderedAccessViews) {
    ctx_funcs->pfnCsSetUnorderedAccessViews = AEROGPU_D3D11_WDK_DDI(CsSetUnorderedAccessViews11);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetPredication) {
    using Fn = decltype(ctx_funcs->pfnSetPredication);
    ctx_funcs->pfnSetPredication = AEROGPU_D3D11_WDK_DDI(SetPredicationThunk<Fn>::Impl);
  }

  ctx_funcs->pfnSetViewports = AEROGPU_D3D11_WDK_DDI(SetViewports11);
  ctx_funcs->pfnSetScissorRects = AEROGPU_D3D11_WDK_DDI(SetScissorRects11);
  ctx_funcs->pfnSetRasterizerState = AEROGPU_D3D11_WDK_DDI(SetRasterizerState11);
  ctx_funcs->pfnSetBlendState = AEROGPU_D3D11_WDK_DDI(SetBlendState11);
  ctx_funcs->pfnSetDepthStencilState = AEROGPU_D3D11_WDK_DDI(SetDepthStencilState11);
  ctx_funcs->pfnSetRenderTargets = AEROGPU_D3D11_WDK_DDI(SetRenderTargets11);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews) {
    ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews =
        AEROGPU_D3D11_WDK_DDI(
            SetRenderTargetsAndUavsThunk<decltype(ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews)>::Impl);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews11_1) {
    ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews11_1 =
        AEROGPU_D3D11_WDK_DDI(
            SetRenderTargetsAndUavsThunk<decltype(ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews11_1)>::Impl);
  }

  ctx_funcs->pfnClearState = AEROGPU_D3D11_WDK_DDI(ClearState11);
  ctx_funcs->pfnClearRenderTargetView = AEROGPU_D3D11_WDK_DDI(ClearRenderTargetView11);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnClearUnorderedAccessViewUint) {
    using Fn = decltype(ctx_funcs->pfnClearUnorderedAccessViewUint);
    if constexpr (std::is_convertible_v<decltype(&ClearUnorderedAccessViewUint11), Fn>) {
      ctx_funcs->pfnClearUnorderedAccessViewUint = AEROGPU_D3D11_WDK_DDI(ClearUnorderedAccessViewUint11);
    } else {
      ctx_funcs->pfnClearUnorderedAccessViewUint = &DdiStub<Fn>::Call;
    }
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnClearUnorderedAccessViewFloat) {
    using Fn = decltype(ctx_funcs->pfnClearUnorderedAccessViewFloat);
    if constexpr (std::is_convertible_v<decltype(&ClearUnorderedAccessViewFloat11), Fn>) {
      ctx_funcs->pfnClearUnorderedAccessViewFloat = AEROGPU_D3D11_WDK_DDI(ClearUnorderedAccessViewFloat11);
    } else {
      ctx_funcs->pfnClearUnorderedAccessViewFloat = &DdiStub<Fn>::Call;
    }
  }
  ctx_funcs->pfnClearDepthStencilView = AEROGPU_D3D11_WDK_DDI(ClearDepthStencilView11);
  ctx_funcs->pfnDraw = AEROGPU_D3D11_WDK_DDI(Draw11);
  ctx_funcs->pfnDrawIndexed = AEROGPU_D3D11_WDK_DDI(DrawIndexed11);
  ctx_funcs->pfnDrawInstanced = AEROGPU_D3D11_WDK_DDI(DrawInstanced11);
  ctx_funcs->pfnDrawIndexedInstanced = AEROGPU_D3D11_WDK_DDI(DrawIndexedInstanced11);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDrawInstancedIndirect) {
    using Fn = decltype(ctx_funcs->pfnDrawInstancedIndirect);
    if constexpr (std::is_convertible_v<decltype(&DrawInstancedIndirect11), Fn>) {
      ctx_funcs->pfnDrawInstancedIndirect = AEROGPU_D3D11_WDK_DDI(DrawInstancedIndirect11);
    } else {
      ctx_funcs->pfnDrawInstancedIndirect = &DdiStub<Fn>::Call;
    }
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDrawIndexedInstancedIndirect) {
    using Fn = decltype(ctx_funcs->pfnDrawIndexedInstancedIndirect);
    if constexpr (std::is_convertible_v<decltype(&DrawIndexedInstancedIndirect11), Fn>) {
      ctx_funcs->pfnDrawIndexedInstancedIndirect = AEROGPU_D3D11_WDK_DDI(DrawIndexedInstancedIndirect11);
    } else {
      ctx_funcs->pfnDrawIndexedInstancedIndirect = &DdiStub<Fn>::Call;
    }
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDispatch) { ctx_funcs->pfnDispatch = AEROGPU_D3D11_WDK_DDI(Dispatch11); }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDispatchIndirect) {
    using Fn = decltype(ctx_funcs->pfnDispatchIndirect);
    if constexpr (std::is_convertible_v<decltype(&DispatchIndirect11), Fn>) {
      ctx_funcs->pfnDispatchIndirect = AEROGPU_D3D11_WDK_DDI(DispatchIndirect11);
    } else {
      ctx_funcs->pfnDispatchIndirect = &DdiStub<Fn>::Call;
    }
  }

  ctx_funcs->pfnCopyResource = AEROGPU_D3D11_WDK_DDI(CopyResource11);
  ctx_funcs->pfnCopySubresourceRegion = AEROGPU_D3D11_WDK_DDI(CopySubresourceRegion11);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCopyStructureCount) {
    using Fn = decltype(ctx_funcs->pfnCopyStructureCount);
    if constexpr (std::is_convertible_v<decltype(&CopyStructureCount11), Fn>) {
      ctx_funcs->pfnCopyStructureCount = AEROGPU_D3D11_WDK_DDI(CopyStructureCount11);
    } else {
      ctx_funcs->pfnCopyStructureCount = &DdiStub<Fn>::Call;
    }
  }

  // Map can be HRESULT or void depending on interface version.
  if constexpr (std::is_same_v<decltype(ctx_funcs->pfnMap), decltype(&Map11)>) {
    ctx_funcs->pfnMap = AEROGPU_D3D11_WDK_DDI(Map11);
  } else {
    ctx_funcs->pfnMap = AEROGPU_D3D11_WDK_DDI(Map11Void);
  }
  ctx_funcs->pfnUnmap = AEROGPU_D3D11_WDK_DDI(Unmap11);
  {
    using Fn = decltype(ctx_funcs->pfnUpdateSubresourceUP);
    if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = AEROGPU_D3D11_WDK_DDI(UpdateSubresourceUP11);
    } else if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11Args), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = AEROGPU_D3D11_WDK_DDI(UpdateSubresourceUP11Args);
    } else if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11ArgsAndSysMem), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = AEROGPU_D3D11_WDK_DDI(UpdateSubresourceUP11ArgsAndSysMem);
    } else {
      ctx_funcs->pfnUpdateSubresourceUP = &DdiStub<Fn>::Call;
    }
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnUpdateSubresource) {
    using Fn = decltype(ctx_funcs->pfnUpdateSubresource);
    if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11), Fn>) {
      ctx_funcs->pfnUpdateSubresource = AEROGPU_D3D11_WDK_DDI(UpdateSubresourceUP11);
    } else {
      ctx_funcs->pfnUpdateSubresource = &DdiStub<Fn>::Call;
    }
  }

  if constexpr (has_member_pfnStagingResourceMap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnStagingResourceMap), decltype(&StagingResourceMap11)>) {
      ctx_funcs->pfnStagingResourceMap = AEROGPU_D3D11_WDK_DDI(StagingResourceMap11);
    } else {
      ctx_funcs->pfnStagingResourceMap = AEROGPU_D3D11_WDK_DDI(StagingResourceMap11Void);
    }
  }
  if constexpr (has_member_pfnStagingResourceUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnStagingResourceUnmap = AEROGPU_D3D11_WDK_DDI(StagingResourceUnmap11);
  }

  if constexpr (has_member_pfnDynamicIABufferMapDiscard<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicIABufferMapDiscard), decltype(&DynamicIABufferMapDiscard11)>) {
      ctx_funcs->pfnDynamicIABufferMapDiscard = AEROGPU_D3D11_WDK_DDI(DynamicIABufferMapDiscard11);
    } else {
      ctx_funcs->pfnDynamicIABufferMapDiscard = AEROGPU_D3D11_WDK_DDI(DynamicIABufferMapDiscard11Void);
    }
  }
  if constexpr (has_member_pfnDynamicIABufferMapNoOverwrite<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicIABufferMapNoOverwrite), decltype(&DynamicIABufferMapNoOverwrite11)>) {
      ctx_funcs->pfnDynamicIABufferMapNoOverwrite = AEROGPU_D3D11_WDK_DDI(DynamicIABufferMapNoOverwrite11);
    } else {
      ctx_funcs->pfnDynamicIABufferMapNoOverwrite = AEROGPU_D3D11_WDK_DDI(DynamicIABufferMapNoOverwrite11Void);
    }
  }
  if constexpr (has_member_pfnDynamicIABufferUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnDynamicIABufferUnmap = AEROGPU_D3D11_WDK_DDI(DynamicIABufferUnmap11);
  }

  if constexpr (has_member_pfnDynamicConstantBufferMapDiscard<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicConstantBufferMapDiscard), decltype(&DynamicConstantBufferMapDiscard11)>) {
      ctx_funcs->pfnDynamicConstantBufferMapDiscard = AEROGPU_D3D11_WDK_DDI(DynamicConstantBufferMapDiscard11);
    } else {
      ctx_funcs->pfnDynamicConstantBufferMapDiscard = AEROGPU_D3D11_WDK_DDI(DynamicConstantBufferMapDiscard11Void);
    }
  }
  if constexpr (has_member_pfnDynamicConstantBufferUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnDynamicConstantBufferUnmap = AEROGPU_D3D11_WDK_DDI(DynamicConstantBufferUnmap11);
  }

  ctx_funcs->pfnFlush = AEROGPU_D3D11_WDK_DDI(Flush11);
  BindPresentAndRotate(ctx_funcs);
  if (!ValidateNoNullDdiTable("D3D11DDI_DEVICEFUNCS", pCreateDevice->pDeviceFuncs, sizeof(*pCreateDevice->pDeviceFuncs)) ||
      !ValidateNoNullDdiTable("D3D11DDI_DEVICECONTEXTFUNCS", ctx_funcs, sizeof(*ctx_funcs))) {
    SetImmediateContextHandle(pCreateDevice, nullptr);
    DestroyWddmContext(dev);
    ctx->~AeroGpuDeviceContext();
    delete callbacks_copy;
    dev->runtime_callbacks = nullptr;
    dev->~Device();
    return E_NOINTERFACE;
  }

  return S_OK;
}

// -------------------------------------------------------------------------------------------------
// OpenAdapter11 export
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapter11Impl(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  // Always emit the module path once. This is the quickest way to confirm the
  // correct UMD bitness was loaded on Win7 x64 (System32 vs SysWOW64).
  LogModulePathOnce();
  AEROGPU_D3D10_11_LOG_CALL();

  // Win7 D3D11 uses `D3D10DDIARG_OPENADAPTER` for negotiation:
  // - `Interface` selects D3D11 DDI
  // - `Version` selects the struct layout for the device/context function tables
  //
  // Different WDKs use slightly different constant names for `Interface`; accept
  // both where available but always clamp `Version` to the struct layout this
  // binary was compiled against.
  bool interface_ok = (pOpenData->Interface == D3D11DDI_INTERFACE_VERSION);
#ifdef D3D11DDI_INTERFACE
  interface_ok = interface_ok || (pOpenData->Interface == D3D11DDI_INTERFACE);
#endif
  if (!interface_ok) {
    return E_INVALIDARG;
  }

  // `D3D10DDIARG_OPENADAPTER::Version` negotiation constant.
  // Some WDKs expose `D3D11DDI_SUPPORTED`; others only provide `D3D11DDI_INTERFACE_VERSION`.
#if defined(D3D11DDI_SUPPORTED)
  constexpr UINT supported_version = D3D11DDI_SUPPORTED;
#else
  constexpr UINT supported_version = D3D11DDI_INTERFACE_VERSION;
#endif
  if (pOpenData->Version == 0) {
    pOpenData->Version = supported_version;
  } else if (pOpenData->Version < supported_version) {
    return E_NOINTERFACE;
  } else if (pOpenData->Version > supported_version) {
    pOpenData->Version = supported_version;
  }

  auto* adapter = new (std::nothrow) Adapter();
  if (!adapter) {
    return E_OUTOFMEMORY;
  }
  adapter->d3d11_ddi_version = pOpenData->Version;
  adapter->runtime_callbacks = GetAdapterCallbacks(pOpenData);
  InitUmdPrivate(adapter);
  pOpenData->hAdapter.pDrvPrivate = adapter;

  auto* funcs = reinterpret_cast<D3D11DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  *funcs = MakeStubAdapterFuncs11();
  funcs->pfnGetCaps = AEROGPU_D3D11_WDK_DDI(GetCaps11);
  funcs->pfnCalcPrivateDeviceSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateDeviceSize11);
  if constexpr (has_member_pfnCalcPrivateDeviceContextSize<D3D11DDI_ADAPTERFUNCS>::value) {
    funcs->pfnCalcPrivateDeviceContextSize = AEROGPU_D3D11_WDK_DDI(CalcPrivateDeviceContextSize11);
  }
  funcs->pfnCreateDevice = AEROGPU_D3D11_WDK_DDI(CreateDevice11);
  funcs->pfnCloseAdapter = AEROGPU_D3D11_WDK_DDI(CloseAdapter11);
  if (!ValidateNoNullDdiTable("D3D11DDI_ADAPTERFUNCS", funcs, sizeof(*funcs))) {
    pOpenData->hAdapter.pDrvPrivate = nullptr;
    DestroyKmtAdapterHandle(adapter);
    delete adapter;
    return E_NOINTERFACE;
  }
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  try {
    return OpenAdapter11Impl(pOpenData);
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}

} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
