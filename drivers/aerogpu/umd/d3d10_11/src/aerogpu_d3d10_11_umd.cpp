// AeroGPU Windows 7 D3D10/11 UMD (minimal milestone implementation).
//
// This implementation focuses on the smallest working surface area required for
// D3D11 FL10_0 triangle-style samples.
//
// Key design: D3D10/11 DDIs are translated into the same AeroGPU command stream
// ("AeroGPU IR") used by the D3D9 UMD:
//   drivers/aerogpu/protocol/aerogpu_cmd.h
//
// The real Windows 7 build should be compiled with WDK headers and wired to the
// KMD submission path. For repository builds (no WDK), this code uses a minimal
// DDI ABI subset declared in `include/aerogpu_d3d10_11_umd.h`.

#if !defined(_WIN32) || !defined(AEROGPU_UMD_USE_WDK_HEADERS)

#include "../include/aerogpu_d3d10_11_umd.h"

#include <atomic>
#include <cstdio>
#include <condition_variable>
#include <cstring>
#include <mutex>
#include <new>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"

namespace {

#if defined(_WIN32)
// Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
// correct UMD bitness was loaded (System32 vs SysWOW64).
void LogModulePathOnce() {
  static bool logged = false;
  if (logged) {
    return;
  }
  logged = true;

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
}
#endif

constexpr aerogpu_handle_t kInvalidHandle = 0;

// D3D11_BIND_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;
constexpr uint32_t kD3D11BindDepthStencil = 0x40;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;

// D3D11_RESOURCE_MISC_SHARED (numeric value from d3d11.h).
constexpr uint32_t kD3D11ResourceMiscShared = 0x2;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
uint32_t HashSemanticName(const char* s) {
  if (!s) {
    return 0;
  }
  uint32_t hash = 2166136261u;
  for (const unsigned char* p = reinterpret_cast<const unsigned char*>(s); *p; ++p) {
    hash ^= *p;
    hash *= 16777619u;
  }
  return hash;
}

uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case kDxgiFormatB8G8R8X8Unorm:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    case kDxgiFormatR8G8B8A8Unorm:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    case kDxgiFormatD24UnormS8Uint:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    case kDxgiFormatD32Float:
      return AEROGPU_FORMAT_D32_FLOAT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
  switch (aerogpu_format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_D24_UNORM_S8_UINT:
    case AEROGPU_FORMAT_D32_FLOAT:
      return 4;
    case AEROGPU_FORMAT_B5G6R5_UNORM:
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
      return 2;
    default:
      return 4;
  }
}

uint32_t dxgi_index_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR32Uint:
      return AEROGPU_INDEX_FORMAT_UINT32;
    case kDxgiFormatR16Uint:
    default:
      return AEROGPU_INDEX_FORMAT_UINT16;
  }
}

