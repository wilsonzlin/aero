// AeroGPU Windows 7 D3D10 UMD (WDK DDI implementation).
//
// This translation layer is built only when the project is compiled against the
// Windows WDK D3D10 UMD DDI headers (d3d10umddi.h / d3d10_1umddi.h).
//
// The repository build (without WDK headers) uses a minimal ABI subset in
// `aerogpu_d3d10_11_umd.cpp` instead.
//
// Goal of this file: provide a non-null, minimally-correct D3D10DDI adapter +
// device function surface (exports + vtables) sufficient for basic D3D10
// create/draw/present on Windows 7 (WDDM 1.1), and for DXGI swapchain-driven
// present paths that call RotateResourceIdentities.

#include "../include/aerogpu_d3d10_11_umd.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <atomic>
#include <condition_variable>
#include <cstdarg>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <new>
#include <tuple>
#include <type_traits>
#include <vector>

#include <d3d10.h>
#include <d3d10_1.h>

#include "aerogpu_cmd_writer.h"

namespace {

// -----------------------------------------------------------------------------
// Logging (opt-in)
// -----------------------------------------------------------------------------

// Define AEROGPU_D3D10_WDK_TRACE_CAPS=1 to emit OutputDebugStringA traces for
// D3D10DDI adapter caps queries. This is intentionally lightweight so that
// missing caps types can be discovered quickly on real Win7 systems without
// having to attach a debugger first.
#if !defined(AEROGPU_D3D10_WDK_TRACE_CAPS)
  #define AEROGPU_D3D10_WDK_TRACE_CAPS 0
#endif

void DebugLog(const char* fmt, ...) {
#if AEROGPU_D3D10_WDK_TRACE_CAPS
  char buf[512];
  va_list args;
  va_start(args, fmt);
  _vsnprintf_s(buf, sizeof(buf), _TRUNCATE, fmt, args);
  va_end(args);
  OutputDebugStringA(buf);
#else
  (void)fmt;
#endif
}

constexpr aerogpu_handle_t kInvalidHandle = 0;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatR32G32B32A32Float = 2;
constexpr uint32_t kDxgiFormatR32G32B32Float = 6;
constexpr uint32_t kDxgiFormatR32G32Float = 16;
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;

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

// D3D10_BIND_* and D3D11_BIND_* share values for the common subset we care about.
constexpr uint32_t kD3D10BindVertexBuffer = 0x1;
constexpr uint32_t kD3D10BindIndexBuffer = 0x2;
constexpr uint32_t kD3D10BindConstantBuffer = 0x4;
constexpr uint32_t kD3D10BindShaderResource = 0x8;
constexpr uint32_t kD3D10BindRenderTarget = 0x20;
constexpr uint32_t kD3D10BindDepthStencil = 0x40;

uint32_t bind_flags_to_usage_flags(uint32_t bind_flags) {
  uint32_t usage = AEROGPU_RESOURCE_USAGE_NONE;
  if (bind_flags & kD3D10BindVertexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
  }
  if (bind_flags & kD3D10BindIndexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
  }
  if (bind_flags & kD3D10BindConstantBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER;
  }
  if (bind_flags & kD3D10BindShaderResource) {
    usage |= AEROGPU_RESOURCE_USAGE_TEXTURE;
  }
  if (bind_flags & kD3D10BindRenderTarget) {
    usage |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (bind_flags & kD3D10BindDepthStencil) {
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

  const D3D10DDI_ADAPTERCALLBACKS* callbacks = nullptr;

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

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

struct AeroGpuShaderResourceView {
  aerogpu_handle_t texture = 0;
};

struct AeroGpuBlendState {
  uint32_t dummy = 0;
};

struct AeroGpuRasterizerState {
  uint32_t dummy = 0;
};

struct AeroGpuDepthStencilState {
  uint32_t dummy = 0;
};

struct AeroGpuSampler {
  uint32_t dummy = 0;
};

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  const D3D10DDI_DEVICECALLBACKS* callbacks = nullptr;

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

void SetError(D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->callbacks || !dev->callbacks->pfnSetErrorCb) {
    return;
  }
  dev->callbacks->pfnSetErrorCb(hDevice, hr);
}

// -----------------------------------------------------------------------------
// Generic stubs for unimplemented device DDIs
// -----------------------------------------------------------------------------
//
// D3D10DDI_DEVICEFUNCS is a large vtable. For bring-up we prefer populating every
// function pointer with a safe stub rather than leaving it NULL (null vtable
// calls in the D3D10 runtime are fatal). These templates let us generate stubs
// that exactly match the calling convention/signature of each function pointer
// without having to manually write hundreds of prototypes.
template <typename Fn>
struct NotImpl;

template <typename... Args>
struct NotImpl<void(APIENTRY*)(Args...)> {
  static void APIENTRY Fn(Args... args) {
    // Most device DDIs are (HDEVICE, ...). Only call SetError when we can prove
    // the first argument is the expected handle type.
    if constexpr (sizeof...(Args) > 0) {
      using First = typename std::tuple_element<0, std::tuple<Args...>>::type;
      if constexpr (std::is_same<typename std::remove_cv<typename std::remove_reference<First>::type>::type,
                                 D3D10DDI_HDEVICE>::value) {
        SetError(std::get<0>(std::tie(args...)), E_NOTIMPL);
      }
    }
  }
};

template <typename... Args>
struct NotImpl<HRESULT(APIENTRY*)(Args...)> {
  static HRESULT APIENTRY Fn(Args...) {
    return E_NOTIMPL;
  }
};

template <typename Ret, typename... Args>
struct NotImpl<Ret(APIENTRY*)(Args...)> {
  static Ret APIENTRY Fn(Args...) {
    return Ret{};
  }
};

template <typename Fn>
struct Noop;

template <typename... Args>
struct Noop<void(APIENTRY*)(Args...)> {
  static void APIENTRY Fn(Args...) {
    // Intentionally do nothing (treated as supported but ignored).
  }
};

template <typename... Args>
struct Noop<HRESULT(APIENTRY*)(Args...)> {
  static HRESULT APIENTRY Fn(Args...) {
    return S_OK;
  }
};

template <typename Ret, typename... Args>
struct Noop<Ret(APIENTRY*)(Args...)> {
  static Ret APIENTRY Fn(Args...) {
    return Ret{};
  }
};

#define AEROGPU_DEFINE_HAS_MEMBER(member)                                                            \
  template <typename T, typename = void>                                                             \
  struct has_##member : std::false_type {};                                                          \
  template <typename T>                                                                              \
  struct has_##member<T, std::void_t<decltype(&T::member)>> : std::true_type {};

// The D3D10 DDI surface can vary slightly across WDK versions. Use member
// detection + if constexpr so we can populate fields when present without
// making compilation conditional on a specific SDK revision.
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawInstanced)
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawIndexedInstanced)
AEROGPU_DEFINE_HAS_MEMBER(pfnDrawAuto)
AEROGPU_DEFINE_HAS_MEMBER(pfnSoSetTargets)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetPredication)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTextFilterSize)
AEROGPU_DEFINE_HAS_MEMBER(pfnGenMips)
AEROGPU_DEFINE_HAS_MEMBER(pfnGenerateMips)
AEROGPU_DEFINE_HAS_MEMBER(pfnResolveSubresource)
AEROGPU_DEFINE_HAS_MEMBER(pfnClearState)
AEROGPU_DEFINE_HAS_MEMBER(pfnCalcPrivateQuerySize)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateQuery)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyQuery)
AEROGPU_DEFINE_HAS_MEMBER(pfnCalcPrivatePredicateSize)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreatePredicate)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyPredicate)
AEROGPU_DEFINE_HAS_MEMBER(pfnCalcPrivateCounterSize)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateCounter)
AEROGPU_DEFINE_HAS_MEMBER(pfnDestroyCounter)
AEROGPU_DEFINE_HAS_MEMBER(pfnCalcPrivateGeometryShaderWithStreamOutputSize)
AEROGPU_DEFINE_HAS_MEMBER(pfnCreateGeometryShaderWithStreamOutput)

