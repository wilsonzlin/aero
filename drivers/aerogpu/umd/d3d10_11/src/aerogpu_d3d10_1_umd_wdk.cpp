// AeroGPU Windows 7 D3D10.1 UMD DDI glue.
//
// This translation unit is compiled only when the official D3D10/10.1 DDI
// headers are available (Windows SDK/WDK). The repository build (no WDK) keeps a
// minimal compat implementation in `aerogpu_d3d10_11_umd.cpp`.
//
// The goal of this file is to let the Win7 D3D10.1 runtime (`d3d10_1.dll`)
// negotiate a 10.1-capable interface via `OpenAdapter10_2`, create a device, and
// drive the minimal draw/present path.
//
// NOTE: This intentionally keeps capability reporting conservative (FL10_0
// baseline) and stubs unsupported entrypoints with safe defaults.

#include "../include/aerogpu_d3d10_11_umd.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#include <d3d10_1umddi.h>
#include <d3d10_1.h>
#include <d3dkmthk.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <limits>
#include <mutex>
#include <new>
#include <type_traits>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"

#ifndef NT_SUCCESS
  #define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_TIMEOUT
  #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
#endif

namespace {

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING

// D3D10_BIND_* subset (numeric values from d3d10.h).
constexpr uint32_t kD3D10BindVertexBuffer = 0x1;
constexpr uint32_t kD3D10BindIndexBuffer = 0x2;
constexpr uint32_t kD3D10BindConstantBuffer = 0x4;
constexpr uint32_t kD3D10BindShaderResource = 0x8;
constexpr uint32_t kD3D10BindRenderTarget = 0x20;
constexpr uint32_t kD3D10BindDepthStencil = 0x40;

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

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;

  // Optional D3DKMT adapter handle for dev-only escapes (e.g. QUERY_FENCE).
  // This is best-effort bring-up plumbing; the real submission path should use
  // runtime callbacks and context-owned sync objects instead.
  D3DKMT_HANDLE kmt_adapter = 0;
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

  // Map state (for UP resources backed by `storage`).
  bool mapped = false;
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;

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

struct AeroGpuBlendState {
  uint32_t dummy = 0;
};

struct AeroGpuRasterizerState {
  uint32_t dummy = 0;
};

struct AeroGpuDepthStencilState {
  uint32_t dummy = 0;
};

using SetErrorFn = void(AEROGPU_APIENTRY*)(D3D10DDI_HRTDEVICE, HRESULT);

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  D3D10DDI_HRTDEVICE hrt_device{};
  SetErrorFn pfn_set_error = nullptr;

  aerogpu::CmdWriter cmd;