uint32_t bind_flags_to_usage_flags(uint32_t bind_flags) {
  uint32_t usage = AEROGPU_RESOURCE_USAGE_NONE;
  if (bind_flags & kD3D11BindVertexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
  }
  if (bind_flags & kD3D11BindIndexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
  }
  if (bind_flags & kD3D11BindConstantBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER;
  }
  if (bind_flags & kD3D11BindShaderResource) {
    usage |= AEROGPU_RESOURCE_USAGE_TEXTURE;
  }
  if (bind_flags & kD3D11BindRenderTarget) {
    usage |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (bind_flags & kD3D11BindDepthStencil) {
    usage |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return usage;
}

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

struct AeroGpuAdapter {
  std::atomic<uint32_t> next_handle{1};

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible backing allocation ID. In a real WDDM build this comes from
  // WDDM allocation private driver data (aerogpu_wddm_alloc_priv) so it remains
  // stable across OpenResource/OpenAllocation. 0 means "host allocated" (no
  // allocation-table entry).
  uint32_t backing_alloc_id = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  bool is_shared = false;
  bool is_shared_alias = false;

  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;

  // Buffer fields.
  uint64_t size_bytes = 0;

  // Texture2D fields.
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t mip_levels = 1;
  uint32_t array_size = 1;
  uint32_t dxgi_format = 0;
  uint32_t row_pitch_bytes = 0;

  // CPU-visible backing storage for resource uploads.
  //
  // The initial milestone keeps resource data management very conservative:
  // - Buffers can be initialized at CreateResource time.
  // - Texture2D initial data is supported for the common {mips=1, array=1} case.
  //
  // A real WDDM build should map these updates onto real allocations.
  std::vector<uint8_t> storage;
};

struct AeroGpuShader {
  aerogpu_handle_t handle = 0;
  uint32_t stage = AEROGPU_SHADER_STAGE_VERTEX;
  std::vector<uint8_t> dxbc;
};

struct AeroGpuInputLayout {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct AeroGpuRenderTargetView {
  aerogpu_handle_t texture = 0;
};

struct AeroGpuDepthStencilView {
  aerogpu_handle_t texture = 0;
};

// The initial milestone treats pipeline state objects as opaque handles. They
// are accepted and can be bound, but the host translator currently relies on
// conservative defaults for any state not explicitly encoded in the command
// stream.
struct AeroGpuBlendState {
  uint32_t dummy = 0;
};
struct AeroGpuRasterizerState {
  uint32_t dummy = 0;
};
struct AeroGpuDepthStencilState {
  uint32_t dummy = 0;
};

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

// -------------------------------------------------------------------------------------------------
// Win7 WDK build: Real D3D11 DDI entrypoints (FL10_0 skeleton)
// -------------------------------------------------------------------------------------------------

struct AeroGpuShaderResourceView {
  aerogpu_handle_t texture = 0;
};

struct AeroGpuSampler {
  uint32_t dummy = 0;
};

struct AeroGpuImmediateContext;

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  // Runtime callbacks (for error reporting from void DDI entrypoints).
  const D3D11DDI_DEVICECALLBACKS* callbacks = nullptr;
  D3D11DDI_HDEVICE hDevice = {};

  AeroGpuImmediateContext* immediate = nullptr;
};

struct AeroGpuImmediateContext {
  AeroGpuDevice* device = nullptr;
  std::mutex mutex;
  aerogpu::CmdWriter cmd;

  // Cached state.
  aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  uint32_t current_rtv_count = 0;
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  AeroGpuImmediateContext() {
    cmd.reset();
  }
};

template <typename THandle, typename TObject>
TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

void SetError(AeroGpuDevice* dev, HRESULT hr) {
  if (!dev || !dev->callbacks) {
    return;
  }
  // Win7 D3D11 runtime expects pfnSetErrorCb for void-returning DDI failures.
  if (dev->callbacks->pfnSetErrorCb) {
    dev->callbacks->pfnSetErrorCb(dev->hDevice, hr);
  }
}

uint64_t submit_locked(AeroGpuImmediateContext* ctx) {
  if (!ctx || !ctx->device || ctx->cmd.empty()) {
    return 0;
  }

  AeroGpuAdapter* adapter = ctx->device->adapter;
  if (!adapter) {
    return 0;
  }

  ctx->cmd.finalize();

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  ctx->cmd.reset();
  return fence;
}

void flush_locked(AeroGpuImmediateContext* ctx) {
  submit_locked(ctx);
}

// -------------------------------------------------------------------------------------------------
// Device DDI (D3D11DDI_DEVICEFUNCS)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice11(D3D11DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  // Immediate context is owned by the device allocation (allocated via new below).
  if (dev->immediate) {
    dev->immediate->~AeroGpuImmediateContext();
    dev->immediate = nullptr;
  }

  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource11(D3D11DDI_HDEVICE hDevice,
                                          const D3D11DDIARG_CREATERESOURCE*,
                                          D3D11DDI_HRESOURCE hResource,
                                          D3D11DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = dev->adapter->next_handle.fetch_add(1);
  res->kind = ResourceKind::Unknown;

  // The Win7 WDK DDI contains rich resource descriptors; the bring-up skeleton
  // does not attempt to fully translate them yet. It allocates a stable handle
  // so that subsequent view/bind calls can reference the resource.
  return S_OK;
}

void AEROGPU_APIENTRY DestroyResource11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle != kInvalidHandle) {
    auto* cmd = dev->immediate ? dev->immediate->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE)
                               : nullptr;
    if (cmd) {
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
    }
  }
  res->~AeroGpuResource();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRenderTargetViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRenderTargetView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                                  D3D11DDI_HRENDERTARGETVIEW hView,
                                                  D3D11DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* rtv = new (hView.pDrvPrivate) AeroGpuRenderTargetView();
  (void)pDesc;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRenderTargetView11(D3D11DDI_HDEVICE, D3D11DDI_HRENDERTARGETVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D11DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hView);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilViewSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilView11(D3D11DDI_HDEVICE hDevice,
                                                  const D3D11DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                                  D3D11DDI_HDEPTHSTENCILVIEW hView,
                                                  D3D11DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dsv = new (hView.pDrvPrivate) AeroGpuDepthStencilView();
  (void)pDesc;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilView11(D3D11DDI_HDEVICE, D3D11DDI_HDEPTHSTENCILVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hView);
  dsv->~AeroGpuDepthStencilView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize11(D3D11DDI_HDEVICE,
                                                            const D3D11DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView11(D3D11DDI_HDEVICE hDevice,
                                                    const D3D11DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                    D3D11DDI_HSHADERRESOURCEVIEW hView,
                                                    D3D11DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* srv = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  (void)pDesc;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView11(D3D11DDI_HDEVICE, D3D11DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* srv = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  srv->~AeroGpuShaderResourceView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateVertexShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEVERTEXSHADER*) {
  return sizeof(AeroGpuShader);
}

HRESULT AEROGPU_APIENTRY CreateVertexShader11(D3D11DDI_HDEVICE hDevice,
                                              const D3D11DDIARG_CREATEVERTEXSHADER*,
                                              D3D11DDI_HVERTEXSHADER hShader,
                                              D3D11DDI_HRTVERTEXSHADER) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  sh->stage = AEROGPU_SHADER_STAGE_VERTEX;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyVertexShader11(D3D11DDI_HDEVICE, D3D11DDI_HVERTEXSHADER hShader) {
  if (!hShader.pDrvPrivate) {
    return;
  }
  auto* sh = FromHandle<D3D11DDI_HVERTEXSHADER, AeroGpuShader>(hShader);
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivatePixelShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEPIXELSHADER*) {
  return sizeof(AeroGpuShader);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader11(D3D11DDI_HDEVICE hDevice,
                                             const D3D11DDIARG_CREATEPIXELSHADER*,
                                             D3D11DDI_HPIXELSHADER hShader,
                                             D3D11DDI_HRTPIXELSHADER) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  sh->stage = AEROGPU_SHADER_STAGE_PIXEL;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyPixelShader11(D3D11DDI_HDEVICE, D3D11DDI_HPIXELSHADER hShader) {
  if (!hShader.pDrvPrivate) {
    return;
  }
  auto* sh = FromHandle<D3D11DDI_HPIXELSHADER, AeroGpuShader>(hShader);
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateGeometryShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEGEOMETRYSHADER*) {
  return sizeof(AeroGpuShader);
}

HRESULT AEROGPU_APIENTRY CreateGeometryShader11(D3D11DDI_HDEVICE hDevice,
                                                const D3D11DDIARG_CREATEGEOMETRYSHADER*,
                                                D3D11DDI_HGEOMETRYSHADER hShader,
                                                D3D11DDI_HRTGEOMETRYSHADER) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  // Geometry stage isn't represented in the current command stream; keep as vertex-stage for hashing/ID purposes.
  sh->stage = AEROGPU_SHADER_STAGE_VERTEX;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyGeometryShader11(D3D11DDI_HDEVICE, D3D11DDI_HGEOMETRYSHADER hShader) {
  if (!hShader.pDrvPrivate) {
    return;
  }
  auto* sh = FromHandle<D3D11DDI_HGEOMETRYSHADER, AeroGpuShader>(hShader);
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateElementLayoutSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEELEMENTLAYOUT*) {
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateElementLayout11(D3D11DDI_HDEVICE hDevice,
                                               const D3D11DDIARG_CREATEELEMENTLAYOUT*,
                                               D3D11DDI_HELEMENTLAYOUT hLayout,
                                               D3D11DDI_HRTELEMENTLAYOUT) {
  if (!hDevice.pDrvPrivate || !hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = dev->adapter->next_handle.fetch_add(1);
  return S_OK;
}

void AEROGPU_APIENTRY DestroyElementLayout11(D3D11DDI_HDEVICE, D3D11DDI_HELEMENTLAYOUT hLayout) {
  if (!hLayout.pDrvPrivate) {
    return;
  }
  auto* layout = FromHandle<D3D11DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  layout->~AeroGpuInputLayout();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler11(D3D11DDI_HDEVICE hDevice,
                                         const D3D11DDIARG_CREATESAMPLER*,
                                         D3D11DDI_HSAMPLER hSampler,
                                         D3D11DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hSampler.pDrvPrivate) AeroGpuSampler();
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler11(D3D11DDI_HDEVICE, D3D11DDI_HSAMPLER hSampler) {
  if (!hSampler.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D11DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  s->~AeroGpuSampler();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEBLENDSTATE*) {
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState11(D3D11DDI_HDEVICE hDevice,
                                            const D3D11DDIARG_CREATEBLENDSTATE*,
                                            D3D11DDI_HBLENDSTATE hState,
                                            D3D11DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState11(D3D11DDI_HDEVICE, D3D11DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D11DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERASTERIZERSTATE*) {
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState11(D3D11DDI_HDEVICE hDevice,
                                                 const D3D11DDIARG_CREATERASTERIZERSTATE*,
                                                 D3D11DDI_HRASTERIZERSTATE hState,
                                                 D3D11DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState11(D3D11DDI_HDEVICE, D3D11DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D11DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEDEPTHSTENCILSTATE*) {
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState11(D3D11DDI_HDEVICE hDevice,
                                                   const D3D11DDIARG_CREATEDEPTHSTENCILSTATE*,
                                                   D3D11DDI_HDEPTHSTENCILSTATE hState,
                                                   D3D11DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState11(D3D11DDI_HDEVICE, D3D11DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D11DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
}

// -------------------------------------------------------------------------------------------------
// Immediate context DDI (D3D11DDI_DEVICECONTEXTFUNCS)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY IaSetInputLayout11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HELEMENTLAYOUT hLayout) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  aerogpu_handle_t handle = 0;
  if (hLayout.pDrvPrivate) {
    handle = FromHandle<D3D11DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout)->handle;
  }
  ctx->current_input_layout = handle;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetVertexBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                           UINT StartSlot,
                                           UINT NumBuffers,
                                           const D3D11DDI_HRESOURCE* phBuffers,
                                           const UINT* pStrides,
                                           const UINT* pOffsets) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  if (!phBuffers || !pStrides || !pOffsets || NumBuffers == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  std::vector<aerogpu_vertex_buffer_binding> bindings;
  bindings.resize(NumBuffers);
  for (UINT i = 0; i < NumBuffers; i++) {
    bindings[i].buffer = phBuffers[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(phBuffers[i])->handle : 0;
    bindings[i].stride_bytes = pStrides[i];
    bindings[i].offset_bytes = pOffsets[i];
    bindings[i].reserved0 = 0;
  }

  auto* cmd = ctx->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, bindings.data(), bindings.size() * sizeof(bindings[0]));
  cmd->start_slot = StartSlot;
  cmd->buffer_count = NumBuffers;
}

void AEROGPU_APIENTRY IaSetIndexBuffer11(D3D11DDI_HDEVICECONTEXT hCtx,
                                         D3D11DDI_HRESOURCE hBuffer,
                                         DXGI_FORMAT format,
                                         UINT offset) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = hBuffer.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format));
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetTopology11(D3D11DDI_HDEVICECONTEXT hCtx, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);
  if (ctx->current_topology == static_cast<uint32_t>(topology)) {
    return;
  }
  ctx->current_topology = static_cast<uint32_t>(topology);

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = ctx->current_topology;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY VsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HVERTEXSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  ctx->current_vs = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HVERTEXSHADER, AeroGpuShader>(hShader)->handle : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = ctx->current_vs;
  cmd->ps = ctx->current_ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY PsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HPIXELSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  ctx->current_ps = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HPIXELSHADER, AeroGpuShader>(hShader)->handle : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = ctx->current_vs;
  cmd->ps = ctx->current_ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY GsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HGEOMETRYSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  ctx->current_gs = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HGEOMETRYSHADER, AeroGpuShader>(hShader)->handle : 0;
  // Geometry stage not yet translated into the command stream.
}

void AEROGPU_APIENTRY VsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT,
                                             UINT,
                                             UINT,
                                             const D3D11DDI_HRESOURCE*,
                                             const UINT*,
                                             const UINT*) {}
void AEROGPU_APIENTRY PsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT,
                                             UINT,
                                             UINT,
                                             const D3D11DDI_HRESOURCE*,
                                             const UINT*,
                                             const UINT*) {}
void AEROGPU_APIENTRY GsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT,
                                             UINT,
                                             UINT,
                                             const D3D11DDI_HRESOURCE*,
                                             const UINT*,
                                             const UINT*) {}

void AEROGPU_APIENTRY VsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT StartSlot,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !phViews || NumViews == 0) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  for (UINT i = 0; i < NumViews; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i])->texture;
    }
    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
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
  if (!hCtx.pDrvPrivate || !phViews || NumViews == 0) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  for (UINT i = 0; i < NumViews; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D11DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i])->texture;
    }
    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
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
  if (!hCtx.pDrvPrivate || !pViewports || NumViewports == 0) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  const auto& vp = pViewports[0];
  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(vp.TopLeftX);
  cmd->y_f32 = f32_bits(vp.TopLeftY);
  cmd->width_f32 = f32_bits(vp.Width);
  cmd->height_f32 = f32_bits(vp.Height);
  cmd->min_depth_f32 = f32_bits(vp.MinDepth);
  cmd->max_depth_f32 = f32_bits(vp.MaxDepth);
}

