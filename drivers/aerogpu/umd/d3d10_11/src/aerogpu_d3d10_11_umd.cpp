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

#include "../include/aerogpu_d3d10_11_umd.h"

#include <algorithm>
#include <cassert>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <new>
#include <type_traits>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  #include <d3dkmthk.h>
  #include "../../../protocol/aerogpu_dbgctl_escape.h"

  #ifndef NT_SUCCESS
    #define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
  #endif

  #ifndef STATUS_TIMEOUT
    #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
  #endif

  #ifndef STATUS_NOT_SUPPORTED
    #define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
  #endif
#endif

#ifndef FAILED
#define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

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
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
// Win7 D3D11 runtime requests a specific user-mode DDI interface version. If we
// accept a version, we must fill function tables whose struct layout matches
// that version (otherwise the runtime can crash during device creation).
constexpr UINT kAeroGpuWin7D3D11DdiInterfaceVersion = D3D11DDI_INTERFACE_VERSION;

// Compile-time sanity (avoid sizeof assertions; layouts vary across WDKs).
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICEFUNCS::pfnCreateResource)>,
              "Expected D3D11DDI_DEVICEFUNCS::pfnCreateResource");
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw)>,
              "Expected D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw");
#endif

// D3D11_BIND_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;
constexpr uint32_t kD3D11BindDepthStencil = 0x40;

// D3D11_USAGE subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11UsageDefault = 0;
constexpr uint32_t kD3D11UsageImmutable = 1;
constexpr uint32_t kD3D11UsageDynamic = 2;
constexpr uint32_t kD3D11UsageStaging = 3;

// D3D11_CPU_ACCESS_FLAG subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11CpuAccessWrite = 0x10000;
constexpr uint32_t kD3D11CpuAccessRead = 0x20000;

// D3D11_MAP subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11MapRead = 1;
constexpr uint32_t kD3D11MapWrite = 2;
constexpr uint32_t kD3D11MapReadWrite = 3;
constexpr uint32_t kD3D11MapWriteDiscard = 4;
constexpr uint32_t kD3D11MapWriteNoOverwrite = 5;

// D3D11_MAP_FLAG_DO_NOT_WAIT (numeric value from d3d11.h).
constexpr uint32_t kD3D11MapFlagDoNotWait = 0x100000;

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
  UINT d3d11_ddi_interface_version = 0;

  std::atomic<uint32_t> next_handle{1};

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  D3D10DDI_HRTADAPTER hrt_adapter = {};
  const void* adapter_callbacks = nullptr;
#endif
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
  uint32_t usage = 0;
  uint32_t cpu_access_flags = 0;

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

  // Map/unmap tracking.
  bool mapped = false;
  bool mapped_write = false;
  uint32_t mapped_subresource = 0;
  uint64_t mapped_offset_bytes = 0;
  uint64_t mapped_size_bytes = 0;
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

  // Runtime callbacks and handles (error reporting + WDDM submission).
  const D3D11DDI_DEVICECALLBACKS* callbacks = nullptr;
  const D3DDDI_DEVICECALLBACKS* ddi_callbacks = nullptr;
  D3D11DDI_HRTDEVICE hrt_device = {};
  D3D10DDI_HRTDEVICE hrt_device10 = {};
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
    dev->callbacks->pfnSetErrorCb(dev->hrt_device, hr);
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