  // Fence tracking for WDDM-backed synchronization (used by Map READ / DO_NOT_WAIT semantics).
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  // Monitored fence state for Win7/WDDM 1.1.
  // These fields are expected to be initialized by the real WDDM submission path.
  D3DKMT_HANDLE kmt_fence_syncobj = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
  D3DKMT_HANDLE kmt_adapter = 0;

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

void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

struct AeroGpuD3dkmtProcs {
  decltype(&D3DKMTOpenAdapterFromHdc) pfn_open_adapter_from_hdc = nullptr;
  decltype(&D3DKMTCloseAdapter) pfn_close_adapter = nullptr;
  decltype(&D3DKMTEscape) pfn_escape = nullptr;
  decltype(&D3DKMTWaitForSynchronizationObject) pfn_wait_for_syncobj = nullptr;
};

const AeroGpuD3dkmtProcs& GetAeroGpuD3dkmtProcs() {
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
    p.pfn_close_adapter =
        reinterpret_cast<decltype(&D3DKMTCloseAdapter)>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
    p.pfn_escape = reinterpret_cast<decltype(&D3DKMTEscape)>(GetProcAddress(gdi32, "D3DKMTEscape"));
    p.pfn_wait_for_syncobj = reinterpret_cast<decltype(&D3DKMTWaitForSynchronizationObject)>(
        GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject"));
    return p;
  }();
  return procs;
}

void InitKmtAdapterHandle(AeroGpuAdapter* adapter) {
  if (!adapter || adapter->kmt_adapter) {
    return;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_open_adapter_from_hdc) {
    return;
  }

  HDC hdc = GetDC(nullptr);
  if (!hdc) {
    return;
  }

  D3DKMT_OPENADAPTERFROMHDC open{};
  open.hDc = hdc;
  open.hAdapter = 0;
  std::memset(&open.AdapterLuid, 0, sizeof(open.AdapterLuid));
  open.VidPnSourceId = 0;

  const NTSTATUS st = procs.pfn_open_adapter_from_hdc(&open);
  ReleaseDC(nullptr, hdc);

  if (NT_SUCCESS(st) && open.hAdapter) {
    adapter->kmt_adapter = open.hAdapter;
  }
}

void DestroyKmtAdapterHandle(AeroGpuAdapter* adapter) {
  if (!adapter || !adapter->kmt_adapter) {
    return;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (procs.pfn_close_adapter) {
    D3DKMT_CLOSEADAPTER close{};
    close.hAdapter = adapter->kmt_adapter;
    (void)procs.pfn_close_adapter(&close);
  }

  adapter->kmt_adapter = 0;
}

void UpdateCompletedFence(AeroGpuDevice* dev, uint64_t completed) {
  if (!dev) {
    return;
  }

  atomic_max_u64(&dev->last_completed_fence, completed);

  if (!dev->adapter) {
    return;
  }

  AeroGpuAdapter* adapter = dev->adapter;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->completed_fence < completed) {
      adapter->completed_fence = completed;
    }
  }
  adapter->fence_cv.notify_all();
}

uint64_t AeroGpuQueryCompletedFence(AeroGpuDevice* dev) {
  if (!dev) {
    return 0;
  }

  if (dev->monitored_fence_value) {
    const uint64_t completed = *dev->monitored_fence_value;
    UpdateCompletedFence(dev, completed);
    return completed;
  }

  // Dev-only fallback: ask the KMD for its fence tracking state via Escape.
  if (dev->kmt_adapter) {
    const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
    if (procs.pfn_escape) {
      aerogpu_escape_query_fence_out q{};
      q.hdr.version = AEROGPU_ESCAPE_VERSION;
      q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
      q.hdr.size = sizeof(q);
      q.hdr.reserved0 = 0;

      D3DKMT_ESCAPE e{};
      e.hAdapter = dev->kmt_adapter;
      e.hDevice = 0;
      e.hContext = 0;
      e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
      e.Flags.Value = 0;
      e.pPrivateDriverData = &q;
      e.PrivateDriverDataSize = sizeof(q);

      const NTSTATUS st = procs.pfn_escape(&e);
      if (NT_SUCCESS(st)) {
        atomic_max_u64(&dev->last_submitted_fence, static_cast<uint64_t>(q.last_submitted_fence));
        UpdateCompletedFence(dev, static_cast<uint64_t>(q.last_completed_fence));
      }
    }
  }

  if (dev->adapter) {
    uint64_t completed = 0;
    {
      std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
      completed = dev->adapter->completed_fence;
    }
    UpdateCompletedFence(dev, completed);
  }

  return dev->last_completed_fence.load(std::memory_order_relaxed);
}