void AEROGPU_APIENTRY SetScissorRects11(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumRects, const D3D10_DDI_RECT* pRects) {
  if (!hCtx.pDrvPrivate || !pRects || NumRects == 0) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);

  const D3D10_DDI_RECT& r = pRects[0];
  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  cmd->x = r.left;
  cmd->y = r.top;
  cmd->width = r.right - r.left;
  cmd->height = r.bottom - r.top;
}

void AEROGPU_APIENTRY SetRasterizerState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HRASTERIZERSTATE) {
  // Conservative: accept but do not encode yet.
}

void AEROGPU_APIENTRY SetBlendState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HBLENDSTATE, const FLOAT[4], UINT) {
  // Conservative: accept but do not encode yet.
}

void AEROGPU_APIENTRY SetDepthStencilState11(D3D11DDI_HDEVICECONTEXT, D3D11DDI_HDEPTHSTENCILSTATE, UINT) {
  // Conservative: accept but do not encode yet.
}

void AEROGPU_APIENTRY SetRenderTargets11(D3D11DDI_HDEVICECONTEXT hCtx,
                                         UINT NumViews,
                                         const D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                         D3D11DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  ctx->current_rtv_count = (NumViews > AEROGPU_MAX_RENDER_TARGETS) ? AEROGPU_MAX_RENDER_TARGETS : NumViews;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    ctx->current_rtvs[i] = 0;
  }
  for (uint32_t i = 0; i < ctx->current_rtv_count; i++) {
    if (phRtvs && phRtvs[i].pDrvPrivate) {
      ctx->current_rtvs[i] =
          FromHandle<D3D11DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phRtvs[i])->texture;
    }
  }
  ctx->current_dsv = hDsv.pDrvPrivate ? FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = ctx->current_rtv_count;
  cmd->depth_stencil = ctx->current_dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = ctx->current_rtvs[i];
  }
}

