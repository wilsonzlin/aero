// AeroGPU Windows 7 D3D11 UMD (WDK build).
//
// This translation unit is compiled only when the official Win7 D3D11 DDI headers
// (`d3d10umddi.h` / `d3d11umddi.h`) are available.
//
// Goal: provide a crash-free FL10_0-capable D3D11DDI surface that translates the
// Win7 runtime's DDIs into the shared AeroGPU command stream.

#include "../include/aerogpu_d3d10_11_umd.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <d3d11.h>
#include <d3dkmthk.h>

#include <algorithm>
#include <cmath>
#include <cstring>
#include <mutex>
#include <new>
#include <type_traits>
#include <utility>
#include <vector>

#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_11_log.h"

namespace {

using namespace aerogpu::d3d10_11;

constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
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

static bool QueryUmdPrivateFromPrimaryDisplay(aerogpu_umd_private_v1* out) {
  if (!out) {
    return false;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_open_adapter_from_hdc || !procs.pfn_close_adapter || !procs.pfn_query_adapter_info) {
    return false;
  }

  wchar_t displayName[CCHDEVICENAME] = {};
  if (!GetPrimaryDisplayName(displayName)) {
    return false;
  }

  HDC hdc = CreateDCW(L"DISPLAY", displayName, nullptr, nullptr);
  if (!hdc) {
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC open{};
  open.hDc = hdc;
  open.hAdapter = 0;
  std::memset(&open.AdapterLuid, 0, sizeof(open.AdapterLuid));
  open.VidPnSourceId = 0;

  const NTSTATUS st = procs.pfn_open_adapter_from_hdc(&open);
  DeleteDC(hdc);
  if (!NtSuccess(st) || !open.hAdapter) {
    return false;
  }

  bool found = false;

  aerogpu_umd_private_v1 blob;
  std::memset(&blob, 0, sizeof(blob));

  D3DKMT_QUERYADAPTERINFO q{};
  q.hAdapter = open.hAdapter;
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

    if (blob.size_bytes != sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    *out = blob;
    found = true;
    break;
  }

  D3DKMT_CLOSEADAPTER close{};
  close.hAdapter = open.hAdapter;
  (void)procs.pfn_close_adapter(&close);

  return found;
}

static void InitUmdPrivate(Adapter* adapter) {
  if (!adapter || adapter->umd_private_valid) {
    return;
  }

  aerogpu_umd_private_v1 blob{};
  if (!QueryUmdPrivateFromPrimaryDisplay(&blob)) {
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
    return cd->pDeviceCallbacks;
  }
  if constexpr (has_member_pUMCallbacks<D3D11DDIARG_CREATEDEVICE>::value) {
    return cd->pUMCallbacks;
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

static void SetError(Device* dev, HRESULT hr) {
  if (!dev) {
    return;
  }
  auto* callbacks = reinterpret_cast<const D3D11DDI_DEVICECALLBACKS*>(dev->runtime_callbacks);
  if (callbacks && callbacks->pfnSetErrorCb && dev->runtime_device) {
    callbacks->pfnSetErrorCb(MakeRtDeviceHandle(dev), hr);
  }
}

static Device* DeviceFromContext(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuDeviceContext>(hCtx);
  return ctx ? ctx->dev : nullptr;
}

static void EmitBindShadersLocked(Device* dev) {
  if (!dev) {
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
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
  const size_t off = static_cast<size_t>(offset_bytes);
  const size_t sz = static_cast<size_t>(size_bytes);
  if (off > res->storage.size() || sz > res->storage.size() - off) {
    return;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
      AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + off, sz);
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->offset_bytes = offset_bytes;
  cmd->size_bytes = size_bytes;
}

template <typename TFnPtr>
struct DdiStub;

template <typename Ret, typename... Args>
struct DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args...) {
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
      // Size queries must not return 0 to avoid runtimes treating the object as
      // unsupported and then dereferencing null private memory.
      return sizeof(void*);
    } else if constexpr (std::is_same_v<Ret, void>) {
      return;
    } else {
      return Ret{};
    }
  }
};

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
static void StubPresentAndRotate(TFuncs* funcs) {
  if (!funcs) {
    return;
  }

  if constexpr (HasPresent<TFuncs>::value) {
    funcs->pfnPresent = &DdiStub<decltype(funcs->pfnPresent)>::Call;
  }
  if constexpr (HasRotateResourceIdentities<TFuncs>::value) {
    funcs->pfnRotateResourceIdentities = &DdiStub<decltype(funcs->pfnRotateResourceIdentities)>::Call;
  }
}

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

static D3D11DDI_DEVICEFUNCS MakeStubDeviceFuncs11() {
  D3D11DDI_DEVICEFUNCS funcs = {};

#define STUB_FIELD(field) funcs.field = &DdiStub<decltype(funcs.field)>::Call
  STUB_FIELD(pfnDestroyDevice);

  STUB_FIELD(pfnCalcPrivateResourceSize);
  STUB_FIELD(pfnCreateResource);
  STUB_FIELD(pfnDestroyResource);

  STUB_FIELD(pfnOpenResource);

  STUB_FIELD(pfnCalcPrivateShaderResourceViewSize);
  STUB_FIELD(pfnCreateShaderResourceView);
  STUB_FIELD(pfnDestroyShaderResourceView);

  STUB_FIELD(pfnCalcPrivateRenderTargetViewSize);
  STUB_FIELD(pfnCreateRenderTargetView);
  STUB_FIELD(pfnDestroyRenderTargetView);

  STUB_FIELD(pfnCalcPrivateDepthStencilViewSize);
  STUB_FIELD(pfnCreateDepthStencilView);
  STUB_FIELD(pfnDestroyDepthStencilView);

  STUB_FIELD(pfnCalcPrivateUnorderedAccessViewSize);
  STUB_FIELD(pfnCreateUnorderedAccessView);
  STUB_FIELD(pfnDestroyUnorderedAccessView);

  STUB_FIELD(pfnCalcPrivateVertexShaderSize);
  STUB_FIELD(pfnCreateVertexShader);
  STUB_FIELD(pfnDestroyVertexShader);

  STUB_FIELD(pfnCalcPrivatePixelShaderSize);
  STUB_FIELD(pfnCreatePixelShader);
  STUB_FIELD(pfnDestroyPixelShader);

  STUB_FIELD(pfnCalcPrivateGeometryShaderSize);
  STUB_FIELD(pfnCreateGeometryShader);
  STUB_FIELD(pfnDestroyGeometryShader);

  STUB_FIELD(pfnCalcPrivateGeometryShaderWithStreamOutputSize);
  STUB_FIELD(pfnCreateGeometryShaderWithStreamOutput);

  STUB_FIELD(pfnCalcPrivateHullShaderSize);
  STUB_FIELD(pfnCreateHullShader);
  STUB_FIELD(pfnDestroyHullShader);

  STUB_FIELD(pfnCalcPrivateDomainShaderSize);
  STUB_FIELD(pfnCreateDomainShader);
  STUB_FIELD(pfnDestroyDomainShader);

  STUB_FIELD(pfnCalcPrivateComputeShaderSize);
  STUB_FIELD(pfnCreateComputeShader);
  STUB_FIELD(pfnDestroyComputeShader);

  STUB_FIELD(pfnCalcPrivateElementLayoutSize);
  STUB_FIELD(pfnCreateElementLayout);
  STUB_FIELD(pfnDestroyElementLayout);

  STUB_FIELD(pfnCalcPrivateSamplerSize);
  STUB_FIELD(pfnCreateSampler);
  STUB_FIELD(pfnDestroySampler);

  STUB_FIELD(pfnCalcPrivateBlendStateSize);
  STUB_FIELD(pfnCreateBlendState);
  STUB_FIELD(pfnDestroyBlendState);

  STUB_FIELD(pfnCalcPrivateRasterizerStateSize);
  STUB_FIELD(pfnCreateRasterizerState);
  STUB_FIELD(pfnDestroyRasterizerState);

  STUB_FIELD(pfnCalcPrivateDepthStencilStateSize);
  STUB_FIELD(pfnCreateDepthStencilState);
  STUB_FIELD(pfnDestroyDepthStencilState);

  STUB_FIELD(pfnCalcPrivateQuerySize);
  STUB_FIELD(pfnCreateQuery);
  STUB_FIELD(pfnDestroyQuery);

  STUB_FIELD(pfnCalcPrivatePredicateSize);
  STUB_FIELD(pfnCreatePredicate);
  STUB_FIELD(pfnDestroyPredicate);

  STUB_FIELD(pfnCalcPrivateCounterSize);
  STUB_FIELD(pfnCreateCounter);
  STUB_FIELD(pfnDestroyCounter);

  STUB_FIELD(pfnCalcPrivateDeferredContextSize);
  STUB_FIELD(pfnCreateDeferredContext);
  STUB_FIELD(pfnDestroyDeferredContext);

  STUB_FIELD(pfnCalcPrivateCommandListSize);
  STUB_FIELD(pfnCreateCommandList);
  STUB_FIELD(pfnDestroyCommandList);

  STUB_FIELD(pfnCalcPrivateClassLinkageSize);
  STUB_FIELD(pfnCreateClassLinkage);
  STUB_FIELD(pfnDestroyClassLinkage);

  STUB_FIELD(pfnCalcPrivateClassInstanceSize);
  STUB_FIELD(pfnCreateClassInstance);
  STUB_FIELD(pfnDestroyClassInstance);

  // Optional device-level queries present in some D3D11 DDI revisions. Always
  // keep them non-null when the field exists to avoid runtime NULL dereferences.
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCheckCounterInfo) {
    STUB_FIELD(pfnCheckCounterInfo);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCheckCounter) {
    STUB_FIELD(pfnCheckCounter);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnGetDeviceRemovedReason) {
    STUB_FIELD(pfnGetDeviceRemovedReason);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnGetExceptionMode) {
    STUB_FIELD(pfnGetExceptionMode);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnSetExceptionMode) {
    STUB_FIELD(pfnSetExceptionMode);
  }
  __if_exists(D3D11DDI_DEVICEFUNCS::pfnCheckDeferredContextHandleSizes) {
    STUB_FIELD(pfnCheckDeferredContextHandleSizes);
  }
#undef STUB_FIELD

  StubPresentAndRotate(&funcs);
  return funcs;
}

static D3D11DDI_DEVICECONTEXTFUNCS MakeStubContextFuncs11() {
  D3D11DDI_DEVICECONTEXTFUNCS funcs = {};

#define STUB_FIELD(field) funcs.field = &DdiStub<decltype(funcs.field)>::Call
  STUB_FIELD(pfnIaSetInputLayout);
  STUB_FIELD(pfnIaSetVertexBuffers);
  STUB_FIELD(pfnIaSetIndexBuffer);
  STUB_FIELD(pfnIaSetTopology);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSoSetTargets) {
    STUB_FIELD(pfnSoSetTargets);
  }

