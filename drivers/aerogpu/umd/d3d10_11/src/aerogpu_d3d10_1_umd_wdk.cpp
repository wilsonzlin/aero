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

#include "aerogpu_d3d10_11_wdk_abi_asserts.h"

#include <d3d10_1umddi.h>
#include <d3d10_1.h>
#include <d3dkmthk.h>

#include <array>
#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <limits>
#include <mutex>
#include <new>
#include <type_traits>
#include <unordered_map>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_wddm_submit.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../common/aerogpu_win32_security.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"
#include "../../../protocol/aerogpu_win7_abi.h"

#ifndef NT_SUCCESS
  #define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_TIMEOUT
  #define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
#endif

// Implemented in `aerogpu_d3d10_umd_wdk.cpp` (D3D10.0 DDI).
HRESULT AEROGPU_APIENTRY AeroGpuOpenAdapter10Wdk(D3D10DDIARG_OPENADAPTER* pOpenData);

namespace {

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr uint32_t kAeroGpuDeviceLiveCookie = 0xA3E0D301u;
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr HRESULT kHrPending = static_cast<HRESULT>(0x8000000Au); // E_PENDING
constexpr HRESULT kHrNtStatusGraphicsGpuBusy =
    static_cast<HRESULT>(0xD01E0102L); // HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)
constexpr uint32_t kAeroGpuTimeoutMsInfinite = ~0u;

struct AeroGpuAdapter;

constexpr uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

constexpr uint32_t AlignUpU32(uint32_t value, uint32_t alignment) {
  return static_cast<uint32_t>((value + alignment - 1) & ~(alignment - 1));
}

uint64_t AllocateGlobalToken() {
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";
    HANDLE mapping =
        aerogpu::win32::CreateFileMappingWBestEffortLowIntegrity(
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
    return static_cast<uint64_t>(token);
  }

  return 0;
}

static uint64_t splitmix64(uint64_t x) {
  x += 0x9E3779B97F4A7C15ULL;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
  return x ^ (x >> 31);
}

static uint64_t fallback_entropy(uint64_t counter) {
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

static aerogpu_handle_t allocate_rng_fallback_handle() {
  static std::atomic<uint64_t> counter{1};
  static const uint64_t salt = splitmix64(fallback_entropy(0));

  for (;;) {
    const uint64_t ctr = counter.fetch_add(1, std::memory_order_relaxed);
    const uint64_t mixed = splitmix64(salt ^ fallback_entropy(ctr));
    const uint32_t low31 = static_cast<uint32_t>(mixed & 0x7FFFFFFFu);
    if (low31 != 0) {
      return static_cast<aerogpu_handle_t>(0x80000000u | low31);
    }
  }
}

aerogpu_handle_t AllocateGlobalHandle(AeroGpuAdapter* adapter);

// Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
// correct UMD bitness was loaded (System32 vs SysWOW64).
void LogModulePathOnce() {
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

// D3D10_BIND_* subset (numeric values from d3d10.h).
constexpr uint32_t kD3D10BindVertexBuffer = 0x1;
constexpr uint32_t kD3D10BindIndexBuffer = 0x2;
constexpr uint32_t kD3D10BindConstantBuffer = 0x4;
constexpr uint32_t kD3D10BindShaderResource = 0x8;
constexpr uint32_t kD3D10BindRenderTarget = 0x20;
constexpr uint32_t kD3D10BindDepthStencil = 0x40;

constexpr uint32_t kAeroGpuD3D10MaxSrvSlots = 128;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
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
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;
constexpr uint32_t kDxgiFormatB8G8R8A8Typeless = 90;
constexpr uint32_t kDxgiFormatB8G8R8A8UnormSrgb = 91;
constexpr uint32_t kDxgiFormatB8G8R8X8Typeless = 92;
constexpr uint32_t kDxgiFormatB8G8R8X8UnormSrgb = 93;
constexpr uint32_t kDxgiFormatBc7Typeless = 97;
constexpr uint32_t kDxgiFormatBc7Unorm = 98;
constexpr uint32_t kDxgiFormatBc7UnormSrgb = 99;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
//
// D3D semantic matching is case-insensitive. The AeroGPU ILAY protocol only stores a 32-bit hash
// (not the original string), so we canonicalize to ASCII uppercase before hashing.
uint32_t HashSemanticName(const char* s) {
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

uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
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

// D3D9 D3DFORMAT subset (numeric values from d3d9types.h).
//
// AeroGPU encodes legacy D3D9 shared-surface descriptors into
// `aerogpu_wddm_alloc_priv.reserved0` (see `AEROGPU_WDDM_ALLOC_PRIV_DESC_*` macros).
// When the D3D10.1 runtime opens such a resource, the OpenResource DDI does not
// necessarily provide enough information to reconstruct the resource
// description, so we fall back to this encoding.
constexpr uint32_t kD3d9FmtA8R8G8B8 = 21; // D3DFMT_A8R8G8B8
constexpr uint32_t kD3d9FmtX8R8G8B8 = 22; // D3DFMT_X8R8G8B8
constexpr uint32_t kD3d9FmtA8B8G8R8 = 32; // D3DFMT_A8B8G8R8
constexpr uint32_t kD3d9FmtX8B8G8R8 = 33; // D3DFMT_X8B8G8R8

static bool D3d9FormatToDxgi(uint32_t d3d9_format, uint32_t* dxgi_format_out, uint32_t* bpp_out) {
  if (dxgi_format_out) {
    *dxgi_format_out = 0;
  }
  if (bpp_out) {
    *bpp_out = 0;
  }
  if (!dxgi_format_out || !bpp_out) {
    return false;
  }

  switch (d3d9_format) {
    case kD3d9FmtA8R8G8B8:
      *dxgi_format_out = kDxgiFormatB8G8R8A8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtX8R8G8B8:
      *dxgi_format_out = kDxgiFormatB8G8R8X8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtA8B8G8R8:
      *dxgi_format_out = kDxgiFormatR8G8B8A8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtX8B8G8R8:
      // DXGI has no X8 variant; treat as UNORM and rely on bind flags/sampling
      // to ignore alpha when needed.
      *dxgi_format_out = kDxgiFormatR8G8B8A8Unorm;
      *bpp_out = 4;
      return true;
    default:
      return false;
  }
}

static bool FixupLegacyPrivForOpenResource(aerogpu_wddm_alloc_priv_v2* priv) {
  if (!priv) {
    return false;
  }
  if (priv->kind != AEROGPU_WDDM_ALLOC_KIND_UNKNOWN) {
    return true;
  }

  if (AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(priv->reserved0)) {
    const uint32_t d3d9_format = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_FORMAT(priv->reserved0));
    const uint32_t width = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_WIDTH(priv->reserved0));
    const uint32_t height = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_HEIGHT(priv->reserved0));
    if (width == 0 || height == 0) {
      return false;
    }

    uint32_t dxgi_format = 0;
    uint32_t bpp = 0;
    if (!D3d9FormatToDxgi(d3d9_format, &dxgi_format, &bpp)) {
      return false;
    }

    const uint64_t row_pitch = static_cast<uint64_t>(width) * static_cast<uint64_t>(bpp);
    if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
      return false;
    }

    priv->kind = AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D;
    priv->width = width;
    priv->height = height;
    priv->format = dxgi_format;
    priv->row_pitch_bytes = static_cast<uint32_t>(row_pitch);
    return true;
  }

  // If no descriptor marker is present, treat legacy v1 blobs as generic buffers.
  if (priv->size_bytes != 0) {
    priv->kind = AEROGPU_WDDM_ALLOC_KIND_BUFFER;
    return true;
  }

  return false;
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

static AerogpuTextureFormatLayout aerogpu_texture_format_layout(uint32_t aerogpu_format) {
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

static bool aerogpu_format_is_block_compressed(uint32_t aerogpu_format) {
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  return layout.valid && (layout.block_width != 1 || layout.block_height != 1);
}

static uint32_t aerogpu_div_round_up_u32(uint32_t value, uint32_t divisor) {
  return (value + divisor - 1) / divisor;
}

static uint32_t aerogpu_texture_min_row_pitch_bytes(uint32_t aerogpu_format, uint32_t width) {
  if (width == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width == 0 || layout.bytes_per_block == 0) {
    return 0;
  }
  const uint64_t blocks_w = static_cast<uint64_t>(aerogpu_div_round_up_u32(width, layout.block_width));
  const uint64_t row_bytes = blocks_w * static_cast<uint64_t>(layout.bytes_per_block);
  if (row_bytes == 0 || row_bytes > UINT32_MAX) {
    return 0;
  }
  return static_cast<uint32_t>(row_bytes);
}

static uint32_t aerogpu_texture_num_rows(uint32_t aerogpu_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return aerogpu_div_round_up_u32(height, layout.block_height);
}

static uint64_t aerogpu_texture_required_size_bytes(uint32_t aerogpu_format, uint32_t row_pitch_bytes, uint32_t height) {
  if (row_pitch_bytes == 0) {
    return 0;
  }
  const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, height);
  return static_cast<uint64_t>(row_pitch_bytes) * static_cast<uint64_t>(rows);
}

uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
  // BC formats are block-compressed and do not have a bytes-per-texel representation.
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width != 1 || layout.block_height != 1) {
    return 0;
  }
  return layout.bytes_per_block;
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

static uint32_t aerogpu_mip_dim(uint32_t base, uint32_t mip_level) {
  if (base == 0) {
    return 0;
  }
  const uint32_t shifted = (mip_level >= 32) ? 0u : (base >> mip_level);
  return std::max(1u, shifted);
}