// Waits for `fence` to be completed. `timeout_ms == 0` means "infinite wait".
//
// On timeout, returns `DXGI_ERROR_WAS_STILL_DRAWING` (useful for D3D11 Map DO_NOT_WAIT).
HRESULT AeroGpuWaitForFence(AeroGpuDevice* dev, uint64_t fence, uint32_t timeout_ms) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (fence == 0) {
    return S_OK;
  }

  if (AeroGpuQueryCompletedFence(dev) >= fence) {
    return S_OK;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (dev->kmt_fence_syncobj && procs.pfn_wait_for_syncobj) {
    const D3DKMT_HANDLE handles[1] = {dev->kmt_fence_syncobj};
    const UINT64 fence_values[1] = {fence};

    D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
    args.hAdapter = dev->kmt_adapter;
    args.ObjectCount = 1;
    args.ObjectHandleArray = handles;
    args.FenceValueArray = fence_values;
    args.Timeout = timeout_ms ? static_cast<UINT64>(timeout_ms) : ~0ull;

    const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
    if (st == STATUS_TIMEOUT) {
      return kDxgiErrorWasStillDrawing;
    }
    if (!NT_SUCCESS(st)) {
      return E_FAIL;
    }

    (void)AeroGpuQueryCompletedFence(dev);
    return S_OK;
  }

  // Fallback for bring-up: treat submissions as synchronous and wait on the local CV.
  if (!dev->adapter) {
    return E_FAIL;
  }

  AeroGpuAdapter* adapter = dev->adapter;
  std::unique_lock<std::mutex> lock(adapter->fence_mutex);
  auto ready = [&] { return adapter->completed_fence >= fence; };

  if (ready()) {
    atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
    return S_OK;
  }

  if (timeout_ms == 0) {
    adapter->fence_cv.wait(lock, ready);
    atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
    return S_OK;
  }

  if (!adapter->fence_cv.wait_for(lock, std::chrono::milliseconds(timeout_ms), ready)) {
    return kDxgiErrorWasStillDrawing;
  }

  atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
  return S_OK;
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
  const bool complete_immediately = (dev->kmt_fence_syncobj == 0 && dev->monitored_fence_value == nullptr);
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    if (complete_immediately) {
      adapter->completed_fence = fence;
    }
  }
  if (complete_immediately) {
    adapter->fence_cv.notify_all();
  }

  atomic_max_u64(&dev->last_submitted_fence, fence);
  if (complete_immediately) {
    atomic_max_u64(&dev->last_completed_fence, fence);
  }

  dev->cmd.reset();
  return fence;
}

void flush_locked(AeroGpuDevice* dev) {
  submit_locked(dev);
}

void set_error(AeroGpuDevice* dev, HRESULT hr) {
  if (!dev || !dev->pfn_set_error || !dev->hrt_device.pDrvPrivate) {
    return;
  }
  dev->pfn_set_error(dev->hrt_device, hr);
}

void emit_upload_resource_locked(AeroGpuDevice* dev,
                                 const AeroGpuResource* res,
                                 uint64_t offset_bytes,
                                 uint64_t size_bytes) {
  if (!dev || !res || res->handle == kInvalidHandle || !size_bytes) {
    return;
  }

  if (offset_bytes > res->storage.size()) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  const size_t remaining = res->storage.size() - static_cast<size_t>(offset_bytes);
  if (size_bytes > remaining) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (size_bytes > std::numeric_limits<size_t>::max()) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }

  const uint8_t* payload = res->storage.data() + static_cast<size_t>(offset_bytes);
  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
      AEROGPU_CMD_UPLOAD_RESOURCE, payload, static_cast<size_t>(size_bytes));
  if (!cmd) {
    set_error(dev, E_FAIL);
    return;
  }
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
      return 0;
    } else if constexpr (std::is_same_v<Ret, void>) {
      return;
    } else {
      return Ret{};
    }
  }
};

