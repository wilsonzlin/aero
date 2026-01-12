// AeroGPU D3D10/11 UMD - shared internal encoder/state tracker.
//
// This header intentionally contains no WDK-specific types so it can be reused by
// both the repository "portable" build (minimal ABI subset) and the real Win7
// WDK build (`d3d10umddi.h` / `d3d11umddi.h`).
//
// The D3D10 and D3D11 DDIs are translated into the same AeroGPU command stream
// defined in `drivers/aerogpu/protocol/aerogpu_cmd.h`.
#pragma once

#include <algorithm>
#include <array>
#include <atomic>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <new>
#include <unordered_map>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "../../common/aerogpu_win32_security.h"
#include "aerogpu_d3d10_11_log.h"
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include "aerogpu_d3d10_11_wddm_submit.h"
#endif
#include "../../../protocol/aerogpu_umd_private.h"

#if defined(_WIN32)
  #include <windows.h>
#endif

namespace aerogpu::d3d10_11 {

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr uint32_t kDeviceDestroyLiveCookie = 0xA3E0D311u;
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
constexpr uint32_t kDxgiFormatR8G8B8A8Typeless = 27;
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatR8G8B8A8UnormSrgb = 29;
constexpr uint32_t kDxgiFormatBc1Typeless = 70;
constexpr uint32_t kDxgiFormatBc1Unorm = 71;
constexpr uint32_t kDxgiFormatBc1UnormSrgb = 72;
constexpr uint32_t kDxgiFormatBc2Typeless = 73;
constexpr uint32_t kDxgiFormatBc2Unorm = 74;
constexpr uint32_t kDxgiFormatBc2UnormSrgb = 75;
constexpr uint32_t kDxgiFormatBc3Typeless = 76;
constexpr uint32_t kDxgiFormatBc3Unorm = 77;
constexpr uint32_t kDxgiFormatBc3UnormSrgb = 78;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;
constexpr uint32_t kDxgiFormatB8G8R8A8Typeless = 90;
constexpr uint32_t kDxgiFormatB8G8R8A8UnormSrgb = 91;
constexpr uint32_t kDxgiFormatB8G8R8X8Typeless = 92;
constexpr uint32_t kDxgiFormatB8G8R8X8UnormSrgb = 93;
constexpr uint32_t kDxgiFormatBc7Typeless = 97;
constexpr uint32_t kDxgiFormatBc7Unorm = 98;
constexpr uint32_t kDxgiFormatBc7UnormSrgb = 99;

inline uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
//
// D3D semantic matching is case-insensitive. The AeroGPU ILAY protocol only stores a 32-bit hash
// (not the original string), so we must canonicalize the semantic name prior to hashing to preserve
// D3D semantics across the guest→host boundary.
//
// Canonical form: ASCII uppercase.
inline uint32_t HashSemanticName(const char* s) {
  if (!s) {
    return 0;
  }
  uint32_t hash = 2166136261u;
  for (const unsigned char* p = reinterpret_cast<const unsigned char*>(s); *p; ++p) {
    unsigned char c = *p;
    if (c >= 'a' && c <= 'z') {
      c = static_cast<unsigned char>(c - 'a' + 'A');
    }
    hash ^= c;
    hash *= 16777619u;
  }
  return hash;
}

inline uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8Typeless:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case kDxgiFormatB8G8R8A8UnormSrgb:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB;
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8Typeless:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    case kDxgiFormatB8G8R8X8UnormSrgb:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8Typeless:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    case kDxgiFormatR8G8B8A8UnormSrgb:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB;
    case kDxgiFormatBc1Typeless:
    case kDxgiFormatBc1Unorm:
      return AEROGPU_FORMAT_BC1_RGBA_UNORM;
    case kDxgiFormatBc1UnormSrgb:
      return AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB;
    case kDxgiFormatBc2Typeless:
    case kDxgiFormatBc2Unorm:
      return AEROGPU_FORMAT_BC2_RGBA_UNORM;
    case kDxgiFormatBc2UnormSrgb:
      return AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB;
    case kDxgiFormatBc3Typeless:
    case kDxgiFormatBc3Unorm:
      return AEROGPU_FORMAT_BC3_RGBA_UNORM;
    case kDxgiFormatBc3UnormSrgb:
      return AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB;
    case kDxgiFormatBc7Typeless:
    case kDxgiFormatBc7Unorm:
      return AEROGPU_FORMAT_BC7_RGBA_UNORM;
    case kDxgiFormatBc7UnormSrgb:
      return AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB;
    case kDxgiFormatD24UnormS8Uint:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    case kDxgiFormatD32Float:
      return AEROGPU_FORMAT_D32_FLOAT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

struct AerogpuTextureFormatLayout {
  // For linear formats, block_width/block_height are 1 and bytes_per_block is
  // the bytes-per-texel value.
  //
  // For BC formats, block_width/block_height are 4 and bytes_per_block is the
  // bytes-per-4x4-block value.
  uint32_t block_width = 0;
  uint32_t block_height = 0;
  uint32_t bytes_per_block = 0;
  bool valid = false;
};

inline AerogpuTextureFormatLayout aerogpu_texture_format_layout(uint32_t aerogpu_format) {
  switch (aerogpu_format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
    case AEROGPU_FORMAT_D24_UNORM_S8_UINT:
    case AEROGPU_FORMAT_D32_FLOAT:
      return AerogpuTextureFormatLayout{1, 1, 4, true};
    case AEROGPU_FORMAT_B5G6R5_UNORM:
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
      return AerogpuTextureFormatLayout{1, 1, 2, true};
    case AEROGPU_FORMAT_BC1_RGBA_UNORM:
    case AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB:
      return AerogpuTextureFormatLayout{4, 4, 8, true};
    case AEROGPU_FORMAT_BC2_RGBA_UNORM:
    case AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB:
    case AEROGPU_FORMAT_BC3_RGBA_UNORM:
    case AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB:
    case AEROGPU_FORMAT_BC7_RGBA_UNORM:
    case AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB:
      return AerogpuTextureFormatLayout{4, 4, 16, true};
    default:
      return AerogpuTextureFormatLayout{};
  }
}

inline bool aerogpu_format_is_block_compressed(uint32_t aerogpu_format) {
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  return layout.valid && (layout.block_width != 1 || layout.block_height != 1);
}

inline uint32_t aerogpu_div_round_up_u32(uint32_t value, uint32_t divisor) {
  return (value + divisor - 1) / divisor;
}

inline uint32_t aerogpu_texture_min_row_pitch_bytes(uint32_t aerogpu_format, uint32_t width) {
  if (width == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width == 0 || layout.bytes_per_block == 0) {
    return 0;
  }

  const uint64_t blocks_w =
      static_cast<uint64_t>(aerogpu_div_round_up_u32(width, layout.block_width));
  const uint64_t row_bytes = blocks_w * static_cast<uint64_t>(layout.bytes_per_block);
  if (row_bytes == 0 || row_bytes > UINT32_MAX) {
    return 0;
  }
  return static_cast<uint32_t>(row_bytes);
}

inline uint32_t aerogpu_texture_num_rows(uint32_t aerogpu_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return aerogpu_div_round_up_u32(height, layout.block_height);
}

inline uint64_t aerogpu_texture_required_size_bytes(uint32_t aerogpu_format,
                                                    uint32_t row_pitch_bytes,
                                                    uint32_t height) {
  if (row_pitch_bytes == 0) {
    return 0;
  }
  const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, height);
  return static_cast<uint64_t>(row_pitch_bytes) * static_cast<uint64_t>(rows);
}

