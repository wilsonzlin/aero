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

#include <algorithm>
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
  if (callbacks && callbacks->pfnSetErrorCb) {
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
#undef STUB_FIELD

  return funcs;
}

static D3D11DDI_DEVICECONTEXTFUNCS MakeStubContextFuncs11() {
  D3D11DDI_DEVICECONTEXTFUNCS funcs = {};

#define STUB_FIELD(field) funcs.field = &DdiStub<decltype(funcs.field)>::Call
  STUB_FIELD(pfnIaSetInputLayout);
  STUB_FIELD(pfnIaSetVertexBuffers);
  STUB_FIELD(pfnIaSetIndexBuffer);
  STUB_FIELD(pfnIaSetTopology);

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

  STUB_FIELD(pfnClearState);
  STUB_FIELD(pfnClearRenderTargetView);
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
  STUB_FIELD(pfnResolveSubresource);
  STUB_FIELD(pfnGenerateMips);

  STUB_FIELD(pfnBegin);
  STUB_FIELD(pfnEnd);
  STUB_FIELD(pfnSetPredication);

  STUB_FIELD(pfnMap);
  STUB_FIELD(pfnUnmap);
  STUB_FIELD(pfnFlush);

  STUB_FIELD(pfnPresent);
  STUB_FIELD(pfnRotateResourceIdentities);
#undef STUB_FIELD

  return funcs;
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

      struct FeatureLevelsCapsPtr {
        UINT NumFeatureLevels;
        const D3D_FEATURE_LEVEL* pFeatureLevels;
      };

      // Common layout: {UINT NumFeatureLevels; const D3D_FEATURE_LEVEL* pFeatureLevels;}
      if (size == sizeof(FeatureLevelsCapsPtr)) {
        auto* out = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out->NumFeatureLevels = 1;
        out->pFeatureLevels = kLevels;
        return S_OK;
      }

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
  res->handle = dev->adapter->next_handle.fetch_add(1);
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

  out->handle = dev->adapter->next_handle.fetch_add(1);
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
  layout->handle = dev->adapter->next_handle.fetch_add(1);

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
  dev->current_dsv = 0;
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_gs = 0;
  dev->current_input_layout = 0;
  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

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
  if (NumViews && phRtvs && phRtvs[0].pDrvPrivate) {
    dev->current_rtv = FromHandle<D3D11DDI_HRENDERTARGETVIEW, RenderTargetView>(phRtvs[0])->texture;
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

void AEROGPU_APIENTRY ClearRenderTargetView11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRENDERTARGETVIEW, const FLOAT rgba[4]) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !rgba) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
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

static HRESULT MapCore11(D3D11DDI_HDEVICECONTEXT hCtx,
                         D3D11DDI_HRESOURCE hResource,
                         UINT,
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
  return MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY Map11Void(D3D11DDI_HDEVICECONTEXT hCtx,
                                D3D11DDI_HRESOURCE hResource,
                                UINT subresource,
                                D3D11_DDI_MAP map_type,
                                UINT map_flags,
                                D3D11DDI_MAPPED_SUBRESOURCE* pMapped) {
  (void)MapCore11(hCtx, hResource, subresource, map_type, map_flags, pMapped);
}

void AEROGPU_APIENTRY Unmap11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE hResource, UINT) {
  auto* dev = DeviceFromContext(hCtx);
  if (!dev || !hResource.pDrvPrivate) {
    return;
  }

  auto* res = FromHandle<D3D11DDI_HRESOURCE, Resource>(hResource);
  if (!res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped) {
    return;
  }

  const bool is_write = (res->mapped_map_type != D3D11_MAP_READ);
  if (is_write && !res->storage.empty()) {
    EmitUploadLocked(dev, res, res->mapped_offset, res->mapped_size);
  }

  res->mapped = false;
}

void AEROGPU_APIENTRY UpdateSubresourceUP11(D3D11DDI_HDEVICECONTEXT hCtx,
                                           D3D11DDI_HRESOURCE hDstResource,
                                           UINT,
                                           const D3D10_DDI_BOX*,
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
    const size_t bytes = std::min(res->storage.size(), static_cast<size_t>(res->size_bytes));
    std::memcpy(res->storage.data(), pSysMem, bytes);
    EmitUploadLocked(dev, res, 0, bytes);
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(pSysMem);
    const uint32_t pitch = src_pitch ? src_pitch : res->row_pitch_bytes;
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src_bytes + static_cast<size_t>(y) * pitch,
                  res->row_pitch_bytes);
    }
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
  cmd->flags = (pPresent->SyncInterval == 1) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
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

  // Map can be HRESULT or void depending on interface version.
  if constexpr (std::is_same_v<decltype(ctx_funcs->pfnMap), decltype(&Map11)>) {
    ctx_funcs->pfnMap = &Map11;
  } else {
    ctx_funcs->pfnMap = &Map11Void;
  }
  ctx_funcs->pfnUnmap = &Unmap11;
  ctx_funcs->pfnUpdateSubresourceUP = &UpdateSubresourceUP11;

  ctx_funcs->pfnFlush = &Flush11;
  ctx_funcs->pfnPresent = &Present11;
  ctx_funcs->pfnRotateResourceIdentities = &RotateResourceIdentities11;

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