void AEROGPU_APIENTRY ClearRenderTargetView11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HRENDERTARGETVIEW,
                                              const FLOAT rgba[4]) {
  if (!hCtx.pDrvPrivate || !rgba) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(rgba[0]);
  cmd->color_rgba_f32[1] = f32_bits(rgba[1]);
  cmd->color_rgba_f32[2] = f32_bits(rgba[2]);
  cmd->color_rgba_f32[3] = f32_bits(rgba[3]);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;
}

void AEROGPU_APIENTRY ClearDepthStencilView11(D3D11DDI_HDEVICECONTEXT hCtx,
                                              D3D11DDI_HDEPTHSTENCILVIEW,
                                              UINT flags,
                                              FLOAT depth,
                                              UINT8 stencil) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);

  uint32_t aer_flags = 0;
  if (flags & 0x1u) {
    aer_flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (flags & 0x2u) {
    aer_flags |= AEROGPU_CLEAR_STENCIL;
  }

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = aer_flags;
  cmd->color_rgba_f32[0] = 0;
  cmd->color_rgba_f32[1] = 0;
  cmd->color_rgba_f32[2] = 0;
  cmd->color_rgba_f32[3] = 0;
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
}

void AEROGPU_APIENTRY Draw11(D3D11DDI_HDEVICECONTEXT hCtx, UINT VertexCount, UINT StartVertexLocation) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = VertexCount;
  cmd->instance_count = 1;
  cmd->first_vertex = StartVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexed11(D3D11DDI_HDEVICECONTEXT hCtx, UINT IndexCount, UINT StartIndexLocation, INT BaseVertexLocation) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = IndexCount;
  cmd->instance_count = 1;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY Flush11(D3D11DDI_HDEVICECONTEXT hCtx) {
  if (!hCtx.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }
  std::lock_guard<std::mutex> lock(ctx->mutex);
  flush_locked(ctx);
}

HRESULT AEROGPU_APIENTRY Present11(D3D11DDI_HDEVICECONTEXT hCtx, const D3D10DDIARG_PRESENT* pPresent) {
  if (!hCtx.pDrvPrivate || !pPresent) {
    return E_INVALIDARG;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);
  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  cmd->scanout_id = 0;
  cmd->flags = (pPresent->SyncInterval == 1) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

  submit_locked(ctx);
  return S_OK;
}

void AEROGPU_APIENTRY RotateResourceIdentities11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE* pResources, UINT numResources) {
  if (!hCtx.pDrvPrivate || !pResources || numResources < 2) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx) {
    return;
  }

  std::lock_guard<std::mutex> lock(ctx->mutex);

  auto* first = FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pResources[0]);
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;

  for (UINT i = 0; i + 1 < numResources; ++i) {
    auto* dst = FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pResources[i]);
    auto* src = FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pResources[i + 1]);
    if (!dst || !src) {
      return;
    }
    dst->handle = src->handle;
  }

  auto* last = FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pResources[numResources - 1]);
  if (last) {
    last->handle = saved;
  }
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI (D3D11DDI_ADAPTERFUNCS)
// -------------------------------------------------------------------------------------------------