// -------------------------------------------------------------------------------------------------
// D3D10.1 Device DDI (minimal subset + conservative stubs)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
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

  // The Win7 DDI passes a superset of D3D10_RESOURCE_DIMENSION/D3D11_RESOURCE_DIMENSION.
  // For bring-up we only accept buffers and 2D textures.
  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Buffer;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

    if (pDesc->pInitialDataUP) {
      const auto& init = pDesc->pInitialDataUP[0];
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
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      emit_upload_resource_locked(dev, res, 0, res->storage.size());
    }
    return S_OK;
  }

  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_TEXTURE2D) {
    if (pDesc->ArraySize != 1) {
      return E_NOTIMPL;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_NOTIMPL;
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Texture2D;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    if (!pDesc->pMipInfoList) {
      res->~AeroGpuResource();
      return E_INVALIDARG;
    }
    res->width = pDesc->pMipInfoList[0].TexelWidth;
    res->height = pDesc->pMipInfoList[0].TexelHeight;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);
    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);

    if (pDesc->pInitialDataUP) {
      if (res->mip_levels != 1 || res->array_size != 1) {
        res->~AeroGpuResource();
        return E_NOTIMPL;
      }

      const auto& init = pDesc->pInitialDataUP[0];
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
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      emit_upload_resource_locked(dev, res, 0, res->storage.size());
    }
    return S_OK;
  }

  return E_NOTIMPL;
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
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

SIZE_T AEROGPU_APIENTRY CalcPrivateVertexShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEVERTEXSHADER*) {
  return sizeof(AeroGpuShader);
}

SIZE_T AEROGPU_APIENTRY CalcPrivatePixelShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEPIXELSHADER*) {
  return sizeof(AeroGpuShader);
}

template <typename TShaderHandle>
static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const void* pCode,
                                  SIZE_T code_size,
                                  TShaderHandle hShader,
                                  uint32_t stage) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate || !pCode || !code_size) {
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
  sh->dxbc.resize(code_size);
  std::memcpy(sh->dxbc.data(), pCode, code_size);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const D3D10DDIARG_CREATEVERTEXSHADER* pDesc,
                                            D3D10DDI_HVERTEXSHADER hShader,
                                            D3D10DDI_HRTVERTEXSHADER) {
  if (!pDesc) {
    return E_INVALIDARG;
  }
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->CodeSize, hShader, AEROGPU_SHADER_STAGE_VERTEX);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const D3D10DDIARG_CREATEPIXELSHADER* pDesc,
                                           D3D10DDI_HPIXELSHADER hShader,
                                           D3D10DDI_HRTPIXELSHADER) {
  if (!pDesc) {
    return E_INVALIDARG;
  }
  return CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->CodeSize, hShader, AEROGPU_SHADER_STAGE_PIXEL);
}

template <typename TShaderHandle>
void DestroyShaderCommon(D3D10DDI_HDEVICE hDevice, TShaderHandle hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sh = reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate);
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

void AEROGPU_APIENTRY DestroyVertexShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HVERTEXSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}