  STUB_FIELD(pfnVsSetShader);
  STUB_FIELD(pfnVsSetConstantBuffers);
  STUB_FIELD(pfnVsSetShaderResources);
  STUB_FIELD(pfnVsSetSamplers);

  STUB_FIELD(pfnPsSetShader);
  STUB_FIELD(pfnPsSetConstantBuffers);
  STUB_FIELD(pfnPsSetShaderResources);
  STUB_FIELD(pfnPsSetSamplers);

  STUB_FIELD(pfnGsSetShader);
  STUB_FIELD(pfnGsSetConstantBuffers);
  STUB_FIELD(pfnGsSetShaderResources);
  STUB_FIELD(pfnGsSetSamplers);

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShader) {
    STUB_FIELD(pfnHsSetShader);
    STUB_FIELD(pfnHsSetConstantBuffers);
    STUB_FIELD(pfnHsSetShaderResources);
    STUB_FIELD(pfnHsSetSamplers);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShader) {
    STUB_FIELD(pfnDsSetShader);
    STUB_FIELD(pfnDsSetConstantBuffers);
    STUB_FIELD(pfnDsSetShaderResources);
    STUB_FIELD(pfnDsSetSamplers);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShader) {
    STUB_FIELD(pfnCsSetShader);
    STUB_FIELD(pfnCsSetConstantBuffers);
    STUB_FIELD(pfnCsSetShaderResources);
    STUB_FIELD(pfnCsSetSamplers);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetUnorderedAccessViews) {
    STUB_FIELD(pfnCsSetUnorderedAccessViews);
  }

  STUB_FIELD(pfnSetViewports);
  STUB_FIELD(pfnSetScissorRects);
  STUB_FIELD(pfnSetRasterizerState);
  STUB_FIELD(pfnSetBlendState);
  STUB_FIELD(pfnSetDepthStencilState);
  STUB_FIELD(pfnSetRenderTargets);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews) {
    STUB_FIELD(pfnSetRenderTargetsAndUnorderedAccessViews);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews11_1) {
    STUB_FIELD(pfnSetRenderTargetsAndUnorderedAccessViews11_1);
  }

  STUB_FIELD(pfnClearState);
  STUB_FIELD(pfnClearRenderTargetView);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnClearUnorderedAccessViewUint) {
    STUB_FIELD(pfnClearUnorderedAccessViewUint);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnClearUnorderedAccessViewFloat) {
    STUB_FIELD(pfnClearUnorderedAccessViewFloat);
  }
  STUB_FIELD(pfnClearDepthStencilView);

  STUB_FIELD(pfnDraw);
  STUB_FIELD(pfnDrawIndexed);
  STUB_FIELD(pfnDrawInstanced);
  STUB_FIELD(pfnDrawIndexedInstanced);
  STUB_FIELD(pfnDrawAuto);
  STUB_FIELD(pfnDrawInstancedIndirect);
  STUB_FIELD(pfnDrawIndexedInstancedIndirect);

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDispatch) {
    STUB_FIELD(pfnDispatch);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDispatchIndirect) {
    STUB_FIELD(pfnDispatchIndirect);
  }

  STUB_FIELD(pfnUpdateSubresourceUP);
  STUB_FIELD(pfnCopyResource);
  STUB_FIELD(pfnCopySubresourceRegion);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCopyStructureCount) {
    STUB_FIELD(pfnCopyStructureCount);
  }
  STUB_FIELD(pfnResolveSubresource);
  STUB_FIELD(pfnGenerateMips);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetResourceMinLOD) {
    STUB_FIELD(pfnSetResourceMinLOD);
  }

  STUB_FIELD(pfnBegin);
  STUB_FIELD(pfnEnd);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnQueryGetData) {
    STUB_FIELD(pfnQueryGetData);
  }
  STUB_FIELD(pfnSetPredication);
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnExecuteCommandList) {
    STUB_FIELD(pfnExecuteCommandList);
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnFinishCommandList) {
    STUB_FIELD(pfnFinishCommandList);
  }

  STUB_FIELD(pfnMap);
  STUB_FIELD(pfnUnmap);
  STUB_FIELD(pfnFlush);
