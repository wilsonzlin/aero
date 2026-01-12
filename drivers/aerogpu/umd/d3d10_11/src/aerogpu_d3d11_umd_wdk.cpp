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
#include "aerogpu_d3d10_11_log.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_win7_abi.h"

#ifndef DXGI_ERROR_WAS_STILL_DRAWING
  #define DXGI_ERROR_WAS_STILL_DRAWING ((HRESULT)0x887A000AL)
#endif

namespace {

using namespace aerogpu::d3d10_11;

// Compile-time sanity: keep local checks to "member exists" only.
//
// ABI-critical size/offset conformance checks (struct layout + x86 export
// decoration) are handled separately by `aerogpu_d3d10_11_wdk_abi_asserts.h`
// when building against real WDK headers.
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICEFUNCS::pfnCreateResource)>,
              "Expected D3D11DDI_DEVICEFUNCS::pfnCreateResource");
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw)>,
              "Expected D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw");

constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
}

constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr HRESULT kHrPending = static_cast<HRESULT>(0x8000000Au); // E_PENDING
constexpr HRESULT kHrNtStatusGraphicsGpuBusy =
    static_cast<HRESULT>(0xD01E0102L); // HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)

#ifndef STATUS_TIMEOUT
  #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
#endif

#ifndef WAIT_TIMEOUT
  #define WAIT_TIMEOUT 258L
#endif

#ifndef ERROR_TIMEOUT
  #define ERROR_TIMEOUT 1460L
#endif

constexpr uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

constexpr uint32_t AlignUpU32(uint32_t value, uint32_t alignment) {
  return static_cast<uint32_t>((value + alignment - 1) & ~(alignment - 1));
}

static bool GetPrimaryDisplayName(wchar_t out[CCHDEVICENAME]) {
  if (!out) {
    return false;
  }

  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  wcsncpy(out, L"\\\\.\\DISPLAY1", CCHDEVICENAME - 1);
  out[CCHDEVICENAME - 1] = 0;
  return true;
}

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

// Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
// correct UMD bitness was loaded (System32 vs SysWOW64).
static void LogModulePathOnce() {
  static std::once_flag once;
  std::call_once(once, [] {
    HMODULE module = NULL;
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                               GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCSTR>(&LogModulePathOnce),
                           &module)) {
      char path[MAX_PATH] = {};
      if (GetModuleFileNameA(module, path, static_cast<DWORD>(sizeof(path))) != 0) {
        char buf[MAX_PATH + 64] = {};
        snprintf(buf, sizeof(buf), "aerogpu-d3d10_11: module_path=%s\n", path);
        OutputDebugStringA(buf);
      }
    }
  });
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
static void InitLockForWrite(D3DDDICB_LOCK* lock) {
  if (!lock) {
    return;
  }
  // `D3DDDICB_LOCKFLAGS` bit names vary slightly across WDK releases.
  __if_exists(D3DDDICB_LOCK::Flags) {
    __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) {
      lock->Flags.WriteOnly = 1;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Write) {
      lock->Flags.Write = 1;
    }
  }
}

static void SetError(Device* dev, HRESULT hr) {
  if (!dev) {
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

static void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }
  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
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
  } else if (timeout == ~0ull) {
    timeout_ms = ~0u;
  } else if (timeout >= static_cast<UINT64>(~0u)) {
    timeout_ms = ~0u;
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

static void TrackStagingWriteLocked(Device* dev, Resource* dst) {
  if (!dev || !dst) {
    return;
  }
  if (dst->usage != kD3D11UsageStaging) {
    return;
  }
  if ((dst->cpu_access_flags & kD3D11CpuAccessRead) == 0) {
    return;
  }
  dev->pending_staging_writes.push_back(dst);
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

static bool ConsumeWddmAllocPrivV2(const void* priv_data, UINT priv_data_size, aerogpu_wddm_alloc_priv_v2* out) {
  if (out) {
    std::memset(out, 0, sizeof(*out));
  }
  if (!out || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return false;
  }

  // The v1 and v2 layouts share the same header (magic/version). Probe the
  // version and decode into a v2-shaped struct.
  aerogpu_wddm_alloc_priv header{};
  std::memcpy(&header, priv_data, sizeof(header));
  if (header.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC) {
    return false;
  }

  if (header.version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
    if (priv_data_size < sizeof(aerogpu_wddm_alloc_priv_v2)) {
      return false;
    }
    std::memcpy(out, priv_data, sizeof(*out));
    return true;
  }

  if (header.version == AEROGPU_WDDM_ALLOC_PRIV_VERSION) {
    out->magic = header.magic;
    out->version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
    out->alloc_id = header.alloc_id;
    out->flags = header.flags;
    out->share_token = header.share_token;
    out->size_bytes = header.size_bytes;
    out->reserved0 = header.reserved0;
    out->kind = AEROGPU_WDDM_ALLOC_KIND_UNKNOWN;
    out->width = 0;
    out->height = 0;
    out->format = 0;
    out->row_pitch_bytes = 0;
    out->reserved1 = 0;
    return true;
  }

  return false;
}

static void TrackWddmAllocForSubmitLocked(Device* dev, const Resource* res) {
  if (!dev || !res) {
    return;
  }
  if (res->backing_alloc_id == 0 || res->wddm_allocation_handle == 0) {
    return;
  }

  const uint32_t handle = res->wddm_allocation_handle;
  if (std::find(dev->wddm_submit_allocation_handles.begin(),
                dev->wddm_submit_allocation_handles.end(),
                handle) != dev->wddm_submit_allocation_handles.end()) {
    return;
  }
  dev->wddm_submit_allocation_handles.push_back(handle);
}

static void TrackBoundTargetsForSubmitLocked(Device* dev) {
  if (!dev) {
    return;
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_rtv_resource);
  TrackWddmAllocForSubmitLocked(dev, dev->current_dsv_resource);
}

static void TrackDrawStateLocked(Device* dev) {
  if (!dev) {
    return;
  }

  TrackBoundTargetsForSubmitLocked(dev);
  TrackWddmAllocForSubmitLocked(dev, dev->current_vb);
  TrackWddmAllocForSubmitLocked(dev, dev->current_ib);

  for (Resource* res : dev->current_vs_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_ps_cbs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }

  for (Resource* res : dev->current_vs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (Resource* res : dev->current_ps_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
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

static bool SupportsSrgbFormats(const Device* dev) {
  // ABI 1.2 adds explicit sRGB format variants. When running against an older
  // host/device ABI, map sRGB DXGI formats to UNORM so the command stream stays
  // backwards compatible.
  if (!dev || !dev->adapter || !dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
}

static bool SupportsBcFormats(const Device* dev) {
  if (!dev || !dev->adapter || !dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
}

static uint32_t dxgi_format_to_aerogpu_compat(const Device* dev, uint32_t dxgi_format) {
  if (!SupportsSrgbFormats(dev)) {
    switch (dxgi_format) {
      case kDxgiFormatB8G8R8A8UnormSrgb:
        dxgi_format = kDxgiFormatB8G8R8A8Unorm;
        break;
      case kDxgiFormatB8G8R8X8UnormSrgb:
        dxgi_format = kDxgiFormatB8G8R8X8Unorm;
        break;
      case kDxgiFormatR8G8B8A8UnormSrgb:
        dxgi_format = kDxgiFormatR8G8B8A8Unorm;
        break;
      default:
        break;
    }
  }
  return dxgi_format_to_aerogpu(dxgi_format);
}

static Device* DeviceFromContext(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuDeviceContext>(hCtx);
  return ctx ? ctx->dev : nullptr;
}

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

static void EmitBindShadersLocked(Device* dev) {
  if (!dev) {
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->vs = dev->current_vs;
  cmd->ps = dev->current_ps;
  // NOTE: The current AeroGPU protocol does not include a dedicated geometry
  // shader slot. We intentionally do not forward GS for now.
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

static void EmitUploadLocked(Device* dev, Resource* res, uint64_t offset_bytes, uint64_t size_bytes) {
  if (!dev || !res || !res->handle || size_bytes == 0) {
    return;
  }
  if (offset_bytes > static_cast<uint64_t>(SIZE_MAX) || size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  uint64_t upload_offset = offset_bytes;
  uint64_t upload_size = size_bytes;
  if (res->kind == ResourceKind::Buffer) {
    const uint64_t end = offset_bytes + size_bytes;
    if (end < offset_bytes) {
      return;
    }
    const uint64_t aligned_start = offset_bytes & ~3ull;
    const uint64_t aligned_end = AlignUpU64(end, 4);
    upload_offset = aligned_start;
    upload_size = aligned_end - aligned_start;
  }

  if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  const size_t off = static_cast<size_t>(upload_offset);
  const size_t sz = static_cast<size_t>(upload_size);
  if (off > res->storage.size() || sz > res->storage.size() - off) {
    return;
  }

  if (res->backing_alloc_id == 0) {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + off, sz);
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return;
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = upload_offset;
    cmd->size_bytes = upload_size;
    return;
  }

  // Guest-backed resources: write into the WDDM allocation, then notify the host
  // that the resource's backing memory changed.
  const auto* ddi = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks);
  if (!ddi || !ddi->pfnLockCb || !ddi->pfnUnlockCb || !dev->runtime_device || res->wddm_allocation_handle == 0) {
    SetError(dev, E_FAIL);
    return;
  }

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock_args.SubresourceIndex = 0; }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock_args.SubResourceIndex = 0; }
  InitLockForWrite(&lock_args);

  HRESULT hr = CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock_args);
  if (FAILED(hr) || !lock_args.pData) {
    SetError(dev, FAILED(hr) ? hr : E_FAIL);
    return;
  }

  HRESULT copy_hr = S_OK;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }

    uint32_t dst_pitch = res->row_pitch_bytes;
    __if_exists(D3DDDICB_LOCK::Pitch) {
      if (lock_args.Pitch) {
        dst_pitch = lock_args.Pitch;
      }
    }
    if (dst_pitch < row_bytes) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }

    uint8_t* dst_base = static_cast<uint8_t*>(lock_args.pData);
    const uint8_t* src_base = res->storage.data();
    for (uint32_t y = 0; y < rows; ++y) {
      const size_t src_off_row = static_cast<size_t>(y) * res->row_pitch_bytes;
      const size_t dst_off_row = static_cast<size_t>(y) * dst_pitch;
      if (src_off_row + row_bytes > res->storage.size()) {
        copy_hr = E_FAIL;
        break;
      }
      std::memcpy(dst_base + dst_off_row, src_base + src_off_row, row_bytes);
      if (dst_pitch > row_bytes) {
        std::memset(dst_base + dst_off_row + row_bytes, 0, dst_pitch - row_bytes);
      }
    }
  } else {
    std::memcpy(static_cast<uint8_t*>(lock_args.pData) + off, res->storage.data() + off, sz);
  }

Unlock:
  D3DDDICB_UNLOCK unlock_args = {};
  unlock_args.hAllocation = lock_args.hAllocation;
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) { unlock_args.SubresourceIndex = 0; }
  __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) { unlock_args.SubResourceIndex = 0; }
  hr = CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &unlock_args);
  if (FAILED(hr)) {
    SetError(dev, hr);
    return;
  }
  if (FAILED(copy_hr)) {
    SetError(dev, copy_hr);
    return;
  }

  TrackWddmAllocForSubmitLocked(dev, res);

  auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  dirty->resource_handle = res->handle;
  dirty->reserved0 = 0;
  dirty->offset_bytes = upload_offset;
  dirty->size_bytes = upload_size;
}

static void EmitDirtyRangeLocked(Device* dev, Resource* res, uint64_t offset_bytes, uint64_t size_bytes) {
  if (!dev || !res || !res->handle || size_bytes == 0) {
    return;
  }

  TrackWddmAllocForSubmitLocked(dev, res);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
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
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->slot = slot;
  cmd->texture = texture;
  cmd->reserved0 = 0;
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
    default:
      return nullptr;
  }
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