uint64_t submit_locked(AeroGpuImmediateContext* ctx, bool want_present, HRESULT* out_hr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!ctx || !ctx->device || ctx->cmd.empty()) {
    return 0;
  }

  AeroGpuDevice* dev = ctx->device;
  AeroGpuAdapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  ctx->cmd.finalize();

  const D3DDDI_DEVICECALLBACKS* cb = dev->ddi_callbacks;
  if (!cb || !cb->pfnAllocateCb || !cb->pfnRenderCb || !cb->pfnDeallocateCb) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    ctx->cmd.reset();
    return 0;
  }

  const uint8_t* src = ctx->cmd.data();
  const size_t src_size = ctx->cmd.size();
  if (src_size < sizeof(aerogpu_cmd_stream_header)) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    ctx->cmd.reset();
    return 0;
  }

  uint64_t last_fence = 0;

  // Chunk at packet boundaries if the runtime returns a smaller-than-requested DMA buffer.
  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;
    const UINT request_bytes =
        static_cast<UINT>(remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header));

    D3DDDICB_ALLOCATE alloc = {};
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) {
      alloc.DmaBufferSize = request_bytes;
    }
    __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) {
      alloc.CommandBufferSize = request_bytes;
    }
    __if_exists(D3DDDICB_ALLOCATE::AllocationListSize) {
      alloc.AllocationListSize = 0;
    }
    __if_exists(D3DDDICB_ALLOCATE::PatchLocationListSize) {
      alloc.PatchLocationListSize = 0;
    }

    HRESULT alloc_hr = CallCbMaybeHandle(cb->pfnAllocateCb, dev->hrt_device, dev->hrt_device10, &alloc);

    void* dma_ptr = nullptr;
    UINT dma_cap = 0;
    __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) {
      dma_ptr = alloc.pDmaBuffer;
    }
    __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) {
      dma_ptr = alloc.pCommandBuffer;
    }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferSize) {
      dma_cap = alloc.DmaBufferSize;
    }
    __if_exists(D3DDDICB_ALLOCATE::CommandBufferSize) {
      dma_cap = alloc.CommandBufferSize;
    }

    if (FAILED(alloc_hr) || !dma_ptr || dma_cap == 0) {
      if (out_hr) {
        *out_hr = FAILED(alloc_hr) ? alloc_hr : E_OUTOFMEMORY;
      }
      ctx->cmd.reset();
      return 0;
    }

    if (dma_cap < sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_hdr)) {
      D3DDDICB_DEALLOCATE dealloc = {};
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
        dealloc.pDmaBuffer = alloc.pDmaBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
        dealloc.pCommandBuffer = alloc.pCommandBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
        dealloc.pAllocationList = alloc.pAllocationList;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
        dealloc.pPatchLocationList = alloc.pPatchLocationList;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, dev->hrt_device10, &dealloc);

      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      ctx->cmd.reset();
      return 0;
    }

    // Build chunk within dma_cap.
    const size_t chunk_begin = cur;
    size_t chunk_end = cur;
    size_t chunk_size = sizeof(aerogpu_cmd_stream_header);

    while (chunk_end < src_size) {
      const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + chunk_end);
      const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
      if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || chunk_end + pkt_size > src_size) {
        assert(false && "AeroGPU command stream contains an invalid packet");
        break;
      }
      if (chunk_size + pkt_size > dma_cap) {
        break;
      }
      chunk_end += pkt_size;
      chunk_size += pkt_size;
    }

    if (chunk_end == chunk_begin) {
      D3DDDICB_DEALLOCATE dealloc = {};
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
        dealloc.pDmaBuffer = alloc.pDmaBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
        dealloc.pCommandBuffer = alloc.pCommandBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
        dealloc.pAllocationList = alloc.pAllocationList;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
        dealloc.pPatchLocationList = alloc.pPatchLocationList;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, dev->hrt_device10, &dealloc);

      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      ctx->cmd.reset();
      return 0;
    }

    // Copy header + selected packets into the runtime DMA buffer.
    auto* dst = static_cast<uint8_t*>(dma_ptr);
    std::memcpy(dst, src, sizeof(aerogpu_cmd_stream_header));
    std::memcpy(dst + sizeof(aerogpu_cmd_stream_header),
                src + chunk_begin,
                chunk_size - sizeof(aerogpu_cmd_stream_header));
    auto* hdr = reinterpret_cast<aerogpu_cmd_stream_header*>(dst);
    hdr->size_bytes = static_cast<uint32_t>(chunk_size);

    const bool is_last_chunk = (chunk_end == src_size);
    const bool do_present = want_present && is_last_chunk && cb->pfnPresentCb != nullptr;

    HRESULT submit_hr = S_OK;
    UINT submission_fence = 0;
    if (do_present) {
      D3DDDICB_PRESENT present = {};
      __if_exists(D3DDDICB_PRESENT::pDmaBuffer) {
        present.pDmaBuffer = alloc.pDmaBuffer;
      }
      __if_exists(D3DDDICB_PRESENT::pCommandBuffer) {
        present.pCommandBuffer = dma_ptr;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferSize) {
        present.DmaBufferSize = static_cast<UINT>(chunk_size);
      }
      __if_exists(D3DDDICB_PRESENT::CommandLength) {
        present.CommandLength = static_cast<UINT>(chunk_size);
      }
      __if_exists(D3DDDICB_PRESENT::pAllocationList) {
        present.pAllocationList = alloc.pAllocationList;
      }
      __if_exists(D3DDDICB_PRESENT::AllocationListSize) {
        present.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_PRESENT::pPatchLocationList) {
        present.pPatchLocationList = alloc.pPatchLocationList;
      }
      __if_exists(D3DDDICB_PRESENT::PatchLocationListSize) {
        present.PatchLocationListSize = 0;
      }

      submit_hr = CallCbMaybeHandle(cb->pfnPresentCb, dev->hrt_device, dev->hrt_device10, &present);
      __if_exists(D3DDDICB_PRESENT::SubmissionFenceId) {
        submission_fence = present.SubmissionFenceId;
      }
    } else {
      D3DDDICB_RENDER render = {};
      __if_exists(D3DDDICB_RENDER::pDmaBuffer) {
        render.pDmaBuffer = alloc.pDmaBuffer;
      }
      __if_exists(D3DDDICB_RENDER::pCommandBuffer) {
        render.pCommandBuffer = dma_ptr;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferSize) {
        render.DmaBufferSize = static_cast<UINT>(chunk_size);
      }
      __if_exists(D3DDDICB_RENDER::CommandLength) {
        render.CommandLength = static_cast<UINT>(chunk_size);
      }
      __if_exists(D3DDDICB_RENDER::pAllocationList) {
        render.pAllocationList = alloc.pAllocationList;
      }
      __if_exists(D3DDDICB_RENDER::AllocationListSize) {
        render.AllocationListSize = 0;
      }
      __if_exists(D3DDDICB_RENDER::pPatchLocationList) {
        render.pPatchLocationList = alloc.pPatchLocationList;
      }
      __if_exists(D3DDDICB_RENDER::PatchLocationListSize) {
        render.PatchLocationListSize = 0;
      }

      submit_hr = CallCbMaybeHandle(cb->pfnRenderCb, dev->hrt_device, dev->hrt_device10, &render);
      __if_exists(D3DDDICB_RENDER::SubmissionFenceId) {
        submission_fence = render.SubmissionFenceId;
      }
    }

    // Always return submission buffers to the runtime.
    {
      D3DDDICB_DEALLOCATE dealloc = {};
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
        dealloc.pDmaBuffer = alloc.pDmaBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
        dealloc.pCommandBuffer = alloc.pCommandBuffer;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
        dealloc.pAllocationList = alloc.pAllocationList;
      }
      __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
        dealloc.pPatchLocationList = alloc.pPatchLocationList;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, dev->hrt_device10, &dealloc);
    }

    if (FAILED(submit_hr)) {
      if (out_hr) {
        *out_hr = submit_hr;
      }
      ctx->cmd.reset();
      return 0;
    }

    if (submission_fence != 0) {
      last_fence = static_cast<uint64_t>(submission_fence);
    }

    cur = chunk_end;
  }

  ctx->cmd.reset();
  return last_fence;
}