#undef STUB_FIELD

  StubPresentAndRotate(&funcs);
  return funcs;
}

static void UnmapLocked(Device* dev, Resource* res) {
  if (!dev || !res || !res->mapped) {
    return;
  }

  const bool is_write = (res->mapped_map_type != D3D11_MAP_READ);
  if (is_write && !res->storage.empty()) {
    EmitUploadLocked(dev, res, res->mapped_offset, res->mapped_size);
  }

  res->mapped = false;
  res->mapped_map_type = 0;
  res->mapped_map_flags = 0;
  res->mapped_offset = 0;
  res->mapped_size = 0;
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

HRESULT AEROGPU_APIENTRY GetCaps11(D3D10DDI_HADAPTER, const D3D11DDIARG_GETCAPS* pGetCaps) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!pGetCaps || !pGetCaps->pData || pGetCaps->DataSize == 0) {
    return E_INVALIDARG;
  }

  void* data = pGetCaps->pData;
  const UINT size = pGetCaps->DataSize;

  auto zero_out = [&] { std::memset(data, 0, size); };

  switch (pGetCaps->Type) {
    case D3D11DDICAPS_TYPE_FEATURE_LEVELS: {
      zero_out();
      static const D3D_FEATURE_LEVEL kLevels[] = {D3D_FEATURE_LEVEL_10_0};

      // Win7 D3D11 runtime expects a "count + inline list" in practice, but be
      // permissive to alternate layouts.
      if (size >= sizeof(UINT) + sizeof(D3D_FEATURE_LEVEL)) {
        auto* out_count = reinterpret_cast<UINT*>(data);
        *out_count = 1;
        auto* out_levels = reinterpret_cast<D3D_FEATURE_LEVEL*>(out_count + 1);
        out_levels[0] = kLevels[0];
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
        case kDxgiFormatB8G8R8X8Unorm:
        case kDxgiFormatR8G8B8A8Unorm:
          support = D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                    D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_BLENDABLE |
                    D3D11_FORMAT_SUPPORT_CPU_LOCKABLE | D3D11_FORMAT_SUPPORT_DISPLAY;
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
      out->NumQualityLevels = (in.SampleCount == 1) ? 1u : 0u;
      return S_OK;
    }

    default:
      // Unknown caps are treated as unsupported. Zero-fill so the runtime won't
      // read garbage, but log the type for bring-up.
      AEROGPU_D3D10_11_LOG("GetCaps11 unknown type=%u (size=%u) -> zero-fill + S_OK",
                           (unsigned)static_cast<uint32_t>(pGetCaps->Type),
                           (unsigned)size);
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
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Device DDIs (object creation)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice11(D3D11DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  dev->~Device();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERESOURCE*) {
  return sizeof(Resource);
}

HRESULT AEROGPU_APIENTRY CreateResource11(D3D11DDI_HDEVICE hDevice,
                                          const D3D11DDIARG_CREATERESOURCE* pDesc,
                                          D3D11DDI_HRESOURCE hResource,
                                          D3D11DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = new (hResource.pDrvPrivate) Resource();
  res->handle = AllocateGlobalHandle(dev->adapter);
  res->bind_flags = static_cast<uint32_t>(pDesc->BindFlags);
  res->misc_flags = static_cast<uint32_t>(pDesc->MiscFlags);
  res->usage = static_cast<uint32_t>(pDesc->Usage);
  res->cpu_access_flags = static_cast<uint32_t>(pDesc->CPUAccessFlags);

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);

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
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(src);
    const uint32_t pitch = src_pitch ? src_pitch : res->row_pitch_bytes;
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src_bytes + static_cast<size_t>(y) * pitch,
                  res->row_pitch_bytes);
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
    try {
      res->storage.resize(static_cast<size_t>(res->size_bytes));
    } catch (...) {
      res->~Resource();
      return E_OUTOFMEMORY;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialData);
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

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~Resource();
      return E_NOTIMPL;
    }

    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);
    const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    try {
      res->storage.resize(static_cast<size_t>(total_bytes));
    } catch (...) {
      res->~Resource();
      return E_OUTOFMEMORY;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = 1;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if constexpr (has_member_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (has_member_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      maybe_copy_initial(pDesc->pInitialData);
    }

    return S_OK;
  }

  res->~Resource();
  return E_NOTIMPL;
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
  if (res->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
  }
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

  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pDesc->hResource) : nullptr;
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
  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pDesc->hResource) : nullptr;
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
  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pDesc->hResource) : nullptr;
  auto* srv = new (hView.pDrvPrivate) ShaderResourceView();
  srv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView11(D3D11DDI_HDEVICE, D3D11DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(hView)->~ShaderResourceView();
}