#undef AEROGPU_DEFINE_HAS_MEMBER

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

// -----------------------------------------------------------------------------
// Device DDI (core bring-up set)
// -----------------------------------------------------------------------------

void APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                const D3D10DDIARG_CREATERESOURCE* pDesc,
                                D3D10DDI_HRESOURCE hResource,
                                D3D10DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = dev->adapter->next_handle.fetch_add(1);
  res->bind_flags = pDesc->BindFlags;
  res->misc_flags = pDesc->MiscFlags;

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);
  if (dim == 1u /* buffer */) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pDesc->ByteWidth;

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;
    return S_OK;
  }

  if (dim == 3u /* texture2d */) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu(static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize ? pDesc->ArraySize : 1;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);
    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = res->array_size;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;
    return S_OK;
  }

  res->~AeroGpuResource();
  return E_NOTIMPL;
}

void APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
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
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
  }
  res->~AeroGpuResource();
}

HRESULT APIENTRY Map(D3D10DDI_HDEVICE hDevice, D3D10DDIARG_MAP* pMap) {
  if (!hDevice.pDrvPrivate || !pMap || !pMap->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pMap->hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->storage.empty()) {
    uint64_t size = 0;
    if (res->kind == ResourceKind::Buffer) {
      size = res->size_bytes;
    } else if (res->kind == ResourceKind::Texture2D) {
      size = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    }
    if (size && size <= static_cast<uint64_t>(SIZE_MAX)) {
      try {
        res->storage.resize(static_cast<size_t>(size));
      } catch (...) {
        return E_OUTOFMEMORY;
      }
    }
  }

  pMap->pData = res->storage.empty() ? nullptr : res->storage.data();
  pMap->RowPitch = (res->kind == ResourceKind::Texture2D) ? res->row_pitch_bytes : 0;
  pMap->DepthPitch = 0;
  return S_OK;
}