inline uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
  // Note: BC formats are block-compressed and do not have a bytes-per-texel
  // representation. Callers that operate on Texture2D memory layouts should use
  // `aerogpu_texture_format_layout` / `aerogpu_texture_min_row_pitch_bytes`.
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width != 1 || layout.block_height != 1) {
    return 0;
  }
  return layout.bytes_per_block;
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

struct Texture2DSubresourceLayout {
  uint32_t mip_level = 0;
  uint32_t array_layer = 0;
  uint32_t width = 0;
  uint32_t height = 0;
  uint64_t offset_bytes = 0;
  // Row pitch in bytes (texel rows for linear formats, block rows for BC).
  uint32_t row_pitch_bytes = 0;
  // Number of "layout rows" in this subresource (texel rows for linear formats, block rows for BC).
  uint32_t rows_in_layout = 0;
  uint64_t size_bytes = 0;
};

inline uint32_t aerogpu_mip_dim(uint32_t base, uint32_t mip_level) {
  if (base == 0) {
    return 0;
  }
  const uint32_t shifted = (mip_level >= 32) ? 0u : (base >> mip_level);
  return std::max(1u, shifted);
}

inline bool build_texture2d_subresource_layouts(uint32_t aerogpu_format,
                                                uint32_t width,
                                                uint32_t height,
                                                uint32_t mip_levels,
                                                uint32_t array_layers,
                                                uint32_t mip0_row_pitch_bytes,
                                                std::vector<Texture2DSubresourceLayout>* out_layouts,
                                                uint64_t* out_total_bytes) {
  if (!out_layouts || !out_total_bytes) {
    return false;
  }
  out_layouts->clear();
  *out_total_bytes = 0;

  if (width == 0 || height == 0 || mip_levels == 0 || array_layers == 0) {
    return false;
  }
  if (mip0_row_pitch_bytes == 0) {
    return false;
  }

  const uint64_t subresource_count = static_cast<uint64_t>(mip_levels) * static_cast<uint64_t>(array_layers);
  if (subresource_count == 0 || subresource_count > static_cast<uint64_t>(SIZE_MAX)) {
    return false;
  }
  out_layouts->reserve(static_cast<size_t>(subresource_count));

  uint64_t offset = 0;
  for (uint32_t layer = 0; layer < array_layers; ++layer) {
    for (uint32_t mip = 0; mip < mip_levels; ++mip) {
      const uint32_t mip_w = aerogpu_mip_dim(width, mip);
      const uint32_t mip_h = aerogpu_mip_dim(height, mip);
      const uint32_t tight_row_pitch = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, mip_w);
      const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, mip_h);
      if (tight_row_pitch == 0 || rows == 0) {
        return false;
      }

      const uint32_t row_pitch = (mip == 0) ? mip0_row_pitch_bytes : tight_row_pitch;
      if (row_pitch < tight_row_pitch) {
        return false;
      }

      const uint64_t size_bytes = static_cast<uint64_t>(row_pitch) * static_cast<uint64_t>(rows);
      if (size_bytes == 0) {
        return false;
      }

      Texture2DSubresourceLayout layout{};
      layout.mip_level = mip;
      layout.array_layer = layer;
      layout.width = mip_w;
      layout.height = mip_h;
      layout.offset_bytes = offset;
      layout.row_pitch_bytes = row_pitch;
      layout.rows_in_layout = rows;
      layout.size_bytes = size_bytes;
      out_layouts->push_back(layout);

      const uint64_t next = offset + size_bytes;
      if (next < offset) {
        return false;
      }
      offset = next;
    }
  }

  *out_total_bytes = offset;
  return true;
}

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
  // Optional kernel adapter handle (D3DKMT_HANDLE in the WDK headers), opened via
  // D3DKMTOpenAdapterFromHdc for direct D3DKMT calls. Stored as u32 so this
  // shared header stays WDK-independent.
  uint32_t kmt_adapter = 0;

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
    HANDLE mapping =
        ::aerogpu::win32::CreateFileMappingWBestEffortLowIntegrity(
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

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  // True if this resource was created as shareable (D3D10/D3D11 `*_RESOURCE_MISC_SHARED`).
  bool is_shared = false;
  // True if this resource is an imported alias created via OpenResource/OpenSharedResource.
  bool is_shared_alias = false;

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
  std::vector<Texture2DSubresourceLayout> tex2d_subresources;

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
  uint32_t mapped_subresource = 0;
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
  bool forced_ndc_z_valid = false;
  float forced_ndc_z = 0.0f;
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
  uint32_t blend_enable = 0;
  uint32_t src_blend = 0;
  uint32_t dest_blend = 0;
  uint32_t blend_op = 0;
  uint32_t src_blend_alpha = 0;
  uint32_t dest_blend_alpha = 0;
  uint32_t blend_op_alpha = 0;
  uint32_t render_target_write_mask = 0xFu;
};
struct RasterizerState {
  uint32_t cull_mode = 0;
  uint32_t front_ccw = 0;
  uint32_t scissor_enable = 0;
  uint32_t depth_clip_enable = 1u;
};
struct DepthStencilState {
  // Stored as raw numeric values so this header remains WDK-free.
  uint32_t depth_enable = 0;
  uint32_t depth_write_mask = 0;
  uint32_t depth_func = 0;
  uint32_t stencil_enable = 0;
};