struct Sampler {
  uint32_t dummy = 0;
};

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESAMPLER*) {
  return sizeof(Sampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_CREATESAMPLER*,
                                         D3D11DDI_HSAMPLER hSampler,
                                         D3D11DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hSampler.pDrvPrivate) Sampler();
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler11(D3D11DDI_HDEVICE, D3D11DDI_HSAMPLER hSampler) {
  if (!hSampler.pDrvPrivate) {
    return;
  }
  FromHandle<D3D11DDI_HSAMPLER, Sampler>(hSampler)->~Sampler();
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

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, out->dxbc.data(), out->dxbc.size());
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
    cmd->shader_handle = sh->handle;
    cmd->reserved0 = 0;
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
  if (!pDesc || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, Device>(hDevice);
  if (!dev) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) Shader();
  const HRESULT hr = CreateShaderCommon(
      hDevice, pDesc->pShaderCode, pDesc->ShaderCodeSize, sh, AEROGPU_SHADER_STAGE_VERTEX /* placeholder */);
  if (FAILED(hr)) {
    return hr;
  }
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
                                            const D3D11DDIARG_CREATEBLENDSTATE*,
                                            D3D11DDI_HBLENDSTATE hState,
                                            D3D11DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) BlendState();
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
                                                 const D3D11DDIARG_CREATERASTERIZERSTATE*,
                                                 D3D11DDI_HRASTERIZERSTATE hState,
                                                 D3D11DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) RasterizerState();
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
                                                   const D3D11DDIARG_CREATEDEPTHSTENCILSTATE*,
                                                   D3D11DDI_HDEPTHSTENCILSTATE hState,
                                                   D3D11DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) DepthStencilState();
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
  dev->current_input_layout = hLayout.pDrvPrivate ? FromHandle<D3D11DDI_HELEMENTLAYOUT, InputLayout>(hLayout)->handle : 0;

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
  dev->current_vs = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HVERTEXSHADER, Shader>(hShader)->handle : 0;
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
  // Geometry stage not yet translated into the command stream.
}