void AEROGPU_APIENTRY DestroyPixelShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HPIXELSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateElementLayoutSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEELEMENTLAYOUT*) {
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateElementLayout(D3D10DDI_HDEVICE hDevice,
                                             const D3D10DDIARG_CREATEELEMENTLAYOUT* pDesc,
                                             D3D10DDI_HELEMENTLAYOUT hLayout,
                                             D3D10DDI_HRTELEMENTLAYOUT) {
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate) {
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

void AEROGPU_APIENTRY DestroyElementLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
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

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                                const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                                D3D10DDI_HRENDERTARGETVIEW hRtv,
                                                D3D10DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !pDesc->hDrvResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hDrvResource);
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRenderTargetView(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                                const D3D10DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                                D3D10DDI_HDEPTHSTENCILVIEW hDsv,
                                                D3D10DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hDsv.pDrvPrivate || !pDesc->hDrvResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hDrvResource);
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilView(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  dsv->~AeroGpuDepthStencilView();
}

void AEROGPU_APIENTRY ClearDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                            D3D10DDI_HDEPTHSTENCILVIEW,
                                            UINT clear_flags,
                                            FLOAT depth,
                                            UINT8 stencil) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t flags = 0;
  if (clear_flags & D3D10_DDI_CLEAR_DEPTH) {
    flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (clear_flags & D3D10_DDI_CLEAR_STENCIL) {
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

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const D3D10_1_DDI_BLEND_DESC*) {
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const D3D10_1_DDI_BLEND_DESC*,
                                          D3D10DDI_HBLENDSTATE hState,
                                          D3D10DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const D3D10_DDI_RASTERIZER_DESC*) {
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const D3D10_DDI_RASTERIZER_DESC*,
                                               D3D10DDI_HRASTERIZERSTATE hState,
                                               D3D10DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE, const D3D10_DDI_DEPTH_STENCIL_DESC*) {
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const D3D10_DDI_DEPTH_STENCIL_DESC*,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState,
                                                 D3D10DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
}

void AEROGPU_APIENTRY ClearRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                            D3D10DDI_HRENDERTARGETVIEW,
                                            const FLOAT rgba[4]) {
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

void AEROGPU_APIENTRY IaSetInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
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

void AEROGPU_APIENTRY IaSetVertexBuffers(D3D10DDI_HDEVICE hDevice,
                                         UINT start_slot,
                                         UINT buffer_count,
                                         const D3D10DDI_HRESOURCE* pBuffers,
                                         const UINT* pStrides,
                                         const UINT* pOffsets) {
  if (!hDevice.pDrvPrivate || !pBuffers || !pStrides || !pOffsets) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  // Minimal: only slot 0 is wired up.
  if (start_slot != 0 || buffer_count == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = pBuffers[0].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pBuffers[0])->handle : 0;
  binding.stride_bytes = pStrides[0];
  binding.offset_bytes = pOffsets[0];
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS,
                                                                           &binding,
                                                                           sizeof(binding));
  cmd->start_slot = 0;
  cmd->buffer_count = 1;
}

void AEROGPU_APIENTRY IaSetIndexBuffer(D3D10DDI_HDEVICE hDevice,
                                       D3D10DDI_HRESOURCE hBuffer,
                                       DXGI_FORMAT format,
                                       UINT offset) {
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
  cmd->format = dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format));
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetTopology(D3D10DDI_HDEVICE hDevice, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
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

void AEROGPU_APIENTRY VsSetShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HVERTEXSHADER hShader) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->current_vs = hShader.pDrvPrivate ? reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate)->handle : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = dev->current_vs;
  cmd->ps = dev->current_ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY PsSetShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HPIXELSHADER hShader) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->current_ps = hShader.pDrvPrivate ? reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate)->handle : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = dev->current_vs;
  cmd->ps = dev->current_ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice, UINT num_viewports, const D3D10_DDI_VIEWPORT* pViewports) {
  if (!hDevice.pDrvPrivate || !pViewports || num_viewports == 0) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  const auto& vp = pViewports[0];

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(vp.TopLeftX);
  cmd->y_f32 = f32_bits(vp.TopLeftY);
  cmd->width_f32 = f32_bits(vp.Width);
  cmd->height_f32 = f32_bits(vp.Height);
  cmd->min_depth_f32 = f32_bits(vp.MinDepth);
  cmd->max_depth_f32 = f32_bits(vp.MaxDepth);
}

void AEROGPU_APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice,
                                       const D3D10DDI_HRENDERTARGETVIEW* pRTVs,
                                       UINT num_rtvs,
                                       D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDevice.pDrvPrivate || !pRTVs || num_rtvs == 0) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t rtv_handle = 0;
  if (pRTVs[0].pDrvPrivate) {
    rtv_handle = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(pRTVs[0])->texture;
  }

  aerogpu_handle_t dsv_handle = 0;
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

void AEROGPU_APIENTRY Draw(D3D10DDI_HDEVICE hDevice, UINT vertex_count, UINT start_vertex) {
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

void AEROGPU_APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, UINT index_count, UINT start_index, INT base_vertex) {
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

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent) {
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

void AEROGPU_APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  flush_locked(dev);
}