struct Device {
  uint32_t destroy_cookie = kDeviceDestroyLiveCookie;
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
  // Runtime-provided per-DMA-buffer private data (if exposed by CreateContext).
  // Some WDK vintages do not expose this in Allocate/GetCommandBuffer, so keep
  // the CreateContext-provided pointer as a fallback.
  void* wddm_dma_private_data = nullptr;
  uint32_t wddm_dma_private_data_bytes = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Shared Win7/WDDM 1.1 submission helper. Only available in WDK builds.
  WddmSubmit wddm_submit;
#endif

  // WDDM allocation handles (D3DKMT_HANDLE values) to include in each submission's
  // allocation list. This is rebuilt for each command buffer submission so the
  // KMD can attach an allocation table that resolves `backing_alloc_id` values in
  // the AeroGPU command stream.
  std::vector<uint32_t> wddm_submit_allocation_handles;

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
  std::array<Resource*, kMaxConstantBufferSlots> current_vs_cbs{};
  std::array<Resource*, kMaxConstantBufferSlots> current_ps_cbs{};
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  InputLayout* current_input_layout_obj = nullptr;
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
  uint32_t current_vs_cb0_first_constant = 0;
  uint32_t current_vs_cb0_num_constants = 0;
  Resource* current_ps_cb0 = nullptr;
  uint32_t current_ps_cb0_first_constant = 0;
  uint32_t current_ps_cb0_num_constants = 0;
  Resource* current_vs_srv0 = nullptr;
  Resource* current_ps_srv0 = nullptr;
  uint32_t current_vs_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_vs_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_ps_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_ps_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  DepthStencilState* current_dss = nullptr;
  uint32_t current_stencil_ref = 0;
  RasterizerState* current_rs = nullptr;
  BlendState* current_bs = nullptr;
  float current_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  uint32_t current_sample_mask = 0xFFFFFFFFu;