void AEROGPU_APIENTRY VsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HRESOURCE*, const UINT*, const UINT*) {}
void AEROGPU_APIENTRY PsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HRESOURCE*, const UINT*, const UINT*) {}
void AEROGPU_APIENTRY GsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HRESOURCE*, const UINT*, const UINT*) {}

void AEROGPU_APIENTRY VsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !phViews || NumViews == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (UINT i = 0; i < NumViews; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(phViews[i])->texture;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
    cmd->slot = StartSlot + i;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
}

void AEROGPU_APIENTRY PsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !phViews || NumViews == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (UINT i = 0; i < NumViews; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, ShaderResourceView>(phViews[i])->texture;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->slot = StartSlot + i;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
}

void AEROGPU_APIENTRY GsSetShaderResources11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HSHADERRESOURCEVIEW*) {}

void AEROGPU_APIENTRY VsSetSamplers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HSAMPLER*) {}
void AEROGPU_APIENTRY PsSetSamplers11(D3D11DDI_HDEVICECONTEXT, UINT, UINT, const D3D11DDI_HSAMPLER*) {}
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
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  cmd->x = r.left;
  cmd->y = r.top;
  cmd->width = r.right - r.left;
  cmd->height = r.bottom - r.top;
}

void AEROGPU_APIENTRY SetRasterizerState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HRASTERIZERSTATE) {}
void AEROGPU_APIENTRY SetBlendState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HBLENDSTATE, const FLOAT[4], UINT) {}
void AEROGPU_APIENTRY SetDepthStencilState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HDEPTHSTENCILSTATE, UINT) {}

void AEROGPU_APIENTRY ClearState11(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_rtv = 0;
  dev->current_rtv_resource = nullptr;
  dev->current_dsv = 0;
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_gs = 0;
  dev->current_input_layout = 0;
  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  dev->current_vb = nullptr;
  dev->current_vb_stride_bytes = 0;
  dev->current_vb_offset_bytes = 0;
  dev->viewport_x = 0.0f;
  dev->viewport_y = 0.0f;
  dev->viewport_width = 0.0f;
  dev->viewport_height = 0.0f;
  dev->viewport_min_depth = 0.0f;
  dev->viewport_max_depth = 1.0f;

  auto* rt_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  rt_cmd->color_count = 0;
  rt_cmd->depth_stencil = 0;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    rt_cmd->colors[i] = 0;
  }

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
    dev->current_rtv = rtv ? rtv->texture : 0;
    dev->current_rtv_resource = rtv ? rtv->resource : nullptr;
  }
  dev->current_dsv = hDsv.pDrvPrivate ? FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, DepthStencilView>(hDsv)->texture : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = dev->current_rtv ? 1u : 0u;
  cmd->depth_stencil = dev->current_dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  if (dev->current_rtv) {
    cmd->colors[0] = dev->current_rtv;
  }
}

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
    case kDxgiFormatB8G8R8X8Unorm:
      px[0] = b;
      px[1] = g;
      px[2] = r;
      px[3] = a;
      break;
    case kDxgiFormatR8G8B8A8Unorm:
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