HRESULT AEROGPU_APIENTRY GetCaps11(D3D10DDI_HADAPTER, const D3D11DDIARG_GETCAPS* pGetCaps) {
  if (!pGetCaps || !pGetCaps->pData || pGetCaps->DataSize == 0) {
    return E_INVALIDARG;
  }

  std::memset(pGetCaps->pData, 0, pGetCaps->DataSize);
  // The minimal bring-up path treats unknown cap types as unsupported and relies
  // on device-creation-time feature level negotiation. Always succeed with a
  // zero-filled response to avoid runtime crashes on unexpected cap queries.
  return S_OK;
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize11(D3D10DDI_HADAPTER, const D3D11DDIARG_CREATEDEVICE*) {
  // Device allocation includes the immediate context object.
  return sizeof(AeroGpuDevice) + sizeof(AeroGpuImmediateContext);
}

HRESULT AEROGPU_APIENTRY CreateDevice11(D3D10DDI_HADAPTER hAdapter, D3D11DDIARG_CREATEDEVICE* pCreate) {
  if (!pCreate || !pCreate->hDevice.pDrvPrivate || !pCreate->pDeviceFuncs || !pCreate->pDeviceContextFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* dev = new (pCreate->hDevice.pDrvPrivate) AeroGpuDevice();
  dev->adapter = adapter;
  dev->callbacks = pCreate->pDeviceCallbacks;
  dev->hDevice = pCreate->hDevice;

  // Place the immediate context immediately after the device object.
  void* ctx_mem = reinterpret_cast<uint8_t*>(pCreate->hDevice.pDrvPrivate) + sizeof(AeroGpuDevice);
  auto* ctx = new (ctx_mem) AeroGpuImmediateContext();
  ctx->device = dev;
  dev->immediate = ctx;

  // Return the immediate context handle expected by the runtime.
  pCreate->hImmediateContext.pDrvPrivate = ctx;

  D3D11DDI_DEVICEFUNCS device_funcs = {};
  device_funcs.pfnDestroyDevice = &DestroyDevice11;
  device_funcs.pfnCalcPrivateResourceSize = &CalcPrivateResourceSize11;
  device_funcs.pfnCreateResource = &CreateResource11;
  device_funcs.pfnDestroyResource = &DestroyResource11;

  device_funcs.pfnCalcPrivateRenderTargetViewSize = &CalcPrivateRenderTargetViewSize11;
  device_funcs.pfnCreateRenderTargetView = &CreateRenderTargetView11;
  device_funcs.pfnDestroyRenderTargetView = &DestroyRenderTargetView11;

  device_funcs.pfnCalcPrivateDepthStencilViewSize = &CalcPrivateDepthStencilViewSize11;
  device_funcs.pfnCreateDepthStencilView = &CreateDepthStencilView11;
  device_funcs.pfnDestroyDepthStencilView = &DestroyDepthStencilView11;

  device_funcs.pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize11;
  device_funcs.pfnCreateShaderResourceView = &CreateShaderResourceView11;
  device_funcs.pfnDestroyShaderResourceView = &DestroyShaderResourceView11;

  device_funcs.pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize11;
  device_funcs.pfnCreateVertexShader = &CreateVertexShader11;
  device_funcs.pfnDestroyVertexShader = &DestroyVertexShader11;

  device_funcs.pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize11;
  device_funcs.pfnCreatePixelShader = &CreatePixelShader11;
  device_funcs.pfnDestroyPixelShader = &DestroyPixelShader11;

  device_funcs.pfnCalcPrivateGeometryShaderSize = &CalcPrivateGeometryShaderSize11;
  device_funcs.pfnCreateGeometryShader = &CreateGeometryShader11;
  device_funcs.pfnDestroyGeometryShader = &DestroyGeometryShader11;

  device_funcs.pfnCalcPrivateElementLayoutSize = &CalcPrivateElementLayoutSize11;
  device_funcs.pfnCreateElementLayout = &CreateElementLayout11;
  device_funcs.pfnDestroyElementLayout = &DestroyElementLayout11;

  device_funcs.pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize11;
  device_funcs.pfnCreateSampler = &CreateSampler11;
  device_funcs.pfnDestroySampler = &DestroySampler11;

  device_funcs.pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize11;
  device_funcs.pfnCreateBlendState = &CreateBlendState11;
  device_funcs.pfnDestroyBlendState = &DestroyBlendState11;

  device_funcs.pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize11;
  device_funcs.pfnCreateRasterizerState = &CreateRasterizerState11;
  device_funcs.pfnDestroyRasterizerState = &DestroyRasterizerState11;

  device_funcs.pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize11;
  device_funcs.pfnCreateDepthStencilState = &CreateDepthStencilState11;
  device_funcs.pfnDestroyDepthStencilState = &DestroyDepthStencilState11;

  *pCreate->pDeviceFuncs = device_funcs;

  D3D11DDI_DEVICECONTEXTFUNCS ctx_funcs = {};
  ctx_funcs.pfnIaSetInputLayout = &IaSetInputLayout11;
  ctx_funcs.pfnIaSetVertexBuffers = &IaSetVertexBuffers11;
  ctx_funcs.pfnIaSetIndexBuffer = &IaSetIndexBuffer11;
  ctx_funcs.pfnIaSetTopology = &IaSetTopology11;

  ctx_funcs.pfnVsSetShader = &VsSetShader11;
  ctx_funcs.pfnVsSetConstantBuffers = &VsSetConstantBuffers11;
  ctx_funcs.pfnVsSetShaderResources = &VsSetShaderResources11;
  ctx_funcs.pfnVsSetSamplers = &VsSetSamplers11;

  ctx_funcs.pfnPsSetShader = &PsSetShader11;
  ctx_funcs.pfnPsSetConstantBuffers = &PsSetConstantBuffers11;
  ctx_funcs.pfnPsSetShaderResources = &PsSetShaderResources11;
  ctx_funcs.pfnPsSetSamplers = &PsSetSamplers11;

  ctx_funcs.pfnGsSetShader = &GsSetShader11;
  ctx_funcs.pfnGsSetConstantBuffers = &GsSetConstantBuffers11;
  ctx_funcs.pfnGsSetShaderResources = &GsSetShaderResources11;
  ctx_funcs.pfnGsSetSamplers = &GsSetSamplers11;

  ctx_funcs.pfnSetViewports = &SetViewports11;
  ctx_funcs.pfnSetScissorRects = &SetScissorRects11;
  ctx_funcs.pfnSetRasterizerState = &SetRasterizerState11;
  ctx_funcs.pfnSetBlendState = &SetBlendState11;
  ctx_funcs.pfnSetDepthStencilState = &SetDepthStencilState11;
  ctx_funcs.pfnSetRenderTargets = &SetRenderTargets11;

  ctx_funcs.pfnClearRenderTargetView = &ClearRenderTargetView11;
  ctx_funcs.pfnClearDepthStencilView = &ClearDepthStencilView11;
  ctx_funcs.pfnDraw = &Draw11;
  ctx_funcs.pfnDrawIndexed = &DrawIndexed11;
  ctx_funcs.pfnFlush = &Flush11;
  ctx_funcs.pfnPresent = &Present11;
  ctx_funcs.pfnRotateResourceIdentities = &RotateResourceIdentities11;

  *pCreate->pDeviceContextFuncs = ctx_funcs;
  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter11(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Exported OpenAdapter entrypoints
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapter11Wdk(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = new AeroGpuAdapter();
  pOpenData->hAdapter.pDrvPrivate = adapter;

  auto* funcs = reinterpret_cast<D3D11DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  std::memset(funcs, 0, sizeof(*funcs));
  funcs->pfnGetCaps = &GetCaps11;
  funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize11;
  funcs->pfnCreateDevice = &CreateDevice11;
  funcs->pfnCloseAdapter = &CloseAdapter11;
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER*) {
  return E_NOTIMPL;
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER*) {
  return E_NOTIMPL;
}

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapter11Wdk(pOpenData);
}

} // extern "C"

#else

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // Cached state.
  aerogpu_handle_t current_rtv = 0;
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  AeroGpuDevice() {
    cmd.reset();
  }
};

template <typename THandle, typename TObject>
TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

uint64_t submit_locked(AeroGpuDevice* dev) {
  if (!dev || dev->cmd.empty()) {
    return 0;
  }

  AeroGpuAdapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  dev->cmd.finalize();

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  dev->cmd.reset();
  return fence;
}

HRESULT flush_locked(AeroGpuDevice* dev) {
  if (dev) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  submit_locked(dev);
  return S_OK;
}

// -------------------------------------------------------------------------------------------------
// Device DDI (plain functions to ensure the correct calling convention)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERESOURCE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                         const AEROGPU_DDIARG_CREATERESOURCE* pDesc,
                                         D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Buffer;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes));
      } catch (...) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = 0;
      dirty->size_bytes = res->size_bytes;
    }
    return S_OK;
  }

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    const uint32_t mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    const bool is_shared = (pDesc->MiscFlags & kD3D11ResourceMiscShared) != 0;
    if (is_shared && mip_levels != 1) {
      // MVP: shared surfaces are single-allocation only.
      return E_NOTIMPL;
    }

    if (pDesc->ArraySize != 1) {
      return E_NOTIMPL;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(pDesc->Format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_NOTIMPL;
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Texture2D;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = mip_levels;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = pDesc->Format;
    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      if (res->mip_levels != 1 || res->array_size != 1) {
        res->~AeroGpuResource();
        return E_NOTIMPL;
      }

      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }

      const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
      if (total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(total_bytes));
      } catch (...) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }

      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch)
                                                : static_cast<size_t>(res->row_pitch_bytes);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    res->row_pitch_bytes);
      }
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = 0;
      dirty->size_bytes = res->storage.size();
    }
    return S_OK;
  }

  return E_NOTIMPL;
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle != kInvalidHandle) {
    // NOTE: For now we emit DESTROY_RESOURCE for both original resources and
    // shared-surface aliases. The host command processor is expected to
    // normalize alias lifetimes, but proper cross-process refcounting may be
    // needed later.
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
  }
  res->~AeroGpuResource();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADER*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuShader);
}