  bool scissor_valid = false;
  int32_t scissor_left = 0;
  int32_t scissor_top = 0;
  int32_t scissor_right = 0;
  int32_t scissor_bottom = 0;

  bool current_vs_forced_z_valid = false;
  float current_vs_forced_z = 0.0f;

  float viewport_x = 0.0f;
  float viewport_y = 0.0f;
  float viewport_width = 0.0f;
  float viewport_height = 0.0f;
  float viewport_min_depth = 0.0f;
  float viewport_max_depth = 1.0f;

  Device() {
    cmd.reset();
  }

  ~Device() {
    destroy_cookie = 0;
  }
};

template <typename THandle, typename TObject>
inline TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

inline void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

inline uint64_t submit_locked(Device* dev, bool want_present = false, HRESULT* out_hr = nullptr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev) {
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->wddm_submit_allocation_handles.clear();
    return 0;
  }

  dev->cmd.finalize();

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  const size_t submit_bytes = dev->cmd.size();
  uint64_t fence = 0;
  const uint32_t* alloc_handles =
      dev->wddm_submit_allocation_handles.empty() ? nullptr : dev->wddm_submit_allocation_handles.data();
  const uint32_t alloc_count = static_cast<uint32_t>(dev->wddm_submit_allocation_handles.size());
  const HRESULT hr =
      dev->wddm_submit.SubmitAeroCmdStream(dev->cmd.data(), dev->cmd.size(), want_present, alloc_handles, alloc_count, &fence);
  if (out_hr) {
    *out_hr = hr;
  }
  dev->cmd.reset();
  dev->wddm_submit_allocation_handles.clear();
  if (FAILED(hr)) {
    dev->pending_staging_writes.clear();
    return 0;
  }

  if (fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, fence);
    for (Resource* res : dev->pending_staging_writes) {
      if (res) {
        res->last_gpu_write_fence = fence;
      }
    }
  }
  dev->pending_staging_writes.clear();

  const uint64_t completed = dev->wddm_submit.QueryCompletedFence();
  atomic_max_u64(&dev->last_completed_fence, completed);
  AEROGPU_D3D10_11_LOG("submit_locked: present=%u bytes=%llu fence=%llu completed=%llu",
                       want_present ? 1u : 0u,
                       static_cast<unsigned long long>(submit_bytes),
                       static_cast<unsigned long long>(fence),
                       static_cast<unsigned long long>(completed));
  return fence;
#else
  (void)want_present;
  Adapter* adapter = dev->adapter;
  if (!adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    return 0;
  }

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
  dev->wddm_submit_allocation_handles.clear();
  return fence;
#endif
}

inline HRESULT flush_locked(Device* dev) {
  HRESULT hr = S_OK;
  (void)submit_locked(dev, /*want_present=*/false, &hr);
  return hr;
}

} // namespace aerogpu::d3d10_11