static float EdgeFn(float ax, float ay, float bx, float by, float px, float py) {
  return (px - ax) * (by - ay) - (py - ay) * (bx - ax);
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
  if (!(rt->dxgi_format == kDxgiFormatB8G8R8A8Unorm || rt->dxgi_format == kDxgiFormatB8G8R8X8Unorm ||
        rt->dxgi_format == kDxgiFormatR8G8B8A8Unorm)) {
    return;
  }
  if (dev->current_topology != AEROGPU_TOPOLOGY_TRIANGLELIST) {
    return;
  }
  if (vertex_count < 3) {
    return;
  }

  // Expect the Win7 test vertex format:
  //   float2 POSITION @ byte 0
  //   float4 COLOR    @ byte 8
  const uint32_t stride = dev->current_vb_stride_bytes;
  const uint32_t base_off = dev->current_vb_offset_bytes;
  if (stride < 24) {
    return;
  }

  const float vp_x = dev->viewport_width > 0.0f ? dev->viewport_x : 0.0f;
  const float vp_y = dev->viewport_height > 0.0f ? dev->viewport_y : 0.0f;
  const float vp_w = dev->viewport_width > 0.0f ? dev->viewport_width : static_cast<float>(rt->width);
  const float vp_h = dev->viewport_height > 0.0f ? dev->viewport_height : static_cast<float>(rt->height);
  if (vp_w <= 0.0f || vp_h <= 0.0f) {
    return;
  }

  struct Vtx {
    float x;
    float y;
    float c[4];
  };

  auto read_vtx = [&](uint32_t idx) -> Vtx {
    Vtx out{};
    const uint64_t byte_off = static_cast<uint64_t>(base_off) + static_cast<uint64_t>(idx) * stride;
    if (byte_off + 24 > vb->storage.size()) {
      return out;
    }
    const uint8_t* p = vb->storage.data() + static_cast<size_t>(byte_off);
    std::memcpy(&out.x, p + 0, sizeof(float));
    std::memcpy(&out.y, p + 4, sizeof(float));
    std::memcpy(&out.c[0], p + 8, sizeof(float) * 4);
    return out;
  };

  // We only need enough for the tests; handle the first triangle.
  const Vtx v0 = read_vtx(first_vertex + 0);
  const Vtx v1 = read_vtx(first_vertex + 1);
  const Vtx v2 = read_vtx(first_vertex + 2);

  const auto to_screen = [&](const Vtx& v, float* out_x, float* out_y) {
    // Input positions are already in NDC (via a pass-through VS in the tests).
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

  const float inv_area = 1.0f / area;

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

      float out_rgba[4] = {};
      for (int i = 0; i < 4; i++) {
        out_rgba[i] = b0 * v0.c[i] + b1 * v1.c[i] + b2 * v2.c[i];
      }

      uint8_t r = U8FromFloat01(out_rgba[0]);
      uint8_t g = U8FromFloat01(out_rgba[1]);
      uint8_t b = U8FromFloat01(out_rgba[2]);
      uint8_t a = U8FromFloat01(out_rgba[3]);

      uint8_t* dst = row + static_cast<size_t>(x) * 4;
      switch (rt->dxgi_format) {
        case kDxgiFormatB8G8R8A8Unorm:
        case kDxgiFormatB8G8R8X8Unorm:
          dst[0] = b;
          dst[1] = g;
          dst[2] = r;
          dst[3] = a;
          break;
        case kDxgiFormatR8G8B8A8Unorm:
          dst[0] = r;
          dst[1] = g;
          dst[2] = b;
          dst[3] = a;
          break;
        default:
          break;
      }
    }
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
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(rgba[0]);
  cmd->color_rgba_f32[1] = f32_bits(rgba[1]);
  cmd->color_rgba_f32[2] = f32_bits(rgba[2]);
  cmd->color_rgba_f32[3] = f32_bits(rgba[3]);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;
}