void APIENTRY Unmap(D3D10DDI_HDEVICE hDevice, const D3D10DDIARG_UNMAP* pUnmap) {
  if (!hDevice.pDrvPrivate || !pUnmap || !pUnmap->hResource.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pUnmap->hResource);
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!res->storage.empty()) {
    auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = 0;
    upload->size_bytes = res->storage.size();
  }
}

void APIENTRY UpdateSubresourceUP(D3D10DDI_HDEVICE hDevice, const D3D10DDIARG_UPDATESUBRESOURCEUP* pUpdate) {
  if (!hDevice.pDrvPrivate || !pUpdate || !pUpdate->hDstResource.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pUpdate->hDstResource);
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!pUpdate->pSysMemUP) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    try {
      res->storage.resize(static_cast<size_t>(res->size_bytes));
    } catch (...) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    std::memcpy(res->storage.data(), pUpdate->pSysMemUP, res->storage.size());
  } else if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    const uint32_t row_pitch = res->row_pitch_bytes ? res->row_pitch_bytes
                                                    : (res->width * bytes_per_pixel_aerogpu(aer_fmt));
    const uint64_t total = static_cast<uint64_t>(row_pitch) * static_cast<uint64_t>(res->height);
    if (total > static_cast<uint64_t>(SIZE_MAX)) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    try {
      res->storage.resize(static_cast<size_t>(total));
    } catch (...) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    const uint8_t* src = static_cast<const uint8_t*>(pUpdate->pSysMemUP);
    const size_t src_pitch = pUpdate->RowPitch ? static_cast<size_t>(pUpdate->RowPitch) : static_cast<size_t>(row_pitch);
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * row_pitch,
                  src + static_cast<size_t>(y) * src_pitch,
                  row_pitch);
    }
  }

  if (!res->storage.empty()) {
    auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = 0;
    upload->size_bytes = res->storage.size();
  }
}

void APIENTRY CopyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hDst, D3D10DDI_HRESOURCE hSrc) {
  if (!hDevice.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hDst);
  auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hSrc);
  if (!dev || !dst || !src) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  try {
    dst->storage = src->storage;
  } catch (...) {
    SetError(hDevice, E_OUTOFMEMORY);
  }
}

void APIENTRY CopySubresourceRegion(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HRESOURCE hDst,
                                    UINT,
                                    UINT dstX,
                                    UINT dstY,
                                    UINT dstZ,
                                    D3D10DDI_HRESOURCE hSrc,
                                    UINT,
                                    const D3D10_DDI_BOX* pSrcBox) {
  // Common bring-up path: full-copy region with no box and zero destination offset.
  if (dstX != 0 || dstY != 0 || dstZ != 0 || pSrcBox) {
    SetError(hDevice, E_NOTIMPL);
    return;
  }
  CopyResource(hDevice, hDst, hSrc);
}

SIZE_T APIENTRY CalcPrivateRenderTargetViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT APIENTRY CreateRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                        D3D10DDI_HRENDERTARGETVIEW hView,
                                        D3D10DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* rtv = new (hView.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  return S_OK;
}

void APIENTRY DestroyRenderTargetView(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hView);
  view->~AeroGpuRenderTargetView();
}

SIZE_T APIENTRY CalcPrivateDepthStencilViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT APIENTRY CreateDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                        D3D10DDI_HDEPTHSTENCILVIEW hView,
                                        D3D10DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* dsv = new (hView.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  return S_OK;
}

void APIENTRY DestroyDepthStencilView(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hView);
  view->~AeroGpuDepthStencilView();
}

SIZE_T APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
                                          const D3D10DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                          D3D10DDI_HSHADERRESOURCEVIEW hView,
                                          D3D10DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* srv = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  srv->texture = res ? res->handle : 0;
  return S_OK;
}

void APIENTRY DestroyShaderResourceView(D3D10DDI_HDEVICE, D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  view->~AeroGpuShaderResourceView();
}

size_t dxbc_size_from_header(const void* pCode) {
  if (!pCode) {
    return 0;
  }
  const uint8_t* bytes = static_cast<const uint8_t*>(pCode);
  const uint32_t magic = *reinterpret_cast<const uint32_t*>(bytes);
  if (magic != 0x43425844u /* 'DXBC' */) {
    return 0;
  }

  // DXBC container stores the total size as a little-endian u32. The exact
  // offset is stable across SM4/SM5 containers in practice.
  const uint32_t candidates[] = {
      *reinterpret_cast<const uint32_t*>(bytes + 16),
      *reinterpret_cast<const uint32_t*>(bytes + 20),
      *reinterpret_cast<const uint32_t*>(bytes + 24),
  };
  for (size_t i = 0; i < sizeof(candidates) / sizeof(candidates[0]); i++) {
    const uint32_t sz = candidates[i];
    if (sz >= 32 && sz < (1u << 26) && (sz % 4) == 0) {
      return sz;
    }
  }
  return 0;
}