static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                  D3D10DDI_HSHADER hShader,
                                  uint32_t stage) {
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate || !pDesc->pCode || !pDesc->CodeSize) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  sh->stage = stage;
  sh->dxbc.resize(pDesc->CodeSize);
  std::memcpy(sh->dxbc.data(), pDesc->pCode, pDesc->CodeSize);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                            D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_VERTEX);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                           D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_PIXEL);
}

void AEROGPU_APIENTRY DestroyShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sh = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader);
  if (!dev || !sh) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (sh->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    cmd->shader_handle = sh->handle;
    cmd->reserved0 = 0;
  }
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateInputLayoutSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEINPUTLAYOUT*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateInputLayout(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATEINPUTLAYOUT* pDesc,
                                           D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate || (!pDesc->NumElements && pDesc->pElements)) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = dev->adapter->next_handle.fetch_add(1);

  const size_t blob_size = sizeof(aerogpu_input_layout_blob_header) +
                           static_cast<size_t>(pDesc->NumElements) * sizeof(aerogpu_input_layout_element_dxgi);
  layout->blob.resize(blob_size);

  auto* hdr = reinterpret_cast<aerogpu_input_layout_blob_header*>(layout->blob.data());
  hdr->magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
  hdr->version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
  hdr->element_count = pDesc->NumElements;
  hdr->reserved0 = 0;

  auto* elems = reinterpret_cast<aerogpu_input_layout_element_dxgi*>(layout->blob.data() + sizeof(*hdr));
  for (uint32_t i = 0; i < pDesc->NumElements; i++) {
    const auto& e = pDesc->pElements[i];
    elems[i].semantic_name_hash = HashSemanticName(e.SemanticName);
    elems[i].semantic_index = e.SemanticIndex;
    elems[i].dxgi_format = e.Format;
    elems[i].input_slot = e.InputSlot;
    elems[i].aligned_byte_offset = e.AlignedByteOffset;
    elems[i].input_slot_class = e.InputSlotClass;
    elems[i].instance_data_step_rate = e.InstanceDataStepRate;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, layout->blob.data(), layout->blob.size());
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hLayout.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  if (!dev || !layout) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (layout->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    cmd->input_layout_handle = layout->handle;
    cmd->reserved0 = 0;
  }
  layout->~AeroGpuInputLayout();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERENDERTARGETVIEW*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRTV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                   D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRTV(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDSV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                   D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pDesc || !hDsv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDSV(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  dsv->~AeroGpuDepthStencilView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEBLENDSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const AEROGPU_DDIARG_CREATEBLENDSTATE*,
                                          D3D10DDI_HBLENDSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERASTERIZERSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const AEROGPU_DDIARG_CREATERASTERIZERSTATE*,
                                               D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE,
                                                         const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
}

void AEROGPU_APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice,
                                       D3D10DDI_HRENDERTARGETVIEW hRtv,
                                       D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t rtv_handle = 0;
  aerogpu_handle_t dsv_handle = 0;
  if (hRtv.pDrvPrivate) {
    rtv_handle = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv)->texture;
  }
  if (hDsv.pDrvPrivate) {
    dsv_handle = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture;
  }

  dev->current_rtv = rtv_handle;
  dev->current_dsv = dsv_handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = 1;
  cmd->depth_stencil = dsv_handle;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  cmd->colors[0] = rtv_handle;
}