void AEROGPU_APIENTRY ClearDepthStencilView11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HDEPTHSTENCILVIEW, UINT flags, FLOAT depth, UINT8 stencil) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
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
  SoftwareDrawTriangleList(dev, VertexCount, StartVertexLocation);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = VertexCount;
  cmd->instance_count = 1;
  cmd->first_vertex = StartVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexed11(D3D11DDI_HDEVICECONTEXT hCtx, UINT IndexCount, UINT StartIndexLocation, INT BaseVertexLocation) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = IndexCount;
  cmd->instance_count = 1;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY CopyResource11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hDstResource, D3D11DDI_HRESOURCE hSrcResource) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }

  auto* dst = hDstResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hDstResource) : nullptr;
  auto* src = hSrcResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(hSrcResource) : nullptr;
  if (!dst || !src) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dst->kind == ResourceKind::Buffer && src->kind == ResourceKind::Buffer) {
    const uint64_t bytes = std::min(dst->size_bytes, src->size_bytes);
    if (bytes && dst->storage.size() >= bytes && src->storage.size() >= bytes) {
      std::memcpy(dst->storage.data(), src->storage.data(), static_cast<size_t>(bytes));
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    cmd->size_bytes = bytes;
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
    return;
  }

  if (dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
    if (dst->storage.size() == src->storage.size()) {
      std::memcpy(dst->storage.data(), src->storage.data(), dst->storage.size());
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
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
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
    return;
  }
}

void AEROGPU_APIENTRY CopySubresourceRegion11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             D3D11DDI_HRESOURCE hDstResource,
                                             UINT,
                                             UINT dst_x,
                                             UINT dst_y,
                                             UINT,
                                             D3D11DDI_HRESOURCE hSrcResource,
                                             UINT,
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
      std::memcpy(dst->storage.data() + static_cast<size_t>(dst_off),
                  src->storage.data() + static_cast<size_t>(src_left),
                  static_cast<size_t>(bytes));
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = dst_off;
    cmd->src_offset_bytes = src_left;
    cmd->size_bytes = bytes;
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
    return;
  }

  if (dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
    const uint32_t src_left = pSrcBox ? static_cast<uint32_t>(pSrcBox->left) : 0;
    const uint32_t src_top = pSrcBox ? static_cast<uint32_t>(pSrcBox->top) : 0;
    const uint32_t src_right = pSrcBox ? static_cast<uint32_t>(pSrcBox->right) : src->width;
    const uint32_t src_bottom = pSrcBox ? static_cast<uint32_t>(pSrcBox->bottom) : src->height;

    if (src_right < src_left || src_bottom < src_top) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width = std::min(src_right - src_left, dst->width > dst_x ? (dst->width - dst_x) : 0u);
    const uint32_t copy_height = std::min(src_bottom - src_top, dst->height > dst_y ? (dst->height - dst_y) : 0u);

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(dst->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    const size_t row_bytes = static_cast<size_t>(copy_width) * bpp;

    if (row_bytes && dst->row_pitch_bytes >= row_bytes && src->row_pitch_bytes >= row_bytes &&
        dst_y + copy_height <= dst->height && src_top + copy_height <= src->height) {
      for (uint32_t y = 0; y < copy_height; y++) {
        const size_t dst_off =
            static_cast<size_t>(dst_y + y) * dst->row_pitch_bytes + static_cast<size_t>(dst_x) * bpp;
        const size_t src_off =
            static_cast<size_t>(src_top + y) * src->row_pitch_bytes + static_cast<size_t>(src_left) * bpp;
        if (dst_off + row_bytes <= dst->storage.size() && src_off + row_bytes <= src->storage.size()) {
          std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
        }
      }
    }

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
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
    return;
  }

  SetError(dev, E_NOTIMPL);
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

  if (res->mapped) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  const uint32_t map_u32 = static_cast<uint32_t>(map_type);
  const bool want_read = (map_u32 == D3D11_MAP_READ || map_u32 == D3D11_MAP_READ_WRITE);
  const bool want_write = (map_u32 != D3D11_MAP_READ);

  if (want_read && !(res->cpu_access_flags & kD3D11CpuAccessRead)) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }
  if (want_write && !(res->cpu_access_flags & kD3D11CpuAccessWrite) && res->usage != kD3D11UsageDynamic &&
      res->usage != kD3D11UsageStaging) {
    SetError(dev, E_INVALIDARG);
    return E_INVALIDARG;
  }

  if (map_u32 == D3D11_MAP_WRITE_DISCARD && res->kind == ResourceKind::Buffer) {
    // Approximate DISCARD renaming by allocating a fresh CPU backing store.
    try {
      res->storage.assign(res->storage.size(), 0);
    } catch (...) {
      SetError(dev, E_OUTOFMEMORY);
      return E_OUTOFMEMORY;
    }
  }

  res->mapped = true;
  res->mapped_map_type = map_u32;
  res->mapped_map_flags = map_flags;
  res->mapped_offset = 0;
  res->mapped_size = res->storage.size();

  pMapped->pData = res->storage.empty() ? nullptr : res->storage.data();
  if (res->kind == ResourceKind::Texture2D) {
    pMapped->RowPitch = res->row_pitch_bytes;
    pMapped->DepthPitch = res->row_pitch_bytes * res->height;
  } else {
    pMapped->RowPitch = static_cast<UINT>(res->storage.size());
    pMapped->DepthPitch = static_cast<UINT>(res->storage.size());
  }
  return S_OK;
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
  (void)MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY Unmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, UINT) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate) {
    return;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  UnmapLocked(dev, res);
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
  if (res->mapped) {
    SetError(dev, E_FAIL);
    return E_FAIL;
  }

  if (map_u32 == D3D11_MAP_WRITE_DISCARD) {
    try {
      res->storage.assign(res->storage.size(), 0);
    } catch (...) {
      SetError(dev, E_OUTOFMEMORY);
      return E_OUTOFMEMORY;
    }
  }

  res->mapped = true;
  res->mapped_map_type = map_u32;
  res->mapped_map_flags = 0;
  res->mapped_offset = 0;
  res->mapped_size = res->storage.size();
  *ppData = res->storage.empty() ? nullptr : res->storage.data();
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
  (void)StagingResourceMap11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY StagingResourceUnmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, UINT) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate) {
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  UnmapLocked(dev, res);
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
  if (!dev || !hResource.pDrvPrivate) {
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  UnmapLocked(dev, res);
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
  if (!dev || !hResource.pDrvPrivate) {
    return;
  }
  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  UnmapLocked(dev, res);
}

void AEROGPU_APIENTRY UpdateSubresourceUP11(D3D11DDI_HDEVICECONTEXT hCtx,
                                            D3D11DDI_HRESOURCE hDstResource,
                                            UINT,
                                            const D3D10_DDI_BOX* pDstBox,
                                            const void* pSysMem,
                                            UINT src_pitch,
                                            UINT) {
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
    if (bytes) {
      std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pSysMem, static_cast<size_t>(bytes));
      EmitUploadLocked(dev, res, dst_off, bytes);
    }
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(pSysMem);
    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    if (bpp == 0 || res->row_pitch_bytes < res->width * bpp) {
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

    const uint32_t copy_width = right - left;
    const uint32_t copy_height = bottom - top;
    const uint32_t row_bytes = copy_width * bpp;
    if (row_bytes == 0 || copy_height == 0) {
      return;
    }

    const uint32_t pitch = src_pitch ? src_pitch : row_bytes;
    if (pitch < row_bytes) {
      SetError(dev, E_INVALIDARG);
      return;
    }

    for (uint32_t y = 0; y < copy_height; y++) {
      const size_t dst_off = static_cast<size_t>(top + y) * res->row_pitch_bytes + static_cast<size_t>(left) * bpp;
      const size_t src_off = static_cast<size_t>(y) * pitch;
      if (dst_off + row_bytes > res->storage.size()) {
        SetError(dev, E_FAIL);
        return;
      }
      std::memcpy(res->storage.data() + dst_off, src_bytes + src_off, row_bytes);
    }

    // Texture updates are not guaranteed to be contiguous in memory (unless the
    // full subresource is updated). For the bring-up path, upload the whole
    // resource after applying the CPU-side update.
    EmitUploadLocked(dev, res, 0, res->storage.size());
    return;
  }

  SetError(dev, E_NOTIMPL);
}