SIZE_T APIENTRY CalcPrivateVertexShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEVERTEXSHADER*) {
  return sizeof(AeroGpuShader);
}
SIZE_T APIENTRY CalcPrivatePixelShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEPIXELSHADER*) {
  return sizeof(AeroGpuShader);
}
SIZE_T APIENTRY CalcPrivateGeometryShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEGEOMETRYSHADER*) {
  return sizeof(AeroGpuShader);
}

HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                           const void* pCode,
                           size_t code_size,
                           D3D10DDI_HSHADER hShader,
                           uint32_t stage) {
  if (!hDevice.pDrvPrivate || !pCode || !code_size || !hShader.pDrvPrivate) {
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
  try {
    sh->dxbc.resize(code_size);
  } catch (...) {
    sh->~AeroGpuShader();
    return E_OUTOFMEMORY;
  }
  std::memcpy(sh->dxbc.data(), pCode, code_size);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                    const D3D10DDIARG_CREATEVERTEXSHADER* pDesc,
                                    D3D10DDI_HSHADER hShader,
                                    D3D10DDI_HRTSHADER) {
  if (!pDesc) {
    return E_INVALIDARG;
  }
  const void* code = nullptr;
  std::memcpy(&code, pDesc, sizeof(code));
  const size_t size = dxbc_size_from_header(code);
  return CreateShaderCommon(hDevice, code, size, hShader, AEROGPU_SHADER_STAGE_VERTEX);
}

HRESULT APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                   const D3D10DDIARG_CREATEPIXELSHADER* pDesc,
                                   D3D10DDI_HSHADER hShader,
                                   D3D10DDI_HRTSHADER) {
  if (!pDesc) {
    return E_INVALIDARG;
  }
  const void* code = nullptr;
  std::memcpy(&code, pDesc, sizeof(code));
  const size_t size = dxbc_size_from_header(code);
  return CreateShaderCommon(hDevice, code, size, hShader, AEROGPU_SHADER_STAGE_PIXEL);
}

HRESULT APIENTRY CreateGeometryShader(D3D10DDI_HDEVICE hDevice,
                                      const D3D10DDIARG_CREATEGEOMETRYSHADER*,
                                      D3D10DDI_HSHADER,
                                      D3D10DDI_HRTSHADER) {
  SetError(hDevice, E_NOTIMPL);
  return E_NOTIMPL;
}

void DestroyShaderCommon(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
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

void APIENTRY DestroyVertexShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}
void APIENTRY DestroyPixelShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}
void APIENTRY DestroyGeometryShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}

SIZE_T APIENTRY CalcPrivateElementLayoutSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEELEMENTLAYOUT*) {
  return sizeof(AeroGpuInputLayout);
}

HRESULT APIENTRY CreateElementLayout(D3D10DDI_HDEVICE hDevice,
                                     const D3D10DDIARG_CREATEELEMENTLAYOUT* pDesc,
                                     D3D10DDI_HELEMENTLAYOUT hLayout,
                                     D3D10DDI_HRTELEMENTLAYOUT) {
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }
  if (pDesc->NumElements && !pDesc->pVertexElements) {
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
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    layout->~AeroGpuInputLayout();
    return E_OUTOFMEMORY;
  }

  auto* hdr = reinterpret_cast<aerogpu_input_layout_blob_header*>(layout->blob.data());
  hdr->magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
  hdr->version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
  hdr->element_count = pDesc->NumElements;
  hdr->reserved0 = 0;

  auto* elems = reinterpret_cast<aerogpu_input_layout_element_dxgi*>(layout->blob.data() + sizeof(*hdr));
  for (uint32_t i = 0; i < pDesc->NumElements; i++) {
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
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void APIENTRY DestroyElementLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
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

SIZE_T APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEBLENDSTATE*) {
  return sizeof(AeroGpuBlendState);
}
HRESULT APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                  const D3D10DDIARG_CREATEBLENDSTATE*,
                                  D3D10DDI_HBLENDSTATE hState,
                                  D3D10DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  return S_OK;
}
void APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERASTERIZERSTATE*) {
  return sizeof(AeroGpuRasterizerState);
}
HRESULT APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                       const D3D10DDIARG_CREATERASTERIZERSTATE*,
                                       D3D10DDI_HRASTERIZERSTATE hState,
                                       D3D10DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  return S_OK;
}
void APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILSTATE*) {
  return sizeof(AeroGpuDepthStencilState);
}
HRESULT APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                         const D3D10DDIARG_CREATEDEPTHSTENCILSTATE*,
                                         D3D10DDI_HDEPTHSTENCILSTATE hState,
                                         D3D10DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  return S_OK;
}
void APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
}