void AEROGPU_APIENTRY ClearRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW, const float rgba[4]) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !rgba) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
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

void AEROGPU_APIENTRY ClearDSV(D3D10DDI_HDEVICE hDevice,
                               D3D10DDI_HDEPTHSTENCILVIEW,
                               uint32_t clear_flags,
                               float depth,
                               uint8_t stencil) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t flags = 0;
  if (clear_flags & AEROGPU_DDI_CLEAR_DEPTH) {
    flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (clear_flags & AEROGPU_DDI_CLEAR_STENCIL) {
    flags |= AEROGPU_CLEAR_STENCIL;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = flags;
  cmd->color_rgba_f32[0] = 0;
  cmd->color_rgba_f32[1] = 0;
  cmd->color_rgba_f32[2] = 0;
  cmd->color_rgba_f32[3] = 0;
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
}

void AEROGPU_APIENTRY SetInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t handle = 0;
  if (hLayout.pDrvPrivate) {
    handle = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout)->handle;
  }
  dev->current_input_layout = handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetVertexBuffer(D3D10DDI_HDEVICE hDevice,
                                      D3D10DDI_HRESOURCE hBuffer,
                                      uint32_t stride,
                                      uint32_t offset) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  binding.stride_bytes = stride;
  binding.offset_bytes = offset;
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS,
                                                                           &binding,
                                                                           sizeof(binding));
  cmd->start_slot = 0;
  cmd->buffer_count = 1;
}

void AEROGPU_APIENTRY SetIndexBuffer(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hBuffer, uint32_t format, uint32_t offset) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(format);
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetViewport(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDI_VIEWPORT* pVp) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pVp) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(pVp->TopLeftX);
  cmd->y_f32 = f32_bits(pVp->TopLeftY);
  cmd->width_f32 = f32_bits(pVp->Width);
  cmd->height_f32 = f32_bits(pVp->Height);
  cmd->min_depth_f32 = f32_bits(pVp->MinDepth);
  cmd->max_depth_f32 = f32_bits(pVp->MaxDepth);
}

void AEROGPU_APIENTRY SetDrawState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hVs, D3D10DDI_HSHADER hPs) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t vs = hVs.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hVs)->handle : 0;
  aerogpu_handle_t ps = hPs.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hPs)->handle : 0;
  dev->current_vs = vs;
  dev->current_ps = ps;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = vs;
  cmd->ps = ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetPrimitiveTopology(D3D10DDI_HDEVICE hDevice, uint32_t topology) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->current_topology == topology) {
    return;
  }
  dev->current_topology = topology;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = topology;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY Draw(D3D10DDI_HDEVICE hDevice, uint32_t vertex_count, uint32_t start_vertex) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, uint32_t index_count, uint32_t start_index, int32_t base_vertex) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDIARG_PRESENT* pPresent) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pPresent) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  cmd->scanout_id = 0;
  cmd->flags = (pPresent->SyncInterval == 1) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

  submit_locked(dev);
  return S_OK;
}

HRESULT AEROGPU_APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return flush_locked(dev);
}