void flush_locked(AeroGpuImmediateContext* ctx) {
  if (ctx) {
    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  HRESULT hr = S_OK;
  submit_locked(ctx, false, &hr);
  if (FAILED(hr)) {
    SetError(ctx->device, hr);
  }
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

  HRESULT hr = S_OK;
  submit_locked(ctx, true, &hr);
  return hr;
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
  if (adapter->d3d11_ddi_interface_version != kAeroGpuWin7D3D11DdiInterfaceVersion) {
    return E_NOINTERFACE;
  }

  auto* dev = new (pCreate->hDevice.pDrvPrivate) AeroGpuDevice();
  dev->adapter = adapter;
  dev->hDevice = pCreate->hDevice;
  __if_exists(D3D11DDIARG_CREATEDEVICE::hRTDevice) {
    dev->hrt_device = pCreate->hRTDevice;
    std::memset(&dev->hrt_device10, 0, sizeof(dev->hrt_device10));
    constexpr size_t kCopyBytes = (sizeof(dev->hrt_device10) < sizeof(pCreate->hRTDevice))
                                     ? sizeof(dev->hrt_device10)
                                     : sizeof(pCreate->hRTDevice);
    std::memcpy(&dev->hrt_device10, &pCreate->hRTDevice, kCopyBytes);
  }
  __if_exists(D3D11DDIARG_CREATEDEVICE::pCallbacks) {
    dev->callbacks = pCreate->pCallbacks;
  }
  __if_exists(D3D11DDIARG_CREATEDEVICE::pDeviceCallbacks) {
    dev->callbacks = pCreate->pDeviceCallbacks;
  }
  __if_exists(D3D11DDIARG_CREATEDEVICE::pUMCallbacks) {
    dev->ddi_callbacks = pCreate->pUMCallbacks;
  }
  if (!dev->ddi_callbacks) {
    dev->ddi_callbacks = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(dev->callbacks);
  }

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

  // Interface-version negotiation: Win7 D3D11 runtime tells us which DDI
  // interface version it will use. If we accept a version, we must fill device
  // and context function tables matching that version's struct layout.
  if (pOpenData->Interface != D3D11DDI_INTERFACE) {
    return E_INVALIDARG;
  }
  if (pOpenData->Version < kAeroGpuWin7D3D11DdiInterfaceVersion) {
    return E_NOINTERFACE;
  }
  if (pOpenData->Version > kAeroGpuWin7D3D11DdiInterfaceVersion) {
    pOpenData->Version = kAeroGpuWin7D3D11DdiInterfaceVersion;
  }

  auto* adapter = new AeroGpuAdapter();
  adapter->d3d11_ddi_interface_version = pOpenData->Version;
  pOpenData->hAdapter.pDrvPrivate = adapter;
  __if_exists(D3D10DDIARG_OPENADAPTER::hRTAdapter) {
    adapter->hrt_adapter = pOpenData->hRTAdapter;
  }
  __if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
    adapter->adapter_callbacks = pOpenData->pAdapterCallbacks;
  }

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

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  LogModulePathOnce();
  char buf[256];
  snprintf(buf,
           sizeof(buf),
           "aerogpu-d3d10_11: OpenAdapter11 Interface=%u Version=%u\n",
           (unsigned)(pOpenData ? pOpenData->Interface : 0),
           (unsigned)(pOpenData ? pOpenData->Version : 0));
  OutputDebugStringA(buf);
#endif
  AEROGPU_D3D10_11_LOG_CALL();
  return OpenAdapter11Wdk(pOpenData);
}

} // extern "C"