SIZE_T APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}
HRESULT APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                               const D3D10DDIARG_CREATESAMPLER*,
                               D3D10DDI_HSAMPLER hSampler,
                               D3D10DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hSampler.pDrvPrivate) AeroGpuSampler();
  return S_OK;
}
void APIENTRY DestroySampler(D3D10DDI_HDEVICE, D3D10DDI_HSAMPLER hSampler) {
  if (!hSampler.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  s->~AeroGpuSampler();
}

void APIENTRY IaSetInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
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

void APIENTRY IaSetVertexBuffers(D3D10DDI_HDEVICE hDevice,
                                 UINT startSlot,
                                 UINT numBuffers,
                                 const D3D10DDI_HRESOURCE* phBuffers,
                                 const UINT* pStrides,
                                 const UINT* pOffsets) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (numBuffers && (!phBuffers || !pStrides || !pOffsets)) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Minimal bring-up: handle the common {start=0,count=1} case.
  if (startSlot != 0 || numBuffers != 1) {
    SetError(hDevice, E_NOTIMPL);
    return;
  }

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = phBuffers[0].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phBuffers[0])->handle : 0;
  binding.stride_bytes = pStrides[0];
  binding.offset_bytes = pOffsets[0];
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
  cmd->start_slot = 0;
  cmd->buffer_count = 1;
}

void APIENTRY IaSetIndexBuffer(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hBuffer, DXGI_FORMAT format, UINT offset) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format));
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void APIENTRY IaSetTopology(D3D10DDI_HDEVICE hDevice, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t topo_u32 = static_cast<uint32_t>(topology);
  if (dev->current_topology == topo_u32) {
    return;
  }
  dev->current_topology = topo_u32;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = topo_u32;
  cmd->reserved0 = 0;
}

void EmitBindShadersLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = dev->current_vs;
  cmd->ps = dev->current_ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void APIENTRY VsSetShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_vs = hShader.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader)->handle : 0;
  EmitBindShadersLocked(dev);
}

void APIENTRY PsSetShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->current_ps = hShader.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader)->handle : 0;
  EmitBindShadersLocked(dev);
}

void APIENTRY GsSetShader(D3D10DDI_HDEVICE, D3D10DDI_HSHADER) {
  // Stub (geometry shader stage not yet supported; valid for this stage to be unbound).
}

void APIENTRY VsSetConstantBuffers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HRESOURCE*) {
  // Stub (constant buffers not yet encoded into the command stream).
}
void APIENTRY PsSetConstantBuffers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HRESOURCE*) {
  // Stub (constant buffers not yet encoded into the command stream).
}
void APIENTRY GsSetConstantBuffers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HRESOURCE*) {
  // Stub.
}

void SetShaderResourcesCommon(D3D10DDI_HDEVICE hDevice,
                              uint32_t shader_stage,
                              UINT startSlot,
                              UINT numViews,
                              const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (numViews && !phViews) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (UINT i = 0; i < numViews; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i])->texture;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = shader_stage;
    cmd->slot = startSlot + i;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
}

void APIENTRY VsSetShaderResources(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numViews, const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_VERTEX, startSlot, numViews, phViews);
}
void APIENTRY PsSetShaderResources(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numViews, const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_PIXEL, startSlot, numViews, phViews);
}
void APIENTRY GsSetShaderResources(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HSHADERRESOURCEVIEW*) {
  // Stub.
}

void APIENTRY VsSetSamplers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HSAMPLER*) {
  // Stub (sampler objects not yet encoded into the command stream).
}
void APIENTRY PsSetSamplers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HSAMPLER*) {
  // Stub (sampler objects not yet encoded into the command stream).
}
void APIENTRY GsSetSamplers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HSAMPLER*) {
  // Stub.
}

void APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice, UINT numViewports, const D3D10_DDI_VIEWPORT* pViewports) {
  if (!hDevice.pDrvPrivate || !numViewports || !pViewports) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
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

void APIENTRY SetScissorRects(D3D10DDI_HDEVICE hDevice, UINT numRects, const D3D10_DDI_RECT* pRects) {
  if (!hDevice.pDrvPrivate || !numRects || !pRects) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const auto& r = pRects[0];
  const int32_t w = r.right - r.left;
  const int32_t h = r.bottom - r.top;
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  cmd->x = r.left;
  cmd->y = r.top;
  cmd->width = w;
  cmd->height = h;
}

void APIENTRY SetRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE) {
  // Stub.
}

void APIENTRY SetBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE, const FLOAT[4], UINT) {
  // Stub.
}

void APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE, UINT) {
  // Stub.
}

void APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice,
                               UINT numViews,
                               const D3D10DDI_HRENDERTARGETVIEW* phViews,
                               D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t rtv_handle = 0;
  aerogpu_handle_t dsv_handle = 0;
  if (numViews && phViews && phViews[0].pDrvPrivate) {
    rtv_handle = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phViews[0])->texture;
  }
  if (hDsv.pDrvPrivate) {
    dsv_handle = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture;
  }

  dev->current_rtv = rtv_handle;
  dev->current_dsv = dsv_handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = numViews ? 1 : 0;
  cmd->depth_stencil = dsv_handle;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  cmd->colors[0] = rtv_handle;
}

