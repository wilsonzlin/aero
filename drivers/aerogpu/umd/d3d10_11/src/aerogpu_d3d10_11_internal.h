// AeroGPU D3D10/11 UMD - shared internal encoder/state tracker.
//
// This header intentionally contains no WDK-specific types so it can be reused by
// both the repository "portable" build (minimal ABI subset) and the real Win7
// WDK build (`d3d10umddi.h` / `d3d11umddi.h`).
//
// The D3D10 and D3D11 DDIs are translated into the same AeroGPU command stream
// defined in `drivers/aerogpu/protocol/aerogpu_cmd.h`.
#pragma once

#include <array>
#include <atomic>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <new>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "../../../protocol/aerogpu_umd_private.h"

#if defined(_WIN32)
  #include <windows.h>
#endif

namespace aerogpu::d3d10_11 {

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr uint32_t kMaxConstantBufferSlots = 14;
constexpr uint32_t kMaxShaderResourceSlots = 128;
constexpr uint32_t kMaxSamplerSlots = 16;

// D3D11_BIND_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;
constexpr uint32_t kD3D11BindDepthStencil = 0x40;

// D3D11_CPU_ACCESS_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11CpuAccessWrite = 0x10000;
constexpr uint32_t kD3D11CpuAccessRead = 0x20000;

// D3D11_USAGE subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11UsageDefault = 0;
constexpr uint32_t kD3D11UsageImmutable = 1;
constexpr uint32_t kD3D11UsageDynamic = 2;
constexpr uint32_t kD3D11UsageStaging = 3;

// D3D11 supports up to 128 shader-resource view slots per stage. We track the
// currently bound SRV resources so RotateResourceIdentities can re-emit bindings
// when swapchain backbuffer handles are rotated.
constexpr uint32_t kAeroGpuD3D11MaxSrvSlots = 128;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatUnknown = 0;
constexpr uint32_t kDxgiFormatR32G32B32A32Float = 2;
constexpr uint32_t kDxgiFormatR32G32B32Float = 6;
constexpr uint32_t kDxgiFormatR32G32Float = 16;
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;

inline uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
inline uint32_t HashSemanticName(const char* s) {
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

inline uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
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

inline uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
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

inline uint32_t dxgi_index_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR32Uint:
      return AEROGPU_INDEX_FORMAT_UINT32;
    case kDxgiFormatR16Uint:
    default:
      return AEROGPU_INDEX_FORMAT_UINT16;
  }
}