#else

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // Fence tracking for WDDM-backed synchronization. Higher-level D3D10/11 code (e.g. Map READ on
  // staging resources) can use these values to wait for GPU completion.
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  // Monitored fence state for Win7/WDDM 1.1.
  //
  // - `kmt_fence_syncobj` should be a monitored-fence sync object that advances as the KMD reports
  //   DMA-buffer completion via DXGK_INTERRUPT_TYPE_DMA_COMPLETED.
  // - `monitored_fence_value` optionally points at the CPU VA of the fence value for fast queries.
  // - `kmt_adapter` is used only for the escape-based fallback query path.
  //
  // These fields are expected to be initialized by the WDK build's device/context creation path.
  D3DKMT_HANDLE kmt_fence_syncobj = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
  D3DKMT_HANDLE kmt_adapter = 0;

  // WDDM submission plumbing (dxgkrnl callback table + runtime handle).
  D3D10DDI_HRTDEVICE hrt_device = {};
  const D3DDDI_DEVICECALLBACKS* callbacks = nullptr;

  // Mark that the next submission is triggered by Present; used to route the
  // final chunk through the Present callback so the KMD hits DxgkDdiPresent.
  bool next_submit_is_present = false;
#endif

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

void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
struct AeroGpuD3dkmtProcs {
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

    p.pfn_escape = reinterpret_cast<decltype(&D3DKMTEscape)>(GetProcAddress(gdi32, "D3DKMTEscape"));
    p.pfn_wait_for_syncobj = reinterpret_cast<decltype(&D3DKMTWaitForSynchronizationObject)>(
        GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject"));
    return p;
  }();
  return procs;
}
#endif

uint64_t AeroGpuQueryCompletedFence(AeroGpuDevice* dev) {
  if (!dev) {
    return 0;
  }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  if (dev->monitored_fence_value) {
    const uint64_t completed = *dev->monitored_fence_value;
    atomic_max_u64(&dev->last_completed_fence, completed);
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
        atomic_max_u64(&dev->last_completed_fence, static_cast<uint64_t>(q.last_completed_fence));
      }
    }
  }

  return dev->last_completed_fence.load(std::memory_order_relaxed);
#else
  if (!dev->adapter) {
    return dev->last_completed_fence.load(std::memory_order_relaxed);
  }

  std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
  const uint64_t completed = dev->adapter->completed_fence;
  atomic_max_u64(&dev->last_completed_fence, completed);
  return completed;
#endif
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  if (!dev->kmt_fence_syncobj) {
    return E_FAIL;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_wait_for_syncobj) {
    return E_FAIL;
  }

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
#else
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
#endif
}