void APIENTRY ClearRenderTargetView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW, const FLOAT color[4]) {
  if (!hDevice.pDrvPrivate || !color) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(color[0]);
  cmd->color_rgba_f32[1] = f32_bits(color[1]);
  cmd->color_rgba_f32[2] = f32_bits(color[2]);
  cmd->color_rgba_f32[3] = f32_bits(color[3]);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;
}

void APIENTRY ClearDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HDEPTHSTENCILVIEW,
                                    UINT clearFlags,
                                    FLOAT depth,
                                    UINT8 stencil) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t flags = 0;
  if (clearFlags & 0x1u) {
    flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (clearFlags & 0x2u) {
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

void APIENTRY Draw(D3D10DDI_HDEVICE hDevice, UINT vertexCount, UINT startVertex) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = vertexCount;
  cmd->instance_count = 1;
  cmd->first_vertex = startVertex;
  cmd->first_instance = 0;
}

void APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, UINT indexCount, UINT startIndex, INT baseVertex) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = indexCount;
  cmd->instance_count = 1;
  cmd->first_index = startIndex;
  cmd->base_vertex = baseVertex;
  cmd->first_instance = 0;
}

void APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
  cmd->reserved0 = 0;
  cmd->reserved1 = 0;
  submit_locked(dev);
}

HRESULT APIENTRY Present(D3D10DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent) {
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
  cmd->flags = (pPresent->SyncInterval >= 1) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

  submit_locked(dev);
  return S_OK;
}

void APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE* phResources, UINT numResources) {
  if (!hDevice.pDrvPrivate || !phResources || numResources < 2) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* first = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[0]);
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;

  for (UINT i = 0; i + 1 < numResources; ++i) {
    auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[i]);
    auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[i + 1]);
    if (!dst || !src) {
      return;
    }
    dst->handle = src->handle;
  }

  auto* last = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[numResources - 1]);
  if (last) {
    last->handle = saved;
  }
}

// -----------------------------------------------------------------------------
// Adapter DDI
// -----------------------------------------------------------------------------

template <typename T, typename = void>
struct has_FormatSupport2 : std::false_type {};

template <typename T>
struct has_FormatSupport2<T, std::void_t<decltype(&T::FormatSupport2)>> : std::true_type {};

HRESULT APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pCaps) {
  if (!pCaps || !pCaps->pData) {
    return E_INVALIDARG;
  }

  DebugLog("aerogpu-d3d10: GetCaps type=%u size=%u\n", (unsigned)pCaps->Type, (unsigned)pCaps->DataSize);

  if (pCaps->DataSize) {
    std::memset(pCaps->pData, 0, pCaps->DataSize);
  }

  switch (pCaps->Type) {
    case D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    case D3D10DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        const uint32_t format = static_cast<uint32_t>(fmt->Format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatB8G8R8X8Unorm:
          case kDxgiFormatR8G8B8A8Unorm:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE;
            break;
          case kDxgiFormatR32G32B32A32Float:
          case kDxgiFormatR32G32B32Float:
          case kDxgiFormatR32G32Float:
            support = D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER;
            break;
          case kDxgiFormatR16Uint:
          case kDxgiFormatR32Uint:
            support = D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER;
            break;
          case kDxgiFormatD24UnormS8Uint:
          case kDxgiFormatD32Float:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_DEPTH_STENCIL;
            break;
          default:
            support = 0;
            break;
        }

        fmt->FormatSupport = support;
        if constexpr (has_FormatSupport2<D3D10DDIARG_FORMAT_SUPPORT>::value) {
          fmt->FormatSupport2 = 0;
        }
      }
      break;

    default:
      break;
  }

  return S_OK;
}

SIZE_T APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuDevice);
}