static void UnbindResourceFromSrvsLocked(Device* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (dev->vs_srvs[slot] == resource) {
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
    if (dev->ps_srvs[slot] == resource) {
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
  }
}

static void EmitSetRenderTargetsLocked(Device* dev) {
  if (!dev) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->color_count = dev->current_rtv ? 1u : 0u;
  cmd->depth_stencil = dev->current_dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  if (dev->current_rtv) {
    cmd->colors[0] = dev->current_rtv;
  }
}

static void UnbindResourceFromOutputsLocked(Device* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  bool changed = false;
  if (dev->current_rtv == resource) {
    dev->current_rtv = 0;
    dev->current_rtv_resource = nullptr;
    changed = true;
  }
  if (dev->current_dsv == resource) {
    dev->current_dsv = 0;
    dev->current_dsv_resource = nullptr;
    changed = true;
  }
  if (changed) {
    EmitSetRenderTargetsLocked(dev);
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

// Validates that the runtime will never see a NULL DDI function pointer.
//
// This is intentionally enabled in release builds. If our `__if_exists` field
// lists ever fall out of sync with the WDK's `d3d11umddi.h` layout, this check
// should fail fast (OpenAdapter/CreateDevice return `E_NOINTERFACE`) instead of
// allowing a later NULL-call crash inside the D3D11 runtime.
static bool ValidateNoNullDdiTable(const char* name, const void* table, size_t bytes) {
  if (!table || bytes == 0) {
    return false;
  }

  // These tables are expected to contain only function pointers, densely packed.
  if ((bytes % sizeof(void*)) != 0) {
    return false;
  }

  const auto* raw = reinterpret_cast<const unsigned char*>(table);
  const size_t count = bytes / sizeof(void*);
  for (size_t i = 0; i < count; ++i) {
    const size_t offset = i * sizeof(void*);
    bool all_zero = true;
    for (size_t j = 0; j < sizeof(void*); ++j) {
      if (raw[offset + j] != 0) {
        all_zero = false;
        break;
      }
    }
    if (!all_zero) {
      continue;
    }

#if defined(_WIN32)
    char buf[256] = {};
    snprintf(buf, sizeof(buf), "aerogpu-d3d11: NULL DDI entry in %s at index=%zu\n", name ? name : "?", i);
    OutputDebugStringA(buf);
#endif

#if !defined(NDEBUG)
    assert(false && "NULL DDI function pointer");
#endif
    return false;
  }
  return true;
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
      funcs->pfnPresent = &Present11;
    } else if constexpr (std::is_convertible_v<decltype(&Present11Device), Fn>) {
      funcs->pfnPresent = &Present11Device;
    } else {
      funcs->pfnPresent = &DdiStub<Fn>::Call;
    }
  }

  if constexpr (HasRotateResourceIdentities<TFuncs>::value) {
    using Fn = decltype(funcs->pfnRotateResourceIdentities);
    if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11), Fn>) {
      funcs->pfnRotateResourceIdentities = &RotateResourceIdentities11;
    } else if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11Device), Fn>) {
      funcs->pfnRotateResourceIdentities = &RotateResourceIdentities11Device;
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

  const bool is_write = (res->mapped_map_type != D3D11_MAP_READ);
  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (is_write && !res->storage.empty()) {
      const uint8_t* src = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
      const uint64_t off = res->mapped_offset;
      const uint64_t size = res->mapped_size;

      if (off <= res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(off);
        const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
          if (copy_bytes) {
            if (res->kind == ResourceKind::Texture2D) {
              const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
              const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
              const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
              const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
              const uint32_t dst_pitch = res->row_pitch_bytes;

              if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 &&
                  src_pitch >= row_bytes && dst_pitch >= row_bytes) {
                for (uint32_t y = 0; y < rows; y++) {
                  uint8_t* dst_row = res->storage.data() + static_cast<size_t>(y) * dst_pitch;
                  const uint8_t* src_row = src + static_cast<size_t>(y) * src_pitch;
                  std::memcpy(dst_row, src_row, row_bytes);
                  if (dst_pitch > row_bytes) {
                    std::memset(dst_row + row_bytes, 0, dst_pitch - row_bytes);
                  }
                }
              } else {
                // Fallback: best-effort linear copy.
                std::memcpy(res->storage.data() + static_cast<size_t>(off), src + static_cast<size_t>(off), copy_bytes);
              }
            } else {
              std::memcpy(res->storage.data() + static_cast<size_t>(off), src + static_cast<size_t>(off), copy_bytes);
            }
          }
      }
    }

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
      EmitDirtyRangeLocked(dev, res, res->mapped_offset, res->mapped_size);
    } else if (!res->storage.empty()) {
      EmitUploadLocked(dev, res, res->mapped_offset, res->mapped_size);
    }
  }

  res->mapped = false;
  res->mapped_map_type = 0;
  res->mapped_map_flags = 0;
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
  bool supports_bc = false;
  if (adapter && adapter->umd_private_valid) {
    const aerogpu_umd_private_v1& blob = adapter->umd_private;
    const uint32_t major = blob.device_abi_version_u32 >> 16;
    const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
    supports_bc = (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
  }
  // ABI 1.2 adds explicit sRGB format variants (same gating as BC formats).
  const bool supports_srgb = supports_bc;

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
    case D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS:
    case D3D11DDICAPS_TYPE_D3D11_OPTIONS:
    case D3D11DDICAPS_TYPE_ARCHITECTURE_INFO:
    case D3D11DDICAPS_TYPE_D3D9_OPTIONS:
      zero_out();
      return S_OK;

    case D3D11DDICAPS_TYPE_SHADER: {
      // Shader model caps for FL10_0: VS/GS/PS are SM4.0; HS/DS/CS are unsupported.
      //
      // The WDK output struct layout has been stable in practice: it begins with
      // six UINT "version tokens" matching the D3D shader bytecode token format:
      //   (program_type << 16) | (major << 4) | minor
      //
      // Be careful about overrunning DataSize: only write fields that fit.
      zero_out();

      constexpr auto ver_token = [](UINT program_type, UINT major, UINT minor) -> UINT {
        return (program_type << 16) | (major << 4) | minor;
      };

      constexpr UINT kShaderTypePixel = 0;
      constexpr UINT kShaderTypeVertex = 1;
      constexpr UINT kShaderTypeGeometry = 2;

      auto write_u32 = [&](size_t offset, UINT value) {
        if (size < offset + sizeof(UINT)) {
          return;
        }
        *reinterpret_cast<UINT*>(reinterpret_cast<uint8_t*>(data) + offset) = value;
      };

      write_u32(0, ver_token(kShaderTypePixel, 4, 0));
      write_u32(sizeof(UINT), ver_token(kShaderTypeVertex, 4, 0));
      write_u32(sizeof(UINT) * 2, ver_token(kShaderTypeGeometry, 4, 0));
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

      UINT support = 0;
      switch (static_cast<uint32_t>(format)) {
        case kDxgiFormatB8G8R8A8Unorm:
        case kDxgiFormatB8G8R8A8Typeless:
          support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                    D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                    D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY;
          break;
        case kDxgiFormatB8G8R8A8UnormSrgb:
          support = supports_srgb ? (D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                                     D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                                     D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY)
                                 : 0;
          break;
        case kDxgiFormatB8G8R8X8Unorm:
        case kDxgiFormatB8G8R8X8Typeless:
          support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                    D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                    D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY;
          break;
        case kDxgiFormatB8G8R8X8UnormSrgb:
          support = supports_srgb ? (D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                                     D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                                     D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY)
                                 : 0;
          break;
        case kDxgiFormatR8G8B8A8Unorm:
        case kDxgiFormatR8G8B8A8Typeless:
          support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                    D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                    D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY;
          break;
        case kDxgiFormatR8G8B8A8UnormSrgb:
          support = supports_srgb ? (D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                                     D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                                     D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY)
                                 : 0;
          break;
        case kDxgiFormatBc1Typeless:
        case kDxgiFormatBc1Unorm:
        case kDxgiFormatBc1UnormSrgb:
        case kDxgiFormatBc2Typeless:
        case kDxgiFormatBc2Unorm:
        case kDxgiFormatBc2UnormSrgb:
        case kDxgiFormatBc3Typeless:
        case kDxgiFormatBc3Unorm:
        case kDxgiFormatBc3UnormSrgb:
        case kDxgiFormatBc7Typeless:
        case kDxgiFormatBc7Unorm:
        case kDxgiFormatBc7UnormSrgb:
          if (supports_bc) {
            support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_SHADER_SAMPLE |
                      D3D11_FORMAT_SUPPORT_CPU_LOCKABLE;
          } else {
            support = 0;
          }
          break;
        case kDxgiFormatD24UnormS8Uint:
        case kDxgiFormatD32Float:
          support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL;
          break;
        case kDxgiFormatR16Uint:
        case kDxgiFormatR32Uint:
          support = D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER;
          break;
        case kDxgiFormatR32G32B32A32Float:
        case kDxgiFormatR32G32B32Float:
        case kDxgiFormatR32G32Float:
          support = D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_VERTEX_BUFFER;
          break;
        default:
          support = 0;
          break;
      }

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
    case static_cast<D3D11DDICAPS_TYPE>(3): { // FORMAT_SUPPORT2
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
      bool supported_format = false;
      switch (static_cast<uint32_t>(in.Format)) {
        case kDxgiFormatB8G8R8A8Unorm:
        case kDxgiFormatB8G8R8A8Typeless:
        case kDxgiFormatB8G8R8X8Unorm:
        case kDxgiFormatB8G8R8X8Typeless:
        case kDxgiFormatR8G8B8A8Unorm:
        case kDxgiFormatR8G8B8A8Typeless:
        case kDxgiFormatD24UnormS8Uint:
        case kDxgiFormatD32Float:
          supported_format = true;
          break;
        case kDxgiFormatB8G8R8A8UnormSrgb:
        case kDxgiFormatB8G8R8X8UnormSrgb:
        case kDxgiFormatR8G8B8A8UnormSrgb:
          supported_format = supports_srgb;
          break;
        default:
          supported_format = false;
          break;
      }
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
  if (!device_mem) {
    return;
  }

  uint32_t cookie = 0;
  std::memcpy(&cookie, device_mem, sizeof(cookie));
  if (cookie != kDeviceDestroyLiveCookie) {
    return;
  }
  cookie = 0;
  std::memcpy(device_mem, &cookie, sizeof(cookie));

  auto* dev = reinterpret_cast<Device*>(device_mem);
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
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
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

  auto* res = new (hResource.pDrvPrivate) Resource();
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

    std::vector<D3DKMT_HANDLE> km_allocs;
    km_allocs.reserve(res->wddm.km_allocation_handles.size());
    for (uint64_t h : res->wddm.km_allocation_handles) {
      km_allocs.push_back(static_cast<D3DKMT_HANDLE>(h));
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = static_cast<UINT>(km_allocs.size());
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_allocs.empty() ? nullptr : km_allocs.data();
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_allocs.empty() ? nullptr : km_allocs.data();
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
                                uint32_t pitch_bytes) -> HRESULT {
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
    res->wddm.km_allocation_handles.push_back(km_alloc);
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

  const auto copy_initial_bytes = [&](const void* src, size_t bytes) {
    if (!src || bytes == 0 || res->storage.empty()) {
      return;
    }
    bytes = std::min(bytes, res->storage.size());
    std::memcpy(res->storage.data(), src, bytes);
    EmitUploadLocked(dev, res, 0, bytes);
  };

  const auto copy_initial_tex2d = [&](const void* src, UINT src_pitch) {
    if (!src || res->row_pitch_bytes == 0 || res->height == 0 || res->storage.empty()) {
      return;
    }
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      return;
    }
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(src);
    const uint32_t pitch = src_pitch ? src_pitch : row_bytes;
    if (pitch < row_bytes) {
      return;
    }
    for (uint32_t y = 0; y < rows; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src_bytes + static_cast<size_t>(y) * pitch,
                  row_bytes);
      if (res->row_pitch_bytes > row_bytes) {
        std::memset(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes + row_bytes,
                    0,
                    res->row_pitch_bytes - row_bytes);
      }
    }
    EmitUploadLocked(dev, res, 0, res->storage.size());
  };

  const auto maybe_copy_initial = [&](auto init_ptr) {
    if (!init_ptr) {
      return;
    }

    using ElemT = std::remove_pointer_t<decltype(init_ptr)>;
    if constexpr (std::is_void_v<ElemT>) {
      if (res->kind == ResourceKind::Buffer) {
        copy_initial_bytes(init_ptr, static_cast<size_t>(res->size_bytes));
      } else {
        copy_initial_bytes(init_ptr, res->storage.size());
      }
    } else if constexpr (has_member_pSysMem<ElemT>::value) {
      const void* sys = init_ptr[0].pSysMem;
      UINT pitch = 0;
      if constexpr (has_member_SysMemPitch<ElemT>::value) {
        pitch = init_ptr[0].SysMemPitch;
      }

      if (res->kind == ResourceKind::Buffer) {
        copy_initial_bytes(sys, static_cast<size_t>(res->size_bytes));
      } else if (res->kind == ResourceKind::Texture2D) {
        copy_initial_tex2d(sys, pitch);
      }
    }
  };

  if (dim == D3D10DDIRESOURCE_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(pDesc->ByteWidth);
    const uint64_t padded_size_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    if (padded_size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      res->~Resource();
      return E_OUTOFMEMORY;
    }
    const uint64_t alloc_size = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 256);
    const bool is_staging = (res->usage == kD3D11UsageStaging);
    bool cpu_visible = is_staging || (res->cpu_access_flags != 0);
    const bool is_rt = (res->bind_flags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D11BindDepthStencil) != 0;
    bool is_shared = false;
    if (res->misc_flags & D3D11_RESOURCE_MISC_SHARED) {
      is_shared = true;
    }
#ifdef D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;
    res->is_shared = is_shared;
    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0);
    if (FAILED(hr)) {
      SetError(dev, hr);
      res->~Resource();
      return hr;
    }
    try {
      res->storage.resize(static_cast<size_t>(padded_size_bytes));
    } catch (...) {
      deallocate_if_needed();
      res->~Resource();
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

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = padded_size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;

    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialData);
    }

    TrackWddmAllocForSubmitLocked(dev, res);

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(dev, E_FAIL);
        deallocate_if_needed();
        res->~Resource();
        return E_FAIL;
      }

      // Shared resources must be importable cross-process as soon as
      // CreateResource returns. Export the resource and force a submission so
      // the host observes the share_token mapping immediately (mirrors D3D9Ex
      // behavior).
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~Resource();
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
        res->~Resource();
        return submit_hr;
      }
    }
    return S_OK;
  }

  if (dim == D3D10DDIRESOURCE_TEXTURE2D) {
    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize ? pDesc->ArraySize : 1;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);

    if (res->mip_levels != 1 || res->array_size != 1) {
      res->~Resource();
      return E_NOTIMPL;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~Resource();
      return E_NOTIMPL;
    }

    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      res->~Resource();
      return E_NOTIMPL;
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      res->~Resource();
      return E_OUTOFMEMORY;
    }
    res->row_pitch_bytes = AlignUpU32(row_bytes, 256);
    const uint64_t total_bytes = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      res->~Resource();
      return E_OUTOFMEMORY;
    }

    const bool is_staging = (res->usage == kD3D11UsageStaging);
    bool cpu_visible = is_staging || (res->cpu_access_flags != 0);
    const bool is_rt = (res->bind_flags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D11BindDepthStencil) != 0;
    bool is_shared = false;
    if (res->misc_flags & D3D11_RESOURCE_MISC_SHARED) {
      is_shared = true;
    }