uint64_t submit_locked(AeroGpuDevice* dev, HRESULT* out_hr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev || dev->cmd.empty()) {
    return 0;
  }

  AeroGpuAdapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  dev->cmd.finalize();

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  const D3D10DDI_HRTDEVICE hrt_device = dev->hrt_device;

  const bool want_present = dev->next_submit_is_present;
  dev->next_submit_is_present = false;

  if (!cb || !cb->pfnAllocateCb || !cb->pfnRenderCb || !cb->pfnDeallocateCb) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->cmd.reset();
    return 0;
  }

  const uint8_t* src = dev->cmd.data();
  const size_t src_size = dev->cmd.size();
  if (src_size < sizeof(aerogpu_cmd_stream_header)) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->cmd.reset();
    return 0;
  }

  uint64_t last_fence = 0;

  // Chunk at packet boundaries if the runtime returns a smaller-than-requested
  // DMA buffer. Each chunk is a self-contained AeroGPU command stream (header +
  // N packets).
  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;

    // Allocate a DMA buffer from the runtime (plus empty allocation/patch lists).
    D3DDDICB_ALLOCATE alloc = {};
    alloc.DmaBufferSize = static_cast<UINT>(remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header));
    alloc.AllocationListSize = 0;
    alloc.PatchLocationListSize = 0;

    HRESULT hr = cb->pfnAllocateCb(hrt_device, &alloc);
    if (FAILED(hr) || !alloc.pDmaBuffer || alloc.DmaBufferSize == 0) {
      if (out_hr) {
        *out_hr = FAILED(hr) ? hr : E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    // Safety: avoid overflow (cmd stream must always contain at least 1 packet per DMA buffer).
    const size_t dma_cap = static_cast<size_t>(alloc.DmaBufferSize);
    if (dma_cap < sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_hdr)) {
      D3DDDICB_DEALLOCATE dealloc = {};
      dealloc.pDmaBuffer = alloc.pDmaBuffer;
      dealloc.pAllocationList = alloc.pAllocationList;
      dealloc.pPatchLocationList = alloc.pPatchLocationList;
      cb->pfnDeallocateCb(hrt_device, &dealloc);

      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    // Build chunk within dma_cap.
    const size_t chunk_begin = cur;
    size_t chunk_end = cur;
    size_t chunk_size = sizeof(aerogpu_cmd_stream_header);

    while (chunk_end < src_size) {
      const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + chunk_end);
      const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
      if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || chunk_end + pkt_size > src_size) {
        // Malformed command stream; should never happen.
        assert(false && "AeroGPU command stream contains an invalid packet");
        break;
      }
      if (chunk_size + pkt_size > dma_cap) {
        break;
      }
      chunk_end += pkt_size;
      chunk_size += pkt_size;
    }

    if (chunk_end == chunk_begin) {
      // No packet fit, bail out.
      D3DDDICB_DEALLOCATE dealloc = {};
      dealloc.pDmaBuffer = alloc.pDmaBuffer;
      dealloc.pAllocationList = alloc.pAllocationList;
      dealloc.pPatchLocationList = alloc.pPatchLocationList;
      cb->pfnDeallocateCb(hrt_device, &dealloc);

      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    // Copy header + selected packets into the runtime DMA buffer.
    auto* dst = static_cast<uint8_t*>(alloc.pDmaBuffer);
    std::memcpy(dst, src, sizeof(aerogpu_cmd_stream_header));
    std::memcpy(dst + sizeof(aerogpu_cmd_stream_header), src + chunk_begin, chunk_size - sizeof(aerogpu_cmd_stream_header));
    auto* hdr = reinterpret_cast<aerogpu_cmd_stream_header*>(dst);
    hdr->size_bytes = static_cast<uint32_t>(chunk_size);

    // Submit: route the last chunk through Present if requested and supported, otherwise Render.
    const bool is_last_chunk = (chunk_end == src_size);
    const bool do_present = want_present && is_last_chunk && cb->pfnPresentCb != nullptr;

    HRESULT submit_hr = S_OK;
    UINT submission_fence = 0;
    if (do_present) {
      D3DDDICB_PRESENT present = {};
      present.pDmaBuffer = alloc.pDmaBuffer;
      present.DmaBufferSize = static_cast<UINT>(chunk_size);
      present.pAllocationList = alloc.pAllocationList;
      present.AllocationListSize = 0;
      present.pPatchLocationList = alloc.pPatchLocationList;
      present.PatchLocationListSize = 0;

      submit_hr = cb->pfnPresentCb(hrt_device, &present);
      submission_fence = present.SubmissionFenceId;
    } else {
      D3DDDICB_RENDER render = {};
      render.pDmaBuffer = alloc.pDmaBuffer;
      render.DmaBufferSize = static_cast<UINT>(chunk_size);
      render.pAllocationList = alloc.pAllocationList;
      render.AllocationListSize = 0;
      render.pPatchLocationList = alloc.pPatchLocationList;
      render.PatchLocationListSize = 0;

      submit_hr = cb->pfnRenderCb(hrt_device, &render);
      submission_fence = render.SubmissionFenceId;
    }

    // Free the allocated submission buffers regardless of render/present success.
    {
      D3DDDICB_DEALLOCATE dealloc = {};
      dealloc.pDmaBuffer = alloc.pDmaBuffer;
      dealloc.pAllocationList = alloc.pAllocationList;
      dealloc.pPatchLocationList = alloc.pPatchLocationList;
      cb->pfnDeallocateCb(hrt_device, &dealloc);
    }

    if (FAILED(submit_hr)) {
      if (out_hr) {
        *out_hr = submit_hr;
      }
      dev->cmd.reset();
      return 0;
    }

    if (submission_fence != 0) {
      last_fence = static_cast<uint64_t>(submission_fence);
    }

    cur = chunk_end;
  }

  if (last_fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, last_fence);
  }
  dev->cmd.reset();
  return last_fence;
#else
  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
#if !defined(_WIN32) || !defined(AEROGPU_UMD_USE_WDK_HEADERS)
    adapter->completed_fence = fence;
#endif
  }
#if !defined(_WIN32) || !defined(AEROGPU_UMD_USE_WDK_HEADERS)
  adapter->fence_cv.notify_all();
#endif

  atomic_max_u64(&dev->last_submitted_fence, fence);
#if !defined(_WIN32) || !defined(AEROGPU_UMD_USE_WDK_HEADERS)
  // Repository build: submissions are treated as synchronous, so the fence is immediately completed.
  atomic_max_u64(&dev->last_completed_fence, fence);
#endif

  dev->cmd.reset();
  return fence;
#endif
}