void AEROGPU_APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE* pResources, uint32_t numResources) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!hDevice.pDrvPrivate || !pResources || numResources < 2) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Rotate AeroGPU handles in-place. This approximates DXGI swapchain buffer
  // flipping without requiring a full allocation-table/patching implementation.
  auto* first = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[0]);
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;

  for (uint32_t i = 0; i + 1 < numResources; ++i) {
    auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i]);
    auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i + 1]);
    if (!dst || !src) {
      return;
    }
    dst->handle = src->handle;
  }

  auto* last = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[numResources - 1]);
  if (last) {
    last->handle = saved;
  }
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* out_funcs = reinterpret_cast<AEROGPU_D3D10_11_DEVICEFUNCS*>(pCreateDevice->pDeviceFuncs);
  if (!out_funcs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;

  AEROGPU_D3D10_11_DEVICEFUNCS funcs = {};
  funcs.pfnDestroyDevice = &DestroyDevice;

  funcs.pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  funcs.pfnCreateResource = &CreateResource;
  funcs.pfnDestroyResource = &DestroyResource;

  funcs.pfnCalcPrivateShaderSize = &CalcPrivateShaderSize;
  funcs.pfnCreateVertexShader = &CreateVertexShader;
  funcs.pfnCreatePixelShader = &CreatePixelShader;
  funcs.pfnDestroyShader = &DestroyShader;

  funcs.pfnCalcPrivateInputLayoutSize = &CalcPrivateInputLayoutSize;
  funcs.pfnCreateInputLayout = &CreateInputLayout;
  funcs.pfnDestroyInputLayout = &DestroyInputLayout;

  funcs.pfnCalcPrivateRTVSize = &CalcPrivateRTVSize;
  funcs.pfnCreateRTV = &CreateRTV;
  funcs.pfnDestroyRTV = &DestroyRTV;

  funcs.pfnCalcPrivateDSVSize = &CalcPrivateDSVSize;
  funcs.pfnCreateDSV = &CreateDSV;
  funcs.pfnDestroyDSV = &DestroyDSV;

  funcs.pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  funcs.pfnCreateBlendState = &CreateBlendState;
  funcs.pfnDestroyBlendState = &DestroyBlendState;

  funcs.pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  funcs.pfnCreateRasterizerState = &CreateRasterizerState;
  funcs.pfnDestroyRasterizerState = &DestroyRasterizerState;

  funcs.pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  funcs.pfnCreateDepthStencilState = &CreateDepthStencilState;
  funcs.pfnDestroyDepthStencilState = &DestroyDepthStencilState;

  funcs.pfnSetRenderTargets = &SetRenderTargets;
  funcs.pfnClearRTV = &ClearRTV;
  funcs.pfnClearDSV = &ClearDSV;

  funcs.pfnSetInputLayout = &SetInputLayout;
  funcs.pfnSetVertexBuffer = &SetVertexBuffer;
  funcs.pfnSetIndexBuffer = &SetIndexBuffer;
  funcs.pfnSetViewport = &SetViewport;
  funcs.pfnSetDrawState = &SetDrawState;
  funcs.pfnSetBlendState = &SetBlendState;
  funcs.pfnSetRasterizerState = &SetRasterizerState;
  funcs.pfnSetDepthStencilState = &SetDepthStencilState;
  funcs.pfnSetPrimitiveTopology = &SetPrimitiveTopology;

  funcs.pfnDraw = &Draw;
  funcs.pfnDrawIndexed = &DrawIndexed;
  funcs.pfnPresent = &Present;
  funcs.pfnFlush = &Flush;
  funcs.pfnRotateResourceIdentities = &RotateResourceIdentities;

  *out_funcs = funcs;
  return S_OK;
}

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pGetCaps) {
  if (!pGetCaps) {
    AEROGPU_D3D10_11_LOG("GetCaps pGetCaps=null");
    return E_INVALIDARG;
  }

  bool recognized = false;

  // For early bring-up, the D3D10/11 caps surface is intentionally conservative.
  // We currently don't expose detailed caps; we only zero the caller buffer so
  // the runtime sees a consistent "unsupported" baseline.
  if (pGetCaps->pData && pGetCaps->DataSize) {
    std::memset(pGetCaps->pData, 0, pGetCaps->DataSize);
  }

  AEROGPU_D3D10_11_LOG("GetCaps Type=%u DataSize=%u recognized=%u",
                       static_cast<unsigned>(pGetCaps->Type),
                       static_cast<unsigned>(pGetCaps->DataSize),
                       recognized ? 1u : 0u);
  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  AEROGPU_D3D10_11_LOG_CALL();
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Exported OpenAdapter entrypoints
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapterCommon(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  LogModulePathOnce();
#endif

  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = new AeroGpuAdapter();
  pOpenData->hAdapter.pDrvPrivate = adapter;

  D3D10DDI_ADAPTERFUNCS funcs = {};
  funcs.pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
  funcs.pfnCreateDevice = &CreateDevice;
  funcs.pfnCloseAdapter = &CloseAdapter;
  funcs.pfnGetCaps = &GetCaps;

  auto* out_funcs = reinterpret_cast<D3D10DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  if (!out_funcs) {
    return E_INVALIDARG;
  }
  *out_funcs = funcs;
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  char buf[256];
  snprintf(buf,
           sizeof(buf),
           "aerogpu-d3d10_11: OpenAdapter10 Interface=%u Version=%u\n",
           (unsigned)(pOpenData ? pOpenData->Interface : 0),
           (unsigned)(pOpenData ? pOpenData->Version : 0));
  OutputDebugStringA(buf);
#endif
  AEROGPU_D3D10_11_LOG_CALL();
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  char buf[256];
  snprintf(buf,
           sizeof(buf),
           "aerogpu-d3d10_11: OpenAdapter10_2 Interface=%u Version=%u\n",
           (unsigned)(pOpenData ? pOpenData->Interface : 0),
           (unsigned)(pOpenData ? pOpenData->Version : 0));
  OutputDebugStringA(buf);
#endif
  AEROGPU_D3D10_11_LOG_CALL();
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  char buf[256];
  snprintf(buf,
           sizeof(buf),
           "aerogpu-d3d10_11: OpenAdapter11 Interface=%u Version=%u\n",
           (unsigned)(pOpenData ? pOpenData->Interface : 0),
           (unsigned)(pOpenData ? pOpenData->Version : 0));
  OutputDebugStringA(buf);
#endif
  AEROGPU_D3D10_11_LOG_CALL();
  return OpenAdapterCommon(pOpenData);
}

} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