void AEROGPU_APIENTRY Map(D3D10DDI_HDEVICE hDevice,
                          const D3D10DDIARG_MAP* pMap,
                          D3D10DDI_MAPPED_SUBRESOURCE* pOut) {
  if (!hDevice.pDrvPrivate || !pMap || !pOut) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pMap->hResource);
  if (!res) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (pMap->Subresource != 0) {
    set_error(dev, E_NOTIMPL);
    return;
  }
  if (res->mapped) {
    set_error(dev, E_FAIL);
    return;
  }

  // Lazily allocate CPU backing so dynamic resources can be updated.
  if (res->storage.empty()) {
    try {
      if (res->kind == ResourceKind::Buffer && res->size_bytes) {
        res->storage.resize(static_cast<size_t>(res->size_bytes), 0);
      } else if (res->kind == ResourceKind::Texture2D && res->width && res->height && res->row_pitch_bytes) {
        res->storage.resize(static_cast<size_t>(res->row_pitch_bytes) * res->height, 0);
      }
    } catch (...) {
      set_error(dev, E_OUTOFMEMORY);
      return;
    }
  }

  res->mapped = true;
  res->mapped_offset = 0;
  res->mapped_size = res->storage.size();

  pOut->pData = res->storage.empty() ? nullptr : res->storage.data();
  pOut->RowPitch = (res->kind == ResourceKind::Texture2D) ? res->row_pitch_bytes : 0;
  pOut->DepthPitch = 0;
}

void AEROGPU_APIENTRY Unmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, UINT subresource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!res) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (subresource != 0) {
    set_error(dev, E_NOTIMPL);
    return;
  }
  if (!res->mapped) {
    set_error(dev, E_FAIL);
    return;
  }

  res->mapped = false;
  if (!res->storage.empty()) {
    emit_upload_resource_locked(dev, res, res->mapped_offset, res->mapped_size);
  }
  res->mapped_offset = 0;
  res->mapped_size = 0;
}

void AEROGPU_APIENTRY UpdateSubresourceUP(D3D10DDI_HDEVICE hDevice,
                                         const D3D10DDIARG_UPDATESUBRESOURCEUP* pArgs,
                                         const void* pSysMem) {
  if (!hDevice.pDrvPrivate || !pArgs || !pSysMem) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pArgs->hDstResource);
  if (!res) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (pArgs->DstSubresource != 0 || pArgs->pDstBox) {
    set_error(dev, E_NOTIMPL);
    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    if (res->storage.empty()) {
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes), 0);
      } catch (...) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
    }
    std::memcpy(res->storage.data(), pSysMem, res->storage.size());
    emit_upload_resource_locked(dev, res, 0, res->storage.size());
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    if (res->storage.empty()) {
      try {
        res->storage.resize(static_cast<size_t>(res->row_pitch_bytes) * res->height, 0);
      } catch (...) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
    }

    const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
    const size_t src_pitch =
        pArgs->RowPitch ? static_cast<size_t>(pArgs->RowPitch) : static_cast<size_t>(res->row_pitch_bytes);
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src + static_cast<size_t>(y) * src_pitch,
                  res->row_pitch_bytes);
    }
    emit_upload_resource_locked(dev, res, 0, res->storage.size());
    return;
  }

  set_error(dev, E_NOTIMPL);
}