HRESULT flush_locked(AeroGpuDevice* dev) {
  if (dev) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  HRESULT hr = S_OK;
  submit_locked(dev, &hr);
  return hr;
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
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->size_bytes = pDesc->ByteWidth;

    if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    try {
      res->storage.resize(static_cast<size_t>(res->size_bytes));
    } catch (...) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }

    bool has_initial = false;
    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }
      std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
      has_initial = true;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (has_initial) {
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = res->size_bytes;

      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = 0;
      dirty->size_bytes = res->size_bytes;
    }
    return S_OK;
  }

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    const bool is_shared = (pDesc->MiscFlags & kD3D11ResourceMiscShared) != 0;
    const uint32_t requested_mip_levels = pDesc->MipLevels;
    const uint32_t mip_levels = requested_mip_levels ? requested_mip_levels : 1;
    if (is_shared && requested_mip_levels != 1) {
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
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = mip_levels;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = pDesc->Format;
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    res->row_pitch_bytes = res->width * bpp;

    uint32_t level_w = res->width ? res->width : 1u;
    uint32_t level_h = res->height ? res->height : 1u;
    uint64_t total_bytes = 0;
    for (uint32_t level = 0; level < res->mip_levels; ++level) {
      const uint32_t level_pitch = level_w * bpp;
      total_bytes += static_cast<uint64_t>(level_pitch) * static_cast<uint64_t>(level_h);
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }

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

    bool has_initial = false;
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

      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch)
                                                : static_cast<size_t>(res->row_pitch_bytes);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    res->row_pitch_bytes);
      }
      has_initial = true;
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

    if (has_initial) {
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = res->storage.size();

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

uint64_t resource_total_bytes(const AeroGpuResource* res) {
  if (!res) {
    return 0;
  }
  if (res->kind == ResourceKind::Buffer) {
    return res->size_bytes;
  }
  if (res->kind == ResourceKind::Texture2D) {
    return static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
  }
  return 0;
}

HRESULT ensure_resource_storage(AeroGpuResource* res, uint64_t size_bytes) {
  if (!res) {
    return E_INVALIDARG;
  }
  if (size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
    return E_OUTOFMEMORY;
  }
  try {
    if (res->storage.size() < static_cast<size_t>(size_bytes)) {
      res->storage.resize(static_cast<size_t>(size_bytes));
    }
  } catch (...) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT map_resource_locked(AeroGpuResource* res,
                            uint32_t subresource,
                            uint32_t map_type,
                            AEROGPU_DDI_MAPPED_SUBRESOURCE* pMapped) {
  if (!res || !pMapped) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }
  if (subresource != 0) {
    return E_INVALIDARG;
  }

  bool want_read = false;
  bool want_write = false;
  switch (map_type) {
    case AEROGPU_DDI_MAP_READ:
      want_read = true;
      break;
    case AEROGPU_DDI_MAP_WRITE:
    case AEROGPU_DDI_MAP_WRITE_DISCARD:
    case AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE:
      want_write = true;
      break;
    case AEROGPU_DDI_MAP_READ_WRITE:
      want_read = true;
      want_write = true;
      break;
    default:
      return E_INVALIDARG;
  }

  const uint64_t total = resource_total_bytes(res);
  if (!total) {
    return E_INVALIDARG;
  }

  HRESULT hr = ensure_resource_storage(res, total);
  if (FAILED(hr)) {
    return hr;
  }

  pMapped->pData = res->storage.data();
  if (res->kind == ResourceKind::Texture2D) {
    pMapped->RowPitch = res->row_pitch_bytes;
    pMapped->DepthPitch = res->row_pitch_bytes * res->height;
  } else {
    pMapped->RowPitch = 0;
    pMapped->DepthPitch = 0;
  }

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = total;
  (void)want_read;
  return S_OK;
}

void unmap_resource_locked(AeroGpuDevice* dev, AeroGpuResource* res, uint32_t subresource) {
  if (!dev || !res) {
    return;
  }
  if (!res->mapped) {
    return;
  }
  if (subresource != res->mapped_subresource) {
    return;
  }

  if (res->mapped_write && res->handle != kInvalidHandle) {
    // For bring-up, inline the updated bytes into the command stream so the host
    // does not need to dereference guest allocations.
    if (res->mapped_offset_bytes + res->mapped_size_bytes <= static_cast<uint64_t>(res->storage.size())) {
      const auto offset = static_cast<size_t>(res->mapped_offset_bytes);
      const auto size = static_cast<size_t>(res->mapped_size_bytes);
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + offset, size);
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = res->mapped_offset_bytes;
      upload->size_bytes = res->mapped_size_bytes;
    }

    auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    dirty->resource_handle = res->handle;
    dirty->reserved0 = 0;
    dirty->offset_bytes = res->mapped_offset_bytes;
    dirty->size_bytes = res->mapped_size_bytes;
  }

  res->mapped = false;
  res->mapped_write = false;
  res->mapped_subresource = 0;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = 0;
}

HRESULT map_dynamic_buffer_locked(AeroGpuResource* res, bool discard, void** ppData) {
  if (!res || !ppData) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }

  const uint64_t total = res->size_bytes;
  HRESULT hr = ensure_resource_storage(res, total);
  if (FAILED(hr)) {
    return hr;
  }

  if (discard) {
    // Approximate DISCARD renaming by allocating a fresh CPU backing store.
    try {
      res->storage.assign(static_cast<size_t>(total), 0);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
  }

  res->mapped = true;
  res->mapped_write = true;
  res->mapped_subresource = 0;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = total;

  *ppData = res->storage.data();
  return S_OK;
}

HRESULT AEROGPU_APIENTRY StagingResourceMap(D3D10DDI_HDEVICE hDevice,
                                           D3D10DDI_HRESOURCE hResource,
                                           uint32_t subresource,
                                           uint32_t map_type,
                                           uint32_t map_flags,
                                           AEROGPU_DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnStagingResourceMap subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));

  if (!pMapped || !hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->kind != ResourceKind::Texture2D) {
    return E_INVALIDARG;
  }
  return map_resource_locked(res, subresource, map_type, pMapped);
}

void AEROGPU_APIENTRY StagingResourceUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, uint32_t subresource) {
  AEROGPU_D3D10_11_LOG("pfnStagingResourceUnmap subresource=%u", static_cast<unsigned>(subresource));

  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  unmap_resource_locked(dev, res, subresource);
}