#ifdef D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;
    res->is_shared = is_shared;
    HRESULT hr = allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared, is_primary, res->row_pitch_bytes);
    if (FAILED(hr)) {
      SetError(dev, hr);
      res->~Resource();
      return hr;
    }

    try {
      res->storage.resize(static_cast<size_t>(total_bytes));
    } catch (...) {
      deallocate_if_needed();
      res->~Resource();
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

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = 1;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;

    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialData);
    }

    TrackWddmAllocForSubmitLocked(dev, res);

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(dev, E_FAIL);
        deallocate_if_needed();
        res->~Resource();
        return E_FAIL;
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~Resource();
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
        res->~Resource();
        return submit_hr;
      }
    }
    return S_OK;
  }

  deallocate_if_needed();
  res->~Resource();
  return E_NOTIMPL;
}

HRESULT AEROGPU_APIENTRY OpenResource11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_OPENRESOURCE* pOpenResource,
                                         D3D11DDI_HRESOURCE hResource,
                                         D3D11DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !pOpenResource || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
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
  const size_t copy_bytes = std::min(static_cast<size_t>(priv_size), sizeof(priv));
  std::memcpy(&priv, priv_data, copy_bytes);
  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC || priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
    return E_INVALIDARG;
  }
  if ((priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) == 0 || priv.share_token == 0 || priv.alloc_id == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = new (hResource.pDrvPrivate) Resource();
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
    res->wddm.km_allocation_handles.push_back(static_cast<uint64_t>(pOpenResource->hKMAllocation));
  }
  __if_exists(D3D11DDIARG_OPENRESOURCE::hAllocation) {
    const uint64_t h = static_cast<uint64_t>(pOpenResource->hAllocation);
    if (h != 0) {
      res->wddm_allocation_handle = static_cast<uint32_t>(h);
      if (res->wddm.km_allocation_handles.empty()) {
        res->wddm.km_allocation_handles.push_back(h);
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
            res->wddm.km_allocation_handles.push_back(h);
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
        res->wddm.km_allocation_handles.push_back(km_alloc);
      }
    }
  }

  if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(priv.size_bytes);
  } else if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(priv.format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~Resource();
      return E_INVALIDARG;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      res->~Resource();
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
        res->~Resource();
        return E_INVALIDARG;
      }
      res->row_pitch_bytes = AlignUpU32(row_bytes, 256);
    }
  } else {
    res->~Resource();
    return E_INVALIDARG;
  }

  auto* import_cmd =
      dev->cmd.append_fixed<aerogpu_cmd_import_shared_surface>(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!import_cmd) {
    res->~Resource();
    return E_OUTOFMEMORY;
  }
  import_cmd->out_resource_handle = res->handle;
  import_cmd->reserved0 = 0;
  import_cmd->share_token = res->share_token;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyResource11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (res->mapped) {
    (void)UnmapLocked(dev, res);
  }

  if (res->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
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
    std::vector<D3DKMT_HANDLE> km_allocs;
    km_allocs.reserve(res->wddm.km_allocation_handles.size());
    for (uint64_t h : res->wddm.km_allocation_handles) {
      km_allocs.push_back(static_cast<D3DKMT_HANDLE>(h));
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = static_cast<D3DKMT_HANDLE>(dev->kmt_context);
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = static_cast<UINT>(km_allocs.size());
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_allocs.empty() ? nullptr : km_allocs.data();
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_allocs.empty() ? nullptr : km_allocs.data();
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
  res->~Resource();
}

// Views

SIZE_T AEROGPU_APIENTRY CalcPrivateRenderTargetViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(RenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRenderTargetView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                                  D3D11DDI_HRENDERTARGETVIEW hView,
                                                  D3D11DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
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
  auto* rtv = new (hView.pDrvPrivate) RenderTargetView();
  rtv->texture = res ? res->handle : 0;
  rtv->resource = res;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRenderTargetView11(D3D11DDI_HDEVICE, D3D11DDI_HRENDERTARGETVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(hView)->~RenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(DepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                                  D3D11DDI_HDEPTHSTENCILVIEW hView,
                                                  D3D11DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
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
  auto* dsv = new (hView.pDrvPrivate) DepthStencilView();
  dsv->texture = res ? res->handle : 0;
  dsv->resource = res;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilView11(D3D11DDI_HDEVICE, D3D11DDI_HDEPTHSTENCILVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hView)->~DepthStencilView();
}

struct ShaderResourceView {
  aerogpu_handle_t texture = 0;
  Resource* resource = nullptr;
};

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(ShaderResourceView);
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView11(D3D11DDI_HDEVICE hDevice,
                                                    const D3D11DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                    D3D11DDI_HSHADERRESOURCEVIEW hView,
                                                    D3D11DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
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
  if (res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }
  if (res->mip_levels != 1 || res->array_size != 1) {
    return E_NOTIMPL;
  }
  auto* srv = new (hView.pDrvPrivate) ShaderResourceView();
  srv->texture = res->handle;
  srv->resource = res;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView11(D3D11DDI_HDEVICE, D3D11DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(hView)->~ShaderResourceView();
}

template <typename T, typename = void>
struct has_member_Desc : std::false_type {};
template <typename T>
struct has_member_Desc<T, std::void_t<decltype(std::declval<T>().Desc)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SamplerDesc : std::false_type {};
template <typename T>
struct has_member_SamplerDesc<T, std::void_t<decltype(std::declval<T>().SamplerDesc)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Filter : std::false_type {};
template <typename T>
struct has_member_Filter<T, std::void_t<decltype(std::declval<T>().Filter)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressU : std::false_type {};
template <typename T>
struct has_member_AddressU<T, std::void_t<decltype(std::declval<T>().AddressU)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressV : std::false_type {};
template <typename T>
struct has_member_AddressV<T, std::void_t<decltype(std::declval<T>().AddressV)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressW : std::false_type {};
template <typename T>
struct has_member_AddressW<T, std::void_t<decltype(std::declval<T>().AddressW)>> : std::true_type {};

static uint32_t aerogpu_sampler_filter_from_d3d_filter(uint32_t filter) {
  // D3D10/11 point filtering is encoded as 0 for MIN_MAG_MIP_POINT; treat all
  // other filters as linear for MVP bring-up.
  return filter == 0 ? AEROGPU_SAMPLER_FILTER_NEAREST : AEROGPU_SAMPLER_FILTER_LINEAR;
}

static uint32_t aerogpu_sampler_address_from_d3d_mode(uint32_t mode) {
  // D3D10/11 numeric values: 1=WRAP, 2=MIRROR, 3=CLAMP.
  switch (mode) {
    case 1:
      return AEROGPU_SAMPLER_ADDRESS_REPEAT;
    case 2:
      return AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT;
    default:
      return AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  }
}

struct Sampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_LINEAR;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
};

template <typename DescT>
static void InitSamplerFromDesc(Sampler* sampler, const DescT& desc) {
  if (!sampler) {
    return;
  }

  uint32_t filter = 1;
  uint32_t addr_u = 3;
  uint32_t addr_v = 3;
  uint32_t addr_w = 3;
  if constexpr (has_member_Filter<DescT>::value) {
    filter = static_cast<uint32_t>(desc.Filter);
  }
  if constexpr (has_member_AddressU<DescT>::value) {
    addr_u = static_cast<uint32_t>(desc.AddressU);
  }
  if constexpr (has_member_AddressV<DescT>::value) {
    addr_v = static_cast<uint32_t>(desc.AddressV);
  }
  if constexpr (has_member_AddressW<DescT>::value) {
    addr_w = static_cast<uint32_t>(desc.AddressW);
  }

  sampler->filter = aerogpu_sampler_filter_from_d3d_filter(filter);
  sampler->address_u = aerogpu_sampler_address_from_d3d_mode(addr_u);
  sampler->address_v = aerogpu_sampler_address_from_d3d_mode(addr_v);
  sampler->address_w = aerogpu_sampler_address_from_d3d_mode(addr_w);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESAMPLER*) {
  return sizeof(Sampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_CREATESAMPLER* pDesc,
                                         D3D11DDI_HSAMPLER hSampler,
                                         D3D11DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* sampler = new (hSampler.pDrvPrivate) Sampler();
  sampler->handle = AllocateGlobalHandle(dev->adapter);
  if (!sampler->handle) {
    sampler->~Sampler();
    return E_FAIL;
  }

  if (pDesc) {
    if constexpr (has_member_Desc<D3D11DDIARG_CREATESAMPLER>::value) {
      InitSamplerFromDesc(sampler, pDesc->Desc);
    } else if constexpr (has_member_SamplerDesc<D3D11DDIARG_CREATESAMPLER>::value) {
      InitSamplerFromDesc(sampler, pDesc->SamplerDesc);
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    sampler->~Sampler();
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
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* sampler = FromHandle<D3D11DDI_HSAMPLER, Sampler>(hSampler);
  if (!dev || !sampler) {
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
  sampler->~Sampler();
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
  out->stage = stage;
  try {
    out->dxbc.resize(static_cast<size_t>(code_size));
  } catch (...) {
    out->~Shader();
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
    out->~Shader();
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
  sh->~Shader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateVertexShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEVERTEXSHADER*) {
  return sizeof(Shader);
}

HRESULT AEROGPU_APIENTRY CreateVertexShader11(D3D11DDI_HDEVICE hDevice,
                                              const D3D11DDIARG_CREATEVERTEXSHADER* pDesc,
                                              D3D11DDI_HVERTEXSHADER hShader,
                                              D3D11DDI_HRTVERTEXSHADER) {
  if (!pDesc || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) Shader();
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_VERTEX);
  if (FAILED(hr)) {
    return hr;
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyVertexShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HVERTEXSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* sh = FromHandle<D3D11DDI_HVERTEXSHADER, Shader>(hShader);
  if (!dev || !sh) {
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
  if (!pDesc || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) Shader();
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_PIXEL);
  if (FAILED(hr)) {
    return hr;
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyPixelShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HPIXELSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* sh = FromHandle<D3D11DDI_HPIXELSHADER, Shader>(hShader);
  if (!dev || !sh) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  DestroyShaderCommon(dev, sh);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateGeometryShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEGEOMETRYSHADER*) {
  return sizeof(Shader);
}

HRESULT AEROGPU_APIENTRY CreateGeometryShader11(D3D11DDI_HDEVICE hDevice,
                                                const D3D11DDIARG_CREATEGEOMETRYSHADER* pDesc,
                                                D3D11DDI_HGEOMETRYSHADER hShader,
                                                D3D11DDI_HRTGEOMETRYSHADER) {
  if (!pDesc || !hShader.pDrvPrivate || !pDesc->pShaderCode || pDesc->ShaderCodeSize == 0) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  (void)new (hShader.pDrvPrivate) Shader();
  // MVP: Geometry shaders are accepted by the Win7 D3D11 runtime at FL10_0, but
  // the AeroGPU command stream / WebGPU backend currently has no geometry-shader
  // stage. To keep the pipeline working for pass-through GS usage (e.g. the
  // Win7 `d3d11_geometry_shader_smoke` test), we treat GS as a no-op and do not
  // forward the DXBC to the host.
  //
  // NOTE: The created Shader's `handle` intentionally stays 0 so
  // `DestroyShaderCommon` does not emit a host-side DESTROY_SHADER for a shader
  // that was never created.
  static std::once_flag log_once;
  std::call_once(log_once, [] {
    AEROGPU_D3D10_11_LOG("CreateGeometryShader11: ignoring geometry shader (no GS stage in AeroGPU/WebGPU yet)");
  });
  return S_OK;
}

void AEROGPU_APIENTRY DestroyGeometryShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HGEOMETRYSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* sh = FromHandle<D3D11DDI_HGEOMETRYSHADER, Shader>(hShader);
  if (!dev || !sh) {
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
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* layout = new (hLayout.pDrvPrivate) InputLayout();
  layout->handle = AllocateGlobalHandle(dev->adapter);

  const UINT elem_count = pDesc->NumElements;
  if (!pDesc->pVertexElements || elem_count == 0) {
    layout->~InputLayout();
    return E_INVALIDARG;
  }

  aerogpu_input_layout_blob_header header{};
  header.magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
  header.version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
  header.element_count = elem_count;
  header.reserved0 = 0;

  std::vector<aerogpu_input_layout_element_dxgi> elems;
  elems.resize(elem_count);
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

  const size_t blob_size = sizeof(header) + elems.size() * sizeof(elems[0]);
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    layout->~InputLayout();
    return E_OUTOFMEMORY;
  }

  std::memcpy(layout->blob.data(), &header, sizeof(header));
  std::memcpy(layout->blob.data() + sizeof(header), elems.data(), elems.size() * sizeof(elems[0]));

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, layout->blob.data(), layout->blob.size());
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;

  return S_OK;
}

void AEROGPU_APIENTRY DestroyElementLayout11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate || !hLayout.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  auto* layout = FromHandle<D3D11DDI_HELEMENTLAYOUT, InputLayout>(hLayout);
  if (!dev || !layout) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (layout->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    cmd->input_layout_handle = layout->handle;
    cmd->reserved0 = 0;
  }
  layout->~InputLayout();
}

// Fixed-function state objects (accepted and bindable; conservative encoding).

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
  state->blend_enable = 0;
  state->src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
  state->dest_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  state->blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  state->src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
  state->dest_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  state->blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  state->render_target_write_mask = 0xFu;

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATEBLENDSTATE::RenderTarget) {
    const auto& rt0 = pDesc->RenderTarget[0];
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
      const auto& desc = pDesc->BlendDesc;
      const auto& rt0 = desc.RenderTarget[0];
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
      const auto& desc = pDesc->Desc;
      const auto& rt0 = desc.RenderTarget[0];
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
        const auto& desc = *pDesc->pBlendDesc;
        const auto& rt0 = desc.RenderTarget[0];
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
  FromHandle<D3D11DDI_HBLENDSTATE, BlendState>(hState)->~BlendState();
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
  state->cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
  state->front_ccw = 0u;
  state->scissor_enable = 0u;
  state->depth_clip_enable = 1u;

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::CullMode) {
    state->cull_mode = static_cast<uint32_t>(pDesc->CullMode);
    state->front_ccw = pDesc->FrontCounterClockwise ? 1u : 0u;
    state->scissor_enable = pDesc->ScissorEnable ? 1u : 0u;
    state->depth_clip_enable = pDesc->DepthClipEnable ? 1u : 0u;
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::RasterizerDesc) {
      const auto& desc = pDesc->RasterizerDesc;
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::Desc) {
      const auto& desc = pDesc->Desc;
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATERASTERIZERSTATE::pRasterizerDesc) {
      if (pDesc->pRasterizerDesc) {
        const auto& desc = *pDesc->pRasterizerDesc;
        state->cull_mode = static_cast<uint32_t>(desc.CullMode);
        state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
        state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
        state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
        filled = true;
      }
    }
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState11(D3D11DDI_HDEVICE, D3D11DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HRASTERIZERSTATE, RasterizerState>(hState)->~RasterizerState();
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

  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::DepthEnable) {
    state->depth_enable = pDesc->DepthEnable ? 1u : 0u;
    state->depth_write_mask = static_cast<uint32_t>(pDesc->DepthWriteMask);
    state->depth_func = static_cast<uint32_t>(pDesc->DepthFunc);
    state->stencil_enable = pDesc->StencilEnable ? 1u : 0u;
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::DepthStencilDesc) {
      const auto& desc = pDesc->DepthStencilDesc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::Desc) {
      const auto& desc = pDesc->Desc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D11DDIARG_CREATEDEPTHSTENCILSTATE::pDepthStencilDesc) {
      if (pDesc->pDepthStencilDesc) {
        const auto& desc = *pDesc->pDepthStencilDesc;
        state->depth_enable = desc.DepthEnable ? 1u : 0u;
        state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
        state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
        state->stencil_enable = desc.StencilEnable ? 1u : 0u;
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
  FromHandle<D3D11DDI_HDEPTHSTENCILSTATE, DepthStencilState>(hState)->~DepthStencilState();
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
  dev->current_input_layout_obj = layout;
  dev->current_input_layout = layout ? layout->handle : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  cmd->input_layout_handle = dev->current_input_layout;
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
  if (!phBuffers || !pStrides || !pOffsets || NumBuffers == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (StartSlot == 0 && NumBuffers >= 1) {
    dev->current_vb =
        phBuffers[0].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[0]) : nullptr;
    dev->current_vb_stride_bytes = pStrides[0];
    dev->current_vb_offset_bytes = pOffsets[0];
  }
  std::vector<aerogpu_vertex_buffer_binding> bindings;
  bindings.resize(NumBuffers);
  for (UINT i = 0; i < NumBuffers; i++) {
    bindings[i].buffer = phBuffers[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(phBuffers[i])->handle : 0;
    bindings[i].stride_bytes = pStrides[i];
    bindings[i].offset_bytes = pOffsets[i];
    bindings[i].reserved0 = 0;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, bindings.data(), bindings.size() * sizeof(bindings[0]));
  cmd->start_slot = StartSlot;
  cmd->buffer_count = NumBuffers;
}

void AEROGPU_APIENTRY IaSetIndexBuffer11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hBuffer, DXGI_FORMAT format, UINT offset) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_ib = hBuffer.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hBuffer) : nullptr;
  dev->current_ib_format = static_cast<uint32_t>(format);
  dev->current_ib_offset_bytes = offset;
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = hBuffer.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hBuffer)->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format));
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetTopology11(D3D11DDI_HDEVICECONTEXT hCtx, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const uint32_t topo = static_cast<uint32_t>(topology);
  if (dev->current_topology == topo) {
    return;
  }
  dev->current_topology = topo;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = topo;
  cmd->reserved0 = 0;
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
  dev->current_vs = sh ? sh->handle : 0;
  dev->current_vs_forced_z_valid = sh ? sh->forced_ndc_z_valid : false;
  dev->current_vs_forced_z = (sh && sh->forced_ndc_z_valid) ? sh->forced_ndc_z : 0.0f;
  EmitBindShadersLocked(dev);
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
  dev->current_ps = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HPIXELSHADER, Shader>(hShader)->handle : 0;
  EmitBindShadersLocked(dev);
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
  dev->current_gs = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HGEOMETRYSHADER, Shader>(hShader)->handle : 0;
  // Geometry shaders are currently ignored (no GS stage in the AeroGPU command
  // stream / WebGPU backend). See CreateGeometryShader11.
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
      b.offset_bytes = offset_bytes > 0xFFFFFFFFull ? 0xFFFFFFFFu : static_cast<uint32_t>(offset_bytes);
      b.size_bytes = size_bytes > 0xFFFFFFFFull ? 0xFFFFFFFFu : static_cast<uint32_t>(size_bytes);
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

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings.data(), buffer_count * sizeof(bindings[0]));
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->buffer_count = static_cast<uint32_t>(buffer_count);
  cmd->reserved0 = 0;

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
void AEROGPU_APIENTRY GsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HRESOURCE*, const UINT*, const UINT*) {}

template <typename THandle>
static bool AnyNonNullHandles(const THandle* handles, UINT count) {
  if (!handles || count == 0) {
    return false;
  }
  for (UINT i = 0; i < count; ++i) {
    if (handles[i].pDrvPrivate) {
      return true;
    }
  }
  return false;
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

// Tessellation and compute stages are unsupported in the current FL10_0 bring-up
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
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
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

void AEROGPU_APIENTRY CsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetUnorderedAccessViews11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                  UINT,
                                                  UINT NumUavs,
                                                  const D3D11DDI_HUNORDEREDACCESSVIEW* phUavs,
                                                  const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phUavs, NumUavs)) {
    return;
  }
  SetError(DeviceFromContext(hCtx), E_NOTIMPL);
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
  if (StartSlot >= kMaxShaderResourceSlots) {
    return;
  }
  if (StartSlot + NumViews > kMaxShaderResourceSlots) {
    NumViews = kMaxShaderResourceSlots - StartSlot;
  }
  for (UINT i = 0; i < NumViews; i++) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);
    aerogpu_handle_t tex = 0;
    Resource* res = nullptr;
    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(phViews[i]);
      if (view) {
        res = view->resource;
        tex = res ? res->handle : view->texture;
      }
    }
    if (tex) {
      UnbindResourceFromOutputsLocked(dev, tex);
    }
    SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, tex);
    if (dev->vs_srvs[slot] == tex) {
      if (slot < dev->current_vs_srvs.size()) {
        dev->current_vs_srvs[slot] = res;
      }
      if (slot == 0) {
        dev->current_vs_srv0 = res;
      }
    }
  }
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
  if (StartSlot >= kMaxShaderResourceSlots) {
    return;
  }
  if (StartSlot + NumViews > kMaxShaderResourceSlots) {
    NumViews = kMaxShaderResourceSlots - StartSlot;
  }
  for (UINT i = 0; i < NumViews; i++) {
    const uint32_t slot = static_cast<uint32_t>(StartSlot + i);
    aerogpu_handle_t tex = 0;
    Resource* res = nullptr;
    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(phViews[i]);
      if (view) {
        res = view->resource;
        tex = res ? res->handle : view->texture;
      }
    }
    if (tex) {
      UnbindResourceFromOutputsLocked(dev, tex);
    }
    SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, tex);
    if (dev->ps_srvs[slot] == tex) {
      if (slot < dev->current_ps_srvs.size()) {
        dev->current_ps_srvs[slot] = res;
      }
      if (slot == 0) {
        dev->current_ps_srv0 = res;
      }
    }
  }
}