static bool build_texture2d_subresource_layouts(uint32_t aerogpu_format,
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

  const uint64_t subresource_count =
      static_cast<uint64_t>(mip_levels) * static_cast<uint64_t>(array_layers);
  if (subresource_count == 0 || subresource_count > static_cast<uint64_t>(SIZE_MAX)) {
    return false;
  }
  try {
    out_layouts->reserve(static_cast<size_t>(subresource_count));
  } catch (...) {
    return false;
  }

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
      try {
        out_layouts->push_back(layout);
      } catch (...) {
        return false;
      }

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

struct AeroGpuAdapter {
  std::atomic<uint32_t> next_handle{1};

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;

  // Optional D3DKMT adapter handle for dev-only calls (e.g. QUERY_FENCE via Escape).
  // This is best-effort bring-up plumbing; the real submission path should use
  // runtime callbacks and context-owned sync objects instead.
  D3DKMT_HANDLE kmt_adapter = 0;
};

aerogpu_handle_t AllocateGlobalHandle(AeroGpuAdapter* adapter) {
  if (!adapter) {
    return kInvalidHandle;
  }

  const uint64_t token = AllocateGlobalToken();
  if (token) {
    return static_cast<aerogpu_handle_t>(token & 0xFFFFFFFFu);
  }
  return allocate_rng_fallback_handle();
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

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible backing allocation ID used by the AeroGPU per-submit allocation table.
  // 0 means "host allocated" (no allocation-table entry).
  uint32_t backing_alloc_id = 0;
  uint32_t backing_offset_bytes = 0;

  // Runtime allocation handle (D3DKMT_HANDLE) used for LockCb/UnlockCb.
  // This is intentionally NOT the same identity as the KMD-visible
  // `DXGK_ALLOCATIONLIST::hAllocation` and must not be used as a stable alloc_id.
  uint32_t wddm_allocation_handle = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  // True if this resource was created as shareable (D3D10/D3D11 `*_RESOURCE_MISC_SHARED`).
  bool is_shared = false;
  bool is_shared_alias = false;
  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;

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

  std::vector<uint8_t> storage;

  // Fence value of the most recent GPU submission that writes into this resource
  // (conservative). Used for staging readback Map(READ) synchronization so
  // Map(DO_NOT_WAIT) does not spuriously fail due to unrelated in-flight work.
  uint64_t last_gpu_write_fence = 0;

  // Map state (for UP resources backed by `storage`).
  bool mapped = false;
  bool mapped_write = false;
  uint32_t mapped_subresource = 0;
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;

  // Win7/WDDM 1.1 runtime mapping state (pfnLockCb/pfnUnlockCb).
  void* mapped_wddm_ptr = nullptr;
  uint64_t mapped_wddm_allocation = 0;
  uint32_t mapped_wddm_pitch = 0;
  uint32_t mapped_wddm_slice_pitch = 0;
};

static bool ConsumeWddmAllocPrivV2(const void* priv_data, UINT priv_data_size, aerogpu_wddm_alloc_priv_v2* out) {
  if (out) {
    std::memset(out, 0, sizeof(*out));
  }
  if (!out || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return false;
  }

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
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuDepthStencilView {
  aerogpu_handle_t texture = 0;
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuShaderResourceView {
  aerogpu_handle_t texture = 0;
  AeroGpuResource* resource = nullptr;
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

// Win7-era WDK headers disagree on whether pfnSetErrorCb takes HRTDEVICE or
// HDEVICE. Keep the callback typed exactly as declared by the active headers,
// then use `std::is_invocable_v` at call sites to choose the right handle type.
using SetErrorFn = decltype(std::declval<std::remove_pointer_t<decltype(std::declval<D3D10_1DDIARG_CREATEDEVICE>().pCallbacks)>>().pfnSetErrorCb);

struct AeroGpuDevice {
  uint32_t live_cookie = kAeroGpuDeviceLiveCookie;
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  D3D10DDI_HRTDEVICE hrt_device{};
  SetErrorFn pfn_set_error = nullptr;
  const D3DDDI_DEVICECALLBACKS* callbacks = nullptr;
  aerogpu::d3d10_11::WddmSubmit wddm_submit;

  aerogpu::CmdWriter cmd;

  // WDDM allocation handles (D3DKMT_HANDLE values) to include in each submission's
  // allocation list. This is rebuilt for each command buffer submission so the
  // KMD can attach an allocation table that resolves `backing_alloc_id` values in
  // the AeroGPU command stream.
  std::vector<uint32_t> wddm_submit_allocation_handles;

  // Fence tracking for WDDM-backed synchronization (used by Map READ / DO_NOT_WAIT semantics).
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  // Staging resources written by commands recorded since the last submission.
  // After submission, their `last_gpu_write_fence` is updated to the returned
  // fence value.
  std::vector<AeroGpuResource*> pending_staging_writes;

  // Monitored fence state for Win7/WDDM 1.1.
  // These fields are expected to be initialized by the real WDDM submission path.
  D3DKMT_HANDLE kmt_device = 0;
  D3DKMT_HANDLE kmt_context = 0;
  D3DKMT_HANDLE kmt_fence_syncobj = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
  D3DKMT_HANDLE kmt_adapter = 0;
  void* dma_buffer_private_data = nullptr;
  UINT dma_buffer_private_data_size = 0;

  aerogpu_handle_t current_rtv = 0;
  aerogpu_handle_t current_dsv = 0;
  std::array<AeroGpuResource*, kAeroGpuD3D10MaxSrvSlots> current_vs_srvs{};
  std::array<AeroGpuResource*, kAeroGpuD3D10MaxSrvSlots> current_ps_srvs{};
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`, `d3d10_1_triangle`).
  AeroGpuResource* current_rtv_res = nullptr;
  AeroGpuResource* current_dsv_res = nullptr;
  AeroGpuResource* current_vb_res = nullptr;
  AeroGpuResource* current_ib_res = nullptr;
  uint32_t current_vb_stride = 0;
  uint32_t current_vb_offset = 0;

  uint32_t viewport_width = 0;
  uint32_t viewport_height = 0;

  AeroGpuDevice() {
    cmd.reset();
  }

  ~AeroGpuDevice() {
    live_cookie = 0;
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

static bool SupportsTransfer(const AeroGpuDevice* dev) {
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

static bool SupportsSrgbFormats(const AeroGpuDevice* dev) {
  // ABI 1.2 adds explicit sRGB format variants. When running against an older
  // host/device ABI, map sRGB DXGI formats to UNORM to keep the command stream
  // compatible.
  if (!dev || !dev->adapter || !dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
}

static bool SupportsBcFormats(const AeroGpuDevice* dev) {
  if (!dev || !dev->adapter || !dev->adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = dev->adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
}

static uint32_t dxgi_format_to_aerogpu_compat(const AeroGpuDevice* dev, uint32_t dxgi_format) {
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

static bool SupportsBcFormatsAdapter(const AeroGpuAdapter* adapter) {
  if (!adapter || !adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = adapter->umd_private;
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
}

static void TrackStagingWriteLocked(AeroGpuDevice* dev, AeroGpuResource* dst) {
  if (!dev || !dst) {
    return;
  }
  if (dst->bind_flags != 0) {
    return;
  }
  if (dst->backing_alloc_id == 0) {
    return;
  }
  dev->pending_staging_writes.push_back(dst);
}
template <typename Fn, typename Handle, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, Handle handle, Args&&... args) {
  // Some WDK revisions disagree on whether the first parameter is a D3D10 or
  // D3D11 runtime device handle; try both when the call site supplies the D3D10
  // handle wrapper.
  if constexpr (std::is_invocable_v<Fn, Handle, Args...>) {
    return fn(handle, std::forward<Args>(args)...);
  } else if constexpr (std::is_same_v<Handle, D3D10DDI_HRTDEVICE> &&
                       std::is_invocable_v<Fn, D3D11DDI_HRTDEVICE, Args...>) {
    D3D11DDI_HRTDEVICE h11{};
    h11.pDrvPrivate = handle.pDrvPrivate;
    return fn(h11, std::forward<Args>(args)...);
  } else {
    return fn(std::forward<Args>(args)...);
  }
}

template <typename T>
T UintPtrToD3dHandle(std::uintptr_t value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<T>(value);
  } else {
    return static_cast<T>(value);
  }
}

struct AeroGpuD3dkmtProcs {
  decltype(&D3DKMTOpenAdapterFromHdc) pfn_open_adapter_from_hdc = nullptr;
  decltype(&D3DKMTCloseAdapter) pfn_close_adapter = nullptr;
  decltype(&D3DKMTQueryAdapterInfo) pfn_query_adapter_info = nullptr;
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
    p.pfn_query_adapter_info =
        reinterpret_cast<decltype(&D3DKMTQueryAdapterInfo)>(GetProcAddress(gdi32, "D3DKMTQueryAdapterInfo"));
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

void InitUmdPrivate(AeroGpuAdapter* adapter) {
  if (!adapter || adapter->umd_private_valid) {
    return;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_query_adapter_info) {
    return;
  }

  InitKmtAdapterHandle(adapter);
  if (!adapter->kmt_adapter) {
    return;
  }

  aerogpu_umd_private_v1 blob;
  std::memset(&blob, 0, sizeof(blob));

  D3DKMT_QUERYADAPTERINFO q{};
  q.hAdapter = adapter->kmt_adapter;
  q.pPrivateDriverData = &blob;
  q.PrivateDriverDataSize = sizeof(blob);

  // Avoid relying on the WDK's numeric KMTQAITYPE_UMDRIVERPRIVATE constant by probing a
  // small range of values and looking for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  for (UINT type = 0; type < 256; ++type) {
    std::memset(&blob, 0, sizeof(blob));
    q.Type = static_cast<KMTQUERYADAPTERINFOTYPE>(type);

    const NTSTATUS st = procs.pfn_query_adapter_info(&q);
    if (!NT_SUCCESS(st)) {
      continue;
    }

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    adapter->umd_private = blob;
    adapter->umd_private_valid = true;
    break;
  }
}

void DestroyKernelDeviceContext(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }

  dev->wddm_submit.Shutdown();
  dev->kmt_fence_syncobj = 0;
  dev->kmt_context = 0;
  dev->kmt_device = 0;
  dev->dma_buffer_private_data = nullptr;
  dev->dma_buffer_private_data_size = 0;
  dev->monitored_fence_value = nullptr;
  dev->last_submitted_fence.store(0, std::memory_order_relaxed);
  dev->last_completed_fence.store(0, std::memory_order_relaxed);
}

HRESULT InitKernelDeviceContext(AeroGpuDevice* dev, D3D10DDI_HADAPTER hAdapter) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->kmt_context && dev->kmt_fence_syncobj) {
    return S_OK;
  }

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  if (!cb) {
    return S_OK;
  }
  const HRESULT hr =
      dev->wddm_submit.Init(cb, hAdapter.pDrvPrivate, dev->hrt_device.pDrvPrivate, dev->kmt_adapter);
  if (FAILED(hr)) {
    DestroyKernelDeviceContext(dev);
    return hr;
  }

  dev->kmt_device = dev->wddm_submit.hDevice();
  dev->kmt_context = dev->wddm_submit.hContext();
  dev->kmt_fence_syncobj = dev->wddm_submit.hSyncObject();
  if (!dev->kmt_device || !dev->kmt_context || !dev->kmt_fence_syncobj) {
    DestroyKernelDeviceContext(dev);
    return E_FAIL;
  }

  return S_OK;
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
  const uint64_t completed = dev->wddm_submit.QueryCompletedFence();
  UpdateCompletedFence(dev, completed);
  return dev->last_completed_fence.load(std::memory_order_relaxed);
}

// Waits for `fence` to be completed.
//
// `timeout_ms` semantics match D3D11 / DXGI Map expectations:
// - 0: non-blocking poll
// - kAeroGpuTimeoutMsInfinite: infinite wait
//
// On timeout/poll miss, returns `DXGI_ERROR_WAS_STILL_DRAWING`.
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

  const HRESULT hr = dev->wddm_submit.WaitForFenceWithTimeout(fence, timeout_ms);
  if (FAILED(hr)) {
    return hr;
  }

  UpdateCompletedFence(dev, fence);
  (void)AeroGpuQueryCompletedFence(dev);
  return S_OK;
}

uint64_t submit_locked(AeroGpuDevice* dev, bool want_present, HRESULT* out_hr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev) {
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
    return 0;
  }
  if (!dev->adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
    return 0;
  }

  dev->cmd.finalize();
  const size_t submit_bytes = dev->cmd.size();

  uint64_t fence = 0;
  const uint32_t* alloc_handles =
      dev->wddm_submit_allocation_handles.empty() ? nullptr : dev->wddm_submit_allocation_handles.data();
  const uint32_t alloc_count = static_cast<uint32_t>(dev->wddm_submit_allocation_handles.size());
  const HRESULT hr =
      dev->wddm_submit.SubmitAeroCmdStream(dev->cmd.data(), dev->cmd.size(), want_present, alloc_handles, alloc_count, &fence);
  dev->cmd.reset();
  dev->wddm_submit_allocation_handles.clear();
  if (FAILED(hr)) {
    dev->pending_staging_writes.clear();
    if (out_hr) {
      *out_hr = hr;
    }
    return 0;
  }

  if (!dev->pending_staging_writes.empty()) {
    for (AeroGpuResource* res : dev->pending_staging_writes) {
      if (res) {
        res->last_gpu_write_fence = fence;
      }
    }
    dev->pending_staging_writes.clear();
  }

  if (fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, fence);
  }
  AEROGPU_D3D10_11_LOG("D3D10.1 submit_locked: present=%u bytes=%llu fence=%llu completed=%llu",
                       want_present ? 1u : 0u,
                       static_cast<unsigned long long>(submit_bytes),
                       static_cast<unsigned long long>(fence),
                       static_cast<unsigned long long>(AeroGpuQueryCompletedFence(dev)));
  return fence;
}

void set_error(AeroGpuDevice* dev, HRESULT hr);
void unmap_resource_locked(AeroGpuDevice* dev, AeroGpuResource* res, uint32_t subresource);

void flush_locked(AeroGpuDevice* dev) {
  if (dev) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
  HRESULT hr = S_OK;
  submit_locked(dev, false, &hr);
  if (FAILED(hr)) {
    set_error(dev, hr);
  }
}

static void TrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res) {
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

static void TrackBoundTargetsForSubmitLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_rtv_res);
  TrackWddmAllocForSubmitLocked(dev, dev->current_dsv_res);
}

static void TrackDrawStateLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }

  TrackBoundTargetsForSubmitLocked(dev);
  TrackWddmAllocForSubmitLocked(dev, dev->current_vb_res);
  TrackWddmAllocForSubmitLocked(dev, dev->current_ib_res);

  for (AeroGpuResource* res : dev->current_vs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (AeroGpuResource* res : dev->current_ps_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
}

void set_error(AeroGpuDevice* dev, HRESULT hr) {
  // Many D3D10/DDI entrypoints are `void` and must signal failures via the
  // runtime callback instead of returning HRESULT. Log these so bring-up can
  // quickly correlate failures to the last DDI call.
  AEROGPU_D3D10_11_LOG("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
  AEROGPU_D3D10_TRACEF("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
  if (!dev || !dev->pfn_set_error) {
    return;
  }
  if constexpr (std::is_invocable_v<SetErrorFn, D3D10DDI_HDEVICE, HRESULT>) {
    D3D10DDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = dev;
    dev->pfn_set_error(hDevice, hr);
  } else {
    if (!dev->hrt_device.pDrvPrivate) {
      return;
    }
    CallCbMaybeHandle(dev->pfn_set_error, dev->hrt_device, hr);
  }
}

static void InitLockForWrite(D3DDDICB_LOCK* lock) {
  if (!lock) {
    return;
  }
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock->SubresourceIndex = 0; }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock->SubResourceIndex = 0; }

  // `D3DDDICB_LOCKFLAGS` bit names vary slightly across WDK releases.
  __if_exists(D3DDDICB_LOCK::Flags) {
    std::memset(&lock->Flags, 0, sizeof(lock->Flags));
    __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) { lock->Flags.WriteOnly = 1; }
    __if_exists(D3DDDICB_LOCKFLAGS::Write) { lock->Flags.Write = 1; }
  }
}

static void InitUnlockForWrite(D3DDDICB_UNLOCK* unlock) {
  if (!unlock) {
    return;
  }
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) { unlock->SubresourceIndex = 0; }
  __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) { unlock->SubResourceIndex = 0; }
}

void emit_upload_resource_locked(AeroGpuDevice* dev,
                                 const AeroGpuResource* res,
                                 uint64_t offset_bytes,
                                 uint64_t size_bytes) {
  if (!dev || !res || res->handle == kInvalidHandle || !size_bytes) {
    return;
  }

  uint64_t upload_offset = offset_bytes;
  uint64_t upload_size = size_bytes;
  if (res->kind == ResourceKind::Buffer) {
    const uint64_t end = offset_bytes + size_bytes;
    if (end < offset_bytes) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    const uint64_t aligned_start = offset_bytes & ~3ull;
    const uint64_t aligned_end = (end + 3ull) & ~3ull;
    upload_offset = aligned_start;
    upload_size = aligned_end - aligned_start;
  }

  if (upload_offset > res->storage.size()) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  const size_t remaining = res->storage.size() - static_cast<size_t>(upload_offset);
  if (upload_size > remaining) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (upload_size > std::numeric_limits<size_t>::max()) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }

  const size_t off = static_cast<size_t>(upload_offset);
  const size_t sz = static_cast<size_t>(upload_size);

  if (res->backing_alloc_id == 0) {
    const uint8_t* payload = res->storage.data() + off;
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, payload, sz);
    if (!cmd) {
      set_error(dev, E_FAIL);
      return;
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = upload_offset;
    cmd->size_bytes = upload_size;
    return;
  }

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  if (!cb || !cb->pfnLockCb || !cb->pfnUnlockCb || res->wddm_allocation_handle == 0) {
    set_error(dev, E_FAIL);
    return;
  }

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
  InitLockForWrite(&lock_args);

  HRESULT hr = CallCbMaybeHandle(cb->pfnLockCb, dev->hrt_device, &lock_args);
  if (FAILED(hr) || !lock_args.pData) {
    set_error(dev, FAILED(hr) ? hr : E_FAIL);
    return;
  }

  HRESULT copy_hr = S_OK;
  if (res->kind == ResourceKind::Texture2D && upload_offset == 0 && upload_size == res->storage.size() &&
      res->mip_levels == 1 && res->array_size == 1) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      copy_hr = E_NOTIMPL;
      goto Unlock;
    }
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

    const uint8_t* src_base = res->storage.data();
    uint8_t* dst_base = static_cast<uint8_t*>(lock_args.pData);
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
  InitUnlockForWrite(&unlock_args);
  hr = CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
  if (FAILED(hr)) {
    set_error(dev, hr);
    return;
  }
  if (FAILED(copy_hr)) {
    set_error(dev, copy_hr);
    return;
  }

  emit_dirty_range_locked(dev, res, upload_offset, upload_size);
}

void emit_dirty_range_locked(AeroGpuDevice* dev,
                             const AeroGpuResource* res,
                             uint64_t offset_bytes,
                             uint64_t size_bytes) {
  if (!dev || !res || res->handle == kInvalidHandle || !size_bytes) {
    return;
  }

  TrackWddmAllocForSubmitLocked(dev, res);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
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
      // Returning zero from a CalcPrivate*Size stub often causes the runtime to
      // pass a null pDrvPrivate, which in turn tends to crash when the runtime
      // tries to create/destroy the object. Return a small non-zero size so the
      // handle always has valid storage, even when Create* returns E_NOTIMPL.
      return sizeof(uint64_t);
    } else if constexpr (std::is_same_v<Ret, void>) {
      return;
    } else {
      return Ret{};
    }
  }
};

template <typename T, typename = void>
struct HasGenMips : std::false_type {};
template <typename T>
struct HasGenMips<T, std::void_t<decltype(((T*)nullptr)->pfnGenMips)>> : std::true_type {};

template <typename T, typename = void>
struct HasOpenResource : std::false_type {};
template <typename T>
struct HasOpenResource<T, std::void_t<decltype(((T*)nullptr)->pfnOpenResource)>> : std::true_type {};

template <typename T, typename = void>
struct HasCalcPrivatePredicateSize : std::false_type {};
template <typename T>
struct HasCalcPrivatePredicateSize<T, std::void_t<decltype(((T*)nullptr)->pfnCalcPrivatePredicateSize)>> : std::true_type {};

template <typename T, typename = void>
struct HasCreatePredicate : std::false_type {};
template <typename T>
struct HasCreatePredicate<T, std::void_t<decltype(((T*)nullptr)->pfnCreatePredicate)>> : std::true_type {};

template <typename T, typename = void>
struct HasDestroyPredicate : std::false_type {};
template <typename T>
struct HasDestroyPredicate<T, std::void_t<decltype(((T*)nullptr)->pfnDestroyPredicate)>> : std::true_type {};

template <typename T, typename = void>
struct HasStagingResourceMap : std::false_type {};
template <typename T>
struct HasStagingResourceMap<T, std::void_t<decltype(((T*)nullptr)->pfnStagingResourceMap)>> : std::true_type {};

template <typename T, typename = void>
struct HasDynamicIABufferMap : std::false_type {};
template <typename T>
struct HasDynamicIABufferMap<T, std::void_t<decltype(((T*)nullptr)->pfnDynamicIABufferMapDiscard)>> : std::true_type {};

template <typename T, typename = void>
struct HasDynamicConstantBufferMap : std::false_type {};
template <typename T>
struct HasDynamicConstantBufferMap<T, std::void_t<decltype(((T*)nullptr)->pfnDynamicConstantBufferMapDiscard)>> : std::true_type {};
#if AEROGPU_D3D10_TRACE
enum class DdiTraceStubId : size_t {
  SetBlendState = 0,
  SetRasterizerState,
  SetDepthStencilState,
  VsSetConstantBuffers,
  PsSetConstantBuffers,
  VsSetShaderResources,
  PsSetShaderResources,
  VsSetSamplers,
  PsSetSamplers,
  GsSetShader,
  GsSetConstantBuffers,
  GsSetShaderResources,
  GsSetSamplers,
  SetScissorRects,
  Map,
  Unmap,
  UpdateSubresourceUP,
  CopyResource,
  CopySubresourceRegion,
  DrawInstanced,
  DrawIndexedInstanced,
  DrawAuto,
  Count,
};

static constexpr const char* kDdiTraceStubNames[static_cast<size_t>(DdiTraceStubId::Count)] = {
    "SetBlendState",
    "SetRasterizerState",
    "SetDepthStencilState",
    "VsSetConstantBuffers",
    "PsSetConstantBuffers",
    "VsSetShaderResources",
    "PsSetShaderResources",
    "VsSetSamplers",
    "PsSetSamplers",
    "GsSetShader",
    "GsSetConstantBuffers",
    "GsSetShaderResources",
    "GsSetSamplers",
    "SetScissorRects",
    "Map",
    "Unmap",
    "UpdateSubresourceUP",
    "CopyResource",
    "CopySubresourceRegion",
    "DrawInstanced",
    "DrawIndexedInstanced",
    "DrawAuto",
};

template <typename FnPtr, DdiTraceStubId Id>
struct DdiTraceStub;

template <typename Ret, typename... Args, DdiTraceStubId Id>
struct DdiTraceStub<Ret(AEROGPU_APIENTRY*)(Args...), Id> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    constexpr const char* kName = kDdiTraceStubNames[static_cast<size_t>(Id)];
    AEROGPU_D3D10_TRACEF("%s (stub)", kName);

    if constexpr (std::is_same_v<Ret, HRESULT>) {
      const HRESULT hr = DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
      return ::aerogpu::d3d10trace::ret_hr(kName, hr);
    } else {
      return DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
    }
  }
};
#endif // AEROGPU_D3D10_TRACE
template <typename TFnPtr>
struct DdiErrorStub;

template <typename... Args>
struct DdiErrorStub<void(AEROGPU_APIENTRY*)(D3D10DDI_HDEVICE, Args...)> {
  static void AEROGPU_APIENTRY Call(D3D10DDI_HDEVICE hDevice, Args...) {
    auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
    set_error(dev, E_NOTIMPL);
  }
};

template <typename TFnPtr>
struct DdiNoopStub;

template <typename Ret, typename... Args>
struct DdiNoopStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args...) {
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return S_OK;
    } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
      return sizeof(uint64_t);
    } else if constexpr (std::is_same_v<Ret, void>) {
      return;
    } else {
      return Ret{};
    }
  }
};

template <typename FuncsT>
void InitDeviceFuncsWithStubs(FuncsT* funcs) {
  if (!funcs) {
    return;
  }

  std::memset(funcs, 0, sizeof(*funcs));

  // The Win7 D3D10.1 runtime can call a surprising set of entrypoints during
  // device initialization (state reset, default binds, etc). A null pointer
  // here is a process crash, so stub-fill first, then override implemented
  // entrypoints in CreateDevice.
  //
  // For state setters we prefer a no-op stub so the runtime can reset bindings
  // without tripping `pfnSetErrorCb`.
  funcs->pfnDestroyDevice = &DdiNoopStub<decltype(funcs->pfnDestroyDevice)>::Call;

  // Resource and object creation/destruction.
  funcs->pfnCalcPrivateResourceSize = &DdiStub<decltype(funcs->pfnCalcPrivateResourceSize)>::Call;
  funcs->pfnCreateResource = &DdiStub<decltype(funcs->pfnCreateResource)>::Call;
  funcs->pfnDestroyResource = &DdiNoopStub<decltype(funcs->pfnDestroyResource)>::Call;

  funcs->pfnCalcPrivateShaderResourceViewSize = &DdiStub<decltype(funcs->pfnCalcPrivateShaderResourceViewSize)>::Call;
  funcs->pfnCreateShaderResourceView = &DdiStub<decltype(funcs->pfnCreateShaderResourceView)>::Call;
  funcs->pfnDestroyShaderResourceView = &DdiNoopStub<decltype(funcs->pfnDestroyShaderResourceView)>::Call;

  funcs->pfnCalcPrivateRenderTargetViewSize = &DdiStub<decltype(funcs->pfnCalcPrivateRenderTargetViewSize)>::Call;
  funcs->pfnCreateRenderTargetView = &DdiStub<decltype(funcs->pfnCreateRenderTargetView)>::Call;
  funcs->pfnDestroyRenderTargetView = &DdiNoopStub<decltype(funcs->pfnDestroyRenderTargetView)>::Call;

  funcs->pfnCalcPrivateDepthStencilViewSize = &DdiStub<decltype(funcs->pfnCalcPrivateDepthStencilViewSize)>::Call;
  funcs->pfnCreateDepthStencilView = &DdiStub<decltype(funcs->pfnCreateDepthStencilView)>::Call;
  funcs->pfnDestroyDepthStencilView = &DdiNoopStub<decltype(funcs->pfnDestroyDepthStencilView)>::Call;

  funcs->pfnCalcPrivateElementLayoutSize = &DdiStub<decltype(funcs->pfnCalcPrivateElementLayoutSize)>::Call;
  funcs->pfnCreateElementLayout = &DdiStub<decltype(funcs->pfnCreateElementLayout)>::Call;
  funcs->pfnDestroyElementLayout = &DdiNoopStub<decltype(funcs->pfnDestroyElementLayout)>::Call;

  funcs->pfnCalcPrivateSamplerSize = &DdiStub<decltype(funcs->pfnCalcPrivateSamplerSize)>::Call;
  funcs->pfnCreateSampler = &DdiStub<decltype(funcs->pfnCreateSampler)>::Call;
  funcs->pfnDestroySampler = &DdiNoopStub<decltype(funcs->pfnDestroySampler)>::Call;

  funcs->pfnCalcPrivateBlendStateSize = &DdiStub<decltype(funcs->pfnCalcPrivateBlendStateSize)>::Call;
  funcs->pfnCreateBlendState = &DdiStub<decltype(funcs->pfnCreateBlendState)>::Call;
  funcs->pfnDestroyBlendState = &DdiNoopStub<decltype(funcs->pfnDestroyBlendState)>::Call;

  funcs->pfnCalcPrivateRasterizerStateSize = &DdiStub<decltype(funcs->pfnCalcPrivateRasterizerStateSize)>::Call;
  funcs->pfnCreateRasterizerState = &DdiStub<decltype(funcs->pfnCreateRasterizerState)>::Call;
  funcs->pfnDestroyRasterizerState = &DdiNoopStub<decltype(funcs->pfnDestroyRasterizerState)>::Call;

  funcs->pfnCalcPrivateDepthStencilStateSize = &DdiStub<decltype(funcs->pfnCalcPrivateDepthStencilStateSize)>::Call;
  funcs->pfnCreateDepthStencilState = &DdiStub<decltype(funcs->pfnCreateDepthStencilState)>::Call;
  funcs->pfnDestroyDepthStencilState = &DdiNoopStub<decltype(funcs->pfnDestroyDepthStencilState)>::Call;

  funcs->pfnCalcPrivateVertexShaderSize = &DdiStub<decltype(funcs->pfnCalcPrivateVertexShaderSize)>::Call;
  funcs->pfnCreateVertexShader = &DdiStub<decltype(funcs->pfnCreateVertexShader)>::Call;
  funcs->pfnDestroyVertexShader = &DdiNoopStub<decltype(funcs->pfnDestroyVertexShader)>::Call;

  funcs->pfnCalcPrivateGeometryShaderSize = &DdiStub<decltype(funcs->pfnCalcPrivateGeometryShaderSize)>::Call;
  funcs->pfnCreateGeometryShader = &DdiStub<decltype(funcs->pfnCreateGeometryShader)>::Call;
  funcs->pfnDestroyGeometryShader = &DdiNoopStub<decltype(funcs->pfnDestroyGeometryShader)>::Call;

  // Optional stream output variant.
  funcs->pfnCalcPrivateGeometryShaderWithStreamOutputSize =
      &DdiStub<decltype(funcs->pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call;
  funcs->pfnCreateGeometryShaderWithStreamOutput =
      &DdiStub<decltype(funcs->pfnCreateGeometryShaderWithStreamOutput)>::Call;

  funcs->pfnCalcPrivatePixelShaderSize = &DdiStub<decltype(funcs->pfnCalcPrivatePixelShaderSize)>::Call;
  funcs->pfnCreatePixelShader = &DdiStub<decltype(funcs->pfnCreatePixelShader)>::Call;
  funcs->pfnDestroyPixelShader = &DdiNoopStub<decltype(funcs->pfnDestroyPixelShader)>::Call;

  funcs->pfnCalcPrivateQuerySize = &DdiStub<decltype(funcs->pfnCalcPrivateQuerySize)>::Call;
  funcs->pfnCreateQuery = &DdiStub<decltype(funcs->pfnCreateQuery)>::Call;
  funcs->pfnDestroyQuery = &DdiNoopStub<decltype(funcs->pfnDestroyQuery)>::Call;

  // Pipeline binding/state (no-op stubs).
  funcs->pfnIaSetInputLayout = &DdiNoopStub<decltype(funcs->pfnIaSetInputLayout)>::Call;
  funcs->pfnIaSetVertexBuffers = &DdiNoopStub<decltype(funcs->pfnIaSetVertexBuffers)>::Call;
  funcs->pfnIaSetIndexBuffer = &DdiNoopStub<decltype(funcs->pfnIaSetIndexBuffer)>::Call;
  funcs->pfnIaSetTopology = &DdiNoopStub<decltype(funcs->pfnIaSetTopology)>::Call;

  funcs->pfnVsSetShader = &DdiNoopStub<decltype(funcs->pfnVsSetShader)>::Call;
  funcs->pfnVsSetConstantBuffers = &DdiNoopStub<decltype(funcs->pfnVsSetConstantBuffers)>::Call;
  funcs->pfnVsSetShaderResources = &DdiNoopStub<decltype(funcs->pfnVsSetShaderResources)>::Call;
  funcs->pfnVsSetSamplers = &DdiNoopStub<decltype(funcs->pfnVsSetSamplers)>::Call;

  funcs->pfnGsSetShader = &DdiNoopStub<decltype(funcs->pfnGsSetShader)>::Call;
  funcs->pfnGsSetConstantBuffers = &DdiNoopStub<decltype(funcs->pfnGsSetConstantBuffers)>::Call;
  funcs->pfnGsSetShaderResources = &DdiNoopStub<decltype(funcs->pfnGsSetShaderResources)>::Call;
  funcs->pfnGsSetSamplers = &DdiNoopStub<decltype(funcs->pfnGsSetSamplers)>::Call;

  funcs->pfnSoSetTargets = &DdiNoopStub<decltype(funcs->pfnSoSetTargets)>::Call;

  funcs->pfnPsSetShader = &DdiNoopStub<decltype(funcs->pfnPsSetShader)>::Call;
  funcs->pfnPsSetConstantBuffers = &DdiNoopStub<decltype(funcs->pfnPsSetConstantBuffers)>::Call;
  funcs->pfnPsSetShaderResources = &DdiNoopStub<decltype(funcs->pfnPsSetShaderResources)>::Call;
  funcs->pfnPsSetSamplers = &DdiNoopStub<decltype(funcs->pfnPsSetSamplers)>::Call;

  funcs->pfnSetViewports = &DdiNoopStub<decltype(funcs->pfnSetViewports)>::Call;
  funcs->pfnSetScissorRects = &DdiNoopStub<decltype(funcs->pfnSetScissorRects)>::Call;
  funcs->pfnSetRasterizerState = &DdiNoopStub<decltype(funcs->pfnSetRasterizerState)>::Call;
  funcs->pfnSetBlendState = &DdiNoopStub<decltype(funcs->pfnSetBlendState)>::Call;
  funcs->pfnSetDepthStencilState = &DdiNoopStub<decltype(funcs->pfnSetDepthStencilState)>::Call;
  funcs->pfnSetRenderTargets = &DdiNoopStub<decltype(funcs->pfnSetRenderTargets)>::Call;

  // Clears/draws/present. Use error stubs for operations that should not
  // silently succeed.
  funcs->pfnClearRenderTargetView = &DdiNoopStub<decltype(funcs->pfnClearRenderTargetView)>::Call;
  funcs->pfnClearDepthStencilView = &DdiNoopStub<decltype(funcs->pfnClearDepthStencilView)>::Call;

  funcs->pfnDraw = &DdiNoopStub<decltype(funcs->pfnDraw)>::Call;
  funcs->pfnDrawIndexed = &DdiNoopStub<decltype(funcs->pfnDrawIndexed)>::Call;
  funcs->pfnDrawInstanced = &DdiNoopStub<decltype(funcs->pfnDrawInstanced)>::Call;
  funcs->pfnDrawIndexedInstanced = &DdiNoopStub<decltype(funcs->pfnDrawIndexedInstanced)>::Call;
  funcs->pfnDrawAuto = &DdiNoopStub<decltype(funcs->pfnDrawAuto)>::Call;

  funcs->pfnPresent = &DdiStub<decltype(funcs->pfnPresent)>::Call;
  funcs->pfnFlush = &DdiNoopStub<decltype(funcs->pfnFlush)>::Call;
  funcs->pfnRotateResourceIdentities = &DdiNoopStub<decltype(funcs->pfnRotateResourceIdentities)>::Call;

  // Resource update/copy.
  funcs->pfnMap = &DdiStub<decltype(funcs->pfnMap)>::Call;
  funcs->pfnUnmap = &DdiNoopStub<decltype(funcs->pfnUnmap)>::Call;
  funcs->pfnUpdateSubresourceUP = &DdiErrorStub<decltype(funcs->pfnUpdateSubresourceUP)>::Call;
  funcs->pfnCopyResource = &DdiErrorStub<decltype(funcs->pfnCopyResource)>::Call;
  funcs->pfnCopySubresourceRegion = &DdiErrorStub<decltype(funcs->pfnCopySubresourceRegion)>::Call;

  // Misc helpers (optional in many apps, but keep non-null).
  funcs->pfnGenerateMips = &DdiErrorStub<decltype(funcs->pfnGenerateMips)>::Call;
  funcs->pfnResolveSubresource = &DdiErrorStub<decltype(funcs->pfnResolveSubresource)>::Call;

  funcs->pfnBegin = &DdiErrorStub<decltype(funcs->pfnBegin)>::Call;
  funcs->pfnEnd = &DdiErrorStub<decltype(funcs->pfnEnd)>::Call;

  funcs->pfnSetPredication = &DdiNoopStub<decltype(funcs->pfnSetPredication)>::Call;
  funcs->pfnClearState = &DdiNoopStub<decltype(funcs->pfnClearState)>::Call;

  funcs->pfnSetTextFilterSize = &DdiNoopStub<decltype(funcs->pfnSetTextFilterSize)>::Call;
  funcs->pfnReadFromSubresource = &DdiErrorStub<decltype(funcs->pfnReadFromSubresource)>::Call;
  funcs->pfnWriteToSubresource = &DdiErrorStub<decltype(funcs->pfnWriteToSubresource)>::Call;

  funcs->pfnCalcPrivateCounterSize = &DdiStub<decltype(funcs->pfnCalcPrivateCounterSize)>::Call;
  funcs->pfnCreateCounter = &DdiStub<decltype(funcs->pfnCreateCounter)>::Call;
  funcs->pfnDestroyCounter = &DdiNoopStub<decltype(funcs->pfnDestroyCounter)>::Call;

  // Specialized map helpers (if present in the function table for this interface version).
  using DeviceFuncs = std::remove_pointer_t<decltype(funcs)>;
  if constexpr (HasOpenResource<DeviceFuncs>::value) {
    funcs->pfnOpenResource = &DdiStub<decltype(funcs->pfnOpenResource)>::Call;
  }
  if constexpr (HasGenMips<DeviceFuncs>::value) {
    funcs->pfnGenMips = &DdiErrorStub<decltype(funcs->pfnGenMips)>::Call;
  }
  if constexpr (HasCalcPrivatePredicateSize<DeviceFuncs>::value) {
    funcs->pfnCalcPrivatePredicateSize = &DdiStub<decltype(funcs->pfnCalcPrivatePredicateSize)>::Call;
  }
  if constexpr (HasCreatePredicate<DeviceFuncs>::value) {
    funcs->pfnCreatePredicate = &DdiStub<decltype(funcs->pfnCreatePredicate)>::Call;
  }
  if constexpr (HasDestroyPredicate<DeviceFuncs>::value) {
    funcs->pfnDestroyPredicate = &DdiNoopStub<decltype(funcs->pfnDestroyPredicate)>::Call;
  }
  if constexpr (HasStagingResourceMap<DeviceFuncs>::value) {
    funcs->pfnStagingResourceMap = &DdiStub<decltype(funcs->pfnStagingResourceMap)>::Call;
    funcs->pfnStagingResourceUnmap = &DdiNoopStub<decltype(funcs->pfnStagingResourceUnmap)>::Call;
  }
  if constexpr (HasDynamicIABufferMap<DeviceFuncs>::value) {
    funcs->pfnDynamicIABufferMapDiscard = &DdiStub<decltype(funcs->pfnDynamicIABufferMapDiscard)>::Call;
    funcs->pfnDynamicIABufferMapNoOverwrite = &DdiStub<decltype(funcs->pfnDynamicIABufferMapNoOverwrite)>::Call;
    funcs->pfnDynamicIABufferUnmap = &DdiNoopStub<decltype(funcs->pfnDynamicIABufferUnmap)>::Call;
  }
  if constexpr (HasDynamicConstantBufferMap<DeviceFuncs>::value) {
    funcs->pfnDynamicConstantBufferMapDiscard = &DdiStub<decltype(funcs->pfnDynamicConstantBufferMapDiscard)>::Call;
    funcs->pfnDynamicConstantBufferUnmap = &DdiNoopStub<decltype(funcs->pfnDynamicConstantBufferUnmap)>::Call;
  }
}

// CopyResource is used by the Win7 staging readback path (copy backbuffer ->
// staging, then Map). Prefer emitting COPY_* commands so the host executor can
// perform the copy; for staging destinations request WRITEBACK_DST so Map(READ)
// observes the updated bytes.
static uint64_t resource_total_bytes(const AeroGpuDevice* dev, const AeroGpuResource* res);

template <typename FnPtr>
struct CopyResourceImpl;

template <typename Ret, typename... Args>
struct CopyResourceImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    D3D10DDI_HDEVICE hDevice{};
    bool has_device = false;
    D3D10DDI_HRESOURCE res_args[2]{};
    uint32_t count = 0;

    auto capture = [&](auto v) {
      using T = std::decay_t<decltype(v)>;
      if constexpr (std::is_same_v<T, D3D10DDI_HDEVICE>) {
        if (!has_device) {
          hDevice = v;
          has_device = true;
        }
      }
      if constexpr (std::is_same_v<T, D3D10DDI_HRESOURCE>) {
        if (count < 2) {
          res_args[count++] = v;
        }
      }
    };
    (capture(args), ...);

    auto* dev = has_device ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
    if (dev) {
      dev->mutex.lock();
    }

    auto finish = [&](HRESULT hr) -> Ret {
      if (FAILED(hr)) {
        set_error(dev, hr);
      }
      if (dev) {
        dev->mutex.unlock();
      }
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return hr;
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    };

    if (count < 2) {
      return finish(E_INVALIDARG);
    }

    auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(res_args[0]);
    auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(res_args[1]);
    if (!dst || !src) {
      return finish(E_INVALIDARG);
    }

    if (!dev) {
      return finish(E_INVALIDARG);
    }

    try {
      if (dst->kind != src->kind) {
        return finish(E_INVALIDARG);
      }

      if (dst->kind == ResourceKind::Buffer) {
        const uint64_t copy_bytes = std::min<uint64_t>(dst->size_bytes, src->size_bytes);

        const uint64_t dst_storage_bytes = AlignUpU64(dst->size_bytes ? dst->size_bytes : 1, 4);
        const uint64_t src_storage_bytes = AlignUpU64(src->size_bytes ? src->size_bytes : 1, 4);
        if (dst_storage_bytes > static_cast<uint64_t>(SIZE_MAX) || src_storage_bytes > static_cast<uint64_t>(SIZE_MAX)) {
          return finish(E_OUTOFMEMORY);
        }

        if (dst->storage.size() < static_cast<size_t>(dst_storage_bytes)) {
          dst->storage.resize(static_cast<size_t>(dst_storage_bytes), 0);
        }
        if (src->storage.size() < static_cast<size_t>(src_storage_bytes)) {
          src->storage.resize(static_cast<size_t>(src_storage_bytes), 0);
        }

        if (copy_bytes) {
          std::memcpy(dst->storage.data(), src->storage.data(), static_cast<size_t>(copy_bytes));
        }

        const bool transfer_aligned = ((copy_bytes & 3ull) == 0);
        const bool same_buffer = (dst->handle == src->handle);
        if (SupportsTransfer(dev) && transfer_aligned && !same_buffer) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          TrackWddmAllocForSubmitLocked(dev, src);

          auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
          if (!cmd) {
            return finish(E_OUTOFMEMORY);
          }
          cmd->dst_buffer = dst->handle;
          cmd->src_buffer = src->handle;
          cmd->dst_offset_bytes = 0;
          cmd->src_offset_bytes = 0;
          cmd->size_bytes = copy_bytes;
          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->bind_flags == 0 && dst->backing_alloc_id != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          cmd->flags = copy_flags;
          cmd->reserved0 = 0;
          TrackStagingWriteLocked(dev, dst);
        } else if (copy_bytes) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          emit_upload_resource_locked(dev, dst, 0, copy_bytes);
        }
      } else if (dst->kind == ResourceKind::Texture2D) {
        if (dst->dxgi_format != src->dxgi_format) {
          return finish(E_INVALIDARG);
        }

        const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
        if (aer_fmt == AEROGPU_FORMAT_INVALID) {
          return finish(E_NOTIMPL);
        }
        if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
          return finish(E_NOTIMPL);
        }

        const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
        if (!fmt_layout.valid || fmt_layout.block_width == 0 || fmt_layout.block_height == 0 ||
            fmt_layout.bytes_per_block == 0) {
          return finish(E_INVALIDARG);
        }

        auto ensure_layout = [&](AeroGpuResource* res) -> bool {
          if (!res) {
            return false;
          }
          if (res->row_pitch_bytes == 0) {
            const uint32_t min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
            if (min_row == 0) {
              return false;
            }
            res->row_pitch_bytes = AlignUpU32(min_row, 256);
          }
          uint64_t total_bytes = 0;
          return build_texture2d_subresource_layouts(aer_fmt,
                                                     res->width,
                                                     res->height,
                                                     res->mip_levels,
                                                     res->array_size,
                                                     res->row_pitch_bytes,
                                                     &res->tex2d_subresources,
                                                     &total_bytes);
        };
        if (!ensure_layout(dst) || !ensure_layout(src)) {
          return finish(E_INVALIDARG);
        }

        const uint64_t dst_total = resource_total_bytes(dev, dst);
        const uint64_t src_total = resource_total_bytes(dev, src);
        if (dst_total > static_cast<uint64_t>(SIZE_MAX) || src_total > static_cast<uint64_t>(SIZE_MAX)) {
          return finish(E_OUTOFMEMORY);
        }
        if (dst->storage.size() < static_cast<size_t>(dst_total)) {
          dst->storage.resize(static_cast<size_t>(dst_total), 0);
        }
        if (src->storage.size() < static_cast<size_t>(src_total)) {
          src->storage.resize(static_cast<size_t>(src_total), 0);
        }

        const uint32_t subresource_count =
            static_cast<uint32_t>(std::min(dst->tex2d_subresources.size(), src->tex2d_subresources.size()));

        for (uint32_t sub = 0; sub < subresource_count; ++sub) {
          const Texture2DSubresourceLayout dst_sub = dst->tex2d_subresources[sub];
          const Texture2DSubresourceLayout src_sub = src->tex2d_subresources[sub];

          const uint32_t copy_w = std::min(dst_sub.width, src_sub.width);
          const uint32_t copy_h = std::min(dst_sub.height, src_sub.height);
          if (copy_w == 0 || copy_h == 0) {
            continue;
          }

          const uint32_t copy_width_blocks = aerogpu_div_round_up_u32(copy_w, fmt_layout.block_width);
          const uint32_t copy_height_blocks = aerogpu_div_round_up_u32(copy_h, fmt_layout.block_height);
          const uint64_t row_bytes_u64 = static_cast<uint64_t>(copy_width_blocks) *
                                        static_cast<uint64_t>(fmt_layout.bytes_per_block);
          if (row_bytes_u64 == 0 || row_bytes_u64 > static_cast<uint64_t>(SIZE_MAX)) {
            return finish(E_OUTOFMEMORY);
          }
          const size_t row_bytes = static_cast<size_t>(row_bytes_u64);

          if (dst_sub.row_pitch_bytes < row_bytes_u64 || src_sub.row_pitch_bytes < row_bytes_u64) {
            return finish(E_INVALIDARG);
          }
          if (copy_height_blocks > dst_sub.rows_in_layout || copy_height_blocks > src_sub.rows_in_layout) {
            return finish(E_INVALIDARG);
          }

          for (uint32_t y = 0; y < copy_height_blocks; ++y) {
            const uint64_t src_off_u64 =
                src_sub.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(src_sub.row_pitch_bytes);
            const uint64_t dst_off_u64 =
                dst_sub.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(dst_sub.row_pitch_bytes);
            if (src_off_u64 > src_total || dst_off_u64 > dst_total) {
              return finish(E_INVALIDARG);
            }
            const size_t src_off = static_cast<size_t>(src_off_u64);
            const size_t dst_off = static_cast<size_t>(dst_off_u64);
            if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
              return finish(E_INVALIDARG);
            }
            std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
          }
        }

        const bool same_texture = (dst->handle == src->handle);
        if (SupportsTransfer(dev) && !same_texture) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          TrackWddmAllocForSubmitLocked(dev, src);

          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->bind_flags == 0 && dst->backing_alloc_id != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          for (uint32_t sub = 0; sub < subresource_count; ++sub) {
            const Texture2DSubresourceLayout dst_sub = dst->tex2d_subresources[sub];
            const Texture2DSubresourceLayout src_sub = src->tex2d_subresources[sub];

            const uint32_t copy_w = std::min(dst_sub.width, src_sub.width);
            const uint32_t copy_h = std::min(dst_sub.height, src_sub.height);
            if (copy_w == 0 || copy_h == 0) {
              continue;
            }

            auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
            if (!cmd) {
              return finish(E_OUTOFMEMORY);
            }
            cmd->dst_texture = dst->handle;
            cmd->src_texture = src->handle;
            cmd->dst_mip_level = dst_sub.mip_level;
            cmd->dst_array_layer = dst_sub.array_layer;
            cmd->src_mip_level = src_sub.mip_level;
            cmd->src_array_layer = src_sub.array_layer;
            cmd->dst_x = 0;
            cmd->dst_y = 0;
            cmd->src_x = 0;
            cmd->src_y = 0;
            cmd->width = copy_w;
            cmd->height = copy_h;
            cmd->flags = copy_flags;
            cmd->reserved0 = 0;
          }
          TrackStagingWriteLocked(dev, dst);
        } else if (dst_total != 0) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          emit_upload_resource_locked(dev, dst, 0, dst_total);
        }
      }
    } catch (...) {
      return finish(E_OUTOFMEMORY);
    }

    return finish(S_OK);
  }
};

// Minimal CPU-side CopySubresourceRegion implementation (full-copy only). Some
// D3D10.x runtimes may implement CopyResource in terms of CopySubresourceRegion.
template <typename FnPtr>
struct CopySubresourceRegionImpl;

template <typename Ret, typename... Args>
struct CopySubresourceRegionImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    D3D10DDI_HDEVICE hDevice{};
    bool has_device = false;
    D3D10DDI_HRESOURCE res_args[2]{};
    uint32_t count = 0;
    std::array<uint32_t, 8> u32_args{};
    size_t u32_count = 0;
    const D3D10_DDI_BOX* src_box = nullptr;

    auto capture = [&](auto v) {
      using T = std::decay_t<decltype(v)>;
      if constexpr (std::is_same_v<T, D3D10DDI_HDEVICE>) {
        if (!has_device) {
          hDevice = v;
          has_device = true;
        }
      } else if constexpr (std::is_same_v<T, D3D10DDI_HRESOURCE>) {
        if (count < 2) {
          res_args[count++] = v;
        }
      } else if constexpr (std::is_same_v<T, UINT>) {
        if (u32_count < u32_args.size()) {
          u32_args[u32_count++] = static_cast<uint32_t>(v);
        }
      } else if constexpr (std::is_pointer_v<T> &&
                           std::is_same_v<std::remove_cv_t<std::remove_pointer_t<T>>, D3D10_DDI_BOX>) {
        src_box = v;
      }
    };
    (capture(args), ...);

    auto* dev = has_device ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;

    if (count < 2 || !dev) {
      set_error(dev, E_INVALIDARG);
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_INVALIDARG;
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    }

    auto finish = [&](HRESULT hr) -> Ret {
      if (FAILED(hr)) {
        set_error(dev, hr);
      }
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return hr;
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    };

    if (u32_count < 5) {
      return finish(E_INVALIDARG);
    }

    const uint32_t dst_subresource = u32_args[0];
    const uint32_t dst_x = u32_args[1];
    const uint32_t dst_y = u32_args[2];
    const uint32_t dst_z = u32_args[3];
    const uint32_t src_subresource = u32_args[4];

    auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(res_args[0]);
    auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(res_args[1]);
    if (!dst || !src) {
      return finish(E_INVALIDARG);
    }

    std::lock_guard<std::mutex> lock(dev->mutex);

    if (dst->kind != src->kind) {
      return finish(E_INVALIDARG);
    }

    try {
      if (dst->kind == ResourceKind::Buffer) {
        if (dst_subresource != 0 || src_subresource != 0) {
          return finish(E_INVALIDARG);
        }
        if (dst_y != 0 || dst_z != 0) {
          return finish(E_NOTIMPL);
        }

        const uint64_t dst_off = static_cast<uint64_t>(dst_x);
        uint64_t src_left = 0;
        uint64_t src_right = src->size_bytes;
        if (src_box) {
          if (src_box->right < src_box->left || src_box->top != 0 || src_box->bottom != 1 ||
              src_box->front != 0 || src_box->back != 1) {
            return finish(E_INVALIDARG);
          }
          src_left = static_cast<uint64_t>(src_box->left);
          src_right = static_cast<uint64_t>(src_box->right);
        }
        if (src_right < src_left) {
          return finish(E_INVALIDARG);
        }

        const uint64_t requested = src_right - src_left;
        const uint64_t max_src = (src_left < src->size_bytes) ? (src->size_bytes - src_left) : 0;
        const uint64_t max_dst = (dst_off < dst->size_bytes) ? (dst->size_bytes - dst_off) : 0;
        const uint64_t bytes = std::min(std::min(requested, max_src), max_dst);

        const uint64_t dst_storage_u64 = AlignUpU64(dst->size_bytes ? dst->size_bytes : 1, 4);
        const uint64_t src_storage_u64 = AlignUpU64(src->size_bytes ? src->size_bytes : 1, 4);
        if (dst_storage_u64 > static_cast<uint64_t>(SIZE_MAX) || src_storage_u64 > static_cast<uint64_t>(SIZE_MAX)) {
          return finish(E_OUTOFMEMORY);
        }
        if (dst->storage.size() < static_cast<size_t>(dst_storage_u64)) {
          dst->storage.resize(static_cast<size_t>(dst_storage_u64), 0);
        }
        if (src->storage.size() < static_cast<size_t>(src_storage_u64)) {
          src->storage.resize(static_cast<size_t>(src_storage_u64), 0);
        }

        if (bytes) {
          std::memcpy(dst->storage.data() + static_cast<size_t>(dst_off),
                      src->storage.data() + static_cast<size_t>(src_left),
                      static_cast<size_t>(bytes));
        }

        const bool transfer_aligned = ((dst_off & 3ull) == 0) && ((src_left & 3ull) == 0) && ((bytes & 3ull) == 0);
        const bool same_buffer = (dst->handle == src->handle);
        if (SupportsTransfer(dev) && transfer_aligned && bytes && !same_buffer) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          TrackWddmAllocForSubmitLocked(dev, src);

          auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
          if (!cmd) {
            return finish(E_OUTOFMEMORY);
          }
          cmd->dst_buffer = dst->handle;
          cmd->src_buffer = src->handle;
          cmd->dst_offset_bytes = dst_off;
          cmd->src_offset_bytes = src_left;
          cmd->size_bytes = bytes;
          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->bind_flags == 0 && dst->backing_alloc_id != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          cmd->flags = copy_flags;
          cmd->reserved0 = 0;
          TrackStagingWriteLocked(dev, dst);
        } else if (bytes) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          emit_upload_resource_locked(dev, dst, dst_off, bytes);
        }
        return finish(S_OK);
      }

      if (dst->kind == ResourceKind::Texture2D) {
        if (dst_z != 0) {
          return finish(E_INVALIDARG);
        }
        if (dst->dxgi_format != src->dxgi_format) {
          return finish(E_INVALIDARG);
        }

        const uint32_t aer_fmt = dxgi_format_to_aerogpu(dst->dxgi_format);
        if (aer_fmt == AEROGPU_FORMAT_INVALID) {
          return finish(E_NOTIMPL);
        }
        if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
          return finish(E_NOTIMPL);
        }
        const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
        if (!fmt_layout.valid || fmt_layout.block_width == 0 || fmt_layout.block_height == 0 ||
            fmt_layout.bytes_per_block == 0) {
          return finish(E_INVALIDARG);
        }

        auto ensure_layout = [&](AeroGpuResource* res) -> bool {
          if (!res) {
            return false;
          }
          if (res->row_pitch_bytes == 0) {
            const uint32_t min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
            if (min_row == 0) {
              return false;
            }
            res->row_pitch_bytes = AlignUpU32(min_row, 256);
          }
          uint64_t total_bytes = 0;
          return build_texture2d_subresource_layouts(aer_fmt,
                                                     res->width,
                                                     res->height,
                                                     res->mip_levels,
                                                     res->array_size,
                                                     res->row_pitch_bytes,
                                                     &res->tex2d_subresources,
                                                     &total_bytes);
        };
        if (!ensure_layout(dst) || !ensure_layout(src)) {
          return finish(E_INVALIDARG);
        }

        const uint64_t dst_sub_count =
            static_cast<uint64_t>(dst->mip_levels) * static_cast<uint64_t>(dst->array_size);
        const uint64_t src_sub_count =
            static_cast<uint64_t>(src->mip_levels) * static_cast<uint64_t>(src->array_size);
        if (dst_sub_count == 0 || src_sub_count == 0 ||
            dst_subresource >= dst_sub_count || src_subresource >= src_sub_count ||
            dst_subresource >= dst->tex2d_subresources.size() ||
            src_subresource >= src->tex2d_subresources.size()) {
          return finish(E_INVALIDARG);
        }

        const Texture2DSubresourceLayout dst_sub = dst->tex2d_subresources[dst_subresource];
        const Texture2DSubresourceLayout src_sub = src->tex2d_subresources[src_subresource];

        uint32_t src_left = 0;
        uint32_t src_top = 0;
        uint32_t src_right = src_sub.width;
        uint32_t src_bottom = src_sub.height;
        if (src_box) {
          if (src_box->right < src_box->left || src_box->bottom < src_box->top ||
              src_box->front != 0 || src_box->back != 1) {
            return finish(E_INVALIDARG);
          }
          src_left = static_cast<uint32_t>(src_box->left);
          src_top = static_cast<uint32_t>(src_box->top);
          src_right = static_cast<uint32_t>(src_box->right);
          src_bottom = static_cast<uint32_t>(src_box->bottom);
        }
        if (src_right > src_sub.width || src_bottom > src_sub.height) {
          return finish(E_INVALIDARG);
        }
        if (dst_x > dst_sub.width || dst_y > dst_sub.height) {
          return finish(E_INVALIDARG);
        }

        const uint32_t src_extent_w = src_right - src_left;
        const uint32_t src_extent_h = src_bottom - src_top;
        const uint32_t max_dst_w = dst_sub.width - dst_x;
        const uint32_t max_dst_h = dst_sub.height - dst_y;
        const uint32_t copy_w = std::min(src_extent_w, max_dst_w);
        const uint32_t copy_h = std::min(src_extent_h, max_dst_h);
        if (copy_w == 0 || copy_h == 0) {
          return finish(S_OK);
        }

        const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
          return (v % align) == 0 || v == extent;
        };
        if (fmt_layout.block_width > 1 || fmt_layout.block_height > 1) {
          if (!aligned_or_edge(src_left, fmt_layout.block_width, src_sub.width) ||
              !aligned_or_edge(src_right, fmt_layout.block_width, src_sub.width) ||
              !aligned_or_edge(dst_x, fmt_layout.block_width, dst_sub.width) ||
              !aligned_or_edge(dst_x + copy_w, fmt_layout.block_width, dst_sub.width) ||
              !aligned_or_edge(src_top, fmt_layout.block_height, src_sub.height) ||
              !aligned_or_edge(src_bottom, fmt_layout.block_height, src_sub.height) ||
              !aligned_or_edge(dst_y, fmt_layout.block_height, dst_sub.height) ||
              !aligned_or_edge(dst_y + copy_h, fmt_layout.block_height, dst_sub.height)) {
            return finish(E_INVALIDARG);
          }
        }

        const uint32_t src_x_blocks = src_left / fmt_layout.block_width;
        const uint32_t src_y_blocks = src_top / fmt_layout.block_height;
        const uint32_t dst_x_blocks = dst_x / fmt_layout.block_width;
        const uint32_t dst_y_blocks = dst_y / fmt_layout.block_height;

        const uint32_t copy_width_blocks = aerogpu_div_round_up_u32(copy_w, fmt_layout.block_width);
        const uint32_t copy_height_blocks = aerogpu_div_round_up_u32(copy_h, fmt_layout.block_height);
        const uint64_t row_bytes_u64 =
            static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
        if (row_bytes_u64 == 0 || row_bytes_u64 > static_cast<uint64_t>(SIZE_MAX)) {
          return finish(E_OUTOFMEMORY);
        }
        const size_t row_bytes = static_cast<size_t>(row_bytes_u64);

        const uint64_t dst_total = resource_total_bytes(dev, dst);
        const uint64_t src_total = resource_total_bytes(dev, src);
        if (dst_total > static_cast<uint64_t>(SIZE_MAX) || src_total > static_cast<uint64_t>(SIZE_MAX)) {
          return finish(E_OUTOFMEMORY);
        }
        if (dst->storage.size() < static_cast<size_t>(dst_total)) {
          dst->storage.resize(static_cast<size_t>(dst_total), 0);
        }
        if (src->storage.size() < static_cast<size_t>(src_total)) {
          src->storage.resize(static_cast<size_t>(src_total), 0);
        }

        if (copy_height_blocks > dst_sub.rows_in_layout || copy_height_blocks > src_sub.rows_in_layout) {
          return finish(E_INVALIDARG);
        }
        if (dst_x_blocks > (dst_sub.row_pitch_bytes / fmt_layout.bytes_per_block) ||
            src_x_blocks > (src_sub.row_pitch_bytes / fmt_layout.bytes_per_block)) {
          return finish(E_INVALIDARG);
        }

        for (uint32_t y = 0; y < copy_height_blocks; ++y) {
          const uint64_t src_off_u64 =
              src_sub.offset_bytes +
              static_cast<uint64_t>(src_y_blocks + y) * static_cast<uint64_t>(src_sub.row_pitch_bytes) +
              static_cast<uint64_t>(src_x_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
          const uint64_t dst_off_u64 =
              dst_sub.offset_bytes +
              static_cast<uint64_t>(dst_y_blocks + y) * static_cast<uint64_t>(dst_sub.row_pitch_bytes) +
              static_cast<uint64_t>(dst_x_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
          if (src_off_u64 > src_total || dst_off_u64 > dst_total) {
            return finish(E_INVALIDARG);
          }
          const size_t src_off = static_cast<size_t>(src_off_u64);
          const size_t dst_off = static_cast<size_t>(dst_off_u64);
          if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
            return finish(E_INVALIDARG);
          }
          std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
        }

        const bool same_texture = (dst->handle == src->handle);
        if (SupportsTransfer(dev) && !same_texture) {
          TrackWddmAllocForSubmitLocked(dev, dst);
          TrackWddmAllocForSubmitLocked(dev, src);

          auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
          if (!cmd) {
            return finish(E_OUTOFMEMORY);
          }
          cmd->dst_texture = dst->handle;
          cmd->src_texture = src->handle;
          cmd->dst_mip_level = dst_sub.mip_level;
          cmd->dst_array_layer = dst_sub.array_layer;
          cmd->src_mip_level = src_sub.mip_level;
          cmd->src_array_layer = src_sub.array_layer;
          cmd->dst_x = dst_x;
          cmd->dst_y = dst_y;
          cmd->src_x = src_left;
          cmd->src_y = src_top;
          cmd->width = copy_w;
          cmd->height = copy_h;
          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->bind_flags == 0 && dst->backing_alloc_id != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          cmd->flags = copy_flags;
          cmd->reserved0 = 0;
          TrackStagingWriteLocked(dev, dst);
        } else {
          TrackWddmAllocForSubmitLocked(dev, dst);
          emit_upload_resource_locked(dev, dst, dst_sub.offset_bytes, dst_sub.size_bytes);
        }
        return finish(S_OK);
      }
    } catch (...) {
      return finish(E_OUTOFMEMORY);
    }

    return finish(E_NOTIMPL);
  }
};

// -------------------------------------------------------------------------------------------------
// D3D10.1 Device DDI (minimal subset + conservative stubs)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_TRACEF("DestroyDevice hDevice=%p", hDevice.pDrvPrivate);
  void* device_mem = hDevice.pDrvPrivate;
  if (!device_mem) {
    return;
  }
  uint32_t cookie = 0;
  std::memcpy(&cookie, device_mem, sizeof(cookie));
  if (cookie != kAeroGpuDeviceLiveCookie) {
    return;
  }
  cookie = 0;
  std::memcpy(device_mem, &cookie, sizeof(cookie));

  auto* dev = reinterpret_cast<AeroGpuDevice*>(device_mem);
  DestroyKernelDeviceContext(dev);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERESOURCE*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateResourceSize");
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATERESOURCE* pDesc,
                                        D3D10DDI_HRESOURCE hResource,
                                        D3D10DDI_HRTRESOURCE hRTResource) {
  const void* init_ptr = nullptr;
  if (pDesc) {
    __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      init_ptr = pDesc->pInitialDataUP;
    }
    __if_not_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialData) {
        init_ptr = pDesc->pInitialData;
      }
    }
  }
  AEROGPU_D3D10_TRACEF(
      "CreateResource hDevice=%p hResource=%p dim=%u bind=0x%x misc=0x%x byteWidth=%u w=%u h=%u mips=%u array=%u fmt=%u "
      "init=%p",
      hDevice.pDrvPrivate,
      hResource.pDrvPrivate,
      pDesc ? static_cast<unsigned>(pDesc->ResourceDimension) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->BindFlags) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->MiscFlags) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->ByteWidth) : 0u,
      (pDesc && pDesc->pMipInfoList) ? static_cast<unsigned>(pDesc->pMipInfoList[0].TexelWidth) : 0u,
      (pDesc && pDesc->pMipInfoList) ? static_cast<unsigned>(pDesc->pMipInfoList[0].TexelHeight) : 0u,
       pDesc ? static_cast<unsigned>(pDesc->MipLevels) : 0u,
       pDesc ? static_cast<unsigned>(pDesc->ArraySize) : 0u,
       pDesc ? static_cast<unsigned>(pDesc->Format) : 0u,
       init_ptr);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  uint32_t usage = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
    usage = static_cast<uint32_t>(pDesc ? pDesc->Usage : 0);
  }

  uint32_t cpu_access = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::CPUAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc ? pDesc->CPUAccessFlags : 0);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::CpuAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc ? pDesc->CpuAccessFlags : 0);
  }

  uint32_t sample_count = 0;
  uint32_t sample_quality = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::SampleDesc) {
    sample_count = static_cast<uint32_t>(pDesc ? pDesc->SampleDesc.Count : 0);
    sample_quality = static_cast<uint32_t>(pDesc ? pDesc->SampleDesc.Quality : 0);
  }

  uint64_t resource_flags_bits = 0;
  uint32_t resource_flags_size = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::ResourceFlags) {
    resource_flags_size = static_cast<uint32_t>(sizeof(pDesc->ResourceFlags));
    const size_t n = std::min(sizeof(resource_flags_bits), sizeof(pDesc->ResourceFlags));
    if (pDesc) {
      std::memcpy(&resource_flags_bits, &pDesc->ResourceFlags, n);
    }
  }

  uint32_t num_allocations = 0;
  const void* allocation_info = nullptr;
  const void* primary_desc = nullptr;
  __if_exists(D3D10DDIARG_CREATERESOURCE::NumAllocations) {
    num_allocations = static_cast<uint32_t>(pDesc ? pDesc->NumAllocations : 0);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::pAllocationInfo) {
    allocation_info = pDesc ? pDesc->pAllocationInfo : nullptr;
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::pPrimaryDesc) {
    primary_desc = pDesc ? pDesc->pPrimaryDesc : nullptr;
  }

  const uint32_t tex_w =
      (pDesc && pDesc->pMipInfoList) ? static_cast<uint32_t>(pDesc->pMipInfoList[0].TexelWidth) : 0;
  const uint32_t tex_h =
      (pDesc && pDesc->pMipInfoList) ? static_cast<uint32_t>(pDesc->pMipInfoList[0].TexelHeight) : 0;

  uint32_t primary = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::pPrimaryDesc) {
    primary = (pDesc && pDesc->pPrimaryDesc != nullptr) ? 1u : 0u;
  }

  AEROGPU_D3D10_11_LOG(
      "trace_resources: D3D10.1 CreateResource dim=%u bind=0x%08X usage=%u cpu=0x%08X misc=0x%08X fmt=%u "
      "byteWidth=%u w=%u h=%u mips=%u array=%u sample=(%u,%u) rflags=0x%llX rflags_size=%u primary=%u "
      "mipInfoList=%p init=%p num_alloc=%u alloc_info=%p primary_desc=%p",
      pDesc ? static_cast<unsigned>(pDesc->ResourceDimension) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->BindFlags) : 0u,
      static_cast<unsigned>(usage),
      static_cast<unsigned>(cpu_access),
      pDesc ? static_cast<unsigned>(pDesc->MiscFlags) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->Format) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->ByteWidth) : 0u,
      static_cast<unsigned>(tex_w),
      static_cast<unsigned>(tex_h),
      pDesc ? static_cast<unsigned>(pDesc->MipLevels) : 0u,
      pDesc ? static_cast<unsigned>(pDesc->ArraySize) : 0u,
      static_cast<unsigned>(sample_count),
      static_cast<unsigned>(sample_quality),
      static_cast<unsigned long long>(resource_flags_bits),
      static_cast<unsigned>(resource_flags_size),
      static_cast<unsigned>(primary),
      pDesc ? pDesc->pMipInfoList : nullptr,
      init_ptr,
      static_cast<unsigned>(num_allocations),
      allocation_info,
      primary_desc);
#endif
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // The Win7 DDI passes a superset of D3D10_RESOURCE_DIMENSION/D3D11_RESOURCE_DIMENSION.
  // For bring-up we only accept buffers and 2D textures.
  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  if (!cb || !cb->pfnAllocateCb || !cb->pfnDeallocateCb) {
    set_error(dev, E_FAIL);
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = AllocateGlobalHandle(dev->adapter);
  res->bind_flags = pDesc->BindFlags;
  res->misc_flags = pDesc->MiscFlags;

  bool is_primary = false;
  __if_exists(D3D10DDIARG_CREATERESOURCE::pPrimaryDesc) {
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
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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

    (void)CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
    res->wddm_allocation_handle = 0;
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
    __if_exists(D3D10DDIARG_CREATERESOURCE::NumAllocations) {
      if (pDesc->NumAllocations < 1) {
        return E_INVALIDARG;
      }
      if (pDesc->NumAllocations != 1) {
        return E_NOTIMPL;
      }
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
      alloc_id = static_cast<uint32_t>(AllocateGlobalHandle(dev->adapter)) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
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
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      #ifdef D3D10_USAGE_STAGING
      if (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING)) {
        priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING;
      }
      #else
      if (static_cast<uint32_t>(pDesc->Usage) == 3u) {
        priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING;
      }
      #endif
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
      alloc.hContext = UintPtrToD3dHandle<decltype(alloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
    }
    __if_exists(D3DDDICB_ALLOCATE::hResource) {
      alloc.hResource = hRTResource;
    }
    __if_exists(D3DDDICB_ALLOCATE::NumAllocations) {
      alloc.NumAllocations = 1;
    }
    __if_exists(D3DDDICB_ALLOCATE::pAllocationInfo) {
      alloc.pAllocationInfo = alloc_info;
    }
    __if_exists(D3DDDICB_ALLOCATE::Flags) {
      alloc.Flags.Value = 0;
      alloc.Flags.CreateResource = 1;
      if (is_shared) {
        alloc.Flags.CreateShared = 1;
      }
      __if_exists(decltype(alloc.Flags)::Primary) {
        alloc.Flags.Primary = want_primary ? 1u : 0u;
      }
    }
    __if_exists(D3DDDICB_ALLOCATE::ResourceFlags) {
      alloc.ResourceFlags.Value = 0;
      alloc.ResourceFlags.RenderTarget = is_rt ? 1u : 0u;
      alloc.ResourceFlags.ZBuffer = is_ds ? 1u : 0u;
    }

    const HRESULT hr = CallCbMaybeHandle(cb->pfnAllocateCb, dev->hrt_device, &alloc);
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
            AEROGPU_D3D10_11_LOG("D3D10.1 CreateResource: shared allocation missing/invalid private driver data");
          });
        } else {
          static std::once_flag log_once;
          std::call_once(log_once, [] {
            AEROGPU_D3D10_11_LOG("D3D10.1 CreateResource: shared allocation missing share_token in returned private data");
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
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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
      (void)CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
      return E_FAIL;
    }

    if (is_shared && !share_token_ok) {
      // If the KMD does not return a stable token, shared surface interop cannot
      // work across processes; fail cleanly. Free the allocation handles that
      // were created by AllocateCb before returning an error.
      D3DDDICB_DEALLOCATE dealloc = {};
      D3DKMT_HANDLE h = static_cast<D3DKMT_HANDLE>(km_alloc);
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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
      (void)CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
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
    __if_exists(AllocationInfoT::hAllocation) {
      runtime_alloc = static_cast<uint32_t>(alloc_info[0].hAllocation);
    }
    res->wddm_allocation_handle = runtime_alloc ? runtime_alloc : static_cast<uint32_t>(km_alloc);
    return S_OK;
  };

  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pDesc->ByteWidth;
    const uint64_t padded_size_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    const uint64_t alloc_size = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 256);

    bool cpu_visible = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::CPUAccessFlags) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    __if_exists(D3D10DDIARG_CREATERESOURCE::CpuAccessFlags) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    bool is_staging = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      #ifdef D3D10_USAGE_STAGING
      is_staging = (usage == static_cast<uint32_t>(D3D10_USAGE_STAGING));
      #else
      is_staging = (usage == 3u);
      #endif
      cpu_visible = cpu_visible || is_staging;
    }

    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
  #ifdef D3D10_DDI_RESOURCE_MISC_SHARED
    is_shared = (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED) != 0;
  #else
    is_shared = (res->misc_flags & D3D10_RESOURCE_MISC_SHARED) != 0;
  #endif
#ifdef D3D10_DDI_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
#ifdef D3D10_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D10_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
    #ifdef D3D10_USAGE_DYNAMIC
      want_host_owned = (usage == static_cast<uint32_t>(D3D10_USAGE_DYNAMIC));
    #else
      want_host_owned = (usage == 2u);
    #endif
    }
    want_host_owned = want_host_owned && !is_shared;

    HRESULT alloc_hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0);
    if (FAILED(alloc_hr)) {
      set_error(dev, alloc_hr);
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(alloc_hr);
    }

    if (want_host_owned) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u alloc_id=%u size=%llu",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->backing_alloc_id),
                         static_cast<unsigned long long>(res->size_bytes));
#endif

    auto copy_initial_data = [&](auto init_data) -> HRESULT {
      if (!init_data) {
        return S_OK;
      }
      const auto& init = init_data[0];
      if (!init.pSysMem) {
        return E_INVALIDARG;
      }
      if (padded_size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(padded_size_bytes));
      } catch (...) {
        return E_OUTOFMEMORY;
      }
      if (res->size_bytes) {
        std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
      }
      return S_OK;
    };

    HRESULT init_hr = S_OK;
    __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      init_hr = copy_initial_data(pDesc->pInitialDataUP);
    }
    __if_not_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialData) {
        init_hr = copy_initial_data(pDesc->pInitialData);
      }
    }
    if (FAILED(init_hr)) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(init_hr);
    }

    TrackWddmAllocForSubmitLocked(dev, res);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = padded_size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      emit_upload_resource_locked(dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        set_error(dev, E_FAIL);
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(E_FAIL);
      }

      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;

      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        set_error(dev, submit_hr);
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(submit_hr);
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_TEXTURE2D) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    if (!pDesc->pMipInfoList) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->pMipInfoList[0].TexelWidth;
    res->height = pDesc->pMipInfoList[0].TexelHeight;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);
    if (res->mip_levels == 0 || res->array_size == 0) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (row_bytes == 0) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
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
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }

    bool cpu_visible = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::CPUAccessFlags) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    __if_exists(D3D10DDIARG_CREATERESOURCE::CpuAccessFlags) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    bool is_staging = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      #ifdef D3D10_USAGE_STAGING
      is_staging = (usage == static_cast<uint32_t>(D3D10_USAGE_STAGING));
      #else
      is_staging = (usage == 3u);
      #endif
      cpu_visible = cpu_visible || is_staging;
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
  #ifdef D3D10_DDI_RESOURCE_MISC_SHARED
    is_shared = (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED) != 0;
  #else
    is_shared = (res->misc_flags & D3D10_RESOURCE_MISC_SHARED) != 0;
  #endif
#ifdef D3D10_DDI_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
#ifdef D3D10_RESOURCE_MISC_SHARED_KEYEDMUTEX
    if (res->misc_flags & D3D10_RESOURCE_MISC_SHARED_KEYEDMUTEX) {
      is_shared = true;
    }
#endif
    if (is_shared && (res->mip_levels != 1 || res->array_size != 1)) {
      // Keep shared surface interop conservative: only support the legacy single-subresource layout.
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
    #ifdef D3D10_USAGE_DYNAMIC
      want_host_owned = (usage == static_cast<uint32_t>(D3D10_USAGE_DYNAMIC));
    #else
      want_host_owned = (usage == 2u);
    #endif
    }
    want_host_owned = want_host_owned && !is_shared;
    if (want_host_owned && (res->mip_levels != 1 || res->array_size != 1)) {
      // Host-owned Texture2D updates go through UPLOAD_RESOURCE, which cannot address
      // non-(mip0,layer0) subresources in the current stable command stream.
      want_host_owned = false;
    }

    HRESULT alloc_hr = allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared, is_primary, res->row_pitch_bytes);
    if (FAILED(alloc_hr)) {
      set_error(dev, alloc_hr);
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(alloc_hr);
    }

    if (want_host_owned) {
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

    auto copy_initial_data = [&](auto init_data) -> HRESULT {
      if (!init_data) {
        return S_OK;
      }
      if (total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return E_OUTOFMEMORY;
      }

      try {
        res->storage.resize(static_cast<size_t>(total_bytes));
      } catch (...) {
        return E_OUTOFMEMORY;
      }

      std::fill(res->storage.begin(), res->storage.end(), 0);

      const uint64_t subresource_count =
          static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
      if (subresource_count == 0) {
        return E_INVALIDARG;
      }
      if (subresource_count > static_cast<uint64_t>(res->tex2d_subresources.size())) {
        return E_FAIL;
      }

      for (uint32_t sub = 0; sub < static_cast<uint32_t>(subresource_count); ++sub) {
        const auto& init = init_data[sub];
        if (!init.pSysMem) {
          return E_INVALIDARG;
        }
        const Texture2DSubresourceLayout& dst_layout = res->tex2d_subresources[sub];

        const uint32_t src_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst_layout.width);
        const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, dst_layout.height);
        if (src_row_bytes == 0 || rows == 0) {
          return E_INVALIDARG;
        }
        if (dst_layout.row_pitch_bytes < src_row_bytes) {
          return E_INVALIDARG;
        }

        const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
        const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch)
                                                  : static_cast<size_t>(src_row_bytes);
        if (src_pitch < src_row_bytes) {
          return E_INVALIDARG;
        }

        if (dst_layout.offset_bytes > res->storage.size()) {
          return E_INVALIDARG;
        }
        const size_t dst_base = static_cast<size_t>(dst_layout.offset_bytes);
        for (uint32_t y = 0; y < rows; ++y) {
          const size_t dst_off = dst_base + static_cast<size_t>(y) * dst_layout.row_pitch_bytes;
          const size_t src_off = static_cast<size_t>(y) * src_pitch;
          if (dst_off + src_row_bytes > res->storage.size()) {
            return E_INVALIDARG;
          }
          std::memcpy(res->storage.data() + dst_off, src + src_off, src_row_bytes);
          if (dst_layout.row_pitch_bytes > src_row_bytes) {
            std::memset(res->storage.data() + dst_off + src_row_bytes,
                        0,
                        dst_layout.row_pitch_bytes - src_row_bytes);
          }
        }
      }
      return S_OK;
    };

    HRESULT init_hr = S_OK;
    __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      init_hr = copy_initial_data(pDesc->pInitialDataUP);
    }
    __if_not_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
      __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialData) {
        init_hr = copy_initial_data(pDesc->pInitialData);
      }
    }
    if (FAILED(init_hr)) {
      deallocate_if_needed();
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(init_hr);
    }

    TrackWddmAllocForSubmitLocked(dev, res);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = res->array_size;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->backing_offset_bytes;
    cmd->reserved0 = 0;
    if (!res->storage.empty()) {
      emit_upload_resource_locked(dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        set_error(dev, E_FAIL);
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(E_FAIL);
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;

      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        set_error(dev, submit_hr);
        deallocate_if_needed();
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(submit_hr);
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  deallocate_if_needed();
  res->~AeroGpuResource();
  AEROGPU_D3D10_RET_HR(E_NOTIMPL);
}

HRESULT AEROGPU_APIENTRY OpenResource(D3D10DDI_HDEVICE hDevice,
                                      const D3D10DDIARG_OPENRESOURCE* pOpenResource,
                                      D3D10DDI_HRESOURCE hResource,
                                      D3D10DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !pOpenResource || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  const void* priv_data = nullptr;
  uint32_t priv_size = 0;
  uint32_t num_allocations = 1;
  __if_exists(D3D10DDIARG_OPENRESOURCE::NumAllocations) {
    if (pOpenResource->NumAllocations < 1) {
      return E_INVALIDARG;
    }
    num_allocations = static_cast<uint32_t>(pOpenResource->NumAllocations);
  }

  // OpenResource DDI structs vary across WDK header vintages. Some headers
  // expose the preserved private driver data at the per-allocation level; prefer
  // that when present and fall back to the top-level fields.
  __if_exists(D3D10DDIARG_OPENRESOURCE::pOpenAllocationInfo) {
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
  __if_exists(D3D10DDIARG_OPENRESOURCE::pPrivateDriverData) {
    if (!priv_data) {
      priv_data = pOpenResource->pPrivateDriverData;
    }
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::PrivateDriverDataSize) {
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

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = AllocateGlobalHandle(dev->adapter);
  res->backing_alloc_id = static_cast<uint32_t>(priv.alloc_id);
  res->backing_offset_bytes = 0;
  res->wddm_allocation_handle = 0;
  res->share_token = static_cast<uint64_t>(priv.share_token);
  res->is_shared = true;
  res->is_shared_alias = true;

  __if_exists(D3D10DDIARG_OPENRESOURCE::BindFlags) {
    res->bind_flags = pOpenResource->BindFlags;
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::MiscFlags) {
    res->misc_flags = pOpenResource->MiscFlags;
  }

  __if_exists(D3D10DDIARG_OPENRESOURCE::hKMResource) {
    res->wddm.km_resource_handle = static_cast<uint64_t>(pOpenResource->hKMResource);
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::hKMAllocation) {
    res->wddm.km_allocation_handles.push_back(static_cast<uint64_t>(pOpenResource->hKMAllocation));
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::hAllocation) {
    const uint64_t h = static_cast<uint64_t>(pOpenResource->hAllocation);
    if (h != 0) {
      res->wddm_allocation_handle = static_cast<uint32_t>(h);
      if (res->wddm.km_allocation_handles.empty()) {
        res->wddm.km_allocation_handles.push_back(h);
      }
    }
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::phAllocations) {
    __if_exists(D3D10DDIARG_OPENRESOURCE::NumAllocations) {
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
  __if_exists(D3D10DDIARG_OPENRESOURCE::pOpenAllocationInfo) {
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
      res->~AeroGpuResource();
      return E_INVALIDARG;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      res->~AeroGpuResource();
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
        res->~AeroGpuResource();
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
      res->~AeroGpuResource();
      return E_INVALIDARG;
    }
    if (total_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      res->storage.resize(static_cast<size_t>(total_bytes), 0);
    }
  } else {
    res->~AeroGpuResource();
    return E_INVALIDARG;
  }

  auto* import_cmd =
      dev->cmd.append_fixed<aerogpu_cmd_import_shared_surface>(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!import_cmd) {
    res->~AeroGpuResource();
    return E_OUTOFMEMORY;
  }
  import_cmd->out_resource_handle = res->handle;
  import_cmd->reserved0 = 0;
  import_cmd->share_token = res->share_token;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_TRACEF("DestroyResource hDevice=%p hResource=%p", hDevice.pDrvPrivate, hResource.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!dev->pending_staging_writes.empty()) {
    dev->pending_staging_writes.erase(
        std::remove(dev->pending_staging_writes.begin(), dev->pending_staging_writes.end(), res),
        dev->pending_staging_writes.end());
  }
  if (res->mapped) {
    unmap_resource_locked(dev, res, res->mapped_subresource);
  }
  bool rt_state_changed = false;
  if (dev->current_rtv_res == res) {
    dev->current_rtv_res = nullptr;
    dev->current_rtv = 0;
    rt_state_changed = true;
  }
  if (dev->current_dsv_res == res) {
    dev->current_dsv_res = nullptr;
    dev->current_dsv = 0;
    rt_state_changed = true;
  }
  if (rt_state_changed) {
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
  if (dev->current_vb_res == res) {
    dev->current_vb_res = nullptr;
    dev->current_vb_stride = 0;
    dev->current_vb_offset = 0;
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS, nullptr, 0);
    cmd->start_slot = 0;
    cmd->buffer_count = 0;
  }
  if (dev->current_ib_res == res) {
    dev->current_ib_res = nullptr;
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
    cmd->buffer = 0;
    cmd->format = AEROGPU_INDEX_FORMAT_UINT16;
    cmd->offset_bytes = 0;
    cmd->reserved0 = 0;
  }

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (dev->current_vs_srvs[slot] == res) {
      dev->current_vs_srvs[slot] = nullptr;
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }
  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (dev->current_ps_srvs[slot] == res) {
      dev->current_ps_srvs[slot] = nullptr;
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }

  if (res->handle != kInvalidHandle) {
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
      set_error(dev, submit_hr);
    }
  }

  if (res->wddm.km_resource_handle != 0 || !res->wddm.km_allocation_handles.empty()) {
    const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
    if (!cb || !cb->pfnDeallocateCb) {
      set_error(dev, E_FAIL);
    } else {
      std::vector<D3DKMT_HANDLE> km_allocs;
      km_allocs.reserve(res->wddm.km_allocation_handles.size());
      for (uint64_t h : res->wddm.km_allocation_handles) {
        km_allocs.push_back(static_cast<D3DKMT_HANDLE>(h));
      }

      D3DDDICB_DEALLOCATE dealloc = {};
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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

      const auto call_dealloc = [&]() -> HRESULT {
        if constexpr (std::is_same_v<decltype(CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc)),
                                     HRESULT>) {
          return CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
        } else {
          CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
          return S_OK;
        }
      };

      const HRESULT dealloc_hr = call_dealloc();
      if (FAILED(dealloc_hr)) {
        set_error(dev, dealloc_hr);
      }
    }

    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
    res->wddm_allocation_handle = 0;
  }
  res->~AeroGpuResource();
}

// -------------------------------------------------------------------------------------------------
// Map/unmap (Win7 D3D11 runtimes may use specialized entrypoints).
// -------------------------------------------------------------------------------------------------

constexpr uint32_t kD3DMapRead = 1;
constexpr uint32_t kD3DMapWrite = 2;
constexpr uint32_t kD3DMapReadWrite = 3;
constexpr uint32_t kD3DMapWriteDiscard = 4;
constexpr uint32_t kD3DMapWriteNoOverwrite = 5;
// D3D10_MAP_FLAG_DO_NOT_WAIT (numeric value from d3d10.h / d3d10_1.h).
constexpr uint32_t kD3DMapFlagDoNotWait = 0x100000;

static void InitLockArgsForMap(D3DDDICB_LOCK* lock, uint32_t subresource, uint32_t map_type, uint32_t map_flags) {
  if (!lock) {
    return;
  }
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock->SubresourceIndex = subresource;
  }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
    lock->SubResourceIndex = subresource;
  }
  __if_exists(D3DDDICB_LOCK::Offset) {
    lock->Offset = 0;
  }
  __if_exists(D3DDDICB_LOCK::Size) {
    lock->Size = 0;
  }

  const bool do_not_wait = (map_flags & kD3DMapFlagDoNotWait) != 0;
  const bool is_read_only = (map_type == kD3DMapRead);
  const bool is_write_only = (map_type == kD3DMapWrite || map_type == kD3DMapWriteDiscard || map_type == kD3DMapWriteNoOverwrite);
  const bool discard = (map_type == kD3DMapWriteDiscard);
  const bool no_overwrite = (map_type == kD3DMapWriteNoOverwrite);

  __if_exists(D3DDDICB_LOCK::Flags) {
    std::memset(&lock->Flags, 0, sizeof(lock->Flags));

    __if_exists(D3DDDICB_LOCKFLAGS::ReadOnly) {
      lock->Flags.ReadOnly = is_read_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) {
      lock->Flags.WriteOnly = is_write_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Write) {
      // For READ_WRITE the Win7 contract treats the lock as read+write (no explicit "write" bit).
      lock->Flags.Write = is_write_only ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Discard) {
      lock->Flags.Discard = discard ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverwrite) {
      lock->Flags.NoOverwrite = no_overwrite ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverWrite) {
      lock->Flags.NoOverWrite = no_overwrite ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::DoNotWait) {
      lock->Flags.DoNotWait = do_not_wait ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::DonotWait) {
      lock->Flags.DonotWait = do_not_wait ? 1u : 0u;
    }
  }
}

static void InitUnlockArgsForMap(D3DDDICB_UNLOCK* unlock, uint32_t subresource) {
  if (!unlock) {
    return;
  }
  __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
    unlock->SubresourceIndex = subresource;
  }
  __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
    unlock->SubResourceIndex = subresource;
  }
}

HRESULT sync_read_map_locked(AeroGpuDevice* dev, const AeroGpuResource* res, uint32_t map_type, uint32_t map_flags) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  const bool want_read = (map_type == kD3DMapRead || map_type == kD3DMapReadWrite);
  if (!want_read) {
    return S_OK;
  }

  // Only apply implicit readback synchronization for staging-style resources.
  if (res->bind_flags != 0) {
    return S_OK;
  }

  // Ensure any pending command stream is submitted so we have a fence to observe.
  if (!dev->cmd.empty()) {
    HRESULT submit_hr = S_OK;
    submit_locked(dev, /*want_present=*/false, &submit_hr);
    if (FAILED(submit_hr)) {
      return submit_hr;
    }
  }

  const uint64_t fence = res->last_gpu_write_fence;
  if (fence == 0) {
    return S_OK;
  }

  const bool do_not_wait = (map_flags & kD3DMapFlagDoNotWait) != 0;
  const uint32_t timeout_ms = do_not_wait ? 0u : kAeroGpuTimeoutMsInfinite;
  return AeroGpuWaitForFence(dev, fence, timeout_ms);
}

static uint64_t resource_total_bytes(const AeroGpuDevice* dev, const AeroGpuResource* res) {
  if (!res) {
    return 0;
  }
  switch (res->kind) {
    case ResourceKind::Buffer:
      return res->size_bytes;
    case ResourceKind::Texture2D: {
      if (!res->tex2d_subresources.empty()) {
        const Texture2DSubresourceLayout& last = res->tex2d_subresources.back();
        const uint64_t end = last.offset_bytes + last.size_bytes;
        if (end < last.offset_bytes) {
          return 0;
        }
        return end;
      }

      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      if (aer_fmt == AEROGPU_FORMAT_INVALID) {
        return 0;
      }
      return aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
    }
    default:
      return 0;
  }
}

HRESULT ensure_resource_storage(AeroGpuResource* res, uint64_t bytes) {
  if (!res) {
    return E_INVALIDARG;
  }
  uint64_t want = bytes;
  if (res->kind == ResourceKind::Buffer) {
    want = AlignUpU64(bytes ? bytes : 1, 4);
  }
  if (want > static_cast<uint64_t>(std::numeric_limits<size_t>::max())) {
    return E_OUTOFMEMORY;
  }
  if (res->storage.size() >= static_cast<size_t>(want)) {
    return S_OK;
  }
  try {
    res->storage.resize(static_cast<size_t>(want), 0);
  } catch (...) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT map_resource_locked(AeroGpuDevice* dev,
                            AeroGpuResource* res,
                            uint32_t subresource,
                            uint32_t map_type,
                            uint32_t map_flags,
                            D3D10DDI_MAPPED_SUBRESOURCE* pMapped) {
  if (!dev || !res || !pMapped) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }

  bool want_write = false;
  switch (map_type) {
    case kD3DMapRead:
      break;
    case kD3DMapWrite:
    case kD3DMapReadWrite:
    case kD3DMapWriteDiscard:
    case kD3DMapWriteNoOverwrite:
      want_write = true;
      break;
    default:
      return E_INVALIDARG;
  }
  const bool want_read = (map_type == kD3DMapRead || map_type == kD3DMapReadWrite);

  const uint64_t total = resource_total_bytes(dev, res);
  if (!total) {
    return E_INVALIDARG;
  }

  uint64_t map_offset = 0;
  uint64_t map_size = total;
  uint32_t map_row_pitch = 0;
  if (res->kind == ResourceKind::Buffer) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
  } else if (res->kind == ResourceKind::Texture2D) {
    const uint64_t subresource_count =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    if (subresource_count == 0 || subresource >= subresource_count) {
      return E_INVALIDARG;
    }
    if (subresource >= res->tex2d_subresources.size()) {
      return E_FAIL;
    }
    const Texture2DSubresourceLayout& sub_layout = res->tex2d_subresources[subresource];
    map_offset = sub_layout.offset_bytes;
    map_size = sub_layout.size_bytes;
    map_row_pitch = sub_layout.row_pitch_bytes;
    const uint64_t end = map_offset + map_size;
    if (end < map_offset || end > total) {
      return E_INVALIDARG;
    }
    if (map_size == 0) {
      return E_INVALIDARG;
    }
  } else {
    return E_INVALIDARG;
  }

  HRESULT hr = ensure_resource_storage(res, total);
  if (FAILED(hr)) {
    return hr;
  }

  if (map_type == kD3DMapWriteDiscard) {
    // Discard contents are undefined; clear for deterministic tests.
    if (res->kind == ResourceKind::Buffer) {
      try {
        res->storage.assign(static_cast<size_t>(total), 0);
      } catch (...) {
        return E_OUTOFMEMORY;
      }
    } else if (res->kind == ResourceKind::Texture2D) {
      if (map_offset < res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(map_offset);
        const size_t clear_bytes = static_cast<size_t>(std::min<uint64_t>(map_size, remaining));
        std::fill(res->storage.begin() + static_cast<size_t>(map_offset),
                  res->storage.begin() + static_cast<size_t>(map_offset) + clear_bytes,
                  0);
      }
    }
  }

  const bool allow_storage_map = (res->backing_alloc_id == 0) && !(want_read && res->bind_flags == 0);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    if (res->storage.empty()) {
      pMapped->pData = nullptr;
    } else {
      pMapped->pData = res->storage.data() + static_cast<size_t>(map_offset);
    }
    if (res->kind == ResourceKind::Texture2D) {
      pMapped->RowPitch = map_row_pitch;
      pMapped->DepthPitch = static_cast<UINT>(map_size);
    } else {
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
    }

    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_offset = map_offset;
    res->mapped_size = map_size;
    return S_OK;
  };

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  if (!cb || !cb->pfnLockCb || !cb->pfnUnlockCb || res->wddm_allocation_handle == 0) {
    if (allow_storage_map) {
      return map_storage();
    }
    return E_FAIL;
  }

  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_allocation = 0;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;

  const uint32_t alloc_handle = res->wddm_allocation_handle;
  D3DDDICB_LOCK lock_cb = {};
  lock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
  const uint32_t lock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : subresource;
  InitLockArgsForMap(&lock_cb, lock_subresource, map_type, map_flags);

  const bool do_not_wait = (map_flags & kD3DMapFlagDoNotWait) != 0;
  hr = CallCbMaybeHandle(cb->pfnLockCb, dev->hrt_device, &lock_cb);
  if (hr == kDxgiErrorWasStillDrawing || hr == kHrNtStatusGraphicsGpuBusy ||
      (do_not_wait && (hr == kHrPending || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) ||
                       hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT) || hr == static_cast<HRESULT>(0x10000102L)))) {
    hr = kDxgiErrorWasStillDrawing;
  }
  if (hr == kDxgiErrorWasStillDrawing) {
    if (allow_storage_map && !want_read) {
      return map_storage();
    }
    return kDxgiErrorWasStillDrawing;
  }
  if (FAILED(hr)) {
    if (allow_storage_map) {
      return map_storage();
    }
    return hr;
  }
  if (!lock_cb.pData) {
    D3DDDICB_UNLOCK unlock_cb = {};
    unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
    InitUnlockArgsForMap(&unlock_cb, lock_subresource);
    (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
    if (allow_storage_map) {
      return map_storage();
    }
    return E_FAIL;
  }

  res->mapped_wddm_ptr = lock_cb.pData;
  res->mapped_wddm_allocation = static_cast<uint64_t>(alloc_handle);
  __if_exists(D3DDDICB_LOCK::Pitch) {
    res->mapped_wddm_pitch = lock_cb.Pitch;
  }
  __if_exists(D3DDDICB_LOCK::SlicePitch) {
    res->mapped_wddm_slice_pitch = lock_cb.SlicePitch;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  if (!res->storage.empty()) {
    if (map_type == kD3DMapWriteDiscard) {
      if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memset(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    0,
                    static_cast<size_t>(map_size));
      }
    } else if (!is_guest_backed) {
      if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memcpy(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    res->storage.data() + static_cast<size_t>(map_offset),
                    static_cast<size_t>(map_size));
      }
    } else if (want_read) {
      if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memcpy(res->storage.data() + static_cast<size_t>(map_offset),
                    static_cast<const uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    static_cast<size_t>(map_size));
      }
    }
  }

  if (res->kind == ResourceKind::Texture2D) {
    pMapped->pData = static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset);
    pMapped->RowPitch = map_row_pitch;
    pMapped->DepthPitch = static_cast<UINT>(map_size);
  } else {
    pMapped->pData = lock_cb.pData;
    pMapped->RowPitch = 0;
    pMapped->DepthPitch = 0;
  }

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_offset = map_offset;
  res->mapped_size = map_size;
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

  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (res->mapped_write && !res->storage.empty() && res->mapped_size) {
      const uint8_t* src = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
      const uint64_t off = res->mapped_offset;
      const uint64_t size = res->mapped_size;
      if (off <= res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(off);
        const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
        if (copy_bytes) {
          std::memcpy(res->storage.data() + static_cast<size_t>(off),
                      src + static_cast<size_t>(off),
                      copy_bytes);
        }
      }
    }

    const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
    if (cb && cb->pfnUnlockCb) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation =
          UintPtrToD3dHandle<decltype(unlock_cb.hAllocation)>(static_cast<std::uintptr_t>(res->mapped_wddm_allocation));
      const uint32_t unlock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : subresource;
      InitUnlockArgsForMap(&unlock_cb, unlock_subresource);
      const HRESULT unlock_hr = CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (FAILED(unlock_hr)) {
        set_error(dev, unlock_hr);
      }
    }
  }

  if (res->mapped_write && res->mapped_size != 0) {
    uint64_t upload_offset = res->mapped_offset;
    uint64_t upload_size = res->mapped_size;
    if (res->kind == ResourceKind::Buffer) {
      const uint64_t end = res->mapped_offset + res->mapped_size;
      if (end < res->mapped_offset) {
        return;
      }
      upload_offset = res->mapped_offset & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      upload_size = upload_end - upload_offset;
    }

    if (res->backing_alloc_id != 0) {
      emit_dirty_range_locked(dev, res, upload_offset, upload_size);
    } else {
      emit_upload_resource_locked(dev, res, upload_offset, upload_size);
    }
  }

  res->mapped = false;
  res->mapped_write = false;
  res->mapped_subresource = 0;
  res->mapped_offset = 0;
  res->mapped_size = 0;
  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_allocation = 0;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;
}

HRESULT map_dynamic_buffer_locked(AeroGpuDevice* dev, AeroGpuResource* res, bool discard, void** ppData) {
  if (!dev || !res || !ppData) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }

  const uint64_t total = res->size_bytes;
  const uint64_t storage_bytes = AlignUpU64(total ? total : 1, 4);
  HRESULT hr = ensure_resource_storage(res, storage_bytes);
  if (FAILED(hr)) {
    return hr;
  }

  if (discard) {
    // Approximate DISCARD renaming by allocating a fresh CPU backing store.
    try {
      res->storage.assign(static_cast<size_t>(storage_bytes), 0);
    } catch (...) {
      return E_OUTOFMEMORY;
    }
  }

  const bool allow_storage_map = (res->backing_alloc_id == 0);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    res->mapped = true;
    res->mapped_write = true;
    res->mapped_subresource = 0;
    res->mapped_offset = 0;
    res->mapped_size = total;
    *ppData = res->storage.empty() ? nullptr : res->storage.data();
    return S_OK;
  };

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
  if (!cb || !cb->pfnLockCb || !cb->pfnUnlockCb || res->wddm_allocation_handle == 0) {
    if (allow_storage_map) {
      return map_storage();
    }
    return E_FAIL;
  }

  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_allocation = 0;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;

  const uint32_t alloc_handle = res->wddm_allocation_handle;
  D3DDDICB_LOCK lock_cb = {};
  lock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) {
    lock_cb.SubresourceIndex = 0;
  }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) {
    lock_cb.SubResourceIndex = 0;
  }
  __if_exists(D3DDDICB_LOCK::Flags) {
    std::memset(&lock_cb.Flags, 0, sizeof(lock_cb.Flags));
    __if_exists(D3DDDICB_LOCKFLAGS::WriteOnly) {
      lock_cb.Flags.WriteOnly = 1u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Write) {
      lock_cb.Flags.Write = 1u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::Discard) {
      lock_cb.Flags.Discard = discard ? 1u : 0u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverwrite) {
      lock_cb.Flags.NoOverwrite = discard ? 0u : 1u;
    }
    __if_exists(D3DDDICB_LOCKFLAGS::NoOverWrite) {
      lock_cb.Flags.NoOverWrite = discard ? 0u : 1u;
    }
  }

  hr = CallCbMaybeHandle(cb->pfnLockCb, dev->hrt_device, &lock_cb);
  if (hr == kDxgiErrorWasStillDrawing) {
    if (allow_storage_map) {
      return map_storage();
    }
    return hr;
  }
  if (FAILED(hr)) {
    if (allow_storage_map) {
      return map_storage();
    }
    return hr;
  }
  if (!lock_cb.pData) {
    D3DDDICB_UNLOCK unlock_cb = {};
    unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
    InitUnlockArgsForMap(&unlock_cb, /*subresource=*/0);
    (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
    if (allow_storage_map) {
      return map_storage();
    }
    return E_FAIL;
  }

  res->mapped_wddm_ptr = lock_cb.pData;
  res->mapped_wddm_allocation = static_cast<uint64_t>(alloc_handle);
  __if_exists(D3DDDICB_LOCK::Pitch) {
    res->mapped_wddm_pitch = lock_cb.Pitch;
  }
  __if_exists(D3DDDICB_LOCK::SlicePitch) {
    res->mapped_wddm_slice_pitch = lock_cb.SlicePitch;
  }

  if (!res->storage.empty()) {
    if (discard) {
      std::memset(lock_cb.pData, 0, res->storage.size());
    } else {
      std::memcpy(lock_cb.pData, res->storage.data(), res->storage.size());
    }
  }

  res->mapped = true;
  res->mapped_write = true;
  res->mapped_subresource = 0;
  res->mapped_offset = 0;
  res->mapped_size = total;
  *ppData = lock_cb.pData;
  return S_OK;
}

template <typename = void>
HRESULT AEROGPU_APIENTRY StagingResourceMap(D3D10DDI_HDEVICE hDevice,
                                            D3D10DDI_HRESOURCE hResource,
                                            UINT subresource,
                                            D3D10_DDI_MAP map_type,
                                            UINT map_flags,
                                            D3D10DDI_MAPPED_SUBRESOURCE* pMapped) {
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
  const uint32_t map_type_u = static_cast<uint32_t>(map_type);
  HRESULT sync_hr = sync_read_map_locked(dev, res, map_type_u, map_flags);
  if (FAILED(sync_hr)) {
    return sync_hr;
  }
  return map_resource_locked(dev, res, subresource, map_type_u, map_flags, pMapped);
}

template <typename = void>
void AEROGPU_APIENTRY StagingResourceUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, UINT subresource) {
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

template <typename = void>
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
  if ((res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(dev, res, /*discard=*/true, ppData);
}

template <typename = void>
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
  if ((res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(dev, res, /*discard=*/false, ppData);
}

template <typename = void>
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

template <typename = void>
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
  if ((res->bind_flags & kD3D10BindConstantBuffer) == 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return map_dynamic_buffer_locked(dev, res, /*discard=*/true, ppData);
}

template <typename = void>
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
                             UINT subresource,
                             D3D10_DDI_MAP map_type,
                             UINT map_flags,
                             D3D10DDI_MAPPED_SUBRESOURCE* pMapped) {
  AEROGPU_D3D10_11_LOG("pfnMap subresource=%u map_type=%u map_flags=0x%X",
                       static_cast<unsigned>(subresource),
                       static_cast<unsigned>(map_type),
                       static_cast<unsigned>(map_flags));
  AEROGPU_D3D10_TRACEF_VERBOSE("Map hDevice=%p hResource=%p sub=%u type=%u flags=0x%X",
                               hDevice.pDrvPrivate,
                               hResource.pDrvPrivate,
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

  const uint32_t map_type_u = static_cast<uint32_t>(map_type);
  if (map_type_u == kD3DMapWriteDiscard) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(dev, res, /*discard=*/true, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
    if (res->bind_flags & kD3D10BindConstantBuffer) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(dev, res, /*discard=*/true, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
  } else if (map_type_u == kD3DMapWriteNoOverwrite) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) {
      void* data = nullptr;
      HRESULT hr = map_dynamic_buffer_locked(dev, res, /*discard=*/false, &data);
      if (FAILED(hr)) {
        return hr;
      }
      pMapped->pData = data;
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
      return S_OK;
    }
  }

  // Conservative: only support generic map on buffers and staging textures for now.
  HRESULT sync_hr = sync_read_map_locked(dev, res, map_type_u, map_flags);
  if (FAILED(sync_hr)) {
    return sync_hr;
  }
  if (res->kind == ResourceKind::Texture2D && res->bind_flags == 0) {
    return map_resource_locked(dev, res, subresource, map_type_u, map_flags, pMapped);
  }
  if (res->kind == ResourceKind::Buffer) {
    return map_resource_locked(dev, res, subresource, map_type_u, map_flags, pMapped);
  }
  return E_NOTIMPL;
}

SIZE_T AEROGPU_APIENTRY CalcPrivateVertexShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEVERTEXSHADER*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateVertexShaderSize");
  return sizeof(AeroGpuShader);
}

SIZE_T AEROGPU_APIENTRY CalcPrivatePixelShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEPIXELSHADER*) {
  AEROGPU_D3D10_TRACEF("CalcPrivatePixelShaderSize");
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
  sh->handle = AllocateGlobalHandle(dev->adapter);
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

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const D3D10DDIARG_CREATEVERTEXSHADER* pDesc,
                                            D3D10DDI_HVERTEXSHADER hShader,
                                            D3D10DDI_HRTVERTEXSHADER) {
  AEROGPU_D3D10_TRACEF("CreateVertexShader codeSize=%u", pDesc ? static_cast<unsigned>(pDesc->CodeSize) : 0u);
  if (!pDesc) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->CodeSize, hShader, AEROGPU_SHADER_STAGE_VERTEX);
  AEROGPU_D3D10_RET_HR(hr);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const D3D10DDIARG_CREATEPIXELSHADER* pDesc,
                                           D3D10DDI_HPIXELSHADER hShader,
                                           D3D10DDI_HRTPIXELSHADER) {
  AEROGPU_D3D10_TRACEF("CreatePixelShader codeSize=%u", pDesc ? static_cast<unsigned>(pDesc->CodeSize) : 0u);
  if (!pDesc) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  const HRESULT hr = CreateShaderCommon(hDevice, pDesc->pShaderCode, pDesc->CodeSize, hShader, AEROGPU_SHADER_STAGE_PIXEL);
  AEROGPU_D3D10_RET_HR(hr);
}

template <typename TShaderHandle>
void DestroyShaderCommon(D3D10DDI_HDEVICE hDevice, TShaderHandle hShader) {
  AEROGPU_D3D10_TRACEF("DestroyShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
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
  AEROGPU_D3D10_TRACEF("CalcPrivateElementLayoutSize");
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateElementLayout(D3D10DDI_HDEVICE hDevice,
                                             const D3D10DDIARG_CREATEELEMENTLAYOUT* pDesc,
                                             D3D10DDI_HELEMENTLAYOUT hLayout,
                                             D3D10DDI_HRTELEMENTLAYOUT) {
  AEROGPU_D3D10_TRACEF("CreateElementLayout elements=%u", pDesc ? static_cast<unsigned>(pDesc->NumElements) : 0u);
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = AllocateGlobalHandle(dev->adapter);

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
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyElementLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_TRACEF("DestroyElementLayout hDevice=%p hLayout=%p", hDevice.pDrvPrivate, hLayout.pDrvPrivate);
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
  AEROGPU_D3D10_TRACEF("CalcPrivateRenderTargetViewSize");
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                                const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                                D3D10DDI_HRENDERTARGETVIEW hRtv,
                                                D3D10DDI_HRTRENDERTARGETVIEW) {
  D3D10DDI_HRESOURCE hResource{};
  void* res_private = nullptr;
  if (pDesc) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
      hResource = pDesc->hDrvResource;
      res_private = pDesc->hDrvResource.pDrvPrivate;
    }
    __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
      hResource = pDesc->hResource;
      res_private = pDesc->hResource.pDrvPrivate;
    }
  }
  AEROGPU_D3D10_TRACEF("CreateRenderTargetView hDevice=%p hResource=%p",
                       hDevice.pDrvPrivate,
                       res_private);
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  rtv->resource = res;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyRenderTargetView(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_TRACEF("DestroyRenderTargetView hRtv=%p", hRtv.pDrvPrivate);
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILVIEW*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateDepthStencilViewSize");
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                                const D3D10DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                                D3D10DDI_HDEPTHSTENCILVIEW hDsv,
                                                D3D10DDI_HRTDEPTHSTENCILVIEW) {
  D3D10DDI_HRESOURCE hResource{};
  void* res_private = nullptr;
  if (pDesc) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
      hResource = pDesc->hDrvResource;
      res_private = pDesc->hDrvResource.pDrvPrivate;
    }
    __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
      hResource = pDesc->hResource;
      res_private = pDesc->hResource.pDrvPrivate;
    }
  }
  AEROGPU_D3D10_TRACEF("CreateDepthStencilView hDevice=%p hResource=%p",
                       hDevice.pDrvPrivate,
                       res_private);
  if (!hDevice.pDrvPrivate || !pDesc || !hDsv.pDrvPrivate || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  dsv->resource = res;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyDepthStencilView(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_TRACEF("DestroyDepthStencilView hDsv=%p", hDsv.pDrvPrivate);
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
  AEROGPU_D3D10_TRACEF_VERBOSE("ClearDepthStencilView hDevice=%p flags=0x%x depth=%f stencil=%u",
                               hDevice.pDrvPrivate,
                               clear_flags,
                               depth,
                               static_cast<unsigned>(stencil));
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackBoundTargetsForSubmitLocked(dev);

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

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
                                                  const D3D10DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                  D3D10DDI_HSHADERRESOURCEVIEW hView,
                                                  D3D10DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  D3D10DDI_HRESOURCE hResource{};
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    hResource = pDesc->hDrvResource;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    hResource = pDesc->hResource;
  }
  if (!hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  auto* srv = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  srv->texture = res ? res->handle : 0;
  srv->resource = res;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView(D3D10DDI_HDEVICE, D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  view->~AeroGpuShaderResourceView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                                       const D3D10DDIARG_CREATESAMPLER*,
                                       D3D10DDI_HSAMPLER hSampler,
                                       D3D10DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hSampler.pDrvPrivate) AeroGpuSampler();
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler(D3D10DDI_HDEVICE, D3D10DDI_HSAMPLER hSampler) {
  if (!hSampler.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  s->~AeroGpuSampler();
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
                                            D3D10DDI_HRENDERTARGETVIEW hRtv,
                                            const FLOAT rgba[4]) {
  if (!hDevice.pDrvPrivate || !rgba) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("ClearRenderTargetView hDevice=%p rgba=[%f %f %f %f]",
                               hDevice.pDrvPrivate,
                               rgba[0],
                               rgba[1],
                               rgba[2],
                               rgba[3]);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackBoundTargetsForSubmitLocked(dev);

  auto* view = hRtv.pDrvPrivate ? FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv) : nullptr;
  auto* res = view ? view->resource : nullptr;

  if (res && res->kind == ResourceKind::Texture2D && res->width && res->height) {
    auto float_to_unorm8 = [](float v) -> uint8_t {
      if (v <= 0.0f) {
        return 0;
      }
      if (v >= 1.0f) {
        return 255;
      }
      const float scaled = v * 255.0f + 0.5f;
      if (scaled <= 0.0f) {
        return 0;
      }
      if (scaled >= 255.0f) {
        return 255;
      }
      return static_cast<uint8_t>(scaled);
    };

    const uint8_t r = float_to_unorm8(rgba[0]);
    const uint8_t g = float_to_unorm8(rgba[1]);
    const uint8_t b = float_to_unorm8(rgba[2]);
    const uint8_t a = float_to_unorm8(rgba[3]);

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    if (aer_fmt == AEROGPU_FORMAT_INVALID || bpp != 4) {
      // Only maintain CPU-side shadow clears for the uncompressed 32-bit RGBA/BGRA formats
      // used by the bring-up render-target path.
      goto EmitClearCmd;
    }

    if (res->row_pitch_bytes == 0) {
      res->row_pitch_bytes = res->width * bpp;
    }
    const uint64_t total_bytes = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
    if (total_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      if (res->storage.size() < static_cast<size_t>(total_bytes)) {
        try {
          res->storage.resize(static_cast<size_t>(total_bytes));
        } catch (...) {
          set_error(dev, E_OUTOFMEMORY);
          return;
        }
      }

      const uint32_t row_bytes = res->width * bpp;
      for (uint32_t y = 0; y < res->height; ++y) {
        uint8_t* row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        for (uint32_t x = 0; x < res->width; ++x) {
          uint8_t* px = row + static_cast<size_t>(x) * 4;
          switch (res->dxgi_format) {
            case kDxgiFormatR8G8B8A8Unorm:
            case kDxgiFormatR8G8B8A8UnormSrgb:
            case kDxgiFormatR8G8B8A8Typeless:
              px[0] = r;
              px[1] = g;
              px[2] = b;
              px[3] = a;
              break;
            case kDxgiFormatB8G8R8X8Unorm:
            case kDxgiFormatB8G8R8X8UnormSrgb:
            case kDxgiFormatB8G8R8X8Typeless:
              px[0] = b;
              px[1] = g;
              px[2] = r;
              px[3] = 255;
              break;
            case kDxgiFormatB8G8R8A8Unorm:
            case kDxgiFormatB8G8R8A8UnormSrgb:
            case kDxgiFormatB8G8R8A8Typeless:
            default:
              px[0] = b;
              px[1] = g;
              px[2] = r;
              px[3] = a;
              break;
          }
        }
        if (res->row_pitch_bytes > row_bytes) {
          std::memset(row + row_bytes, 0, res->row_pitch_bytes - row_bytes);
        }
      }
    }
  }

EmitClearCmd:
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
  AEROGPU_D3D10_TRACEF_VERBOSE("IaSetInputLayout hDevice=%p hLayout=%p", hDevice.pDrvPrivate, hLayout.pDrvPrivate);
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
  if (!hDevice.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  if (buffer_count == 0) {
    // We only model vertex buffer slot 0 in the minimal bring-up path. If the
    // runtime unbinds a different slot, ignore it rather than accidentally
    // clearing slot 0 state.
    if (start_slot != 0) {
      return;
    }
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->current_vb_res = nullptr;
    dev->current_vb_stride = 0;
    dev->current_vb_offset = 0;

    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
        AEROGPU_CMD_SET_VERTEX_BUFFERS, nullptr, 0);
    cmd->start_slot = 0;
    cmd->buffer_count = 0;
    return;
  }

  if (!pBuffers || !pStrides || !pOffsets) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  // Minimal: only slot 0 / count 1 is wired up.
  if (start_slot != 0 || buffer_count != 1) {
    set_error(dev, E_NOTIMPL);
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("IaSetVertexBuffers hDevice=%p buf=%p stride=%u offset=%u",
                               hDevice.pDrvPrivate,
                               pBuffers[0].pDrvPrivate,
                               pStrides[0],
                               pOffsets[0]);

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* vb_res = pBuffers[0].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pBuffers[0]) : nullptr;
  dev->current_vb_res = vb_res;
  dev->current_vb_stride = pStrides[0];
  dev->current_vb_offset = pOffsets[0];

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = vb_res ? vb_res->handle : 0;
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
  AEROGPU_D3D10_TRACEF_VERBOSE("IaSetIndexBuffer hDevice=%p hBuffer=%p fmt=%u offset=%u",
                               hDevice.pDrvPrivate,
                               hBuffer.pDrvPrivate,
                               static_cast<unsigned>(format),
                               offset);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* ib_res = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer) : nullptr;
  dev->current_ib_res = ib_res;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = ib_res ? ib_res->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format));
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY IaSetTopology(D3D10DDI_HDEVICE hDevice, D3D10_DDI_PRIMITIVE_TOPOLOGY topology) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("IaSetTopology hDevice=%p topology=%u", hDevice.pDrvPrivate, static_cast<unsigned>(topology));
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
  AEROGPU_D3D10_TRACEF_VERBOSE("VsSetShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
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
  AEROGPU_D3D10_TRACEF_VERBOSE("PsSetShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
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

void SetShaderResourcesCommon(D3D10DDI_HDEVICE hDevice,
                              uint32_t shader_stage,
                              UINT start_slot,
                              UINT num_views,
                              const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hDevice.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (UINT i = 0; i < num_views; i++) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);
    aerogpu_handle_t tex = 0;
    AeroGpuResource* res = nullptr;
    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i]);
      res = view ? view->resource : nullptr;
      tex = res ? res->handle : (view ? view->texture : 0);
    }
    if (slot < dev->current_vs_srvs.size() && shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
      dev->current_vs_srvs[slot] = res;
    } else if (slot < dev->current_ps_srvs.size() && shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
      dev->current_ps_srvs[slot] = res;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = shader_stage;
    cmd->slot = slot;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
}

void AEROGPU_APIENTRY ClearState(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (dev->current_vs_srvs[slot]) {
      dev->current_vs_srvs[slot] = nullptr;
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }
  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (dev->current_ps_srvs[slot]) {
      dev->current_ps_srvs[slot] = nullptr;
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
    }
  }

  dev->current_rtv = 0;
  dev->current_rtv_res = nullptr;
  dev->current_dsv = 0;
  dev->current_dsv_res = nullptr;
  dev->viewport_width = 0;
  dev->viewport_height = 0;
  auto* rt_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  rt_cmd->color_count = 0;
  rt_cmd->depth_stencil = 0;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    rt_cmd->colors[i] = 0;
  }

  dev->current_vs = 0;
  dev->current_ps = 0;
  auto* bind_cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  bind_cmd->vs = 0;
  bind_cmd->ps = 0;
  bind_cmd->cs = 0;
  bind_cmd->reserved0 = 0;

  dev->current_input_layout = 0;
  auto* il_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  il_cmd->input_layout_handle = 0;
  il_cmd->reserved0 = 0;

  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  auto* topo_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  topo_cmd->topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  topo_cmd->reserved0 = 0;

  dev->current_vb_res = nullptr;
  dev->current_vb_stride = 0;
  dev->current_vb_offset = 0;
  auto* vb_cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS, nullptr, 0);
  vb_cmd->start_slot = 0;
  vb_cmd->buffer_count = 0;

  dev->current_ib_res = nullptr;
  auto* ib_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  ib_cmd->buffer = 0;
  ib_cmd->format = AEROGPU_INDEX_FORMAT_UINT16;
  ib_cmd->offset_bytes = 0;
  ib_cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY VsSetShaderResources(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_views,
                                          const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_VERTEX, start_slot, num_views, phViews);
}

void AEROGPU_APIENTRY PsSetShaderResources(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_views,
                                          const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_PIXEL, start_slot, num_views, phViews);
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
  AEROGPU_D3D10_TRACEF_VERBOSE("SetViewports hDevice=%p x=%f y=%f w=%f h=%f min=%f max=%f",
                               hDevice.pDrvPrivate,
                               vp.TopLeftX,
                               vp.TopLeftY,
                               vp.Width,
                               vp.Height,
                               vp.MinDepth,
                               vp.MaxDepth);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (vp.Width > 0.0f && vp.Height > 0.0f) {
    dev->viewport_width = static_cast<uint32_t>(vp.Width);
    dev->viewport_height = static_cast<uint32_t>(vp.Height);
  }

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
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("SetRenderTargets hDevice=%p hRtv=%p hDsv=%p",
                               hDevice.pDrvPrivate,
                               (pRTVs && num_rtvs > 0) ? pRTVs[0].pDrvPrivate : nullptr,
                               hDsv.pDrvPrivate);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t rtv_handle = 0;
  AeroGpuResource* rtv_res = nullptr;
  if (pRTVs && num_rtvs > 0 && pRTVs[0].pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(pRTVs[0]);
    rtv_res = view ? view->resource : nullptr;
    rtv_handle = rtv_res ? rtv_res->handle : (view ? view->texture : 0);
  }

  aerogpu_handle_t dsv_handle = 0;
  AeroGpuResource* dsv_res = nullptr;
  if (hDsv.pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
    dsv_res = view ? view->resource : nullptr;
    dsv_handle = dsv_res ? dsv_res->handle : (view ? view->texture : 0);
  }

  dev->current_rtv = rtv_handle;
  dev->current_rtv_res = rtv_res;
  dev->current_dsv = dsv_handle;
  dev->current_dsv_res = dsv_res;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = (pRTVs && num_rtvs > 0) ? 1 : 0;
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
  AEROGPU_D3D10_TRACEF_VERBOSE("Draw hDevice=%p vc=%u start=%u", hDevice.pDrvPrivate, vertex_count, start_vertex);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);

  if (vertex_count == 3 && dev->current_topology == static_cast<uint32_t>(D3D10_DDI_PRIMITIVE_TOPOLOGY_TRIANGLELIST) &&
      dev->current_rtv_res && dev->current_vb_res) {
    auto* rt = dev->current_rtv_res;
    auto* vb = dev->current_vb_res;

    if (rt->kind == ResourceKind::Texture2D && vb->kind == ResourceKind::Buffer && rt->width && rt->height &&
        vb->storage.size() >= static_cast<size_t>(dev->current_vb_offset) +
                                static_cast<size_t>(start_vertex + 3) * static_cast<size_t>(dev->current_vb_stride)) {
      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, rt->dxgi_format);
      const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
      if (aer_fmt == AEROGPU_FORMAT_INVALID || bpp != 4) {
        goto EmitDrawCmd;
      }

      if (rt->row_pitch_bytes == 0) {
        rt->row_pitch_bytes = rt->width * bpp;
      }
      const uint64_t rt_bytes = aerogpu_texture_required_size_bytes(aer_fmt, rt->row_pitch_bytes, rt->height);
      if (rt_bytes <= static_cast<uint64_t>(SIZE_MAX) && rt->storage.size() < static_cast<size_t>(rt_bytes)) {
        try {
          rt->storage.resize(static_cast<size_t>(rt_bytes));
        } catch (...) {
          set_error(dev, E_OUTOFMEMORY);
          return;
        }
      }

      auto read_f32 = [](const uint8_t* p) -> float {
        float v = 0.0f;
        std::memcpy(&v, p, sizeof(v));
        return v;
      };

      struct V2 {
        float x;
        float y;
      };

      V2 pos[3]{};
      float col[4]{};
      for (UINT i = 0; i < 3; ++i) {
        const size_t base = static_cast<size_t>(dev->current_vb_offset) +
                            static_cast<size_t>(start_vertex + i) * static_cast<size_t>(dev->current_vb_stride);
        const uint8_t* vtx = vb->storage.data() + base;
        pos[i].x = read_f32(vtx + 0);
        pos[i].y = read_f32(vtx + 4);
        if (i == 0) {
          col[0] = read_f32(vtx + 8);
          col[1] = read_f32(vtx + 12);
          col[2] = read_f32(vtx + 16);
          col[3] = read_f32(vtx + 20);
        }
      }

      auto float_to_unorm8 = [](float v) -> uint8_t {
        if (v <= 0.0f) {
          return 0;
        }
        if (v >= 1.0f) {
          return 255;
        }
        const float scaled = v * 255.0f + 0.5f;
        if (scaled <= 0.0f) {
          return 0;
        }
        if (scaled >= 255.0f) {
          return 255;
        }
        return static_cast<uint8_t>(scaled);
      };

      const uint8_t out_r = float_to_unorm8(col[0]);
      const uint8_t out_g = float_to_unorm8(col[1]);
      const uint8_t out_b = float_to_unorm8(col[2]);
      const uint8_t out_a = float_to_unorm8(col[3]);

      auto ndc_to_px = [&](const V2& p) -> V2 {
        V2 out{};
        out.x = (p.x * 0.5f + 0.5f) * static_cast<float>(rt->width);
        out.y = (-p.y * 0.5f + 0.5f) * static_cast<float>(rt->height);
        return out;
      };

      const V2 v0 = ndc_to_px(pos[0]);
      const V2 v1 = ndc_to_px(pos[1]);
      const V2 v2 = ndc_to_px(pos[2]);

      auto edge = [](const V2& a, const V2& b, float x, float y) -> float {
        return (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x);
      };

      const float area = edge(v0, v1, v2.x, v2.y);
      if (area != 0.0f) {
        const float min_x_f = std::min({v0.x, v1.x, v2.x});
        const float max_x_f = std::max({v0.x, v1.x, v2.x});
        const float min_y_f = std::min({v0.y, v1.y, v2.y});
        const float max_y_f = std::max({v0.y, v1.y, v2.y});

        int min_x = static_cast<int>(std::floor(min_x_f));
        int max_x = static_cast<int>(std::ceil(max_x_f));
        int min_y = static_cast<int>(std::floor(min_y_f));
        int max_y = static_cast<int>(std::ceil(max_y_f));

        min_x = std::max(min_x, 0);
        min_y = std::max(min_y, 0);
        max_x = std::min(max_x, static_cast<int>(rt->width));
        max_y = std::min(max_y, static_cast<int>(rt->height));

        for (int y = min_y; y < max_y; ++y) {
          uint8_t* row = rt->storage.data() + static_cast<size_t>(y) * rt->row_pitch_bytes;
          for (int x = min_x; x < max_x; ++x) {
            const float px = static_cast<float>(x) + 0.5f;
            const float py = static_cast<float>(y) + 0.5f;
            const float w0 = edge(v1, v2, px, py);
            const float w1 = edge(v2, v0, px, py);
            const float w2 = edge(v0, v1, px, py);
            const bool inside = (w0 >= 0.0f && w1 >= 0.0f && w2 >= 0.0f) ||
                                (w0 <= 0.0f && w1 <= 0.0f && w2 <= 0.0f);
            if (!inside) {
              continue;
            }

            uint8_t* dst = row + static_cast<size_t>(x) * 4;
            switch (rt->dxgi_format) {
              case kDxgiFormatR8G8B8A8Unorm:
              case kDxgiFormatR8G8B8A8UnormSrgb:
              case kDxgiFormatR8G8B8A8Typeless:
                dst[0] = out_r;
                dst[1] = out_g;
                dst[2] = out_b;
                dst[3] = out_a;
                break;
              case kDxgiFormatB8G8R8X8Unorm:
              case kDxgiFormatB8G8R8X8UnormSrgb:
              case kDxgiFormatB8G8R8X8Typeless:
                dst[0] = out_b;
                dst[1] = out_g;
                dst[2] = out_r;
                dst[3] = 255;
                break;
              case kDxgiFormatB8G8R8A8Unorm:
              case kDxgiFormatB8G8R8A8UnormSrgb:
              case kDxgiFormatB8G8R8A8Typeless:
              default:
                dst[0] = out_b;
                dst[1] = out_g;
                dst[2] = out_r;
                dst[3] = out_a;
                break;
            }
          }
        }
      }
    }
  }

EmitDrawCmd:
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
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawIndexed hDevice=%p ic=%u start=%u base=%d",
                               hDevice.pDrvPrivate,
                               index_count,
                               start_index,
                               base_vertex);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent) {
  AEROGPU_D3D10_TRACEF("Present hDevice=%p syncInterval=%u",
                       hDevice.pDrvPrivate,
                       pPresent ? static_cast<unsigned>(pPresent->SyncInterval) : 0u);
  if (!hDevice.pDrvPrivate || !pPresent) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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

  auto* src_res = hsrc.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hsrc) : nullptr;
  TrackWddmAllocForSubmitLocked(dev, src_res);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  aerogpu_handle_t src_handle = src_res ? src_res->handle : 0;
  AEROGPU_D3D10_11_LOG("trace_resources: D3D10.1 Present sync=%u src_handle=%u",
                       static_cast<unsigned>(pPresent->SyncInterval),
                       static_cast<unsigned>(src_handle));
#endif

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  cmd->scanout_id = 0;
  bool vsync = (pPresent->SyncInterval != 0);
  if (vsync && dev->adapter && dev->adapter->umd_private_valid) {
    vsync = (dev->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
  }
  cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

  HRESULT hr = S_OK;
  submit_locked(dev, true, &hr);
  AEROGPU_D3D10_RET_HR(hr);
}

void AEROGPU_APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_TRACEF("Flush hDevice=%p", hDevice.pDrvPrivate);
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
  AEROGPU_D3D10_11_LOG("pfnMap(D3D10DDIARG_MAP) subresource=%u",
                        static_cast<unsigned>(pMap ? pMap->Subresource : 0));
  uint32_t map_flags_for_log = 0;
  if (pMap) {
    __if_exists(D3D10DDIARG_MAP::MapFlags) {
      map_flags_for_log = static_cast<uint32_t>(pMap->MapFlags);
    }
    __if_not_exists(D3D10DDIARG_MAP::MapFlags) {
      __if_exists(D3D10DDIARG_MAP::Flags) {
        map_flags_for_log = static_cast<uint32_t>(pMap->Flags);
      }
    }
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("Map2 hDevice=%p hResource=%p sub=%u type=%u flags=0x%X",
                               hDevice.pDrvPrivate,
                               (pMap && pMap->hResource.pDrvPrivate) ? pMap->hResource.pDrvPrivate : nullptr,
                               pMap ? static_cast<unsigned>(pMap->Subresource) : 0u,
                               pMap ? static_cast<unsigned>(pMap->MapType) : 0u,
                               static_cast<unsigned>(map_flags_for_log));
  // Keep this local referenced even when tracing is compiled out.
  (void)map_flags_for_log;
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

  if (res->mapped) {
    set_error(dev, E_FAIL);
    return;
  }

  const uint32_t map_type_u = static_cast<uint32_t>(pMap->MapType);
  uint32_t map_flags_u = 0;
  __if_exists(D3D10DDIARG_MAP::MapFlags) {
    map_flags_u = static_cast<uint32_t>(pMap->MapFlags);
  }
  __if_not_exists(D3D10DDIARG_MAP::MapFlags) {
    __if_exists(D3D10DDIARG_MAP::Flags) {
      map_flags_u = static_cast<uint32_t>(pMap->Flags);
    }
  }

  if (pMap->Subresource != 0) {
    set_error(dev, E_NOTIMPL);
    return;
  }

  if (map_type_u == kD3DMapWriteDiscard) {
    if (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer | kD3D10BindConstantBuffer)) {
      void* data = nullptr;
      const HRESULT hr = map_dynamic_buffer_locked(dev, res, /*discard=*/true, &data);
      if (FAILED(hr)) {
        set_error(dev, hr);
        return;
      }
      pOut->pData = data;
      pOut->RowPitch = 0;
      pOut->DepthPitch = 0;
      return;
    }
  } else if (map_type_u == kD3DMapWriteNoOverwrite) {
    if (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) {
      void* data = nullptr;
      const HRESULT hr = map_dynamic_buffer_locked(dev, res, /*discard=*/false, &data);
      if (FAILED(hr)) {
        set_error(dev, hr);
        return;
      }
      pOut->pData = data;
      pOut->RowPitch = 0;
      pOut->DepthPitch = 0;
      return;
    }
  }

  const HRESULT sync_hr = sync_read_map_locked(dev, res, map_type_u, map_flags_u);
  if (FAILED(sync_hr)) {
    set_error(dev, sync_hr);
    return;
  }
  const HRESULT hr = map_resource_locked(dev, res, pMap->Subresource, map_type_u, map_flags_u, pOut);
  if (FAILED(hr)) {
    set_error(dev, hr);
    return;
  }
}

void AEROGPU_APIENTRY Unmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, UINT subresource) {
  AEROGPU_D3D10_11_LOG("pfnUnmap subresource=%u", static_cast<unsigned>(subresource));
  AEROGPU_D3D10_TRACEF_VERBOSE("Unmap hDevice=%p hResource=%p sub=%u",
                               hDevice.pDrvPrivate,
                               hResource.pDrvPrivate,
                               static_cast<unsigned>(subresource));
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
  if (!res->mapped) {
    set_error(dev, E_FAIL);
    return;
  }
  if (subresource != res->mapped_subresource) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  unmap_resource_locked(dev, res, static_cast<uint32_t>(subresource));
}

void AEROGPU_APIENTRY UpdateSubresourceUP(D3D10DDI_HDEVICE hDevice,
                                         const D3D10DDIARG_UPDATESUBRESOURCEUP* pArgs,
                                         const void* pSysMem) {
  AEROGPU_D3D10_TRACEF_VERBOSE("UpdateSubresourceUP hDevice=%p hDstResource=%p sub=%u rowPitch=%u src=%p",
                               hDevice.pDrvPrivate,
                               (pArgs && pArgs->hDstResource.pDrvPrivate) ? pArgs->hDstResource.pDrvPrivate : nullptr,
                               pArgs ? static_cast<unsigned>(pArgs->DstSubresource) : 0u,
                               pArgs ? static_cast<unsigned>(pArgs->RowPitch) : 0u,
                               pSysMem);
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

  if (res->kind == ResourceKind::Buffer) {
    if (pArgs->DstSubresource != 0) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    uint64_t dst_off = 0;
    uint64_t bytes = res->size_bytes;
    if (pArgs->pDstBox) {
      const auto* box = pArgs->pDstBox;
      if (box->right < box->left || box->top != 0 || box->bottom != 1 || box->front != 0 || box->back != 1) {
        set_error(dev, E_INVALIDARG);
        return;
      }
      dst_off = static_cast<uint64_t>(box->left);
      bytes = static_cast<uint64_t>(box->right - box->left);
    }
    if (dst_off > res->size_bytes || bytes > res->size_bytes - dst_off) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    if (res->storage.empty()) {
      const uint64_t storage_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
      if (storage_bytes > static_cast<uint64_t>(std::numeric_limits<size_t>::max())) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
      try {
        res->storage.resize(static_cast<size_t>(storage_bytes), 0);
      } catch (...) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
    }
    if (bytes > std::numeric_limits<size_t>::max()) {
      set_error(dev, E_OUTOFMEMORY);
      return;
    }
    if (bytes) {
      std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pSysMem, static_cast<size_t>(bytes));
    }
    emit_upload_resource_locked(dev, res, dst_off, bytes);
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t dst_subresource = static_cast<uint32_t>(pArgs->DstSubresource);
    const uint64_t subresource_count =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    if (subresource_count == 0 || dst_subresource >= subresource_count ||
        dst_subresource >= res->tex2d_subresources.size()) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    const Texture2DSubresourceLayout dst_layout = res->tex2d_subresources[dst_subresource];

    if (pArgs->pDstBox) {
      set_error(dev, E_NOTIMPL);
      return;
    }
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      set_error(dev, E_NOTIMPL);
      return;
    }
    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst_layout.width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, dst_layout.height);
    if (row_bytes == 0 || rows == 0 || dst_layout.size_bytes == 0) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (dst_layout.row_pitch_bytes < row_bytes) {
      set_error(dev, E_FAIL);
      return;
    }
    const uint64_t total_bytes = resource_total_bytes(dev, res);
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(std::numeric_limits<size_t>::max())) {
      set_error(dev, E_OUTOFMEMORY);
      return;
    }
    const size_t total_size = static_cast<size_t>(total_bytes);
    if (res->storage.size() < total_size) {
      try {
        res->storage.resize(total_size, 0);
      } catch (...) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
    }

    const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
    const size_t src_pitch =
        pArgs->RowPitch ? static_cast<size_t>(pArgs->RowPitch) : static_cast<size_t>(row_bytes);
    if (src_pitch < row_bytes) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (dst_layout.offset_bytes > res->storage.size()) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    const size_t dst_base = static_cast<size_t>(dst_layout.offset_bytes);
    if (dst_layout.size_bytes > res->storage.size() - dst_base) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    std::memset(res->storage.data() + dst_base, 0, static_cast<size_t>(dst_layout.size_bytes));
    for (uint32_t y = 0; y < rows; y++) {
      std::memcpy(res->storage.data() + dst_base + static_cast<size_t>(y) * dst_layout.row_pitch_bytes,
                  src + static_cast<size_t>(y) * src_pitch,
                  row_bytes);
      if (dst_layout.row_pitch_bytes > row_bytes) {
        std::memset(res->storage.data() + dst_base + static_cast<size_t>(y) * dst_layout.row_pitch_bytes + row_bytes,
                    0,
                    dst_layout.row_pitch_bytes - row_bytes);
      }
    }
    emit_upload_resource_locked(dev, res, dst_layout.offset_bytes, dst_layout.size_bytes);
    return;
  }

  set_error(dev, E_NOTIMPL);
}

void AEROGPU_APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice,
                                               D3D10DDI_HRESOURCE* pResources,
                                               UINT numResources) {
  AEROGPU_D3D10_TRACEF("RotateResourceIdentities hDevice=%p num=%u", hDevice.pDrvPrivate, numResources);
  if (!hDevice.pDrvPrivate || !pResources || numResources < 2) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  AEROGPU_D3D10_11_LOG("trace_resources: D3D10.1 RotateResourceIdentities count=%u",
                       static_cast<unsigned>(numResources));
  for (UINT i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  + slot[%u]=%u",
                         static_cast<unsigned>(i),
                         static_cast<unsigned>(handle));
  }
#endif

  std::vector<AeroGpuResource*> resources;
  resources.reserve(numResources);
  for (UINT i = 0; i < numResources; ++i) {
    auto* res = pResources[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i]) : nullptr;
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

  const AeroGpuResource* ref = resources[0];
  if (!ref || ref->kind != ResourceKind::Texture2D || !(ref->bind_flags & kD3D10BindRenderTarget)) {
    return;
  }
  for (UINT i = 1; i < numResources; ++i) {
    const AeroGpuResource* r = resources[i];
    if (!r || r->kind != ResourceKind::Texture2D || !(r->bind_flags & kD3D10BindRenderTarget) ||
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
    AeroGpuResource::WddmIdentity wddm;
    std::vector<Texture2DSubresourceLayout> tex2d_subresources;
    std::vector<uint8_t> storage;
    uint64_t last_gpu_write_fence = 0;
    bool mapped = false;
    bool mapped_write = false;
    uint32_t mapped_subresource = 0;
    uint64_t mapped_offset = 0;
    uint64_t mapped_size = 0;
  };

  auto take_identity = [](AeroGpuResource* res) -> ResourceIdentity {
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
    id.mapped_write = res->mapped_write;
    id.mapped_subresource = res->mapped_subresource;
    id.mapped_offset = res->mapped_offset;
    id.mapped_size = res->mapped_size;
    return id;
  };

  auto put_identity = [](AeroGpuResource* res, ResourceIdentity&& id) {
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
    res->mapped_write = id.mapped_write;
    res->mapped_subresource = id.mapped_subresource;
    res->mapped_offset = id.mapped_offset;
    res->mapped_size = id.mapped_size;
  };

  ResourceIdentity saved = take_identity(resources[0]);
  for (UINT i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  const bool needs_rebind =
      dev->current_rtv_res &&
      (std::find(resources.begin(), resources.end(), dev->current_rtv_res) != resources.end());
  if (needs_rebind) {
    const aerogpu_handle_t new_rtv = dev->current_rtv_res ? dev->current_rtv_res->handle : 0;
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!cmd) {
      // Undo the rotation (rotate right by one).
      ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
      for (UINT i = numResources - 1; i > 0; --i) {
        put_identity(resources[i], take_identity(resources[i - 1]));
      }
      put_identity(resources[0], std::move(undo_saved));
      set_error(dev, E_OUTOFMEMORY);
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

  auto is_rotated = [&resources](const AeroGpuResource* res) -> bool {
    if (!res) {
      return false;
    }
    return std::find(resources.begin(), resources.end(), res) != resources.end();
  };

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_vs_srvs[slot])) {
      continue;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
    cmd->slot = slot;
    cmd->texture = dev->current_vs_srvs[slot] ? dev->current_vs_srvs[slot]->handle : 0;
    cmd->reserved0 = 0;
  }
  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_ps_srvs[slot])) {
      continue;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->slot = slot;
    cmd->texture = dev->current_ps_srvs[slot] ? dev->current_ps_srvs[slot]->handle : 0;
    cmd->reserved0 = 0;
  }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  for (UINT i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  -> slot[%u]=%u",
                         static_cast<unsigned>(i),
                         static_cast<unsigned>(handle));
  }
#endif
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI (10.1)
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10_1DDIARG_CREATEDEVICE*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateDeviceSize");
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, D3D10_1DDIARG_CREATEDEVICE* pCreateDevice) {
  AEROGPU_D3D10_TRACEF("CreateDevice hAdapter=%p hDevice=%p",
                       hAdapter.pDrvPrivate,
                       pCreateDevice ? pCreateDevice->hDrvDevice.pDrvPrivate : nullptr);
  if (!pCreateDevice || !pCreateDevice->hDrvDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  auto* device = new (pCreateDevice->hDrvDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->kmt_adapter = adapter->kmt_adapter;
  device->hrt_device = pCreateDevice->hRTDevice;
  device->pfn_set_error = pCreateDevice->pCallbacks ? pCreateDevice->pCallbacks->pfnSetErrorCb : nullptr;
  __if_exists(D3D10_1DDIARG_CREATEDEVICE::pUMCallbacks) {
    device->callbacks = pCreateDevice->pUMCallbacks;
  }
  if (!device->callbacks && pCreateDevice->pCallbacks) {
    device->callbacks = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(pCreateDevice->pCallbacks);
  }

  HRESULT init_hr = InitKernelDeviceContext(device, hAdapter);
  if (FAILED(init_hr) || device->kmt_fence_syncobj == 0) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return FAILED(init_hr) ? init_hr : E_FAIL;
  }

  InitDeviceFuncsWithStubs(pCreateDevice->pDeviceFuncs);

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource;
  {
    using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
    if constexpr (HasOpenResource<DeviceFuncs>::value) {
      using Fn = decltype(pCreateDevice->pDeviceFuncs->pfnOpenResource);
      if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
        pCreateDevice->pDeviceFuncs->pfnOpenResource = &OpenResource;
      }
    }
  }
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = &DestroyResource;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = &CalcPrivateVertexShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = &CalcPrivatePixelShaderSize;
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = &CreateVertexShader;
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = &CreatePixelShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = &DestroyVertexShader;
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = &DestroyPixelShader;
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderSize) {
    // Not implemented yet, but keep the entrypoints non-null so runtimes don't
    // crash on unexpected geometry shader probes.
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize =
        &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize)>::Call;
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader =
        &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader)>::Call;
    pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader =
        &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader)>::Call;
  }
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderWithStreamOutputSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call;
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput =
        &DdiStub<decltype(pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput)>::Call;
  }

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
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateShaderResourceViewSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize;
    pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = &CreateShaderResourceView;
    pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = &DestroyShaderResourceView;
  }
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateSamplerSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize;
    pCreateDevice->pDeviceFuncs->pfnCreateSampler = &CreateSampler;
    pCreateDevice->pDeviceFuncs->pfnDestroySampler = &DestroySampler;
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = &CreateBlendState;
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = &DestroyBlendState;
#if AEROGPU_D3D10_TRACE
  #define AEROGPU_D3D10_ASSIGN_STUB(field, id)                                     \
    pCreateDevice->pDeviceFuncs->field =                                           \
        &DdiTraceStub<decltype(pCreateDevice->pDeviceFuncs->field), DdiTraceStubId::id>::Call
#else
  #define AEROGPU_D3D10_ASSIGN_STUB(field, id) \
    pCreateDevice->pDeviceFuncs->field = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->field)>::Call
#endif

  AEROGPU_D3D10_ASSIGN_STUB(pfnSetBlendState, SetBlendState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = &CreateRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = &DestroyRasterizerState;
  AEROGPU_D3D10_ASSIGN_STUB(pfnSetRasterizerState, SetRasterizerState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = &CreateDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = &DestroyDepthStencilState;
  AEROGPU_D3D10_ASSIGN_STUB(pfnSetDepthStencilState, SetDepthStencilState);

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = &IaSetInputLayout;
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = &IaSetVertexBuffers;
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = &IaSetIndexBuffer;
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = &IaSetTopology;

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = &VsSetShader;
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = &PsSetShader;

  AEROGPU_D3D10_ASSIGN_STUB(pfnVsSetConstantBuffers, VsSetConstantBuffers);
  AEROGPU_D3D10_ASSIGN_STUB(pfnPsSetConstantBuffers, PsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources = &VsSetShaderResources;
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources = &PsSetShaderResources;
  AEROGPU_D3D10_ASSIGN_STUB(pfnVsSetSamplers, VsSetSamplers);
  AEROGPU_D3D10_ASSIGN_STUB(pfnPsSetSamplers, PsSetSamplers);

  AEROGPU_D3D10_ASSIGN_STUB(pfnGsSetShader, GsSetShader);
  AEROGPU_D3D10_ASSIGN_STUB(pfnGsSetConstantBuffers, GsSetConstantBuffers);
  AEROGPU_D3D10_ASSIGN_STUB(pfnGsSetShaderResources, GsSetShaderResources);
  AEROGPU_D3D10_ASSIGN_STUB(pfnGsSetSamplers, GsSetSamplers);

  pCreateDevice->pDeviceFuncs->pfnSetViewports = &SetViewports;
  AEROGPU_D3D10_ASSIGN_STUB(pfnSetScissorRects, SetScissorRects);
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = &SetRenderTargets;

  pCreateDevice->pDeviceFuncs->pfnDraw = &Draw;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = &DrawIndexed;
  AEROGPU_D3D10_ASSIGN_STUB(pfnDrawInstanced, DrawInstanced);
  AEROGPU_D3D10_ASSIGN_STUB(pfnDrawIndexedInstanced, DrawIndexedInstanced);
  AEROGPU_D3D10_ASSIGN_STUB(pfnDrawAuto, DrawAuto);
  pCreateDevice->pDeviceFuncs->pfnPresent = &Present;
  pCreateDevice->pDeviceFuncs->pfnFlush = &Flush;
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = &RotateResourceIdentities;
  pCreateDevice->pDeviceFuncs->pfnClearState = &ClearState;

  using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
  if constexpr (HasOpenResource<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnOpenResource = &OpenResource;
  }

  // Map/unmap. Win7 D3D11 runtimes may use specialized entrypoints.
  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  if constexpr (HasStagingResourceMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnStagingResourceMap = &StagingResourceMap<>;
    pCreateDevice->pDeviceFuncs->pfnStagingResourceUnmap = &StagingResourceUnmap<>;
  }
  if constexpr (HasDynamicIABufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard<>;
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite<>;
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferUnmap = &DynamicIABufferUnmap<>;
  }
  if constexpr (HasDynamicConstantBufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard<>;
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmap<>;
  }
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  pCreateDevice->pDeviceFuncs->pfnCopyResource =
      &CopyResourceImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call;
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      &CopySubresourceRegionImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call;

  #undef AEROGPU_D3D10_ASSIGN_STUB

  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  AEROGPU_D3D10_TRACEF("CloseAdapter hAdapter=%p", hAdapter.pDrvPrivate);
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  DestroyKmtAdapterHandle(adapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI (10.0)
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize10(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateDeviceSize10");
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice10(D3D10DDI_HADAPTER hAdapter, D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  AEROGPU_D3D10_TRACEF("CreateDevice10 hAdapter=%p hDevice=%p",
                       hAdapter.pDrvPrivate,
                       pCreateDevice ? pCreateDevice->hDrvDevice.pDrvPrivate : nullptr);
  if (!pCreateDevice || !pCreateDevice->hDrvDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  auto* device = new (pCreateDevice->hDrvDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->kmt_adapter = adapter->kmt_adapter;
  device->hrt_device = pCreateDevice->hRTDevice;
  device->pfn_set_error = pCreateDevice->pCallbacks ? pCreateDevice->pCallbacks->pfnSetErrorCb : nullptr;
  __if_exists(D3D10DDIARG_CREATEDEVICE::pUMCallbacks) {
    device->callbacks = pCreateDevice->pUMCallbacks;
  }
  if (!device->callbacks && pCreateDevice->pCallbacks) {
    device->callbacks = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(pCreateDevice->pCallbacks);
  }

  HRESULT init_hr = InitKernelDeviceContext(device, hAdapter);
  if (FAILED(init_hr) || device->kmt_fence_syncobj == 0) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return FAILED(init_hr) ? init_hr : E_FAIL;
  }

  InitDeviceFuncsWithStubs(pCreateDevice->pDeviceFuncs);

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = &DestroyDevice;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  pCreateDevice->pDeviceFuncs->pfnCreateResource = &CreateResource;
  {
    using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
    if constexpr (HasOpenResource<DeviceFuncs>::value) {
      using Fn = decltype(pCreateDevice->pDeviceFuncs->pfnOpenResource);
      if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
        pCreateDevice->pDeviceFuncs->pfnOpenResource = &OpenResource;
      }
    }
  }
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
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize;
  pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = &CreateShaderResourceView;
  pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = &DestroyShaderResourceView;
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize;
  pCreateDevice->pDeviceFuncs->pfnCreateSampler = &CreateSampler;
  pCreateDevice->pDeviceFuncs->pfnDestroySampler = &DestroySampler;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = &CreateBlendState;
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = &DestroyBlendState;
  pCreateDevice->pDeviceFuncs->pfnSetBlendState =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetBlendState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = &CreateRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = &DestroyRasterizerState;
  pCreateDevice->pDeviceFuncs->pfnSetRasterizerState =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetRasterizerState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = &CreateDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = &DestroyDepthStencilState;
  pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState)>::Call;

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = &IaSetInputLayout;
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = &IaSetVertexBuffers;
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = &IaSetIndexBuffer;
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = &IaSetTopology;

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = &VsSetShader;
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = &PsSetShader;

  pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources = &VsSetShaderResources;
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources = &PsSetShaderResources;
  pCreateDevice->pDeviceFuncs->pfnVsSetSamplers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnVsSetSamplers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPsSetSamplers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnPsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnGsSetShader = &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShader)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources)>::Call;
  pCreateDevice->pDeviceFuncs->pfnGsSetSamplers =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetSamplers)>::Call;

  pCreateDevice->pDeviceFuncs->pfnSetViewports = &SetViewports;
  pCreateDevice->pDeviceFuncs->pfnSetScissorRects =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnSetScissorRects)>::Call;
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = &SetRenderTargets;

  pCreateDevice->pDeviceFuncs->pfnDraw = &Draw;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = &DrawIndexed;
  pCreateDevice->pDeviceFuncs->pfnDrawInstanced = &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced =
      &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced)>::Call;
  pCreateDevice->pDeviceFuncs->pfnDrawAuto = &DdiNoopStub<decltype(pCreateDevice->pDeviceFuncs->pfnDrawAuto)>::Call;
  pCreateDevice->pDeviceFuncs->pfnPresent = &Present;
  pCreateDevice->pDeviceFuncs->pfnFlush = &Flush;
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = &RotateResourceIdentities;
  pCreateDevice->pDeviceFuncs->pfnClearState = &ClearState;

  using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
  if constexpr (HasOpenResource<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnOpenResource = &OpenResource;
  }

  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  pCreateDevice->pDeviceFuncs->pfnCopyResource =
      &CopyResourceImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call;
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      &CopySubresourceRegionImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call;

  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT AEROGPU_APIENTRY GetCaps10(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_GETCAPS* pCaps) {
  AEROGPU_D3D10_TRACEF("GetCaps10 Type=%u DataSize=%u pData=%p",
                       pCaps ? static_cast<unsigned>(pCaps->Type) : 0u,
                       pCaps ? static_cast<unsigned>(pCaps->DataSize) : 0u,
                       pCaps ? pCaps->pData : nullptr);
#if defined(AEROGPU_D3D10_11_CAPS_LOG)
  if (pCaps) {
    char buf[128] = {};
    snprintf(buf,
             sizeof(buf),
             "aerogpu-d3d10_1: GetCaps10 type=%u size=%u\n",
             (unsigned)pCaps->Type,
             (unsigned)pCaps->DataSize);
    OutputDebugStringA(buf);
  }
#endif
  if (!pCaps) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  if (!pCaps->pData || pCaps->DataSize == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  DXGI_FORMAT in_format = DXGI_FORMAT_UNKNOWN;
  if (pCaps->Type == D3D10DDICAPS_TYPE_FORMAT_SUPPORT &&
      pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
    in_format = reinterpret_cast<const D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData)->Format;
  }

  DXGI_FORMAT msaa_format = DXGI_FORMAT_UNKNOWN;
  UINT msaa_sample_count = 0;
  if (pCaps->Type == D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS && pCaps->DataSize >= sizeof(DXGI_FORMAT) + sizeof(UINT)) {
    const uint8_t* in_bytes = reinterpret_cast<const uint8_t*>(pCaps->pData);
    msaa_format = *reinterpret_cast<const DXGI_FORMAT*>(in_bytes);
    msaa_sample_count = *reinterpret_cast<const UINT*>(in_bytes + sizeof(DXGI_FORMAT));
  }

  std::memset(pCaps->pData, 0, pCaps->DataSize);
  const bool supports_bc =
      SupportsBcFormatsAdapter(FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter));
  // ABI 1.2 adds explicit sRGB format variants (same gating as BC formats).
  const bool supports_srgb = supports_bc;

  switch (pCaps->Type) {
    case D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    __if_exists(D3D10DDICAPS_TYPE_SHADER) {
      case D3D10DDICAPS_TYPE_SHADER: {
        // Shader model caps for FL10_0: VS/GS/PS are SM4.0.
        //
        // The exact struct layout varies across WDK revisions, but in practice it
        // begins with UINT "version tokens" using the DXBC encoding:
        //   (program_type << 16) | (major << 4) | minor
        //
        // Only write fields that fit to avoid overrunning DataSize.
        constexpr auto ver_token = [](UINT program_type, UINT major, UINT minor) -> UINT {
          return (program_type << 16) | (major << 4) | minor;
        };
        constexpr UINT kShaderTypePixel = 0;
        constexpr UINT kShaderTypeVertex = 1;
        constexpr UINT kShaderTypeGeometry = 2;

        auto write_u32 = [&](size_t offset, UINT value) {
          if (pCaps->DataSize < offset + sizeof(UINT)) {
            return;
          }
          *reinterpret_cast<UINT*>(reinterpret_cast<uint8_t*>(pCaps->pData) + offset) = value;
        };

        write_u32(0, ver_token(kShaderTypePixel, 4, 0));
        write_u32(sizeof(UINT), ver_token(kShaderTypeVertex, 4, 0));
        write_u32(sizeof(UINT) * 2, ver_token(kShaderTypeGeometry, 4, 0));
        break;
      }
    }

    case D3D10DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        fmt->Format = in_format;
        const uint32_t format = static_cast<uint32_t>(in_format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatB8G8R8A8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatB8G8R8A8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
                                   : 0;
            break;
          case kDxgiFormatB8G8R8X8Unorm:
          case kDxgiFormatB8G8R8X8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatB8G8R8X8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
                                   : 0;
            break;
          case kDxgiFormatR8G8B8A8Unorm:
          case kDxgiFormatR8G8B8A8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatR8G8B8A8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
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
              support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            } else {
              support = 0;
            }
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
        __if_exists(D3D10DDIARG_FORMAT_SUPPORT::FormatSupport2) {
          fmt->FormatSupport2 = 0;
        }
        AEROGPU_D3D10_TRACEF("GetCaps10 FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
      if (pCaps->DataSize >= sizeof(DXGI_FORMAT) + sizeof(UINT) * 2) {
        bool supported_format = false;
        switch (static_cast<uint32_t>(msaa_format)) {
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
        uint8_t* out_bytes = reinterpret_cast<uint8_t*>(pCaps->pData);
        *reinterpret_cast<DXGI_FORMAT*>(out_bytes) = msaa_format;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT)) = msaa_sample_count;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT) + sizeof(UINT)) =
            (msaa_sample_count == 1 && supported_format) ? 1u : 0u;
      }
      break;

    default:
      break;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER hAdapter, const D3D10_1DDIARG_GETCAPS* pCaps) {
  AEROGPU_D3D10_TRACEF("GetCaps Type=%u DataSize=%u pData=%p",
                       pCaps ? static_cast<unsigned>(pCaps->Type) : 0u,
                       pCaps ? static_cast<unsigned>(pCaps->DataSize) : 0u,
                       pCaps ? pCaps->pData : nullptr);
#if defined(AEROGPU_D3D10_11_CAPS_LOG)
  if (pCaps) {
    char buf[128] = {};
    snprintf(buf,
             sizeof(buf),
             "aerogpu-d3d10_1: GetCaps type=%u size=%u\n",
             (unsigned)pCaps->Type,
             (unsigned)pCaps->DataSize);
    OutputDebugStringA(buf);
  }
#endif
  if (!pCaps) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  if (!pCaps->pData || pCaps->DataSize == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  DXGI_FORMAT in_format = DXGI_FORMAT_UNKNOWN;
  if (pCaps->Type == D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT &&
      pCaps->DataSize >= sizeof(D3D10_1DDIARG_FORMAT_SUPPORT)) {
    in_format = reinterpret_cast<const D3D10_1DDIARG_FORMAT_SUPPORT*>(pCaps->pData)->Format;
  }

  DXGI_FORMAT msaa_format = DXGI_FORMAT_UNKNOWN;
  UINT msaa_sample_count = 0;
  if (pCaps->Type == D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS && pCaps->DataSize >= sizeof(DXGI_FORMAT) + sizeof(UINT)) {
    const uint8_t* in_bytes = reinterpret_cast<const uint8_t*>(pCaps->pData);
    msaa_format = *reinterpret_cast<const DXGI_FORMAT*>(in_bytes);
    msaa_sample_count = *reinterpret_cast<const UINT*>(in_bytes + sizeof(DXGI_FORMAT));
  }

  // Default: return zeroed caps (conservative). Specific required queries are
  // handled below.
  std::memset(pCaps->pData, 0, pCaps->DataSize);
  const bool supports_bc =
      SupportsBcFormatsAdapter(FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter));
  // ABI 1.2 adds explicit sRGB format variants (same gating as BC formats).
  const bool supports_srgb = supports_bc;

  switch (pCaps->Type) {
    case D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    __if_exists(D3D10_1DDICAPS_TYPE_SHADER) {
      case D3D10_1DDICAPS_TYPE_SHADER: {
        // Shader model caps for FL10_0: VS/GS/PS are SM4.0.
        //
        // The exact struct layout varies across WDK revisions, but in practice it
        // begins with UINT "version tokens" using the DXBC encoding:
        //   (program_type << 16) | (major << 4) | minor
        //
        // Only write fields that fit to avoid overrunning DataSize.
        constexpr auto ver_token = [](UINT program_type, UINT major, UINT minor) -> UINT {
          return (program_type << 16) | (major << 4) | minor;
        };
        constexpr UINT kShaderTypePixel = 0;
        constexpr UINT kShaderTypeVertex = 1;
        constexpr UINT kShaderTypeGeometry = 2;

        auto write_u32 = [&](size_t offset, UINT value) {
          if (pCaps->DataSize < offset + sizeof(UINT)) {
            return;
          }
          *reinterpret_cast<UINT*>(reinterpret_cast<uint8_t*>(pCaps->pData) + offset) = value;
        };

        write_u32(0, ver_token(kShaderTypePixel, 4, 0));
        write_u32(sizeof(UINT), ver_token(kShaderTypeVertex, 4, 0));
        write_u32(sizeof(UINT) * 2, ver_token(kShaderTypeGeometry, 4, 0));
        break;
      }
    }

    case D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10_1DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10_1DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        fmt->Format = in_format;
        const uint32_t format = static_cast<uint32_t>(in_format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatB8G8R8A8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatB8G8R8A8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
                                   : 0;
            break;
          case kDxgiFormatB8G8R8X8Unorm:
          case kDxgiFormatB8G8R8X8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatB8G8R8X8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
                                   : 0;
            break;
          case kDxgiFormatR8G8B8A8Unorm:
          case kDxgiFormatR8G8B8A8Typeless:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            break;
          case kDxgiFormatR8G8B8A8UnormSrgb:
            support = supports_srgb ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY |
                                       D3D10_FORMAT_SUPPORT_BLENDABLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
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
              support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
            } else {
              support = 0;
            }
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
        AEROGPU_D3D10_TRACEF("GetCaps FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
      if (pCaps->DataSize >= sizeof(DXGI_FORMAT) + sizeof(UINT) * 2) {
        bool supported_format = false;
        switch (static_cast<uint32_t>(msaa_format)) {
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
        uint8_t* out_bytes = reinterpret_cast<uint8_t*>(pCaps->pData);
        *reinterpret_cast<DXGI_FORMAT*>(out_bytes) = msaa_format;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT)) = msaa_sample_count;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT) + sizeof(UINT)) =
            (msaa_sample_count == 1 && supported_format) ? 1u : 0u;
      }
      break;

    default:
      break;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT OpenAdapter_WDK(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_TRACEF("OpenAdapter_WDK iface=%u ver=%u",
                       pOpenData ? static_cast<unsigned>(pOpenData->Interface) : 0u,
                       pOpenData ? static_cast<unsigned>(pOpenData->Version) : 0u);
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  if (pOpenData->Interface == D3D10DDI_INTERFACE_VERSION) {
    AEROGPU_D3D10_RET_HR(AeroGpuOpenAdapter10Wdk(pOpenData));
  }

  if (pOpenData->Interface == D3D10_1DDI_INTERFACE_VERSION) {
    // `Version` is treated as an in/out negotiation field by some runtimes. If
    // the runtime doesn't initialize it, accept 0 and return the supported
    // 10.1 DDI version.
    if (pOpenData->Version == 0) {
      pOpenData->Version = D3D10_1DDI_SUPPORTED;
    } else if (pOpenData->Version < D3D10_1DDI_SUPPORTED) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    } else if (pOpenData->Version > D3D10_1DDI_SUPPORTED) {
      pOpenData->Version = D3D10_1DDI_SUPPORTED;
    }

    auto* adapter = new (std::nothrow) AeroGpuAdapter();
    if (!adapter) {
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    InitKmtAdapterHandle(adapter);
    InitUmdPrivate(adapter);
    pOpenData->hAdapter.pDrvPrivate = adapter;

    auto* funcs = reinterpret_cast<D3D10_1DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
    std::memset(funcs, 0, sizeof(*funcs));
    funcs->pfnGetCaps = &GetCaps;
    funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
    funcs->pfnCreateDevice = &CreateDevice;
    funcs->pfnCloseAdapter = &CloseAdapter;
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  AEROGPU_D3D10_RET_HR(E_INVALIDARG);
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
  LogModulePathOnce();
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter10");
  if (!pOpenData) {
    return E_INVALIDARG;
  }
  // `OpenAdapter10` is the D3D10 entrypoint. Some runtimes treat `Interface` as
  // an in/out negotiation field; accept 0 and default to the D3D10 DDI.
  if (pOpenData->Interface == 0) {
    pOpenData->Interface = D3D10DDI_INTERFACE_VERSION;
  }
  return OpenAdapter_WDK(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  LogModulePathOnce();
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter10_2");
  if (!pOpenData) {
    return E_INVALIDARG;
  }
  // `OpenAdapter10_2` is the D3D10.1 entrypoint. Accept 0 and default to the
  // D3D10.1 DDI.
  if (pOpenData->Interface == 0) {
    pOpenData->Interface = D3D10_1DDI_INTERFACE_VERSION;
  }
  return OpenAdapter_WDK(pOpenData);
}
} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