HRESULT APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  if (!pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->callbacks = pCreateDevice->pCallbacks;
  if (!device->callbacks) {
    device->~AeroGpuDevice();
    return E_INVALIDARG;
  }

  // Populate the full D3D10DDI_DEVICEFUNCS table. Any unimplemented entrypoints
  // should be wired to a stub rather than left NULL; this prevents hard crashes
  // from null vtable calls during runtime bring-up.
  D3D10DDI_DEVICEFUNCS funcs;
  std::memset(&funcs, 0, sizeof(funcs));

  // Optional/rare entrypoints: default them to safe stubs so the runtime never
  // sees NULL function pointers for features we don't support yet.
  if constexpr (has_pfnDrawInstanced<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDrawInstanced = &NotImpl<decltype(funcs.pfnDrawInstanced)>::Fn;
  }
  if constexpr (has_pfnDrawIndexedInstanced<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDrawIndexedInstanced = &NotImpl<decltype(funcs.pfnDrawIndexedInstanced)>::Fn;
  }
  if constexpr (has_pfnDrawAuto<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDrawAuto = &NotImpl<decltype(funcs.pfnDrawAuto)>::Fn;
  }
  if constexpr (has_pfnSoSetTargets<D3D10DDI_DEVICEFUNCS>::value) {
    // Valid to leave SO unbound for bring-up; treat as a no-op.
    funcs.pfnSoSetTargets = &Noop<decltype(funcs.pfnSoSetTargets)>::Fn;
  }
  if constexpr (has_pfnSetPredication<D3D10DDI_DEVICEFUNCS>::value) {
    // Predication is rarely used; ignore for now.
    funcs.pfnSetPredication = &Noop<decltype(funcs.pfnSetPredication)>::Fn;
  }
  if constexpr (has_pfnSetTextFilterSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnSetTextFilterSize = &Noop<decltype(funcs.pfnSetTextFilterSize)>::Fn;
  }
  if constexpr (has_pfnGenMips<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnGenMips = &Noop<decltype(funcs.pfnGenMips)>::Fn;
  }
  if constexpr (has_pfnGenerateMips<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnGenerateMips = &Noop<decltype(funcs.pfnGenerateMips)>::Fn;
  }
  if constexpr (has_pfnResolveSubresource<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnResolveSubresource = &NotImpl<decltype(funcs.pfnResolveSubresource)>::Fn;
  }
  if constexpr (has_pfnClearState<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnClearState = &Noop<decltype(funcs.pfnClearState)>::Fn;
  }
  if constexpr (has_pfnCalcPrivateQuerySize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivateQuerySize = &NotImpl<decltype(funcs.pfnCalcPrivateQuerySize)>::Fn;
  }
  if constexpr (has_pfnCreateQuery<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreateQuery = &NotImpl<decltype(funcs.pfnCreateQuery)>::Fn;
  }
  if constexpr (has_pfnDestroyQuery<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDestroyQuery = &NotImpl<decltype(funcs.pfnDestroyQuery)>::Fn;
  }
  if constexpr (has_pfnCalcPrivatePredicateSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivatePredicateSize = &NotImpl<decltype(funcs.pfnCalcPrivatePredicateSize)>::Fn;
  }
  if constexpr (has_pfnCreatePredicate<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreatePredicate = &NotImpl<decltype(funcs.pfnCreatePredicate)>::Fn;
  }
  if constexpr (has_pfnDestroyPredicate<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDestroyPredicate = &NotImpl<decltype(funcs.pfnDestroyPredicate)>::Fn;
  }
  if constexpr (has_pfnCalcPrivateCounterSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivateCounterSize = &NotImpl<decltype(funcs.pfnCalcPrivateCounterSize)>::Fn;
  }
  if constexpr (has_pfnCreateCounter<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreateCounter = &NotImpl<decltype(funcs.pfnCreateCounter)>::Fn;
  }
  if constexpr (has_pfnDestroyCounter<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDestroyCounter = &NotImpl<decltype(funcs.pfnDestroyCounter)>::Fn;
  }
  if constexpr (has_pfnCalcPrivateGeometryShaderWithStreamOutputSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        &NotImpl<decltype(funcs.pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Fn;
  }
  if constexpr (has_pfnCreateGeometryShaderWithStreamOutput<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreateGeometryShaderWithStreamOutput =
        &NotImpl<decltype(funcs.pfnCreateGeometryShaderWithStreamOutput)>::Fn;
  }

  // Lifecycle.
  funcs.pfnDestroyDevice = &DestroyDevice;

  // Resources.
  funcs.pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  funcs.pfnCreateResource = &CreateResource;
  funcs.pfnDestroyResource = &DestroyResource;
  funcs.pfnMap = &Map;
  funcs.pfnUnmap = &Unmap;
  funcs.pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  funcs.pfnCopyResource = &CopyResource;
  funcs.pfnCopySubresourceRegion = &CopySubresourceRegion;

  // Views.
  funcs.pfnCalcPrivateRenderTargetViewSize = &CalcPrivateRenderTargetViewSize;
  funcs.pfnCreateRenderTargetView = &CreateRenderTargetView;
  funcs.pfnDestroyRenderTargetView = &DestroyRenderTargetView;

  funcs.pfnCalcPrivateDepthStencilViewSize = &CalcPrivateDepthStencilViewSize;
  funcs.pfnCreateDepthStencilView = &CreateDepthStencilView;
  funcs.pfnDestroyDepthStencilView = &DestroyDepthStencilView;

  funcs.pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize;
  funcs.pfnCreateShaderResourceView = &CreateShaderResourceView;
  funcs.pfnDestroyShaderResourceView = &DestroyShaderResourceView;

  // Shaders.
  funcs.pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize;
  funcs.pfnCreateVertexShader = &CreateVertexShader;
  funcs.pfnDestroyVertexShader = &DestroyVertexShader;

  funcs.pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize;
  funcs.pfnCreatePixelShader = &CreatePixelShader;
  funcs.pfnDestroyPixelShader = &DestroyPixelShader;

  funcs.pfnCalcPrivateGeometryShaderSize = &CalcPrivateGeometryShaderSize;
  funcs.pfnCreateGeometryShader = &CreateGeometryShader;
  funcs.pfnDestroyGeometryShader = &DestroyGeometryShader;

  // Input layout.
  funcs.pfnCalcPrivateElementLayoutSize = &CalcPrivateElementLayoutSize;
  funcs.pfnCreateElementLayout = &CreateElementLayout;
  funcs.pfnDestroyElementLayout = &DestroyElementLayout;

  // State objects.
  funcs.pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  funcs.pfnCreateBlendState = &CreateBlendState;
  funcs.pfnDestroyBlendState = &DestroyBlendState;

  funcs.pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  funcs.pfnCreateRasterizerState = &CreateRasterizerState;
  funcs.pfnDestroyRasterizerState = &DestroyRasterizerState;

  funcs.pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  funcs.pfnCreateDepthStencilState = &CreateDepthStencilState;
  funcs.pfnDestroyDepthStencilState = &DestroyDepthStencilState;

  funcs.pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize;
  funcs.pfnCreateSampler = &CreateSampler;
  funcs.pfnDestroySampler = &DestroySampler;

  // Binding/state setting.
  funcs.pfnIaSetInputLayout = &IaSetInputLayout;
  funcs.pfnIaSetVertexBuffers = &IaSetVertexBuffers;
  funcs.pfnIaSetIndexBuffer = &IaSetIndexBuffer;
  funcs.pfnIaSetTopology = &IaSetTopology;

  funcs.pfnVsSetShader = &VsSetShader;
  funcs.pfnVsSetConstantBuffers = &VsSetConstantBuffers;
  funcs.pfnVsSetShaderResources = &VsSetShaderResources;
  funcs.pfnVsSetSamplers = &VsSetSamplers;

  funcs.pfnGsSetShader = &GsSetShader;
  funcs.pfnGsSetConstantBuffers = &GsSetConstantBuffers;
  funcs.pfnGsSetShaderResources = &GsSetShaderResources;
  funcs.pfnGsSetSamplers = &GsSetSamplers;

  funcs.pfnPsSetShader = &PsSetShader;
  funcs.pfnPsSetConstantBuffers = &PsSetConstantBuffers;
  funcs.pfnPsSetShaderResources = &PsSetShaderResources;
  funcs.pfnPsSetSamplers = &PsSetSamplers;

  funcs.pfnSetViewports = &SetViewports;
  funcs.pfnSetScissorRects = &SetScissorRects;
  funcs.pfnSetRasterizerState = &SetRasterizerState;
  funcs.pfnSetBlendState = &SetBlendState;
  funcs.pfnSetDepthStencilState = &SetDepthStencilState;
  funcs.pfnSetRenderTargets = &SetRenderTargets;

  // Clears/draw.
  funcs.pfnClearRenderTargetView = &ClearRenderTargetView;
  funcs.pfnClearDepthStencilView = &ClearDepthStencilView;
  funcs.pfnDraw = &Draw;
  funcs.pfnDrawIndexed = &DrawIndexed;

  // Present.
  funcs.pfnFlush = &Flush;
  funcs.pfnPresent = &Present;
  funcs.pfnRotateResourceIdentities = &RotateResourceIdentities;

  *pCreateDevice->pDeviceFuncs = funcs;
  return S_OK;
}

void APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -----------------------------------------------------------------------------
// Exports (OpenAdapter10 / OpenAdapter10_2)
// -----------------------------------------------------------------------------

HRESULT OpenAdapterCommon(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  if (pOpenData->Interface != D3D10DDI_INTERFACE_VERSION) {
    return E_INVALIDARG;
  }
  // `Version` is treated as an in/out negotiation field by some runtimes. If the
  // runtime doesn't initialize it, accept 0 and return the supported D3D10 DDI
  // version.
  if (pOpenData->Version == 0) {
    pOpenData->Version = D3D10DDI_SUPPORTED;
  } else if (pOpenData->Version < D3D10DDI_SUPPORTED) {
    return E_INVALIDARG;
  }
  if (pOpenData->Version > D3D10DDI_SUPPORTED) {
    pOpenData->Version = D3D10DDI_SUPPORTED;
  }

  auto* adapter = new (std::nothrow) AeroGpuAdapter();
  if (!adapter) {
    return E_OUTOFMEMORY;
  }

  __if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
    adapter->callbacks = pOpenData->pAdapterCallbacks;
  }
  pOpenData->hAdapter.pDrvPrivate = adapter;

  D3D10DDI_ADAPTERFUNCS funcs;
  std::memset(&funcs, 0, sizeof(funcs));
  funcs.pfnGetCaps = &GetCaps;
  funcs.pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
  funcs.pfnCreateDevice = &CreateDevice;
  funcs.pfnCloseAdapter = &CloseAdapter;

  auto* out_funcs = reinterpret_cast<D3D10DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  if (!out_funcs) {
    return E_INVALIDARG;
  }
  *out_funcs = funcs;
  return S_OK;
}

} // namespace

HRESULT AEROGPU_APIENTRY AeroGpuOpenAdapter10Wdk(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapterCommon(pOpenData);
}

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