void AEROGPU_APIENTRY GsSetShaderResources11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HSHADERRESOURCEVIEW*) {}

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

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles.data(), sampler_count * sizeof(handles[0]));
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->sampler_count = sampler_count;
  cmd->reserved0 = 0;

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
void AEROGPU_APIENTRY GsSetSamplers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HSAMPLER*) {}

void AEROGPU_APIENTRY SetViewports11(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumViewports, const D3D10_DDI_VIEWPORT* pViewports) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pViewports || NumViewports == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const auto& vp = pViewports[0];
  dev->viewport_x = vp.TopLeftX;
  dev->viewport_y = vp.TopLeftY;
  dev->viewport_width = vp.Width;
  dev->viewport_height = vp.Height;
  dev->viewport_min_depth = vp.MinDepth;
  dev->viewport_max_depth = vp.MaxDepth;
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(vp.TopLeftX);
  cmd->y_f32 = f32_bits(vp.TopLeftY);
  cmd->width_f32 = f32_bits(vp.Width);
  cmd->height_f32 = f32_bits(vp.Height);
  cmd->min_depth_f32 = f32_bits(vp.MinDepth);
  cmd->max_depth_f32 = f32_bits(vp.MaxDepth);
}

void AEROGPU_APIENTRY SetScissorRects11(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumRects, const D3D10_DDI_RECT* pRects) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pRects || NumRects == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const D3D10_DDI_RECT& r = pRects[0];
  dev->scissor_valid = true;
  dev->scissor_left = r.left;
  dev->scissor_top = r.top;
  dev->scissor_right = r.right;
  dev->scissor_bottom = r.bottom;
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  cmd->x = r.left;
  cmd->y = r.top;
  cmd->width = r.right - r.left;
  cmd->height = r.bottom - r.top;
}