void AEROGPU_APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice,
                                               D3D10DDI_HRESOURCE* pResources,
                                               UINT numResources) {
  if (!hDevice.pDrvPrivate || !pResources || numResources < 2) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* first = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[0]);
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;

  for (UINT i = 0; i + 1 < numResources; ++i) {
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
// Adapter DDI (10.1)
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10_1DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, D3D10_1DDIARG_CREATEDEVICE* pCreateDevice) {
  if (!pCreateDevice || !pCreateDevice->hDrvDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* device = new (pCreateDevice->hDrvDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->kmt_adapter = adapter->kmt_adapter;
  device->hrt_device = pCreateDevice->hRTDevice;
  device->pfn_set_error = pCreateDevice->pCallbacks ? pCreateDevice->pCallbacks->pfnSetErrorCb : nullptr;

  std::memset(pCreateDevice->pDeviceFuncs, 0, sizeof(*pCreateDevice->pDeviceFuncs));

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource;
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = &DestroyResource;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = &CreateVertexShader;
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = &CreatePixelShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = &DestroyVertexShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = &DestroyPixelShader;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = &CalcPrivateElementLayoutSize;
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = &CreateElementLayout;
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = &DestroyElementLayout;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = &CalcPrivateRTVSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = &CreateRenderTargetView;
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = &DestroyRenderTargetView;
  pCreateDevice->pDeviceFuncs->pfnClearRenderTargetView = &ClearRenderTargetView;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = &CalcPrivateDSVSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = &CreateDepthStencilView;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = &DestroyDepthStencilView;
  pCreateDevice->pDeviceFuncs->pfnClearDepthStencilView = &ClearDepthStencilView;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = &CreateBlendState;
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = &DestroyBlendState;
  pCreateDevice->pDeviceFuncs->pfnSetBlendState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetBlendState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = &CreateRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = &DestroyRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnSetRasterizerState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetRasterizerState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = &CreateDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = &DestroyDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = &IaSetInputLayout;
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = &IaSetVertexBuffers;
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = &IaSetIndexBuffer;
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = &IaSetTopology;

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = &VsSetShader;
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = &PsSetShader;

  pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnVsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetSamplers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnGsSetShader = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShader)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnSetViewports = &SetViewports;
  pCreateDevice->pDeviceFuncs->pfnSetScissorRects =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetScissorRects)>::Call;
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = &SetRenderTargets;

  pCreateDevice->pDeviceFuncs->pfnDraw = &Draw;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = &DrawIndexed;
  pCreateDevice->pDeviceFuncs->pfnDrawInstanced = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawAuto = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawAuto)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPresent = &Present;
  pCreateDevice->pDeviceFuncs->pfnFlush = &Flush;
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = &RotateResourceIdentities;

  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  pCreateDevice->pDeviceFuncs->pfnCopyResource = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call;
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call;

  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  DestroyKmtAdapterHandle(adapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI (10.0)
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize10(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice10(D3D10DDI_HADAPTER hAdapter, D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  if (!pCreateDevice || !pCreateDevice->hDrvDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* device = new (pCreateDevice->hDrvDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->kmt_adapter = adapter->kmt_adapter;
  device->hrt_device = pCreateDevice->hRTDevice;
  device->pfn_set_error = pCreateDevice->pCallbacks ? pCreateDevice->pCallbacks->pfnSetErrorCb : nullptr;

  std::memset(pCreateDevice->pDeviceFuncs, 0, sizeof(*pCreateDevice->pDeviceFuncs));

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource;
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = &DestroyResource;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = &CreateVertexShader;
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = &CreatePixelShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = &DestroyVertexShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = &DestroyPixelShader;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = &CalcPrivateElementLayoutSize;
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = &CreateElementLayout;
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = &DestroyElementLayout;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = &CalcPrivateRTVSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = &CreateRenderTargetView;
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = &DestroyRenderTargetView;
  pCreateDevice->pDeviceFuncs->pfnClearRenderTargetView = &ClearRenderTargetView;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = &CalcPrivateDSVSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = &CreateDepthStencilView;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = &DestroyDepthStencilView;
  pCreateDevice->pDeviceFuncs->pfnClearDepthStencilView = &ClearDepthStencilView;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = &CreateBlendState;
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = &DestroyBlendState;
  pCreateDevice->pDeviceFuncs->pfnSetBlendState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetBlendState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = &CreateRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = &DestroyRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnSetRasterizerState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetRasterizerState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = &CreateDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = &DestroyDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = &IaSetInputLayout;
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = &IaSetVertexBuffers;
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = &IaSetIndexBuffer;
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = &IaSetTopology;

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = &VsSetShader;
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = &PsSetShader;

  pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnVsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetSamplers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnGsSetShader =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShader)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetSamplers =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnSetViewports = &SetViewports;
  pCreateDevice->pDeviceFuncs->pfnSetScissorRects =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetScissorRects)>::Call;
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = &SetRenderTargets;

  pCreateDevice->pDeviceFuncs->pfnDraw = &Draw;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = &DrawIndexed;
  pCreateDevice->pDeviceFuncs->pfnDrawInstanced = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawAuto = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawAuto)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPresent = &Present;
  pCreateDevice->pDeviceFuncs->pfnFlush = &Flush;
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = &RotateResourceIdentities;

  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  pCreateDevice->pDeviceFuncs->pfnCopyResource = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call;
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call;

  return S_OK;
}

HRESULT AEROGPU_APIENTRY GetCaps10(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pCaps) {
  if (!pCaps || !pCaps->pData) {
    return E_INVALIDARG;
  }

  std::memset(pCaps->pData, 0, pCaps->DataSize);

  switch (pCaps->Type) {
    case D3D10DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        const uint32_t format = static_cast<uint32_t>(fmt->Format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatR8G8B8A8Unorm:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY;
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
      }
      break;

    default:
      break;
  }

  return S_OK;
}

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10_1DDIARG_GETCAPS* pCaps) {
  if (!pCaps || !pCaps->pData) {
    return E_INVALIDARG;
  }

  // Default: return zeroed caps (conservative). Specific required queries are
  // handled below.
  std::memset(pCaps->pData, 0, pCaps->DataSize);

  switch (pCaps->Type) {
    case D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    case D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10_1DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10_1DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        const uint32_t format = static_cast<uint32_t>(fmt->Format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatB8G8R8X8Unorm:
          case kDxgiFormatR8G8B8A8Unorm:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY;
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
        fmt->FormatSupport2 = 0;
      }
      break;

    default:
      break;
  }

  return S_OK;
}