inline uint32_t bind_flags_to_usage_flags(uint32_t bind_flags) {
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

struct Adapter {
  std::atomic<uint32_t> next_handle{1};

  // Opaque pointer to the runtime's adapter callback table (WDK type depends on
  // D3D10 vs D3D11 and the negotiated interface version).
  const void* runtime_callbacks = nullptr;
  // Negotiated `D3D10DDIARG_OPENADAPTER::Version` value for the D3D11 DDI.
  // Stored so device creation can validate that it is filling function tables
  // matching the negotiated struct layout.
  uint32_t d3d11_ddi_version = 0;

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

#if defined(_WIN32)
namespace detail {

// SplitMix64 mixing function (public domain). Used to scramble fallback entropy.
inline uint64_t splitmix64(uint64_t x) {
  x += 0x9E3779B97F4A7C15ULL;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
  return x ^ (x >> 31);
}

inline bool fill_random_bytes(void* out, size_t len) {
  if (!out || len == 0) {
    return false;
  }

  using RtlGenRandomFn = BOOLEAN(WINAPI*)(PVOID, ULONG);
  using BCryptGenRandomFn = LONG(WINAPI*)(void* hAlgorithm, unsigned char* pbBuffer, ULONG cbBuffer, ULONG dwFlags);

  static RtlGenRandomFn rtl_gen_random = []() -> RtlGenRandomFn {
    HMODULE advapi = GetModuleHandleW(L"advapi32.dll");
    if (!advapi) {
      advapi = LoadLibraryW(L"advapi32.dll");
    }
    if (!advapi) {
      return nullptr;
    }
    return reinterpret_cast<RtlGenRandomFn>(GetProcAddress(advapi, "SystemFunction036"));
  }();

  if (rtl_gen_random) {
    if (rtl_gen_random(out, static_cast<ULONG>(len)) != FALSE) {
      return true;
    }
  }

  static BCryptGenRandomFn bcrypt_gen_random = []() -> BCryptGenRandomFn {
    HMODULE bcrypt = GetModuleHandleW(L"bcrypt.dll");
    if (!bcrypt) {
      bcrypt = LoadLibraryW(L"bcrypt.dll");
    }
    if (!bcrypt) {
      return nullptr;
    }
    return reinterpret_cast<BCryptGenRandomFn>(GetProcAddress(bcrypt, "BCryptGenRandom"));
  }();

  if (bcrypt_gen_random) {
    constexpr ULONG kBcryptUseSystemPreferredRng = 0x00000002UL; // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    const LONG st = bcrypt_gen_random(nullptr,
                                     reinterpret_cast<unsigned char*>(out),
                                     static_cast<ULONG>(len),
                                     kBcryptUseSystemPreferredRng);
    if (st >= 0) {
      return true;
    }
  }

  return false;
}

inline uint64_t fallback_entropy(uint64_t counter) {
  uint64_t entropy = counter;
  entropy ^= (static_cast<uint64_t>(GetCurrentProcessId()) << 32);
  entropy ^= static_cast<uint64_t>(GetCurrentThreadId());

  LARGE_INTEGER qpc{};
  if (QueryPerformanceCounter(&qpc)) {
    entropy ^= static_cast<uint64_t>(qpc.QuadPart);
  }

  entropy ^= static_cast<uint64_t>(GetTickCount64());
  return entropy;
}

inline aerogpu_handle_t allocate_rng_fallback_handle() {
  static std::atomic<uint64_t> g_counter{1};
  static const uint64_t g_salt = []() -> uint64_t {
    uint64_t salt = 0;
    if (fill_random_bytes(&salt, sizeof(salt)) && salt != 0) {
      return salt;
    }
    return splitmix64(fallback_entropy(0));
  }();

  for (;;) {
    const uint64_t ctr = g_counter.fetch_add(1, std::memory_order_relaxed);
    const uint64_t mixed = splitmix64(g_salt ^ fallback_entropy(ctr));
    const uint32_t low31 = static_cast<uint32_t>(mixed & 0x7FFFFFFFu);
    if (low31 != 0) {
      return static_cast<aerogpu_handle_t>(0x80000000u | low31);
    }
  }
}

inline void log_global_handle_fallback_once() {
  static std::once_flag once;
  std::call_once(once, [] {
    OutputDebugStringA(
        "aerogpu-d3d10_11: GlobalHandleCounter mapping unavailable; using RNG fallback\n");
  });
}

} // namespace detail
#endif // defined(_WIN32)

inline aerogpu_handle_t AllocateGlobalHandle(Adapter* adapter) {
  if (!adapter) {
    return kInvalidHandle;
  }
#if defined(_WIN32)
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";

    // Use a permissive DACL so other processes in the session can open and
    // update the counter (e.g. DWM, sandboxed apps, different integrity levels).
    SECURITY_ATTRIBUTES sa{};
    sa.nLength = sizeof(sa);
    sa.bInheritHandle = FALSE;

    SECURITY_DESCRIPTOR sd{};
    if (InitializeSecurityDescriptor(&sd, SECURITY_DESCRIPTOR_REVISION) != FALSE &&
        SetSecurityDescriptorDacl(&sd, TRUE, nullptr, FALSE) != FALSE) {
      sa.lpSecurityDescriptor = &sd; // NULL DACL => allow all access
    } else {
      sa.lpSecurityDescriptor = nullptr;
    }

    HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, 0, sizeof(uint64_t), name);
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
    return static_cast<aerogpu_handle_t>(static_cast<uint64_t>(token) & 0xFFFFFFFFu);
  }

  detail::log_global_handle_fallback_once();
  return detail::allocate_rng_fallback_handle();
#endif

  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == kInvalidHandle) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
}

struct Resource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible guest backing allocation ID. 0 means the resource is host-owned
  // and must be updated via `AEROGPU_CMD_UPLOAD_RESOURCE` payloads.
  uint32_t backing_alloc_id = 0;
  // Byte offset into the guest allocation described by `backing_alloc_id`.
  uint32_t backing_offset_bytes = 0;
  // WDDM allocation handle (D3DKMT_HANDLE in the WDK headers) used for runtime
  // callbacks such as LockCb/UnlockCb. This is stored as a u32 so the shared
  // header stays WDK-independent.
  uint32_t wddm_allocation_handle = 0;

  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;
  uint32_t usage = kD3D11UsageDefault;
  uint32_t cpu_access_flags = 0;

  // WDDM identity (kernel-mode handles / allocation identities). DXGI swapchains
  // on Win7 rotate backbuffers by calling pfnRotateResourceIdentities; when
  // resources are backed by real WDDM allocations, these must rotate alongside
  // the AeroGPU handle.
  struct WddmIdentity {
    uint64_t km_resource_handle = 0;
    std::vector<uint64_t> km_allocation_handles;
  } wddm;

  // Buffer fields.
  uint64_t size_bytes = 0;

  // Texture2D fields.
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t mip_levels = 1;
  uint32_t array_size = 1;
  uint32_t dxgi_format = 0;
  uint32_t row_pitch_bytes = 0;

  // CPU-visible backing storage for resource uploads / staging reads.
  std::vector<uint8_t> storage;

  // Fence value of the most recent GPU submission that writes into this resource
  // (conservative). Used by the WDK D3D11 UMD for staging readback Map(READ)
  // synchronization.
  uint64_t last_gpu_write_fence = 0;

  // Map/unmap tracking (system-memory-backed implementation).
  bool mapped = false;
  uint32_t mapped_map_type = 0;
  uint32_t mapped_map_flags = 0;
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;

  // Win7/WDDM 1.1 runtime mapping state.
  //
  // The WDK UMDs map runtime-managed allocations via `pfnLockCb`/`pfnUnlockCb`.
  // We keep these fields WDK-free (plain integers/pointers) so the core
  // `Resource` struct can be shared with the non-WDK build.
  void* mapped_wddm_ptr = nullptr;
  uint64_t mapped_wddm_allocation = 0;
  uint32_t mapped_wddm_pitch = 0;
  uint32_t mapped_wddm_slice_pitch = 0;
};