static uint32_t D3D11CullModeToAerogpu(uint32_t cull_mode) {
  switch (static_cast<D3D11_CULL_MODE>(cull_mode)) {
    case D3D11_CULL_NONE:
      return AEROGPU_CULL_NONE;
    case D3D11_CULL_FRONT:
      return AEROGPU_CULL_FRONT;
    case D3D11_CULL_BACK:
      return AEROGPU_CULL_BACK;
    default:
      break;
  }
  return AEROGPU_CULL_BACK;
}

static uint32_t D3D11BlendFactorToAerogpu(uint32_t factor, uint32_t fallback) {
  switch (static_cast<D3D11_BLEND>(factor)) {
    case D3D11_BLEND_ZERO:
      return AEROGPU_BLEND_ZERO;
    case D3D11_BLEND_ONE:
      return AEROGPU_BLEND_ONE;
    case D3D11_BLEND_SRC_ALPHA:
      return AEROGPU_BLEND_SRC_ALPHA;
    case D3D11_BLEND_INV_SRC_ALPHA:
      return AEROGPU_BLEND_INV_SRC_ALPHA;
    case D3D11_BLEND_DEST_ALPHA:
      return AEROGPU_BLEND_DEST_ALPHA;
    case D3D11_BLEND_INV_DEST_ALPHA:
      return AEROGPU_BLEND_INV_DEST_ALPHA;
    case D3D11_BLEND_BLEND_FACTOR:
      return AEROGPU_BLEND_CONSTANT;
    case D3D11_BLEND_INV_BLEND_FACTOR:
      return AEROGPU_BLEND_INV_CONSTANT;
    default:
      break;
  }
  return fallback;
}

static uint32_t D3D11BlendOpToAerogpu(uint32_t blend_op) {
  switch (static_cast<D3D11_BLEND_OP>(blend_op)) {
    case D3D11_BLEND_OP_ADD:
      return AEROGPU_BLEND_OP_ADD;
    case D3D11_BLEND_OP_SUBTRACT:
      return AEROGPU_BLEND_OP_SUBTRACT;
    case D3D11_BLEND_OP_REV_SUBTRACT:
      return AEROGPU_BLEND_OP_REV_SUBTRACT;
    case D3D11_BLEND_OP_MIN:
      return AEROGPU_BLEND_OP_MIN;
    case D3D11_BLEND_OP_MAX:
      return AEROGPU_BLEND_OP_MAX;
    default:
      break;
  }
  return AEROGPU_BLEND_OP_ADD;
}

static uint32_t D3D11CompareFuncToAerogpu(uint32_t func) {
  switch (static_cast<D3D11_COMPARISON_FUNC>(func)) {
    case D3D11_COMPARISON_NEVER:
      return AEROGPU_COMPARE_NEVER;
    case D3D11_COMPARISON_LESS:
      return AEROGPU_COMPARE_LESS;
    case D3D11_COMPARISON_EQUAL:
      return AEROGPU_COMPARE_EQUAL;
    case D3D11_COMPARISON_LESS_EQUAL:
      return AEROGPU_COMPARE_LESS_EQUAL;
    case D3D11_COMPARISON_GREATER:
      return AEROGPU_COMPARE_GREATER;
    case D3D11_COMPARISON_NOT_EQUAL:
      return AEROGPU_COMPARE_NOT_EQUAL;
    case D3D11_COMPARISON_GREATER_EQUAL:
      return AEROGPU_COMPARE_GREATER_EQUAL;
    case D3D11_COMPARISON_ALWAYS:
      return AEROGPU_COMPARE_ALWAYS;
    default:
      break;
  }
  return AEROGPU_COMPARE_ALWAYS;
}

static void EmitRasterizerStateLocked(Device* dev, const RasterizerState* rs) {
  if (!dev) {
    return;
  }

  uint32_t cull_mode = static_cast<uint32_t>(D3D11_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  uint32_t depth_clip_enable = 1u;
  if (rs) {
    cull_mode = rs->cull_mode;
    front_ccw = rs->front_ccw;
    scissor_enable = rs->scissor_enable;
    depth_clip_enable = rs->depth_clip_enable;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  cmd->state.fill_mode = AEROGPU_FILL_SOLID;
  cmd->state.cull_mode = D3D11CullModeToAerogpu(cull_mode);
  cmd->state.front_ccw = front_ccw ? 1u : 0u;
  cmd->state.scissor_enable = scissor_enable ? 1u : 0u;
  cmd->state.depth_bias = 0;
  cmd->state.flags = depth_clip_enable ? AEROGPU_RASTERIZER_FLAG_NONE
                                       : AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;
}

static void EmitBlendStateLocked(Device* dev, const BlendState* bs) {
  if (!dev) {
    return;
  }

  uint32_t blend_enable = 0u;
  uint32_t src_blend = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t src_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ONE);
  uint32_t dst_blend_alpha = static_cast<uint32_t>(D3D11_BLEND_ZERO);
  uint32_t blend_op_alpha = static_cast<uint32_t>(D3D11_BLEND_OP_ADD);
  uint32_t write_mask = 0xFu;
  if (bs) {
    blend_enable = bs->blend_enable;
    src_blend = bs->src_blend;
    dst_blend = bs->dest_blend;
    blend_op = bs->blend_op;
    src_blend_alpha = bs->src_blend_alpha;
    dst_blend_alpha = bs->dest_blend_alpha;
    blend_op_alpha = bs->blend_op_alpha;
    write_mask = bs->render_target_write_mask;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  cmd->state.enable = blend_enable ? 1u : 0u;
  cmd->state.src_factor = D3D11BlendFactorToAerogpu(src_blend, AEROGPU_BLEND_ONE);
  cmd->state.dst_factor = D3D11BlendFactorToAerogpu(dst_blend, AEROGPU_BLEND_ZERO);
  cmd->state.blend_op = D3D11BlendOpToAerogpu(blend_op);
  cmd->state.color_write_mask = static_cast<uint8_t>(write_mask & 0xFu);
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
  cmd->state.reserved0[2] = 0;

  cmd->state.src_factor_alpha = D3D11BlendFactorToAerogpu(src_blend_alpha, cmd->state.src_factor);
  cmd->state.dst_factor_alpha = D3D11BlendFactorToAerogpu(dst_blend_alpha, cmd->state.dst_factor);
  cmd->state.blend_op_alpha = D3D11BlendOpToAerogpu(blend_op_alpha);

  cmd->state.blend_constant_rgba_f32[0] = f32_bits(dev->current_blend_factor[0]);
  cmd->state.blend_constant_rgba_f32[1] = f32_bits(dev->current_blend_factor[1]);
  cmd->state.blend_constant_rgba_f32[2] = f32_bits(dev->current_blend_factor[2]);
  cmd->state.blend_constant_rgba_f32[3] = f32_bits(dev->current_blend_factor[3]);
  cmd->state.sample_mask = dev->current_sample_mask;
}

static void EmitDepthStencilStateLocked(Device* dev, const DepthStencilState* dss) {
  if (!dev) {
    return;
  }

  uint32_t depth_enable = 1u;
  uint32_t depth_write_mask = static_cast<uint32_t>(D3D11_DEPTH_WRITE_MASK_ALL);
  uint32_t depth_func = static_cast<uint32_t>(D3D11_COMPARISON_LESS);
  uint32_t stencil_enable = 0u;
  if (dss) {
    depth_enable = dss->depth_enable;
    depth_write_mask = dss->depth_write_mask;
    depth_func = dss->depth_func;
    stencil_enable = dss->stencil_enable;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!cmd) {
    SetError(dev, E_OUTOFMEMORY);
    return;
  }

  cmd->state.depth_enable = depth_enable ? 1u : 0u;
  cmd->state.depth_write_enable = depth_write_mask ? 1u : 0u;
  cmd->state.depth_func = D3D11CompareFuncToAerogpu(depth_func);
  cmd->state.stencil_enable = stencil_enable ? 1u : 0u;
  cmd->state.stencil_read_mask = 0xFF;
  cmd->state.stencil_write_mask = 0xFF;
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
}

void AEROGPU_APIENTRY SetRasterizerState11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRASTERIZERSTATE hState) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_rs = hState.pDrvPrivate ? FromHandle<D3D11DDI_HRASTERIZERSTATE, RasterizerState>(hState) : nullptr;
  EmitRasterizerStateLocked(dev, dev->current_rs);
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
  dev->current_bs = hState.pDrvPrivate ? FromHandle<D3D11DDI_HBLENDSTATE, BlendState>(hState) : nullptr;
  if (blend_factor) {
    std::memcpy(dev->current_blend_factor, blend_factor, sizeof(dev->current_blend_factor));
  } else {
    dev->current_blend_factor[0] = 1.0f;
    dev->current_blend_factor[1] = 1.0f;
    dev->current_blend_factor[2] = 1.0f;
    dev->current_blend_factor[3] = 1.0f;
  }
  dev->current_sample_mask = sample_mask;
  EmitBlendStateLocked(dev, dev->current_bs);
}
void AEROGPU_APIENTRY SetDepthStencilState11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HDEPTHSTENCILSTATE hState,
                                              UINT stencil_ref) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_dss =
      hState.pDrvPrivate ? FromHandle<D3D11DDI_HDEPTHSTENCILSTATE, DepthStencilState>(hState) : nullptr;
  dev->current_stencil_ref = stencil_ref;
  EmitDepthStencilStateLocked(dev, dev->current_dss);
}