HRESULT AEROGPU_APIENTRY DynamicIABufferMapDiscard(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();

  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  if ((res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(res, /*discard=*/true, ppData);
}

HRESULT AEROGPU_APIENTRY DynamicIABufferMapNoOverwrite(D3D10DDI_HDEVICE hDevice,
                                                       D3D10DDI_HRESOURCE hResource,
                                                       void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();

  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  if ((res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(res, /*discard=*/false, ppData);
}

void AEROGPU_APIENTRY DynamicIABufferUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
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
  unmap_resource_locked(dev, res, /*subresource=*/0);
}

HRESULT AEROGPU_APIENTRY DynamicConstantBufferMapDiscard(D3D10DDI_HDEVICE hDevice,
                                                         D3D10DDI_HRESOURCE hResource,
                                                         void** ppData) {
  AEROGPU_D3D10_11_LOG_CALL();

  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  if ((res->bind_flags & kD3D11BindConstantBuffer) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(res, /*discard=*/true, ppData);
}

void AEROGPU_APIENTRY DynamicConstantBufferUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
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
  unmap_resource_locked(dev, res, /*subresource=*/0);
}
HRESULT AEROGPU_APIENTRY Map(D3D10DDI_HDEVICE hDevice,
                             D3D10DDI_HRESOURCE hResource,
                              uint32_t subresource,
                              uint32_t map_type,
                              uint32_t map_flags,
                              AEROGPU_DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnMap subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));

  if (!pMapped || !hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
    if (res->bind_flags & kD3D11BindConstantBuffer) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
  } else if (map_type == AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/false, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
  }

  if (res->kind == ResourceKind::Texture2D && res->bind_flags == 0) {
    return map_resource_locked(res, subresource, map_type, pMapped);
  }

  // Conservative: only support generic map on buffers and staging textures for now.
  if (res->kind == ResourceKind::Buffer) {
    return map_resource_locked(res, subresource, map_type, pMapped);
  }
  return E_NOTIMPL;
}

void AEROGPU_APIENTRY Unmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, uint32_t subresource) {
  AEROGPU_D3D10_11_LOG("pfnUnmap subresource=%u", static_cast<unsigned>(subresource));

  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  unmap_resource_locked(dev, res, subresource);
}

void AEROGPU_APIENTRY UpdateSubresourceUP(D3D10DDI_HDEVICE hDevice,
                                          D3D10DDI_HRESOURCE hResource,
                                          uint32_t dst_subresource,
                                          const AEROGPU_DDI_BOX* pDstBox,
                                          const void* pSysMem,
                                          uint32_t SysMemPitch,
                                          uint32_t SysMemSlicePitch) {
  (void)SysMemSlicePitch;
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate || !pSysMem) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  if (dst_subresource != 0 || pDstBox) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle == kInvalidHandle) {
    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    if (res->storage.size() != static_cast<size_t>(res->size_bytes)) {
      return;
    }
    std::memcpy(res->storage.data(), pSysMem, static_cast<size_t>(res->size_bytes));
  } else if (res->kind == ResourceKind::Texture2D) {
    if (res->storage.empty()) {
      return;
    }
    const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(res->row_pitch_bytes);
    const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src + static_cast<size_t>(y) * src_pitch,
                  res->row_pitch_bytes);
    }
  } else {
    return;
  }

  auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
      AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
  upload->resource_handle = res->handle;
  upload->reserved0 = 0;
  upload->offset_bytes = 0;
  upload->size_bytes = res->storage.size();

  auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  dirty->resource_handle = res->handle;
  dirty->reserved0 = 0;
  dirty->offset_bytes = 0;
  dirty->size_bytes = res->storage.size();
}

void AEROGPU_APIENTRY CopyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hDst, D3D10DDI_HRESOURCE hSrc) {
  if (!hDevice.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hDst);
  auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hSrc);
  if (!dev || !dst || !src) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dst->kind != src->kind) {
    return;
  }

  // Repository builds keep a conservative CPU backing store; simulate the copy
  // immediately so a subsequent staging Map(READ) sees the bytes.
  if (dst->kind == ResourceKind::Buffer) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    cmd->size_bytes = std::min(dst->size_bytes, src->size_bytes);
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const size_t copy_bytes = static_cast<size_t>(cmd->size_bytes);
    if (copy_bytes && src->storage.size() >= copy_bytes) {
      if (dst->storage.size() < copy_bytes) {
        dst->storage.resize(copy_bytes);
      }
      std::memcpy(dst->storage.data(), src->storage.data(), copy_bytes);
    }
  } else if (dst->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0) {
      return;
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
    cmd->width = std::min(dst->width, src->width);
    cmd->height = std::min(dst->height, src->height);
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(src->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aerogpu_format);
    const size_t row_bytes = static_cast<size_t>(cmd->width) * bpp;
    const size_t copy_rows = static_cast<size_t>(cmd->height);
    if (!row_bytes || !copy_rows) {
      return;
    }

    const size_t dst_required = copy_rows * static_cast<size_t>(dst->row_pitch_bytes);
    const size_t src_required = copy_rows * static_cast<size_t>(src->row_pitch_bytes);
    if (src->storage.size() < src_required) {
      return;
    }
    if (dst->storage.size() < dst_required) {
      dst->storage.resize(dst_required);
    }
    if (row_bytes > dst->row_pitch_bytes || row_bytes > src->row_pitch_bytes) {
      return;
    }

    for (size_t y = 0; y < copy_rows; y++) {
      std::memcpy(dst->storage.data() + y * dst->row_pitch_bytes,
                  src->storage.data() + y * src->row_pitch_bytes,
                  row_bytes);
    }
  }
}