void AEROGPU_APIENTRY Flush11(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  (void)flush_locked(dev);
}

HRESULT AEROGPU_APIENTRY Present11(D3D11DDI_HDEVICECONTEXT hCtx, const D3D10DDIARG_PRESENT* pPresent) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pPresent) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  cmd->scanout_id = 0;
  bool vsync = (pPresent->SyncInterval != 0);
  if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
    vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
  }
  cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
  submit_locked(dev);
  return S_OK;
}

void AEROGPU_APIENTRY RotateResourceIdentities11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE* pResources, UINT numResources) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !pResources || numResources < 2) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* first = pResources[0].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[0]) : nullptr;
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;
  for (UINT i = 0; i + 1 < numResources; i++) {
    auto* dst = pResources[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[i]) : nullptr;
    auto* src = pResources[i + 1].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[i + 1]) : nullptr;
    if (!dst || !src) {
      return;
    }
    dst->handle = src->handle;
  }
  auto* last = pResources[numResources - 1].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, Resource>(pResources[numResources - 1]) : nullptr;
  if (last) {
    last->handle = saved;
  }
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
  dev->runtime_callbacks = GetDeviceCallbacks(pCreateDevice);
  dev->runtime_device = GetRtDevicePrivate(pCreateDevice);

  auto* ctx = new (ctx_mem) AeroGpuDeviceContext();
  ctx->dev = dev;
  dev->immediate_context = ctx;

  // Win7 runtimes are known to call a surprisingly large chunk of the D3D11 DDI
  // surface (even for simple triangle samples). Start from fully-stubbed
  // defaults so we never leave NULL function pointers behind.
  *pCreateDevice->pDeviceFuncs = MakeStubDeviceFuncs11();
  *ctx_funcs = MakeStubContextFuncs11();

  // Device funcs.
  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice11;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize11;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource11;
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

  BindPresentAndRotate(pCreateDevice->pDeviceFuncs);

  // Immediate context funcs.
  ctx_funcs->pfnIaSetInputLayout = &IaSetInputLayout11;
  ctx_funcs->pfnIaSetVertexBuffers = &IaSetVertexBuffers11;
  ctx_funcs->pfnIaSetIndexBuffer = &IaSetIndexBuffer11;
  ctx_funcs->pfnIaSetTopology = &IaSetTopology11;

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

  ctx_funcs->pfnSetViewports = &SetViewports11;
  ctx_funcs->pfnSetScissorRects = &SetScissorRects11;
  ctx_funcs->pfnSetRasterizerState = &SetRasterizerState11;
  ctx_funcs->pfnSetBlendState = &SetBlendState11;
  ctx_funcs->pfnSetDepthStencilState = &SetDepthStencilState11;
  ctx_funcs->pfnSetRenderTargets = &SetRenderTargets11;

  ctx_funcs->pfnClearState = &ClearState11;
  ctx_funcs->pfnClearRenderTargetView = &ClearRenderTargetView11;
  ctx_funcs->pfnClearDepthStencilView = &ClearDepthStencilView11;
  ctx_funcs->pfnDraw = &Draw11;
  ctx_funcs->pfnDrawIndexed = &DrawIndexed11;

  ctx_funcs->pfnCopyResource = &CopyResource11;
  ctx_funcs->pfnCopySubresourceRegion = &CopySubresourceRegion11;

  // Map can be HRESULT or void depending on interface version.
  if constexpr (std::is_same_v<decltype(ctx_funcs->pfnMap), decltype(&Map11)>) {
    ctx_funcs->pfnMap = &Map11;
  } else {
    ctx_funcs->pfnMap = &Map11Void;
  }
  ctx_funcs->pfnUnmap = &Unmap11;
  ctx_funcs->pfnUpdateSubresourceUP = &UpdateSubresourceUP11;

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

  return S_OK;
}

// -------------------------------------------------------------------------------------------------
// OpenAdapter11 export
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapter11Impl(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

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

  constexpr UINT supported_version = D3D11DDI_INTERFACE_VERSION;
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
  adapter->runtime_callbacks = GetAdapterCallbacks(pOpenData);
  InitUmdPrivate(adapter);
  pOpenData->hAdapter.pDrvPrivate = adapter;

  auto* funcs = reinterpret_cast<D3D11DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  std::memset(funcs, 0, sizeof(*funcs));
  funcs->pfnGetCaps = &GetCaps11;
  funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize11;
  if constexpr (has_member_pfnCalcPrivateDeviceContextSize<D3D11DDI_ADAPTERFUNCS>::value) {
    funcs->pfnCalcPrivateDeviceContextSize = &CalcPrivateDeviceContextSize11;
  }
  funcs->pfnCreateDevice = &CreateDevice11;
  funcs->pfnCloseAdapter = &CloseAdapter11;
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapter11Impl(pOpenData);
}

} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