void AEROGPU_APIENTRY ClearState11(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Unbind shader resources explicitly (no range command in the protocol yet).
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (dev->vs_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
    }
    if (dev->ps_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
    }
  }

  // Unbind constant buffers and samplers using the range commands.
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> null_cbs{};
  auto emit_null_cbs = [&](uint32_t stage) -> bool {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
        AEROGPU_CMD_SET_CONSTANT_BUFFERS, null_cbs.data(), null_cbs.size() * sizeof(null_cbs[0]));
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return false;
    }
    cmd->shader_stage = stage;
    cmd->start_slot = 0;
    cmd->buffer_count = kMaxConstantBufferSlots;
    cmd->reserved0 = 0;
    return true;
  };
  if (!emit_null_cbs(AEROGPU_SHADER_STAGE_VERTEX) || !emit_null_cbs(AEROGPU_SHADER_STAGE_PIXEL)) {
    return;
  }
  std::memset(dev->vs_constant_buffers, 0, sizeof(dev->vs_constant_buffers));
  std::memset(dev->ps_constant_buffers, 0, sizeof(dev->ps_constant_buffers));
  dev->current_vs_cbs.fill(nullptr);
  dev->current_ps_cbs.fill(nullptr);

  std::array<aerogpu_handle_t, kMaxSamplerSlots> null_samplers{};
  auto emit_null_samplers = [&](uint32_t stage) -> bool {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
        AEROGPU_CMD_SET_SAMPLERS, null_samplers.data(), null_samplers.size() * sizeof(null_samplers[0]));
    if (!cmd) {
      SetError(dev, E_OUTOFMEMORY);
      return false;
    }
    cmd->shader_stage = stage;
    cmd->start_slot = 0;
    cmd->sampler_count = kMaxSamplerSlots;
    cmd->reserved0 = 0;
    return true;
  };
  if (!emit_null_samplers(AEROGPU_SHADER_STAGE_VERTEX) || !emit_null_samplers(AEROGPU_SHADER_STAGE_PIXEL)) {
    return;
  }
  std::memset(dev->vs_samplers, 0, sizeof(dev->vs_samplers));
  std::memset(dev->ps_samplers, 0, sizeof(dev->ps_samplers));

  dev->current_rtv = 0;
  dev->current_rtv_resource = nullptr;
  dev->current_dsv = 0;
  dev->current_dsv_resource = nullptr;
  dev->current_vs_srvs.fill(nullptr);
  dev->current_ps_srvs.fill(nullptr);
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_gs = 0;
  dev->current_input_layout = 0;
  dev->current_input_layout_obj = nullptr;
  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  dev->current_vb = nullptr;
  dev->current_vb_stride_bytes = 0;
  dev->current_vb_offset_bytes = 0;
  dev->current_ib = nullptr;
  dev->current_ib_format = kDxgiFormatUnknown;
  dev->current_ib_offset_bytes = 0;
  dev->current_vs_cb0 = nullptr;
  dev->current_vs_cb0_first_constant = 0;
  dev->current_vs_cb0_num_constants = 0;
  dev->current_ps_cb0 = nullptr;
  dev->current_ps_cb0_first_constant = 0;
  dev->current_ps_cb0_num_constants = 0;
  dev->current_vs_srv0 = nullptr;
  dev->current_ps_srv0 = nullptr;
  dev->current_vs_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_vs_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_ps_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_ps_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  dev->current_dss = nullptr;
  dev->current_stencil_ref = 0;
  dev->current_rs = nullptr;
  dev->current_bs = nullptr;
  dev->current_blend_factor[0] = 1.0f;
  dev->current_blend_factor[1] = 1.0f;
  dev->current_blend_factor[2] = 1.0f;
  dev->current_blend_factor[3] = 1.0f;
  dev->current_sample_mask = 0xFFFFFFFFu;
  dev->scissor_valid = false;
  dev->scissor_left = 0;
  dev->scissor_top = 0;
  dev->scissor_right = 0;
  dev->scissor_bottom = 0;
  dev->current_vs_forced_z_valid = false;
  dev->current_vs_forced_z = 0.0f;
  dev->viewport_x = 0.0f;
  dev->viewport_y = 0.0f;
  dev->viewport_width = 0.0f;
  dev->viewport_height = 0.0f;
  dev->viewport_min_depth = 0.0f;
  dev->viewport_max_depth = 1.0f;

  EmitSetRenderTargetsLocked(dev);

  EmitBlendStateLocked(dev, nullptr);
  EmitDepthStencilStateLocked(dev, nullptr);
  EmitRasterizerStateLocked(dev, nullptr);

  EmitBindShadersLocked(dev);
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
  dev->current_rtv = 0;
  dev->current_rtv_resource = nullptr;
  if (NumViews && phRtvs && phRtvs[0].pDrvPrivate) {
    auto* rtv = FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(phRtvs[0]);
    dev->current_rtv_resource = rtv ? rtv->resource : nullptr;
    dev->current_rtv = dev->current_rtv_resource ? dev->current_rtv_resource->handle : (rtv ? rtv->texture : 0);
  }
  if (hDsv.pDrvPrivate) {
    auto* dsv = FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hDsv);
    dev->current_dsv_resource = dsv ? dsv->resource : nullptr;
    dev->current_dsv = dev->current_dsv_resource ? dev->current_dsv_resource->handle : (dsv ? dsv->texture : 0);
  } else {
    dev->current_dsv = 0;
    dev->current_dsv_resource = nullptr;
  }

  // Auto-unbind SRVs that alias the newly bound render targets/depth buffer.
  UnbindResourceFromSrvsLocked(dev, dev->current_rtv);
  UnbindResourceFromSrvsLocked(dev, dev->current_dsv);

  EmitSetRenderTargetsLocked(dev);
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

  uint8_t px[4] = {0, 0, 0, 0};
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
      break;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatR8G8B8A8Typeless:
      px[0] = r;
      px[1] = g;
      px[2] = b;
      px[3] = a;
      break;
    default:
      return;
  }

  for (uint32_t y = 0; y < rt->height; y++) {
    uint8_t* row = rt->storage.data() + static_cast<size_t>(y) * rt->row_pitch_bytes;
    for (uint32_t x = 0; x < rt->width; x++) {
      std::memcpy(row + static_cast<size_t>(x) * 4, px, sizeof(px));
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
    if (offset == 0xFFFFFFFFu) {
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
  uint32_t write_mask = 0xFu;
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
  Resource* rt = dev->current_rtv_resource;
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
  Resource* rt = dev->current_rtv_resource;
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
    rt = dev->current_rtv_resource;
  }
  SoftwareClearTexture2D(rt, rgba);
  TrackBoundTargetsForSubmitLocked(dev);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
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
  if (flags & 0x1u) {
    SoftwareClearDepthTexture2D(ds, depth);
  }

  TrackBoundTargetsForSubmitLocked(dev);
  uint32_t aer_flags = 0;
  if (flags & 0x1u) {
    aer_flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (flags & 0x2u) {
    aer_flags |= AEROGPU_CLEAR_STENCIL;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = aer_flags;
  cmd->color_rgba_f32[0] = 0;
  cmd->color_rgba_f32[1] = 0;
  cmd->color_rgba_f32[2] = 0;
  cmd->color_rgba_f32[3] = 0;
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
}

void AEROGPU_APIENTRY Draw11(D3D11DDI_HDEVICECONTEXT hCtx, UINT VertexCount, UINT StartVertexLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  SoftwareDrawTriangleList(dev, VertexCount, StartVertexLocation);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
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
  TrackDrawStateLocked(dev);
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawTriangleList(dev, VertexCountPerInstance, StartVertexLocation);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
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

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  SoftwareDrawIndexedTriangleList(dev, IndexCount, StartIndexLocation, BaseVertexLocation);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
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
  TrackDrawStateLocked(dev);
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  SoftwareDrawIndexedTriangleList(dev, IndexCountPerInstance, StartIndexLocation, BaseVertexLocation);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = IndexCountPerInstance;
  cmd->instance_count = InstanceCount;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = StartInstanceLocation;
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

  if (dst_subresource != 0 || src_subresource != 0 || dst_z != 0) {
    SetError(dev, E_NOTIMPL);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dst->kind == ResourceKind::Buffer && src->kind == ResourceKind::Buffer) {
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
      std::memmove(dst->storage.data() + static_cast<size_t>(dst_off),
                   src->storage.data() + static_cast<size_t>(src_left),
                   static_cast<size_t>(bytes));
    }

    EmitUploadLocked(dev, dst, dst_off, bytes);

    const bool transfer_aligned = (((dst_off | src_left | bytes) & 3ull) == 0);
    const bool same_buffer = (dst->handle == src->handle);
    if (!SupportsTransfer(dev) || !transfer_aligned || same_buffer) {
      return;
    }

    TrackWddmAllocForSubmitLocked(dev, src);
    TrackWddmAllocForSubmitLocked(dev, dst);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
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
    TrackStagingWriteLocked(dev, dst);
    return;
  }

  if (dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t src_left = pSrcBox ? static_cast<uint32_t>(pSrcBox->left) : 0;
    const uint32_t src_top = pSrcBox ? static_cast<uint32_t>(pSrcBox->top) : 0;
    const uint32_t src_right = pSrcBox ? static_cast<uint32_t>(pSrcBox->right) : src->width;
    const uint32_t src_bottom = pSrcBox ? static_cast<uint32_t>(pSrcBox->bottom) : src->height;

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

    const uint32_t copy_width = std::min(src_right - src_left, dst->width > dst_x ? (dst->width - dst_x) : 0u);
    const uint32_t copy_height = std::min(src_bottom - src_top, dst->height > dst_y ? (dst->height - dst_y) : 0u);

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
    const uint32_t dst_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst->width);
    const uint32_t src_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, src->width);
    const uint32_t dst_rows_total = aerogpu_texture_num_rows(aer_fmt, dst->height);
    const uint32_t src_rows_total = aerogpu_texture_num_rows(aer_fmt, src->height);
    if (!layout.valid || dst_min_row == 0 || src_min_row == 0 || dst_rows_total == 0 || src_rows_total == 0 ||
        dst->row_pitch_bytes < dst_min_row || src->row_pitch_bytes < src_min_row) {
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
          !aligned_or_edge(src_copy_right, layout.block_width, src->width) ||
          !aligned_or_edge(src_copy_bottom, layout.block_height, src->height) ||
          !aligned_or_edge(dst_copy_right, layout.block_width, dst->width) ||
          !aligned_or_edge(dst_copy_bottom, layout.block_height, dst->height)) {
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

    if (row_bytes && copy_height_blocks && dst_row_needed <= dst->row_pitch_bytes && src_row_needed <= src->row_pitch_bytes &&
        dst_block_top + copy_height_blocks <= dst_rows_total && src_block_top + copy_height_blocks <= src_rows_total) {
      // Staging resources are guest-backed and Map(READ) exposes the runtime
      // allocation pointer. When running without a functional transfer backend,
      // mirror CPU-side texture copies into the allocation so readbacks observe
      // the software shadow (`dst->storage`).
      const bool want_staging_readback = (dst->usage == kD3D11UsageStaging) &&
                                         ((dst->cpu_access_flags & kD3D11CpuAccessRead) != 0) &&
                                         (dst->backing_alloc_id != 0) && (dst->wddm_allocation_handle != 0);

      uint8_t* dst_wddm_bytes = nullptr;
      uint32_t dst_wddm_pitch = 0;
      D3DDDICB_LOCK lock_args = {};
      const auto* ddi = want_staging_readback ? reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->runtime_ddi_callbacks) : nullptr;
      const auto* cb_device =
          want_staging_readback ? reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks) : nullptr;

      const auto lock_for_write = [&](D3DDDICB_LOCK* args) -> HRESULT {
        if (!args || !dev->runtime_device) {
          return E_NOTIMPL;
        }
        if (ddi && ddi->pfnLockCb) {
          return CallCbMaybeHandle(ddi->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
        }
        if (cb_device && cb_device->pfnLockCb) {
          return CallCbMaybeHandle(cb_device->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
        }
        return E_NOTIMPL;
      };

      const auto unlock = [&](D3DDDICB_UNLOCK* args) -> HRESULT {
        if (!args || !dev->runtime_device) {
          return E_NOTIMPL;
        }
        if (ddi && ddi->pfnUnlockCb) {
          return CallCbMaybeHandle(ddi->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
        }
        if (cb_device && cb_device->pfnUnlockCb) {
          return CallCbMaybeHandle(cb_device->pfnUnlockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), args);
        }
        return E_NOTIMPL;
      };

      if (want_staging_readback) {
        const bool has_lock_unlock = (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb) || (cb_device && cb_device->pfnLockCb && cb_device->pfnUnlockCb);
        if (!has_lock_unlock) {
          SetError(dev, E_NOTIMPL);
          return;
        }

        lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(dst->wddm_allocation_handle);
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

        dst_wddm_pitch = dst->row_pitch_bytes;
        __if_exists(D3DDDICB_LOCK::Pitch) {
          if (lock_args.Pitch) {
            dst_wddm_pitch = lock_args.Pitch;
          }
        }
        if (dst_row_needed > dst_wddm_pitch) {
          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
            unlock_args.SubresourceIndex = dst_subresource;
          }
          __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
            unlock_args.SubResourceIndex = dst_subresource;
          }
          (void)unlock(&unlock_args);
          SetError(dev, E_INVALIDARG);
          return;
        }

        dst_wddm_bytes = static_cast<uint8_t*>(lock_args.pData);
      }

      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        const size_t dst_off =
            static_cast<size_t>(dst_block_top + y) * dst->row_pitch_bytes +
            static_cast<size_t>(dst_block_left) * layout.bytes_per_block;
        const size_t src_off =
            static_cast<size_t>(src_block_top + y) * src->row_pitch_bytes +
            static_cast<size_t>(src_block_left) * layout.bytes_per_block;
        if (dst_off + row_bytes <= dst->storage.size() && src_off + row_bytes <= src->storage.size()) {
          std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
          if (dst_wddm_bytes) {
            const size_t dst_wddm_off =
                static_cast<size_t>(dst_block_top + y) * dst_wddm_pitch +
                static_cast<size_t>(dst_block_left) * layout.bytes_per_block;
            std::memcpy(dst_wddm_bytes + dst_wddm_off, src->storage.data() + src_off, row_bytes);
          }
        }
      }

      if (dst_wddm_bytes) {
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
          unlock_args.SubresourceIndex = dst_subresource;
        }
        __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
          unlock_args.SubResourceIndex = dst_subresource;
        }
        HRESULT hr = unlock(&unlock_args);
        if (FAILED(hr)) {
          SetError(dev, hr);
          return;
        }

        EmitDirtyRangeLocked(dev,
                             dst,
                             0,
                             aerogpu_texture_required_size_bytes(aer_fmt, dst->row_pitch_bytes, dst->height));
      }
    }

    if (!SupportsTransfer(dev)) {
      return;
    }

    TrackWddmAllocForSubmitLocked(dev, src);
    TrackWddmAllocForSubmitLocked(dev, dst);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
    cmd->dst_texture = dst->handle;
    cmd->src_texture = src->handle;
    cmd->dst_mip_level = 0;
    cmd->dst_array_layer = 0;
    cmd->src_mip_level = 0;
    cmd->src_array_layer = 0;
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
    TrackStagingWriteLocked(dev, dst);
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
  if (subresource != 0) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if (res->mapped) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  if ((map_flags & ~static_cast<UINT>(D3D11_MAP_FLAG_DO_NOT_WAIT)) != 0) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  const uint32_t map_u32 = static_cast<uint32_t>(map_type);
  bool want_read = false;
  bool want_write = false;
  switch (map_u32) {
    case D3D11_MAP_READ:
      want_read = true;
      break;
    case D3D11_MAP_WRITE:
    case D3D11_MAP_WRITE_DISCARD:
    case D3D11_MAP_WRITE_NO_OVERWRITE:
      want_write = true;
      break;
    case D3D11_MAP_READ_WRITE:
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
      if (map_u32 != D3D11_MAP_WRITE_DISCARD && map_u32 != D3D11_MAP_WRITE_NO_OVERWRITE) {
        SetError(dev, E_INVALIDARG);
        return E_INVALIDARG;
      }
      break;
    case kD3D11UsageStaging: {
      const uint32_t access_mask = kD3D11CpuAccessRead | kD3D11CpuAccessWrite;
      const uint32_t access = res->cpu_access_flags & access_mask;
      if (access == kD3D11CpuAccessRead) {
        if (map_u32 != D3D11_MAP_READ) {
          SetError(dev, E_INVALIDARG);
          return E_INVALIDARG;
        }
      } else if (access == kD3D11CpuAccessWrite) {
        if (map_u32 != D3D11_MAP_WRITE) {
          SetError(dev, E_INVALIDARG);
          return E_INVALIDARG;
        }
      } else if (access == access_mask) {
        if (map_u32 != D3D11_MAP_READ && map_u32 != D3D11_MAP_WRITE && map_u32 != D3D11_MAP_READ_WRITE) {
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
      const bool do_not_wait = (map_flags & D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;
      const UINT64 timeout = do_not_wait ? 0ull : ~0ull;
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

  if (map_u32 == D3D11_MAP_WRITE_DISCARD) {
    if (res->kind == ResourceKind::Buffer) {
      // Approximate DISCARD renaming by allocating a fresh CPU backing store.
      try {
        res->storage.assign(res->storage.size(), 0);
      } catch (...) {
        SetError(dev, E_OUTOFMEMORY);
        return E_OUTOFMEMORY;
      }
    } else if (res->kind == ResourceKind::Texture2D) {
      std::fill(res->storage.begin(), res->storage.end(), 0);
    }
  }

  const bool allow_storage_map = (res->backing_alloc_id == 0) && !(want_read && res->usage == kD3D11UsageStaging);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    pMapped->pData = res->storage.empty() ? nullptr : res->storage.data();
    if (res->kind == ResourceKind::Texture2D) {
      pMapped->RowPitch = res->row_pitch_bytes;
      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      const uint64_t slice = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
      pMapped->DepthPitch = static_cast<UINT>(slice);
    } else {
      pMapped->RowPitch = static_cast<UINT>(res->storage.size());
      pMapped->DepthPitch = static_cast<UINT>(res->storage.size());
    }

    res->mapped = true;
    res->mapped_map_type = map_u32;
    res->mapped_map_flags = map_flags;
    res->mapped_offset = 0;
    res->mapped_size = res->storage.size();
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
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock.SubresourceIndex = subresource;
  }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
    lock.SubResourceIndex = subresource;
  }
  __if_exists(D3DDDICB_LOCK::Flags) {
    std::memset(&lock.Flags, 0, sizeof(lock.Flags));

    const bool do_not_wait = (map_flags & D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;
    __if_exists(D3DDDICB_LOCKFLAGS::DoNotWait) {
      lock.Flags.DoNotWait = do_not_wait ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::DonotWait) {
      lock.Flags.DonotWait = do_not_wait ? 1u : 0u;
    }

    const bool is_read_only = (map_u32 == D3D11_MAP_READ);
    const bool is_write_only =
        (map_u32 == D3D11_MAP_WRITE || map_u32 == D3D11_MAP_WRITE_DISCARD || map_u32 == D3D11_MAP_WRITE_NO_OVERWRITE);
    const bool discard = (map_u32 == D3D11_MAP_WRITE_DISCARD);
    const bool no_overwrite = (map_u32 == D3D11_MAP_WRITE_NO_OVERWRITE);

    __if_exists(D3DDDICB_LOCKFLAGS::ReadOnly) {
      lock.Flags.ReadOnly = is_read_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) {
      lock.Flags.WriteOnly = is_write_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Write) {
      // READ_WRITE is encoded as ReadOnly=0 and WriteOnly/Write=0 (see
      // docs/graphics/win7-d3d11-map-unmap.md §3).
      lock.Flags.Write = is_write_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Discard) {
      lock.Flags.Discard = discard ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverwrite) {
      lock.Flags.NoOverwrite = no_overwrite ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverWrite) {
      lock.Flags.NoOverWrite = no_overwrite ? 1u : 0u;
    }
  }

  HRESULT lock_hr = E_FAIL;
  if (lock_path == LockCbPath::Wddm) {
    lock_hr = CallCbMaybeHandle(cb->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock);
  } else {
    lock_hr = CallCbMaybeHandle(cb_device->pfnLockCb, MakeRtDeviceHandle(dev), MakeRtDeviceHandle10(dev), &lock);
  }
  const bool do_not_wait = (map_flags & D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;
  if (lock_hr == kDxgiErrorWasStillDrawing ||
      (do_not_wait && (lock_hr == kHrPending || lock_hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) ||
                       lock_hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT) || lock_hr == static_cast<HRESULT>(0x10000102L) ||
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
    __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
      unlock.SubresourceIndex = subresource;
    }
    __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
      unlock.SubResourceIndex = subresource;
    }
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

  res->mapped_wddm_ptr = lock.pData;
  res->mapped_wddm_allocation = alloc_handle;
  __if_exists(D3DDDICB_LOCK::Pitch) {
    res->mapped_wddm_pitch = lock.Pitch;
  }
  __if_exists(D3DDDICB_LOCK::SlicePitch) {
    res->mapped_wddm_slice_pitch = lock.SlicePitch;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);

  // Keep the software-backed shadow copy (`res->storage`) in sync with the
  // runtime allocation pointer we hand back to the D3D runtime.
  if (!res->storage.empty()) {
    if (map_u32 == D3D11_MAP_WRITE_DISCARD) {
      // Discard contents are undefined; clear for deterministic tests.
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
        const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
        const uint32_t pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
        const uint64_t bytes = static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows);
        if (pitch != 0 && bytes <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(lock.pData, 0, static_cast<size_t>(bytes));
        }
      } else {
        std::memset(lock.pData, 0, res->storage.size());
      }
    } else if (!is_guest_backed && res->kind == ResourceKind::Texture2D) {
      const uint32_t src_pitch = res->row_pitch_bytes;
      const uint32_t dst_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : src_pitch;

      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
      if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 && src_pitch >= row_bytes && dst_pitch >= row_bytes) {
        auto* dst_bytes = static_cast<uint8_t*>(lock.pData);
        const uint8_t* src_bytes = res->storage.data();
        for (uint32_t y = 0; y < rows; y++) {
          std::memcpy(dst_bytes + static_cast<size_t>(y) * dst_pitch,
                      src_bytes + static_cast<size_t>(y) * src_pitch,
                      row_bytes);
          if (dst_pitch > row_bytes) {
            std::memset(dst_bytes + static_cast<size_t>(y) * dst_pitch + row_bytes, 0, dst_pitch - row_bytes);
          }
        }
      } else {
        std::memcpy(lock.pData, res->storage.data(), res->storage.size());
      }
    } else if (!is_guest_backed) {
      std::memcpy(lock.pData, res->storage.data(), res->storage.size());
    } else if (want_read) {
      // Guest-backed resources are updated by writing directly into the backing
      // allocation (and emitting RESOURCE_DIRTY_RANGE). Avoid overwriting the
      // runtime allocation contents with shadow storage; instead refresh the
      // shadow copy for any Map() that reads.
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t dst_pitch = res->row_pitch_bytes;
        const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : dst_pitch;

        const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
        const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
        const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
        if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 && src_pitch >= row_bytes && dst_pitch >= row_bytes) {
          const uint8_t* src_bytes = static_cast<const uint8_t*>(lock.pData);
          auto* dst_bytes = res->storage.data();
          for (uint32_t y = 0; y < rows; y++) {
            std::memcpy(dst_bytes + static_cast<size_t>(y) * dst_pitch,
                        src_bytes + static_cast<size_t>(y) * src_pitch,
                        row_bytes);
            if (dst_pitch > row_bytes) {
              std::memset(dst_bytes + static_cast<size_t>(y) * dst_pitch + row_bytes, 0, dst_pitch - row_bytes);
            }
          }
        }
      } else {
        std::memcpy(res->storage.data(), lock.pData, res->storage.size());
      }
    }
  }

  pMapped->pData = lock.pData;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
    pMapped->RowPitch = pitch;
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    const uint32_t slice = res->mapped_wddm_slice_pitch ? res->mapped_wddm_slice_pitch
                                                        : static_cast<uint32_t>(static_cast<uint64_t>(pitch) *
                                                                                 static_cast<uint64_t>(rows));
    pMapped->DepthPitch = slice;
  } else {
    pMapped->RowPitch = static_cast<UINT>(res->storage.size());
    pMapped->DepthPitch = static_cast<UINT>(res->storage.size());
  }

  res->mapped = true;
  res->mapped_map_type = map_u32;
  res->mapped_map_flags = map_flags;
  res->mapped_offset = 0;
  uint64_t mapped_size = 0;
  if (res->kind == ResourceKind::Buffer) {
    mapped_size = res->size_bytes;
  } else if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    mapped_size = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
  } else {
    mapped_size = res->storage.size();
  }
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
  if (subresource != 0) {
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
  if (subresource != 0) {
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
  if (res->usage != kD3D11UsageStaging || subresource != 0) {
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
  return DynamicBufferMapCore11(hCtx, hResource, kD3D11BindVertexBuffer | kD3D11BindIndexBuffer, D3D11_MAP_WRITE_DISCARD, ppData);
}

void AEROGPU_APIENTRY DynamicIABufferMapDiscard11Void(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  (void)DynamicIABufferMapDiscard11(hCtx, hResource, ppData);
}

HRESULT AEROGPU_APIENTRY DynamicIABufferMapNoOverwrite11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();
  return DynamicBufferMapCore11(hCtx,
                               hResource,
                               kD3D11BindVertexBuffer | kD3D11BindIndexBuffer,
                               D3D11_MAP_WRITE_NO_OVERWRITE,
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
  return DynamicBufferMapCore11(hCtx, hResource, kD3D11BindConstantBuffer, D3D11_MAP_WRITE_DISCARD, ppData);
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

  if (dst_subresource != 0) {
    SetError(dev, E_NOTIMPL);
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

    std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pSysMem, static_cast<size_t>(bytes));
    if (!is_guest_backed) {
      EmitUploadLocked(dev, res, dst_off, bytes);
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

    std::memcpy(static_cast<uint8_t*>(lock_args.pData) + static_cast<size_t>(dst_off),
                pSysMem,
                static_cast<size_t>(bytes));

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

    EmitDirtyRangeLocked(dev, res, dst_off, bytes);
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(pSysMem);
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aer_fmt);
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (!layout.valid || min_row_bytes == 0 || res->row_pitch_bytes < min_row_bytes) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    uint32_t left = 0;
    uint32_t top = 0;
    uint32_t right = res->width;
    uint32_t bottom = res->height;
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
    if (right > res->width || bottom > res->height) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    if (layout.block_width > 1 || layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((left % layout.block_width) != 0 || (top % layout.block_height) != 0 ||
          !aligned_or_edge(right, layout.block_width, res->width) ||
          !aligned_or_edge(bottom, layout.block_height, res->height)) {
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

    for (uint32_t y = 0; y < copy_height_blocks; y++) {
      const size_t dst_off =
          static_cast<size_t>(block_top + y) * res->row_pitch_bytes + static_cast<size_t>(block_left) * layout.bytes_per_block;
      const size_t src_off = static_cast<size_t>(y) * pitch;
      if (dst_off + row_bytes > res->storage.size()) {
        SetError(dev, E_FAIL);
        return;
      }
      std::memcpy(res->storage.data() + dst_off, src_bytes + src_off, row_bytes);
    }

    if (!is_guest_backed) {
      // Texture updates are not guaranteed to be contiguous in memory (unless the
      // full subresource is updated). For the bring-up path, upload the whole
      // resource after applying the CPU-side update.
      EmitUploadLocked(dev, res, 0, res->storage.size());
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

    uint32_t dst_pitch = res->row_pitch_bytes;
    __if_exists(D3DDDICB_LOCK::Pitch) {
      if (lock_args.Pitch) {
        dst_pitch = lock_args.Pitch;
      }
    }
    const uint32_t required_pitch_end = block_right * layout.bytes_per_block;
    if (dst_pitch < required_pitch_end) {
      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
        unlock_args.SubresourceIndex = dst_subresource;
      }
      __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
        unlock_args.SubResourceIndex = dst_subresource;
      }
      (void)unlock(&unlock_args);
      SetError(dev, E_INVALIDARG);
      return;
    }

    uint8_t* dst_base = static_cast<uint8_t*>(lock_args.pData);
    for (uint32_t y = 0; y < copy_height_blocks; y++) {
      const size_t dst_off =
          static_cast<size_t>(block_top + y) * dst_pitch + static_cast<size_t>(block_left) * layout.bytes_per_block;
      const size_t src_off = static_cast<size_t>(y) * pitch;
      std::memcpy(dst_base + dst_off, src_bytes + src_off, row_bytes);
    }

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

    EmitDirtyRangeLocked(dev,
                         res,
                         0,
                         aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height));
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
  TrackWddmAllocForSubmitLocked(dev, src_res);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  aerogpu_handle_t src_handle = 0;
  src_handle = src_res ? src_res->handle : 0;
  AEROGPU_D3D10_11_LOG("trace_resources: D3D11 Present sync=%u src_handle=%u",
                       static_cast<unsigned>(pPresent->SyncInterval),
                       static_cast<unsigned>(src_handle));
#endif

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!cmd) {
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
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
  resources.reserve(numResources);
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

  struct ResourceIdentity {
    aerogpu_handle_t handle = 0;
    uint32_t backing_alloc_id = 0;
    uint32_t backing_offset_bytes = 0;
    uint32_t wddm_allocation_handle = 0;
    Resource::WddmIdentity wddm;
    std::vector<uint8_t> storage;
    uint64_t last_gpu_write_fence = 0;
    bool mapped = false;
    uint32_t mapped_map_type = 0;
    uint32_t mapped_map_flags = 0;
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
    id.storage = std::move(res->storage);
    id.last_gpu_write_fence = res->last_gpu_write_fence;
    id.mapped = res->mapped;
    id.mapped_map_type = res->mapped_map_type;
    id.mapped_map_flags = res->mapped_map_flags;
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
    res->storage = std::move(id.storage);
    res->last_gpu_write_fence = id.last_gpu_write_fence;
    res->mapped = id.mapped;
    res->mapped_map_type = id.mapped_map_type;
    res->mapped_map_flags = id.mapped_map_flags;
    res->mapped_offset = id.mapped_offset;
    res->mapped_size = id.mapped_size;
  };

  ResourceIdentity saved = take_identity(resources[0]);
  for (UINT i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  const bool needs_rebind =
      dev->current_rtv_resource &&
      (std::find(resources.begin(), resources.end(), dev->current_rtv_resource) != resources.end());
  if (needs_rebind) {
    const aerogpu_handle_t new_rtv = dev->current_rtv_resource ? dev->current_rtv_resource->handle : 0;
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!cmd) {
      // Undo the rotation (rotate right by one).
      ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
      for (UINT i = numResources - 1; i > 0; --i) {
        put_identity(resources[i], take_identity(resources[i - 1]));
      }
      put_identity(resources[0], std::move(undo_saved));
      SetError(dev, E_OUTOFMEMORY);
      return;
    }

    dev->current_rtv = new_rtv;
    cmd->color_count = new_rtv ? 1u : 0u;
    cmd->depth_stencil = dev->current_dsv;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
      cmd->colors[i] = 0;
    }
    if (new_rtv) {
      cmd->colors[0] = new_rtv;
    }
  }

  auto is_rotated = [&resources](const Resource* res) -> bool {
    if (!res) {
      return false;
    }
    return std::find(resources.begin(), resources.end(), res) != resources.end();
  };

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_vs_srvs[slot])) {
      continue;
    }
    SetShaderResourceSlotLocked(dev,
                                AEROGPU_SHADER_STAGE_VERTEX,
                                slot,
                                dev->current_vs_srvs[slot] ? dev->current_vs_srvs[slot]->handle : 0);
  }

  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_ps_srvs[slot])) {
      continue;
    }
    SetShaderResourceSlotLocked(dev,
                                AEROGPU_SHADER_STAGE_PIXEL,
                                slot,
                                dev->current_ps_srvs[slot] ? dev->current_ps_srvs[slot]->handle : 0);
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
  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource11;
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnOpenResource) {
    pCreateDevice->pDeviceFuncs->pfnOpenResource = &OpenResource11;
  }
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = &DestroyResource11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = &CalcPrivateRenderTargetViewSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = &CreateRenderTargetView11;
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = &DestroyRenderTargetView11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = &CalcPrivateDepthStencilViewSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = &CreateDepthStencilView11;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = &DestroyDepthStencilView11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = &CreateShaderResourceView11;
  pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = &DestroyShaderResourceView11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = &CreateVertexShader11;
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = &DestroyVertexShader11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize11;
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = &CreatePixelShader11;
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = &DestroyPixelShader11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize = &CalcPrivateGeometryShaderSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader = &CreateGeometryShader11;
  pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader = &DestroyGeometryShader11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = &CalcPrivateElementLayoutSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = &CreateElementLayout11;
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = &DestroyElementLayout11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateSampler = &CreateSampler11;
  pCreateDevice->pDeviceFuncs->pfnDestroySampler = &DestroySampler11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = &CreateBlendState11;
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = &DestroyBlendState11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = &CreateRasterizerState11;
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = &DestroyRasterizerState11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = &CreateDepthStencilState11;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = &DestroyDepthStencilState11;

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnGetDeviceRemovedReason) {
    pCreateDevice->pDeviceFuncs->pfnGetDeviceRemovedReason = &GetDeviceRemovedReason11;
  }

  BindPresentAndRotate(pCreateDevice->pDeviceFuncs);

  // Immediate context funcs.
  ctx_funcs->pfnIaSetInputLayout = &IaSetInputLayout11;
  ctx_funcs->pfnIaSetVertexBuffers = &IaSetVertexBuffers11;
  ctx_funcs->pfnIaSetIndexBuffer = &IaSetIndexBuffer11;
  ctx_funcs->pfnIaSetTopology = &IaSetTopology11;
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSoSetTargets) {
    using Fn = decltype(ctx_funcs->pfnSoSetTargets);
    ctx_funcs->pfnSoSetTargets = &SoSetTargetsThunk<Fn>::Impl;
  }

  ctx_funcs->pfnVsSetShader = &VsSetShader11;
  ctx_funcs->pfnVsSetConstantBuffers = &VsSetConstantBuffers11;
  ctx_funcs->pfnVsSetShaderResources = &VsSetShaderResources11;
  ctx_funcs->pfnVsSetSamplers = &VsSetSamplers11;

  ctx_funcs->pfnPsSetShader = &PsSetShader11;
  ctx_funcs->pfnPsSetConstantBuffers = &PsSetConstantBuffers11;
  ctx_funcs->pfnPsSetShaderResources = &PsSetShaderResources11;
  ctx_funcs->pfnPsSetSamplers = &PsSetSamplers11;

  ctx_funcs->pfnGsSetShader = &GsSetShader11;
  ctx_funcs->pfnGsSetConstantBuffers = &GsSetConstantBuffers11;
  ctx_funcs->pfnGsSetShaderResources = &GsSetShaderResources11;
  ctx_funcs->pfnGsSetSamplers = &GsSetSamplers11;

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShader) { ctx_funcs->pfnHsSetShader = &HsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetConstantBuffers) {
    ctx_funcs->pfnHsSetConstantBuffers = &HsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShaderResources) {
    ctx_funcs->pfnHsSetShaderResources = &HsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetSamplers) { ctx_funcs->pfnHsSetSamplers = &HsSetSamplers11; }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShader) { ctx_funcs->pfnDsSetShader = &DsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetConstantBuffers) {
    ctx_funcs->pfnDsSetConstantBuffers = &DsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShaderResources) {
    ctx_funcs->pfnDsSetShaderResources = &DsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetSamplers) { ctx_funcs->pfnDsSetSamplers = &DsSetSamplers11; }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShader) { ctx_funcs->pfnCsSetShader = &CsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetConstantBuffers) {
    ctx_funcs->pfnCsSetConstantBuffers = &CsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShaderResources) {
    ctx_funcs->pfnCsSetShaderResources = &CsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetSamplers) { ctx_funcs->pfnCsSetSamplers = &CsSetSamplers11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetUnorderedAccessViews) {
    ctx_funcs->pfnCsSetUnorderedAccessViews = &CsSetUnorderedAccessViews11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetPredication) {
    using Fn = decltype(ctx_funcs->pfnSetPredication);
    ctx_funcs->pfnSetPredication = &SetPredicationThunk<Fn>::Impl;
  }

  ctx_funcs->pfnSetViewports = &SetViewports11;
  ctx_funcs->pfnSetScissorRects = &SetScissorRects11;
  ctx_funcs->pfnSetRasterizerState = &SetRasterizerState11;
  ctx_funcs->pfnSetBlendState = &SetBlendState11;
  ctx_funcs->pfnSetDepthStencilState = &SetDepthStencilState11;
  ctx_funcs->pfnSetRenderTargets = &SetRenderTargets11;
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews) {
    ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews =
        &SetRenderTargetsAndUavsThunk<decltype(ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews)>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews11_1) {
    ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews11_1 =
        &SetRenderTargetsAndUavsThunk<decltype(ctx_funcs->pfnSetRenderTargetsAndUnorderedAccessViews11_1)>::Impl;
  }

  ctx_funcs->pfnClearState = &ClearState11;
  ctx_funcs->pfnClearRenderTargetView = &ClearRenderTargetView11;
  ctx_funcs->pfnClearDepthStencilView = &ClearDepthStencilView11;
  ctx_funcs->pfnDraw = &Draw11;
  ctx_funcs->pfnDrawIndexed = &DrawIndexed11;
  ctx_funcs->pfnDrawInstanced = &DrawInstanced11;
  ctx_funcs->pfnDrawIndexedInstanced = &DrawIndexedInstanced11;

  ctx_funcs->pfnCopyResource = &CopyResource11;
  ctx_funcs->pfnCopySubresourceRegion = &CopySubresourceRegion11;

  // Map can be HRESULT or void depending on interface version.
  if constexpr (std::is_same_v<decltype(ctx_funcs->pfnMap), decltype(&Map11)>) {
    ctx_funcs->pfnMap = &Map11;
  } else {
    ctx_funcs->pfnMap = &Map11Void;
  }
  ctx_funcs->pfnUnmap = &Unmap11;
  {
    using Fn = decltype(ctx_funcs->pfnUpdateSubresourceUP);
    if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = &UpdateSubresourceUP11;
    } else if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11Args), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = &UpdateSubresourceUP11Args;
    } else if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11ArgsAndSysMem), Fn>) {
      ctx_funcs->pfnUpdateSubresourceUP = &UpdateSubresourceUP11ArgsAndSysMem;
    } else {
      ctx_funcs->pfnUpdateSubresourceUP = &DdiStub<Fn>::Call;
    }
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnUpdateSubresource) {
    using Fn = decltype(ctx_funcs->pfnUpdateSubresource);
    if constexpr (std::is_convertible_v<decltype(&UpdateSubresourceUP11), Fn>) {
      ctx_funcs->pfnUpdateSubresource = &UpdateSubresourceUP11;
    } else {
      ctx_funcs->pfnUpdateSubresource = &DdiStub<Fn>::Call;
    }
  }

  if constexpr (has_member_pfnStagingResourceMap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnStagingResourceMap), decltype(&StagingResourceMap11)>) {
      ctx_funcs->pfnStagingResourceMap = &StagingResourceMap11;
    } else {
      ctx_funcs->pfnStagingResourceMap = &StagingResourceMap11Void;
    }
  }
  if constexpr (has_member_pfnStagingResourceUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnStagingResourceUnmap = &StagingResourceUnmap11;
  }

  if constexpr (has_member_pfnDynamicIABufferMapDiscard<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicIABufferMapDiscard), decltype(&DynamicIABufferMapDiscard11)>) {
      ctx_funcs->pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard11;
    } else {
      ctx_funcs->pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard11Void;
    }
  }
  if constexpr (has_member_pfnDynamicIABufferMapNoOverwrite<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicIABufferMapNoOverwrite), decltype(&DynamicIABufferMapNoOverwrite11)>) {
      ctx_funcs->pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite11;
    } else {
      ctx_funcs->pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite11Void;
    }
  }
  if constexpr (has_member_pfnDynamicIABufferUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnDynamicIABufferUnmap = &DynamicIABufferUnmap11;
  }

  if constexpr (has_member_pfnDynamicConstantBufferMapDiscard<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    if constexpr (std::is_same_v<decltype(ctx_funcs->pfnDynamicConstantBufferMapDiscard), decltype(&DynamicConstantBufferMapDiscard11)>) {
      ctx_funcs->pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard11;
    } else {
      ctx_funcs->pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard11Void;
    }
  }
  if constexpr (has_member_pfnDynamicConstantBufferUnmap<D3D11DDI_DEVICECONTEXTFUNCS>::value) {
    ctx_funcs->pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmap11;
  }

  ctx_funcs->pfnFlush = &Flush11;
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
  funcs->pfnGetCaps = &GetCaps11;
  funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize11;
  if constexpr (has_member_pfnCalcPrivateDeviceContextSize<D3D11DDI_ADAPTERFUNCS>::value) {
    funcs->pfnCalcPrivateDeviceContextSize = &CalcPrivateDeviceContextSize11;
  }
  funcs->pfnCreateDevice = &CreateDevice11;
  funcs->pfnCloseAdapter = &CloseAdapter11;
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
  return OpenAdapter11Impl(pOpenData);
}

} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