HRESULT OpenAdapter_WDK(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  if (pOpenData->Interface == D3D10DDI_INTERFACE_VERSION) {
    pOpenData->Version = D3D10DDI_SUPPORTED;
    auto* adapter = new AeroGpuAdapter();
    InitKmtAdapterHandle(adapter);
    pOpenData->hAdapter.pDrvPrivate = adapter;

    auto* funcs = reinterpret_cast<D3D10DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
    std::memset(funcs, 0, sizeof(*funcs));
    funcs->pfnGetCaps = &GetCaps10;
    funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize10;
    funcs->pfnCreateDevice = &CreateDevice10;
    funcs->pfnCloseAdapter = &CloseAdapter;
    return S_OK;
  }
  if (pOpenData->Interface == D3D10_1DDI_INTERFACE_VERSION) {
    // `Version` is treated as an in/out negotiation field by some runtimes. If
    // the runtime doesn't initialize it, accept 0 and return the supported
    // 10.1 DDI version.
    if (pOpenData->Version == 0) {
      pOpenData->Version = D3D10_1DDI_SUPPORTED;
    } else if (pOpenData->Version < D3D10_1DDI_SUPPORTED) {
      return E_INVALIDARG;
    } else if (pOpenData->Version > D3D10_1DDI_SUPPORTED) {
      pOpenData->Version = D3D10_1DDI_SUPPORTED;
    }

    auto* adapter = new AeroGpuAdapter();
    InitKmtAdapterHandle(adapter);
    pOpenData->hAdapter.pDrvPrivate = adapter;

    auto* funcs = reinterpret_cast<D3D10_1DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
    std::memset(funcs, 0, sizeof(*funcs));
    funcs->pfnGetCaps = &GetCaps;
    funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
    funcs->pfnCreateDevice = &CreateDevice;
    funcs->pfnCloseAdapter = &CloseAdapter;
    return S_OK;
  }

  return E_INVALIDARG;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapter_WDK(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapter_WDK(pOpenData);
}

} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