struct Shader {
  aerogpu_handle_t handle = 0;
  uint32_t stage = AEROGPU_SHADER_STAGE_VERTEX;
  std::vector<uint8_t> dxbc;
};

struct InputLayout {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct RenderTargetView {
  aerogpu_handle_t texture = 0;
  Resource* resource = nullptr;
};

struct DepthStencilView {
  aerogpu_handle_t texture = 0;
  Resource* resource = nullptr;
};

// Pipeline state objects are accepted and can be bound, but the host translator
// may use conservative defaults until more encoding is implemented.
struct BlendState {
  uint32_t dummy = 0;
};
struct RasterizerState {
  uint32_t dummy = 0;
};
struct DepthStencilState {
  // Stored as raw numeric values so this header remains WDK-free.
  uint32_t depth_enable = 0;
  uint32_t depth_write_mask = 0;
  uint32_t depth_func = 0;
  uint32_t stencil_enable = 0;
};

struct Device {
  Adapter* adapter = nullptr;
  // Opaque pointer to the runtime's device callback table (contains e.g.
  // pfnSetErrorCb).
  const void* runtime_callbacks = nullptr;
  // Opaque pointer to the runtime's shared WDDM device callback table
  // (`D3DDDI_DEVICECALLBACKS`). Populated by the WDK D3D11 build for real Win7
  // WDDM submissions + fence waits, including LockCb/UnlockCb.
  const void* runtime_ddi_callbacks = nullptr;
  // Opaque pointer to the runtime device handle's private storage. This is used
  // for callbacks that require a `*HRTDEVICE` (e.g. `pfnSetErrorCb`) without
  // including WDK-specific handle types in this shared header.
  void* runtime_device = nullptr;
  // Driver-private pointer backing the immediate context handle. Stored so we
  // can adapt DDIs that sometimes move between device vs context tables across
  // D3D11 DDI interface versions (e.g. Present/RotateResourceIdentities).
  void* immediate_context = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // WDDM submission state (Win7/WDDM 1.1). Handles are stored as plain integers
  // to keep this header WDK-free; the WDK build casts them to `D3DKMT_HANDLE`.
  uint32_t kmt_device = 0;
  uint32_t kmt_context = 0;
  uint32_t kmt_fence_syncobj = 0;

  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  // Staging resources written by commands recorded since the last submission.
  // After submission, their `last_gpu_write_fence` is updated to the returned
  // fence value.
  std::vector<Resource*> pending_staging_writes;

  // Cached state (shared for the initial immediate-context-only implementation).
  aerogpu_handle_t current_rtv = 0;
  Resource* current_rtv_resource = nullptr;
  aerogpu_handle_t current_dsv = 0;
  Resource* current_dsv_resource = nullptr;
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_vs_srvs{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_ps_srvs{};
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};

  // Minimal software-state tracking for the Win7 guest tests. This allows the
  // UMD to produce correct staging readback results even when the submission
  // backend is still a stub.
  Resource* current_vb = nullptr;
  uint32_t current_vb_stride_bytes = 0;
  uint32_t current_vb_offset_bytes = 0;
  Resource* current_ib = nullptr;
  uint32_t current_ib_format = kDxgiFormatUnknown;
  uint32_t current_ib_offset_bytes = 0;
  Resource* current_vs_cb0 = nullptr;
  Resource* current_ps_cb0 = nullptr;
  Resource* current_vs_srv0 = nullptr;
  Resource* current_ps_srv0 = nullptr;
  DepthStencilState* current_dss = nullptr;
  uint32_t current_stencil_ref = 0;

  float viewport_x = 0.0f;
  float viewport_y = 0.0f;
  float viewport_width = 0.0f;
  float viewport_height = 0.0f;
  float viewport_min_depth = 0.0f;
  float viewport_max_depth = 1.0f;

  Device() {
    cmd.reset();
  }
};

template <typename THandle, typename TObject>
inline TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

inline uint64_t submit_locked(Device* dev) {
  if (!dev || dev->cmd.empty()) {
    return 0;
  }

  Adapter* adapter = dev->adapter;
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

  dev->last_submitted_fence.store(fence, std::memory_order_relaxed);
  dev->last_completed_fence.store(fence, std::memory_order_relaxed);
  for (Resource* res : dev->pending_staging_writes) {
    if (res) {
      res->last_gpu_write_fence = fence;
    }
  }
  dev->pending_staging_writes.clear();
  dev->cmd.reset();
  return fence;
}

inline HRESULT flush_locked(Device* dev) {
  submit_locked(dev);
  return S_OK;
}

} // namespace aerogpu::d3d10_11