HRESULT AEROGPU_APIENTRY CopySubresourceRegion(D3D10DDI_HDEVICE hDevice,
                                               D3D10DDI_HRESOURCE hDst,
                                               uint32_t dst_subresource,
                                               uint32_t dst_x,
                                               uint32_t dst_y,
                                               uint32_t dst_z,
                                               D3D10DDI_HRESOURCE hSrc,
                                               uint32_t src_subresource,
                                               const AEROGPU_DDI_BOX* pSrcBox) {
  if (!hDevice.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
    return E_INVALIDARG;
  }

  if (dst_subresource != 0 || src_subresource != 0 || dst_x != 0 || dst_y != 0 || dst_z != 0 || pSrcBox) {
    return E_NOTIMPL;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hDst);
  auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hSrc);
  if (!dev || !dst || !src) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dst->kind != src->kind) {
    return E_INVALIDARG;
  }

  if (dst->kind == ResourceKind::Buffer) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    cmd->size_bytes = std::min(dst->size_bytes, src->size_bytes);
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const size_t copy_bytes = static_cast<size_t>(cmd->size_bytes);
    if (copy_bytes && src->storage.size() >= copy_bytes) {
      if (dst->storage.size() < copy_bytes) {
        dst->storage.resize(copy_bytes);
      }
      std::memcpy(dst->storage.data(), src->storage.data(), copy_bytes);
    }
  } else if (dst->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0) {
      return E_INVALIDARG;
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
    cmd->width = std::min(dst->width, src->width);
    cmd->height = std::min(dst->height, src->height);
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(src->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aerogpu_format);
    const size_t row_bytes = static_cast<size_t>(cmd->width) * bpp;
    const size_t copy_rows = static_cast<size_t>(cmd->height);
    if (!row_bytes || !copy_rows) {
      return S_OK;
    }

    const size_t dst_required = copy_rows * static_cast<size_t>(dst->row_pitch_bytes);
    const size_t src_required = copy_rows * static_cast<size_t>(src->row_pitch_bytes);
    if (src->storage.size() < src_required) {
      return S_OK;
    }
    if (dst->storage.size() < dst_required) {
      dst->storage.resize(dst_required);
    }
    if (row_bytes > dst->row_pitch_bytes || row_bytes > src->row_pitch_bytes) {
      return S_OK;
    }

    for (size_t y = 0; y < copy_rows; y++) {
      std::memcpy(dst->storage.data() + y * dst->row_pitch_bytes,
                  src->storage.data() + y * src->row_pitch_bytes,
                  row_bytes);
    }
  }

  return S_OK;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  dev->next_submit_is_present = true;
#endif

  HRESULT hr = S_OK;
  submit_locked(dev, &hr);
  return hr;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  // Field names vary slightly across WDK versions; use MSVC's `__if_exists` to
  // tolerate both.
  __if_exists(D3D10DDIARG_CREATEDEVICE::hRTDevice) {
    device->hrt_device = pCreateDevice->hRTDevice;
  }
  __if_exists(D3D10DDIARG_CREATEDEVICE::pCallbacks) {
    device->callbacks = pCreateDevice->pCallbacks;
  }
  __if_exists(D3D10DDIARG_CREATEDEVICE::pDeviceCallbacks) {
    device->callbacks = pCreateDevice->pDeviceCallbacks;
  }
#endif

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
  funcs.pfnMap = &Map;
  funcs.pfnUnmap = &Unmap;
  funcs.pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  funcs.pfnCopyResource = &CopyResource;
  funcs.pfnCopySubresourceRegion = &CopySubresourceRegion;

  // Map/unmap. Win7 D3D11 runtimes may use specialized entrypoints.
  funcs.pfnStagingResourceMap = &StagingResourceMap;
  funcs.pfnStagingResourceUnmap = &StagingResourceUnmap;
  funcs.pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard;
  funcs.pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite;
  funcs.pfnDynamicIABufferUnmap = &DynamicIABufferUnmap;
  funcs.pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard;
  funcs.pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmap;

  // The runtime-provided device function table is typically a superset of the
  // subset we populate here. Ensure the full table is zeroed first so any
  // unimplemented entrypoints are nullptr (instead of uninitialized garbage),
  // then copy the implemented prefix.
  std::memset(pCreateDevice->pDeviceFuncs, 0, sizeof(*pCreateDevice->pDeviceFuncs));
  std::memcpy(out_funcs, &funcs, sizeof(funcs));
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS)
  __if_exists(D3D10DDIARG_OPENADAPTER::hRTAdapter) {
    adapter->hrt_adapter = pOpenData->hRTAdapter;
  }
  __if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
    adapter->adapter_callbacks = pOpenData->pAdapterCallbacks;
  }
#endif

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
