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

#include "aerogpu_d3d10_11_wdk_abi_asserts.h"

#include <atomic>
#include <algorithm>
#include <condition_variable>
#include <cstdarg>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <cmath>
#include <mutex>
#include <new>
#include <tuple>
#include <type_traits>
#include <unordered_map>
#include <utility>
#include <vector>

#include <d3d10.h>
#include <d3d10_1.h>
#include <d3dkmthk.h>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"
#include "../../common/aerogpu_win32_security.h"
#include "aerogpu_d3d10_11_wddm_submit.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"
#include "../../../protocol/aerogpu_win7_abi.h"

namespace {

constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
}

constexpr NTSTATUS kStatusTimeout = static_cast<NTSTATUS>(0x00000102L); // STATUS_TIMEOUT
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr HRESULT kHrPending = static_cast<HRESULT>(0x8000000Au); // E_PENDING
constexpr HRESULT kHrNtStatusGraphicsGpuBusy =
    static_cast<HRESULT>(0xD01E0102L); // HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)
constexpr uint32_t kD3DMapFlagDoNotWait = 0x100000;
constexpr uint32_t kAeroGpuTimeoutMsInfinite = ~0u;
constexpr uint32_t kAeroGpuDeviceLiveCookie = 0xA3E0D310u;

// -----------------------------------------------------------------------------
// Logging (opt-in)
// -----------------------------------------------------------------------------

// Define AEROGPU_D3D10_WDK_TRACE_CAPS=1 to emit OutputDebugStringA traces for
// D3D10DDI adapter caps queries. This is intentionally lightweight so that
// missing caps types can be discovered quickly on real Win7 systems without
// having to attach a debugger first.
#if !defined(AEROGPU_D3D10_WDK_TRACE_CAPS)
  #if defined(AEROGPU_D3D10_11_CAPS_LOG)
    #define AEROGPU_D3D10_WDK_TRACE_CAPS 1
  #else
    #define AEROGPU_D3D10_WDK_TRACE_CAPS 0
  #endif
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

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
void TraceCreateResourceDesc(const D3D10DDIARG_CREATERESOURCE* pDesc) {
  if (!pDesc) {
    return;
  }

  uint32_t usage = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
    usage = static_cast<uint32_t>(pDesc->Usage);
  }

  uint32_t cpu_access = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::CPUAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc->CPUAccessFlags);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::CpuAccessFlags) {
    cpu_access = static_cast<uint32_t>(pDesc->CpuAccessFlags);
  }

  uint32_t sample_count = 0;
  uint32_t sample_quality = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::SampleDesc) {
    sample_count = static_cast<uint32_t>(pDesc->SampleDesc.Count);
    sample_quality = static_cast<uint32_t>(pDesc->SampleDesc.Quality);
  }

  uint64_t resource_flags_bits = 0;
  uint32_t resource_flags_size = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::ResourceFlags) {
    resource_flags_size = static_cast<uint32_t>(sizeof(pDesc->ResourceFlags));
    const size_t n = std::min(sizeof(resource_flags_bits), sizeof(pDesc->ResourceFlags));
    std::memcpy(&resource_flags_bits, &pDesc->ResourceFlags, n);
  }

  uint32_t num_allocations = 0;
  const void* allocation_info = nullptr;
  const void* primary_desc = nullptr;
  uint32_t primary = 0;
  __if_exists(D3D10DDIARG_CREATERESOURCE::NumAllocations) {
    num_allocations = static_cast<uint32_t>(pDesc->NumAllocations);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::pAllocationInfo) {
    allocation_info = pDesc->pAllocationInfo;
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::pPrimaryDesc) {
    primary_desc = pDesc->pPrimaryDesc;
    primary = (primary_desc != nullptr) ? 1u : 0u;
  }

  const void* init_ptr = nullptr;
  __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
    init_ptr = pDesc->pInitialDataUP;
  }
  __if_not_exists(D3D10DDIARG_CREATERESOURCE::pInitialDataUP) {
    __if_exists(D3D10DDIARG_CREATERESOURCE::pInitialData) {
      init_ptr = pDesc->pInitialData;
    }
  }

  AEROGPU_D3D10_11_LOG(
      "trace_resources: D3D10 CreateResource dim=%u bind=0x%08X usage=%u cpu=0x%08X misc=0x%08X fmt=%u "
      "byteWidth=%u w=%u h=%u mips=%u array=%u sample=(%u,%u) rflags=0x%llX rflags_size=%u primary=%u init=%p "
      "num_alloc=%u alloc_info=%p primary_desc=%p",
      static_cast<unsigned>(pDesc->ResourceDimension),
      static_cast<unsigned>(pDesc->BindFlags),
      static_cast<unsigned>(usage),
      static_cast<unsigned>(cpu_access),
      static_cast<unsigned>(pDesc->MiscFlags),
      static_cast<unsigned>(pDesc->Format),
      static_cast<unsigned>(pDesc->ByteWidth),
      static_cast<unsigned>(pDesc->Width),
      static_cast<unsigned>(pDesc->Height),
      static_cast<unsigned>(pDesc->MipLevels),
      static_cast<unsigned>(pDesc->ArraySize),
      static_cast<unsigned>(sample_count),
      static_cast<unsigned>(sample_quality),
      static_cast<unsigned long long>(resource_flags_bits),
      static_cast<unsigned>(resource_flags_size),
      static_cast<unsigned>(primary),
      init_ptr,
      static_cast<unsigned>(num_allocations),
      allocation_info,
      primary_desc);
}
#endif  // AEROGPU_UMD_TRACE_RESOURCES

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr uint32_t kMaxConstantBufferSlots = 14;
constexpr uint32_t kMaxShaderResourceSlots = 128;
constexpr uint32_t kMaxSamplerSlots = 16;

constexpr uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

constexpr uint32_t AlignUpU32(uint32_t value, uint32_t alignment) {
  return static_cast<uint32_t>((value + alignment - 1) & ~(alignment - 1));
}

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
// When the D3D10 runtime opens such a resource, the OpenResource DDI does not
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
  const D3D10DDI_ADAPTERCALLBACKS* callbacks = nullptr;

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;
  // Optional kernel adapter handle opened via D3DKMTOpenAdapterFromHdc. Used for
  // D3DKMT thunk fallback paths (e.g. fence waits) and debug Escapes. Best-effort:
  // if this fails, WddmSubmit still prefers runtime callbacks and monitored fences.
  D3DKMT_HANDLE kmt_adapter = 0;

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

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
  static std::atomic<uint64_t> g_counter{1};
  static const uint64_t g_salt = splitmix64(fallback_entropy(0));

  for (;;) {
    const uint64_t ctr = g_counter.fetch_add(1, std::memory_order_relaxed);
    const uint64_t mixed = splitmix64(g_salt ^ fallback_entropy(ctr));
    const uint32_t low31 = static_cast<uint32_t>(mixed & 0x7FFFFFFFu);
    if (low31 != 0) {
      return static_cast<aerogpu_handle_t>(0x80000000u | low31);
    }
  }
}

static void log_global_handle_fallback_once() {
  static std::once_flag once;
  std::call_once(once, [] {
    OutputDebugStringA(
        "aerogpu-d3d10: GlobalHandleCounter mapping unavailable; using RNG fallback\n");
  });
}

static aerogpu_handle_t allocate_global_handle(AeroGpuAdapter* adapter) {
  if (!adapter) {
    return 0;
  }
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
    return static_cast<aerogpu_handle_t>(static_cast<uint64_t>(token) & 0xFFFFFFFFu);
  }

  log_global_handle_fallback_once();
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

static void DestroyKmtAdapterHandle(AeroGpuAdapter* adapter) {
  if (!adapter || adapter->kmt_adapter == 0) {
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

static void InitKmtAdapterHandle(AeroGpuAdapter* adapter) {
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

  adapter->kmt_adapter = open.hAdapter;
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

  const bool found = QueryUmdPrivateFromKmtAdapter(open.hAdapter, out);

  D3DKMT_CLOSEADAPTER close{};
  close.hAdapter = open.hAdapter;
  (void)procs.pfn_close_adapter(&close);

  return found;
}

static void InitUmdPrivate(AeroGpuAdapter* adapter) {
  if (!adapter || adapter->umd_private_valid) {
    return;
  }

  aerogpu_umd_private_v1 blob{};

  InitKmtAdapterHandle(adapter);

  if (adapter->kmt_adapter != 0) {
    if (QueryUmdPrivateFromKmtAdapter(adapter->kmt_adapter, &blob)) {
      adapter->umd_private = blob;
      adapter->umd_private_valid = true;
      return;
    }
  }

  if (!QueryUmdPrivateFromPrimaryDisplay(&blob)) {
    return;
  }

  adapter->umd_private = blob;
  adapter->umd_private_valid = true;
}

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible guest backing allocation ID. 0 means the resource is host-owned
  // and must be updated via `AEROGPU_CMD_UPLOAD_RESOURCE` payloads.
  uint32_t backing_alloc_id = 0;
  // Byte offset into the guest allocation described by `backing_alloc_id`.
  uint32_t backing_offset_bytes = 0;
  // WDDM allocation handle (D3DKMT_HANDLE in WDK headers) used by runtime
  // callbacks such as LockCb/UnlockCb.
  //
  // IMPORTANT: this is *not* the stable cross-layer `alloc_id` (see
  // `aerogpu_wddm_alloc.h`); it is only valid for the originating process'
   // runtime callbacks.
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
  uint32_t usage = 0;
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

  std::vector<uint8_t> storage;

  // Fence value of the most recent GPU submission that writes into this resource
  // (conservative). Used for staging readback Map(READ) synchronization so
  // Map(DO_NOT_WAIT) does not spuriously fail due to unrelated in-flight work.
  uint64_t last_gpu_write_fence = 0;

  // Map state (for resources backed by `storage`).
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
  // D3D10 point filtering is encoded as 0 for MIN_MAG_MIP_POINT; treat all other
  // filters as linear for MVP bring-up.
  return filter == 0 ? AEROGPU_SAMPLER_FILTER_NEAREST : AEROGPU_SAMPLER_FILTER_LINEAR;
}

static uint32_t aerogpu_sampler_address_from_d3d_mode(uint32_t mode) {
  // D3D10 numeric values: 1=WRAP, 2=MIRROR, 3=CLAMP.
  switch (mode) {
    case 1:
      return AEROGPU_SAMPLER_ADDRESS_REPEAT;
    case 2:
      return AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT;
    default:
      return AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  }
}

struct AeroGpuSampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_LINEAR;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
};

template <typename DescT>
static void InitSamplerFromDesc(AeroGpuSampler* sampler, const DescT& desc) {
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

struct AeroGpuDevice {
  uint32_t live_cookie = kAeroGpuDeviceLiveCookie;
  AeroGpuAdapter* adapter = nullptr;
  D3D10DDI_HRTDEVICE hrt_device = {};
  D3D10DDI_DEVICECALLBACKS callbacks = {};
  const D3DDDI_DEVICECALLBACKS* um_callbacks = nullptr;
  uint64_t last_submitted_fence = 0;
  uint64_t last_completed_fence = 0;
  D3DKMT_HANDLE hDevice = 0;
  D3DKMT_HANDLE hContext = 0;
  D3DKMT_HANDLE hSyncObject = 0;
  aerogpu::d3d10_11::WddmSubmit wddm_submit;

  std::mutex mutex;
  aerogpu::CmdWriter cmd;
  std::vector<uint32_t> wddm_submit_allocation_handles;
  std::vector<AeroGpuResource*> pending_staging_writes;

  // Cached state.
  aerogpu_handle_t current_rtv = 0;
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_vs_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_ps_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_vs_cb_resources[kMaxConstantBufferSlots] = {};
  AeroGpuResource* current_ps_cb_resources[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`).
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
std::uintptr_t D3dHandleToUintPtr(T value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<std::uintptr_t>(value);
  } else {
    return static_cast<std::uintptr_t>(value);
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

void DestroyKernelDeviceContext(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  dev->wddm_submit.Shutdown();
  dev->hSyncObject = 0;
  dev->hContext = 0;
  dev->hDevice = 0;
  dev->last_submitted_fence = 0;
  dev->last_completed_fence = 0;
}

HRESULT InitKernelDeviceContext(AeroGpuDevice* dev, D3D10DDI_HADAPTER hAdapter) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->hContext && dev->hSyncObject) {
    return S_OK;
  }

  const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
  if (!cb) {
    return S_OK;
  }

  const D3DKMT_HANDLE kmt_adapter = dev->adapter ? dev->adapter->kmt_adapter : 0;
  const HRESULT hr =
      dev->wddm_submit.Init(cb,
                            hAdapter.pDrvPrivate,
                            dev->hrt_device.pDrvPrivate,
                            kmt_adapter);
  if (FAILED(hr)) {
    DestroyKernelDeviceContext(dev);
    return hr;
  }

  dev->hDevice = dev->wddm_submit.hDevice();
  dev->hContext = dev->wddm_submit.hContext();
  dev->hSyncObject = dev->wddm_submit.hSyncObject();
  if (!dev->hDevice || !dev->hContext || !dev->hSyncObject) {
    DestroyKernelDeviceContext(dev);
    return E_FAIL;
  }

  return S_OK;
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

  dev->last_completed_fence = std::max(dev->last_completed_fence, dev->wddm_submit.QueryCompletedFence());
  if (dev->last_completed_fence >= fence) {
    return S_OK;
  }

  const HRESULT hr = dev->wddm_submit.WaitForFenceWithTimeout(fence, timeout_ms);
  if (SUCCEEDED(hr)) {
    dev->last_completed_fence = std::max(dev->last_completed_fence, fence);
  }
  dev->last_completed_fence = std::max(dev->last_completed_fence, dev->wddm_submit.QueryCompletedFence());
  return hr;
}

void SetError(D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->callbacks.pfnSetErrorCb) {
    return;
  }
  // Win7-era WDK headers disagree on whether pfnSetErrorCb takes HRTDEVICE or
  // HDEVICE. Prefer the HDEVICE form when that's what the signature expects.
  if constexpr (std::is_invocable_v<decltype(dev->callbacks.pfnSetErrorCb), D3D10DDI_HDEVICE, HRESULT>) {
    dev->callbacks.pfnSetErrorCb(hDevice, hr);
  } else {
    if (!dev->hrt_device.pDrvPrivate) {
      return;
    }
    CallCbMaybeHandle(dev->callbacks.pfnSetErrorCb, dev->hrt_device, hr);
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
  // host/device ABI, map sRGB DXGI formats to their UNORM equivalents so the
  // command stream stays compatible.
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

static void TrackStagingWriteLocked(AeroGpuDevice* dev, AeroGpuResource* dst) {
  if (!dev || !dst) {
    return;
  }

  // D3D10 staging readback resources are typically created with no bind flags.
  // Track writes so Map(READ)/Map(DO_NOT_WAIT) can wait on the fence that
  // actually produces the bytes, instead of waiting on the device's latest
  // fence (which can include unrelated work).
  if (dst->bind_flags != 0) {
    return;
  }

  // Prefer to only track CPU-readable staging resources, but fall back to
  // tracking all bindless resources if CPU access flags were not captured (WDK
  // struct layout differences).
  if (dst->cpu_access_flags != 0 &&
      (dst->cpu_access_flags & static_cast<uint32_t>(D3D10_CPU_ACCESS_READ)) == 0) {
    return;
  }

  dev->pending_staging_writes.push_back(dst);
}

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

static void TrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res);

static void EmitUploadLocked(D3D10DDI_HDEVICE hDevice,
                             AeroGpuDevice* dev,
                             AeroGpuResource* res,
                             uint64_t offset_bytes,
                             uint64_t size_bytes) {
  if (!dev || !res || res->handle == kInvalidHandle || size_bytes == 0) {
    return;
  }

  uint64_t upload_offset = offset_bytes;
  uint64_t upload_size = size_bytes;
  if (res->kind == ResourceKind::Buffer) {
    const uint64_t end = offset_bytes + size_bytes;
    if (end < offset_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    upload_offset = offset_bytes & ~3ull;
    const uint64_t upload_end = AlignUpU64(end, 4);
    upload_size = upload_end - upload_offset;
  }
  if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }

  const size_t off = static_cast<size_t>(upload_offset);
  const size_t sz = static_cast<size_t>(upload_size);
  if (off > res->storage.size() || sz > res->storage.size() - off) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  if (res->backing_alloc_id == 0) {
    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + off, sz);
    if (!cmd) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = upload_offset;
    cmd->size_bytes = upload_size;
    return;
  }

  const D3DDDI_DEVICECALLBACKS* ddi = dev->um_callbacks;
  if (!ddi || !ddi->pfnLockCb || !ddi->pfnUnlockCb || res->wddm_allocation_handle == 0) {
    SetError(hDevice, E_FAIL);
    return;
  }

  D3DDDICB_LOCK lock_args = {};
  lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
  __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock_args.SubresourceIndex = 0; }
  __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock_args.SubResourceIndex = 0; }
  InitLockForWrite(&lock_args);

  HRESULT hr = CallCbMaybeHandle(ddi->pfnLockCb, dev->hrt_device, &lock_args);
  if (FAILED(hr) || !lock_args.pData) {
    SetError(hDevice, FAILED(hr) ? hr : E_FAIL);
    return;
  }

  HRESULT copy_hr = S_OK;
  if (res->kind == ResourceKind::Texture2D && upload_offset == 0 && upload_size == res->storage.size()) {
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
  hr = CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
  if (FAILED(hr)) {
    SetError(hDevice, hr);
    return;
  }
  if (FAILED(copy_hr)) {
    SetError(hDevice, copy_hr);
    return;
  }

  TrackWddmAllocForSubmitLocked(dev, res);

  auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  dirty->resource_handle = res->handle;
  dirty->reserved0 = 0;
  dirty->offset_bytes = upload_offset;
  dirty->size_bytes = upload_size;
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

template <typename... Args>
struct NotImpl<SIZE_T(APIENTRY*)(Args...)> {
  static SIZE_T APIENTRY Fn(Args...) {
    // Returning 0 from a CalcPrivate*Size hook often causes the runtime to pass a
    // NULL pDrvPrivate, which can crash if the runtime still tries to call the
    // matching Create/Destroy DDI. Use a small non-zero placeholder so stubs are
    // always safe to call.
    return sizeof(uint64_t);
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
AEROGPU_DEFINE_HAS_MEMBER(pfnOpenResource)
AEROGPU_DEFINE_HAS_MEMBER(pfnSoSetTargets)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetPredication)
AEROGPU_DEFINE_HAS_MEMBER(pfnSetTextFilterSize)
AEROGPU_DEFINE_HAS_MEMBER(pfnGenMips)
AEROGPU_DEFINE_HAS_MEMBER(pfnGenerateMips)
AEROGPU_DEFINE_HAS_MEMBER(pfnResolveSubresource)
AEROGPU_DEFINE_HAS_MEMBER(pfnClearState)
AEROGPU_DEFINE_HAS_MEMBER(pfnBegin)
AEROGPU_DEFINE_HAS_MEMBER(pfnEnd)
AEROGPU_DEFINE_HAS_MEMBER(pfnReadFromSubresource)
AEROGPU_DEFINE_HAS_MEMBER(pfnWriteToSubresource)
AEROGPU_DEFINE_HAS_MEMBER(pfnStagingResourceMap)
AEROGPU_DEFINE_HAS_MEMBER(pfnStagingResourceUnmap)
AEROGPU_DEFINE_HAS_MEMBER(pfnDynamicIABufferMapDiscard)
AEROGPU_DEFINE_HAS_MEMBER(pfnDynamicIABufferMapNoOverwrite)
AEROGPU_DEFINE_HAS_MEMBER(pfnDynamicIABufferUnmap)
AEROGPU_DEFINE_HAS_MEMBER(pfnDynamicConstantBufferMapDiscard)
AEROGPU_DEFINE_HAS_MEMBER(pfnDynamicConstantBufferUnmap)
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
AEROGPU_DEFINE_HAS_MEMBER(CPUAccessFlags)
AEROGPU_DEFINE_HAS_MEMBER(CpuAccessFlags)
AEROGPU_DEFINE_HAS_MEMBER(Usage)

#undef AEROGPU_DEFINE_HAS_MEMBER

uint64_t submit_locked(AeroGpuDevice* dev, bool want_present, HRESULT* out_hr) {
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
  if (!dev->adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
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
    if (out_hr) {
      *out_hr = hr;
    }
    dev->pending_staging_writes.clear();
    return 0;
  }

  if (fence != 0) {
    dev->last_submitted_fence = std::max(dev->last_submitted_fence, fence);
    for (AeroGpuResource* res : dev->pending_staging_writes) {
      if (res) {
        res->last_gpu_write_fence = fence;
      }
    }
  }
  dev->pending_staging_writes.clear();
  AEROGPU_D3D10_11_LOG("D3D10 submit_locked: present=%u bytes=%llu fence=%llu completed=%llu",
                       want_present ? 1u : 0u,
                       static_cast<unsigned long long>(submit_bytes),
                       static_cast<unsigned long long>(fence),
                       static_cast<unsigned long long>(dev->wddm_submit.QueryCompletedFence()));
  return fence;
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
  for (AeroGpuResource* res : dev->current_vs_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (AeroGpuResource* res : dev->current_ps_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (AeroGpuResource* res : dev->current_vs_srv_resources) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
  for (AeroGpuResource* res : dev->current_ps_srv_resources) {
    TrackWddmAllocForSubmitLocked(dev, res);
  }
}

static void SetTextureLocked(AeroGpuDevice* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  if (!dev) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  cmd->shader_stage = shader_stage;
  cmd->slot = slot;
  cmd->texture = texture;
  cmd->reserved0 = 0;
}

static aerogpu_handle_t* ShaderResourceTableForStage(AeroGpuDevice* dev, uint32_t shader_stage) {
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

static aerogpu_handle_t* SamplerTableForStage(AeroGpuDevice* dev, uint32_t shader_stage) {
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

static aerogpu_constant_buffer_binding* ConstantBufferTableForStage(AeroGpuDevice* dev, uint32_t shader_stage) {
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

static void SetShaderResourceSlotLocked(AeroGpuDevice* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
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
  table[slot] = texture;
  SetTextureLocked(dev, shader_stage, slot, texture);
}

static void UnbindResourceFromSrvsLocked(AeroGpuDevice* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (dev->vs_srvs[slot] == resource) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
      if (dev->vs_srvs[slot] == 0) {
        dev->current_vs_srv_resources[slot] = nullptr;
      }
    }
    if (dev->ps_srvs[slot] == resource) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
      if (dev->ps_srvs[slot] == 0) {
        dev->current_ps_srv_resources[slot] = nullptr;
      }
    }
  }
}

static void EmitSetRenderTargetsLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
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

static void UnbindResourceFromOutputsLocked(AeroGpuDevice* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  bool changed = false;
  if (dev->current_rtv == resource) {
    dev->current_rtv = 0;
    dev->current_rtv_res = nullptr;
    changed = true;
  }
  if (dev->current_dsv == resource) {
    dev->current_dsv = 0;
    dev->current_dsv_res = nullptr;
    changed = true;
  }
  if (changed) {
    EmitSetRenderTargetsLocked(dev);
  }
}


// -----------------------------------------------------------------------------
// Device DDI (core bring-up set)
// -----------------------------------------------------------------------------

void APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
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

SIZE_T APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                const D3D10DDIARG_CREATERESOURCE* pDesc,
                                D3D10DDI_HRESOURCE hResource,
                                D3D10DDI_HRTRESOURCE hRTResource) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  TraceCreateResourceDesc(pDesc);
#endif

  if (!dev->hrt_device.pDrvPrivate || !dev->callbacks.pfnAllocateCb || !dev->callbacks.pfnDeallocateCb) {
    SetError(hDevice, E_FAIL);
    return E_FAIL;
  }

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = allocate_global_handle(dev->adapter);
  res->bind_flags = pDesc->BindFlags;
  res->misc_flags = pDesc->MiscFlags;
  if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
    res->usage = static_cast<uint32_t>(pDesc->Usage);
  }
  if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
    res->cpu_access_flags |= static_cast<uint32_t>(pDesc->CPUAccessFlags);
  }
  if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
    res->cpu_access_flags |= static_cast<uint32_t>(pDesc->CpuAccessFlags);
  }

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
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
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
    CallCbMaybeHandle(dev->callbacks.pfnDeallocateCb, dev->hrt_device, &dealloc);
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
      alloc_id = static_cast<uint32_t>(allocate_global_handle(dev->adapter)) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
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
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      if (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING)) {
        priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING;
      }
    }

    // The Win7 KMD owns share_token generation; provide 0 as a placeholder.
    priv.share_token = 0;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(size_bytes);
    priv.reserved0 = static_cast<aerogpu_wddm_u64>(pitch_bytes);
    priv.kind = (res->kind == ResourceKind::Buffer) ? AEROGPU_WDDM_ALLOC_KIND_BUFFER
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
      alloc.hContext = UintPtrToD3dHandle<decltype(alloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
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

    const HRESULT hr = CallCbMaybeHandle(dev->callbacks.pfnAllocateCb, dev->hrt_device, &alloc);
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
            AEROGPU_D3D10_11_LOG("D3D10 CreateResource: shared allocation missing/invalid private driver data");
          });
        } else {
          static std::once_flag log_once;
          std::call_once(log_once, [] {
            AEROGPU_D3D10_11_LOG("D3D10 CreateResource: shared allocation missing share_token in returned private data");
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
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
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
      (void)CallCbMaybeHandle(dev->callbacks.pfnDeallocateCb, dev->hrt_device, &dealloc);
      return E_FAIL;
    }

    if (is_shared && !share_token_ok) {
      // If the KMD does not return a stable token, shared surface interop cannot
      // work across processes; fail cleanly. Free the allocation handles that
      // were created by AllocateCb before returning an error.
      D3DDDICB_DEALLOCATE dealloc = {};
      D3DKMT_HANDLE h = static_cast<D3DKMT_HANDLE>(km_alloc);
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
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
      (void)CallCbMaybeHandle(dev->callbacks.pfnDeallocateCb, dev->hrt_device, &dealloc);
      return E_FAIL;
    }

    res->backing_alloc_id = alloc_id;
    res->backing_offset_bytes = 0;
    res->wddm.km_resource_handle = km_resource;
    res->share_token = is_shared ? share_token : 0;
    res->is_shared = is_shared;
    res->is_shared_alias = false;
    uint32_t runtime_alloc = 0;
    __if_exists(AllocationInfoT::hAllocation) {
      runtime_alloc = static_cast<uint32_t>(alloc_info[0].hAllocation);
    }
    // Prefer the runtime allocation handle (`hAllocation`) for LockCb/UnlockCb,
    // but fall back to the only handle we have if the WDK revision does not
    // expose it.
    res->wddm_allocation_handle = runtime_alloc ? runtime_alloc : static_cast<uint32_t>(km_alloc);
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_allocation_handles.push_back(km_alloc);
    return S_OK;
  };

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);
  if (dim == 1u /* buffer */) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pDesc->ByteWidth;
    const uint64_t padded_size_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    const uint64_t alloc_size = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 256);
    bool cpu_visible = false;
    if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    bool is_staging = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      is_staging = (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING));
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
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
    #ifdef D3D10_USAGE_DYNAMIC
      want_host_owned = (usage == static_cast<uint32_t>(D3D10_USAGE_DYNAMIC));
    #else
      want_host_owned = (usage == 2u);
    #endif
    }
    want_host_owned = want_host_owned && !is_shared;

    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      res->~AeroGpuResource();
      return hr;
    }

    if (want_host_owned) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    }

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
      std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
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
      return init_hr;
    }

    TrackWddmAllocForSubmitLocked(dev, res);

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

    if (!res->storage.empty()) {
      EmitUploadLocked(hDevice, dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(hDevice, E_FAIL);
        deallocate_if_needed();
        res->~AeroGpuResource();
        return E_FAIL;
      }

      // Shared resources must be importable cross-process as soon as CreateResource
      // returns. Since AeroGPU resource creation is expressed via the command
      // stream, export the resource and force a submission so the host observes
      // the share_token mapping immediately (mirrors D3D9Ex behavior).
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;

      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        SetError(hDevice, submit_hr);
        deallocate_if_needed();
        res->~AeroGpuResource();
        return submit_hr;
      }
    }
    return S_OK;
  }

  if (dim == 3u /* texture2d */) {
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize ? pDesc->ArraySize : 1;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);

    if (res->mip_levels != 1 || res->array_size != 1) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    res->row_pitch_bytes = AlignUpU32(row_bytes, 256);

    const uint64_t total_bytes = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
    bool cpu_visible = false;
    if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    bool is_staging = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      is_staging = (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING));
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
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
    #ifdef D3D10_USAGE_DYNAMIC
      want_host_owned = (usage == static_cast<uint32_t>(D3D10_USAGE_DYNAMIC));
    #else
      want_host_owned = (usage == 2u);
    #endif
    }
    want_host_owned = want_host_owned && !is_shared;
    HRESULT hr = allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared, is_primary, res->row_pitch_bytes);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      res->~AeroGpuResource();
      return hr;
    }

    if (want_host_owned) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    }

    auto copy_initial_data = [&](auto init_data) -> HRESULT {
      if (!init_data) {
        return S_OK;
      }
      const auto& init = init_data[0];
      if (!init.pSysMem) {
        return E_INVALIDARG;
      }

      if (total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(total_bytes));
      } catch (...) {
        return E_OUTOFMEMORY;
      }

      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch)
                                                : static_cast<size_t>(row_bytes);
      for (uint32_t y = 0; y < rows; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    row_bytes);
        if (res->row_pitch_bytes > row_bytes) {
          std::memset(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes + row_bytes,
                      0,
                      res->row_pitch_bytes - row_bytes);
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
      return init_hr;
    }

    TrackWddmAllocForSubmitLocked(dev, res);

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

    if (!res->storage.empty()) {
      EmitUploadLocked(hDevice, dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(hDevice, E_FAIL);
        deallocate_if_needed();
        res->~AeroGpuResource();
        return E_FAIL;
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      export_cmd->resource_handle = res->handle;
      export_cmd->reserved0 = 0;
      export_cmd->share_token = res->share_token;

      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        SetError(hDevice, submit_hr);
        deallocate_if_needed();
        res->~AeroGpuResource();
        return submit_hr;
      }
    }
    return S_OK;
  }

  deallocate_if_needed();
  res->~AeroGpuResource();
  return E_NOTIMPL;
}

HRESULT APIENTRY OpenResource(D3D10DDI_HDEVICE hDevice,
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
  res->handle = allocate_global_handle(dev->adapter);
  res->backing_alloc_id = static_cast<uint32_t>(priv.alloc_id);
  res->backing_offset_bytes = 0;
  res->wddm_allocation_handle = 0;
  res->share_token = static_cast<uint64_t>(priv.share_token);
  res->is_shared = true;
  res->is_shared_alias = true;

  // Capture the resource metadata that the runtime provides for the opened
  // resource. Some code paths (e.g. Map(READ) implicit sync heuristics) rely on
  // bind/usage flags to distinguish staging readback resources from GPU-only
  // textures.
  __if_exists(D3D10DDIARG_OPENRESOURCE::BindFlags) {
    res->bind_flags = pOpenResource->BindFlags;
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::MiscFlags) {
    res->misc_flags = pOpenResource->MiscFlags;
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::Usage) {
    res->usage = static_cast<uint32_t>(pOpenResource->Usage);
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::CPUAccessFlags) {
    res->cpu_access_flags |= static_cast<uint32_t>(pOpenResource->CPUAccessFlags);
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::CpuAccessFlags) {
    res->cpu_access_flags |= static_cast<uint32_t>(pOpenResource->CpuAccessFlags);
  }

  // Recover the runtime allocation handle (`hAllocation`) for LockCb/UnlockCb
  // and the KM handles needed for pfnDeallocateCb. Field availability varies
  // across WDK vintages, so treat all as optional.
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

  // Set the resource description from the preserved private data blob (v2).
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
  if (!dev->pending_staging_writes.empty()) {
    dev->pending_staging_writes.erase(
        std::remove(dev->pending_staging_writes.begin(), dev->pending_staging_writes.end(), res),
        dev->pending_staging_writes.end());
  }

  if (res->mapped) {
    if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
      const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
      if (cb && cb->pfnUnlockCb) {
        D3DDDICB_UNLOCK unlock_cb = {};
        unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(res->mapped_wddm_allocation);
        __if_exists(D3DDDICB_UNLOCK::SubresourceIndex) {
          unlock_cb.SubresourceIndex = res->mapped_subresource;
        }
        __if_exists(D3DDDICB_UNLOCK::SubResourceIndex) {
          unlock_cb.SubResourceIndex = res->mapped_subresource;
        }
        (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
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

  if (res->handle != kInvalidHandle) {
    UnbindResourceFromOutputsLocked(dev, res->handle);
    UnbindResourceFromSrvsLocked(dev, res->handle);
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

  for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
    if (dev->current_vs_cb_resources[slot] == res) {
      dev->current_vs_cb_resources[slot] = nullptr;
      dev->vs_constant_buffers[slot] = {};
    }
    if (dev->current_ps_cb_resources[slot] == res) {
      dev->current_ps_cb_resources[slot] = nullptr;
      dev->ps_constant_buffers[slot] = {};
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
      SetError(hDevice, submit_hr);
    }
  }

  if (res->wddm.km_resource_handle != 0 || !res->wddm.km_allocation_handles.empty()) {
    std::vector<D3DKMT_HANDLE> km_allocs;
    km_allocs.reserve(res->wddm.km_allocation_handles.size());
    for (uint64_t h : res->wddm.km_allocation_handles) {
      km_allocs.push_back(static_cast<D3DKMT_HANDLE>(h));
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
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
    const HRESULT hr = CallCbMaybeHandle(dev->callbacks.pfnDeallocateCb, dev->hrt_device, &dealloc);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
    }
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  }

  res->~AeroGpuResource();
}

// D3D10_DDI_MAP subset (numeric values from d3d10umddi.h / d3d10.h).
constexpr uint32_t kD3DMapRead = 1;
constexpr uint32_t kD3DMapWrite = 2;
constexpr uint32_t kD3DMapReadWrite = 3;
constexpr uint32_t kD3DMapWriteDiscard = 4;
constexpr uint32_t kD3DMapWriteNoOverwrite = 5;

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

  if (res->mapped) {
    return E_FAIL;
  }

  uint32_t subresource = 0;
  __if_exists(D3D10DDIARG_MAP::Subresource) {
    subresource = static_cast<uint32_t>(pMap->Subresource);
  }
  if (subresource != 0) {
    return E_NOTIMPL;
  }

  uint32_t map_type_u = kD3DMapWrite;
  __if_exists(D3D10DDIARG_MAP::MapType) {
    map_type_u = static_cast<uint32_t>(pMap->MapType);
  }

  uint32_t map_flags_u = 0;
  __if_exists(D3D10DDIARG_MAP::MapFlags) {
    map_flags_u = static_cast<uint32_t>(pMap->MapFlags);
  }
  __if_not_exists(D3D10DDIARG_MAP::MapFlags) {
    __if_exists(D3D10DDIARG_MAP::Flags) {
      map_flags_u = static_cast<uint32_t>(pMap->Flags);
    }
  }

  bool want_write = false;
  switch (map_type_u) {
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

  const bool want_read = (map_type_u == kD3DMapRead || map_type_u == kD3DMapReadWrite);
  // Only apply implicit synchronization for staging-style resources. For D3D10
  // this maps to resources with no bind flags (typical staging readback).
  if (want_read && res->bind_flags == 0) {
    if (!dev->cmd.empty()) {
      HRESULT submit_hr = S_OK;
      submit_locked(dev, /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        return submit_hr;
      }
    }
    const uint64_t fence = res->last_gpu_write_fence;
    if (fence != 0) {
      const uint32_t timeout_ms = (map_flags_u & kD3DMapFlagDoNotWait) ? 0u : kAeroGpuTimeoutMsInfinite;
      const HRESULT wait = AeroGpuWaitForFence(dev, fence, timeout_ms);
      if (FAILED(wait)) {
        return wait;
      }
    }
  }

  uint64_t size = 0;
  uint64_t storage_size = 0;
  if (res->kind == ResourceKind::Buffer) {
    size = res->size_bytes;
    storage_size = AlignUpU64(size, 4);
  } else if (res->kind == ResourceKind::Texture2D) {
    size = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    storage_size = size;
  }
  if (!size) {
    return E_INVALIDARG;
  }
  if (storage_size > static_cast<uint64_t>(SIZE_MAX)) {
    return E_OUTOFMEMORY;
  }

  try {
    if (map_type_u == kD3DMapWriteDiscard) {
      // Approximate DISCARD renaming by allocating a fresh CPU backing store.
      res->storage.assign(static_cast<size_t>(storage_size), 0);
    } else if (res->storage.size() < static_cast<size_t>(storage_size)) {
      res->storage.resize(static_cast<size_t>(storage_size), 0);
    }
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  const bool allow_storage_map = (res->backing_alloc_id == 0) && !(want_read && res->bind_flags == 0);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_offset = 0;
    res->mapped_size = size;
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    pMap->pData = res->storage.empty() ? nullptr : res->storage.data();
    if (res->kind == ResourceKind::Texture2D) {
      pMap->RowPitch = res->row_pitch_bytes;
      pMap->DepthPitch = res->row_pitch_bytes * res->height;
    } else {
      pMap->RowPitch = 0;
      pMap->DepthPitch = 0;
    }
    return S_OK;
  };

  const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
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
  InitLockArgsForMap(&lock_cb, subresource, map_type_u, map_flags_u);

  const bool do_not_wait = (map_flags_u & kD3DMapFlagDoNotWait) != 0;
  HRESULT hr = CallCbMaybeHandle(cb->pfnLockCb, dev->hrt_device, &lock_cb);
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
    InitUnlockArgsForMap(&unlock_cb, subresource);
    (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
    if (allow_storage_map) {
      return map_storage();
    }
    return E_FAIL;
  }

  res->mapped_wddm_ptr = lock_cb.pData;
  res->mapped_wddm_allocation = alloc_handle;
  __if_exists(D3DDDICB_LOCK::Pitch) {
    res->mapped_wddm_pitch = lock_cb.Pitch;
  }
  __if_exists(D3DDDICB_LOCK::SlicePitch) {
    res->mapped_wddm_slice_pitch = lock_cb.SlicePitch;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  if (!res->storage.empty()) {
    if (map_type_u == kD3DMapWriteDiscard) {
      // Discard contents are undefined; clear for deterministic tests.
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
        const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
        const uint32_t pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
        const uint64_t bytes = static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows);
        if (pitch != 0 && bytes <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(lock_cb.pData, 0, static_cast<size_t>(bytes));
        }
      } else {
        std::memset(lock_cb.pData, 0, res->storage.size());
      }
    } else if (!is_guest_backed && res->kind == ResourceKind::Texture2D) {
      const uint32_t src_pitch = res->row_pitch_bytes;
      const uint32_t dst_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : src_pitch;

      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
      if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 && src_pitch >= row_bytes && dst_pitch >= row_bytes) {
        auto* dst_bytes = static_cast<uint8_t*>(lock_cb.pData);
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
        std::memcpy(lock_cb.pData, res->storage.data(), res->storage.size());
      }
    } else if (!is_guest_backed) {
      std::memcpy(lock_cb.pData, res->storage.data(), res->storage.size());
    } else if (want_read && res->kind == ResourceKind::Texture2D) {
      const uint32_t dst_pitch = res->row_pitch_bytes;
      const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : dst_pitch;

      const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
      if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 && src_pitch >= row_bytes && dst_pitch >= row_bytes) {
        const uint8_t* src_bytes = static_cast<const uint8_t*>(lock_cb.pData);
        auto* dst_bytes = res->storage.data();
        for (uint32_t y = 0; y < rows; y++) {
          std::memcpy(dst_bytes + static_cast<size_t>(y) * dst_pitch,
                      src_bytes + static_cast<size_t>(y) * src_pitch,
                      row_bytes);
          if (dst_pitch > row_bytes) {
            std::memset(dst_bytes + static_cast<size_t>(y) * dst_pitch + row_bytes, 0, dst_pitch - row_bytes);
          }
        }
      } else {
        std::memcpy(res->storage.data(), lock_cb.pData, res->storage.size());
      }
    } else if (want_read) {
      std::memcpy(res->storage.data(), lock_cb.pData, res->storage.size());
    }
  }

  pMap->pData = lock_cb.pData;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
    pMap->RowPitch = pitch;
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    pMap->DepthPitch = res->mapped_wddm_slice_pitch ? res->mapped_wddm_slice_pitch
                                                    : static_cast<uint32_t>(static_cast<uint64_t>(pitch) *
                                                                             static_cast<uint64_t>(rows));
  } else {
    pMap->RowPitch = 0;
    pMap->DepthPitch = 0;
  }

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_offset = 0;
  res->mapped_size = size;
  return S_OK;
}

void unmap_resource_locked(D3D10DDI_HDEVICE hDevice, AeroGpuDevice* dev, AeroGpuResource* res, uint32_t subresource) {
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (res->mapped_write && !res->storage.empty() && res->mapped_size) {
      const uint8_t* src = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
      const size_t off = static_cast<size_t>(res->mapped_offset);
      const size_t bytes = static_cast<size_t>(res->mapped_size);
      const bool range_ok = (off <= res->storage.size()) && (bytes <= (res->storage.size() - off));
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
        const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
        const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
        const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : res->row_pitch_bytes;
        const uint32_t dst_pitch = res->row_pitch_bytes;
        if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 && src_pitch >= row_bytes && dst_pitch >= row_bytes) {
          for (uint32_t y = 0; y < rows; y++) {
            uint8_t* dst_row = res->storage.data() + static_cast<size_t>(y) * dst_pitch;
            const uint8_t* src_row = src + static_cast<size_t>(y) * src_pitch;
            std::memcpy(dst_row, src_row, row_bytes);
            if (dst_pitch > row_bytes) {
              std::memset(dst_row + row_bytes, 0, dst_pitch - row_bytes);
            }
          }
        } else if (range_ok) {
          std::memcpy(res->storage.data() + off, src + off, bytes);
        }
      } else if (range_ok) {
        std::memcpy(res->storage.data() + off, src + off, bytes);
      }
    }

    const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
    if (cb && cb->pfnUnlockCb) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation =
          UintPtrToD3dHandle<decltype(unlock_cb.hAllocation)>(static_cast<std::uintptr_t>(res->mapped_wddm_allocation));
      InitUnlockArgsForMap(&unlock_cb, subresource);
      const HRESULT unlock_hr = CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (FAILED(unlock_hr)) {
        SetError(hDevice, unlock_hr);
      }
    }
  }

  if (res->mapped_write && res->mapped_size != 0) {
    uint64_t upload_offset = res->mapped_offset;
    uint64_t upload_size = res->mapped_size;
    if (res->kind == ResourceKind::Buffer) {
      const uint64_t end = res->mapped_offset + res->mapped_size;
      if (end < res->mapped_offset) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      upload_offset = res->mapped_offset & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      upload_size = upload_end - upload_offset;
    }

    if (!res->storage.empty()) {
      if (upload_offset > static_cast<uint64_t>(res->storage.size())) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const size_t remaining = res->storage.size() - static_cast<size_t>(upload_offset);
      if (upload_size > static_cast<uint64_t>(remaining)) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      if (upload_size > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }

    if (res->backing_alloc_id != 0) {
      TrackWddmAllocForSubmitLocked(dev, res);
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        SetError(hDevice, E_FAIL);
        return;
      }
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = upload_offset;
      cmd->size_bytes = upload_size;
    } else {
      EmitUploadLocked(hDevice, dev, res, upload_offset, upload_size);
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

  uint32_t subresource = 0;
  __if_exists(D3D10DDIARG_UNMAP::Subresource) {
    subresource = static_cast<uint32_t>(pUnmap->Subresource);
  }

  if (!res->mapped) {
    SetError(hDevice, E_FAIL);
    return;
  }
  if (subresource != res->mapped_subresource) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  unmap_resource_locked(hDevice, dev, res, subresource);
}

// -------------------------------------------------------------------------------------------------
// Optional Win7 D3D10 entrypoints for staging and dynamic maps.
//
// Some WDK/runtime combinations route certain Map/Unmap calls through these
// specialized hooks rather than `pfnMap`. Implement them as thin wrappers so the
// D3D10 runtime never observes E_NOTIMPL for common map patterns.
// -------------------------------------------------------------------------------------------------

template <typename = void>
HRESULT APIENTRY StagingResourceMap(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HRESOURCE hResource,
                                    UINT subresource,
                                    D3D10_DDI_MAP map_type,
                                    UINT map_flags,
                                    D3D10DDI_MAPPED_SUBRESOURCE* pMapped) {
  if (!pMapped) {
    return E_INVALIDARG;
  }
  pMapped->pData = nullptr;
  pMapped->RowPitch = 0;
  pMapped->DepthPitch = 0;
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  D3D10DDIARG_MAP map{};
  map.hResource = hResource;
  __if_exists(D3D10DDIARG_MAP::Subresource) {
    map.Subresource = subresource;
  }
  __if_exists(D3D10DDIARG_MAP::MapType) {
    map.MapType = map_type;
  }
  __if_exists(D3D10DDIARG_MAP::MapFlags) {
    map.MapFlags = static_cast<decltype(map.MapFlags)>(map_flags);
  }
  __if_not_exists(D3D10DDIARG_MAP::MapFlags) {
    __if_exists(D3D10DDIARG_MAP::Flags) {
      map.Flags = static_cast<decltype(map.Flags)>(map_flags);
    }
  }

  const HRESULT hr = Map(hDevice, &map);
  if (FAILED(hr)) {
    return hr;
  }

  pMapped->pData = map.pData;
  pMapped->RowPitch = map.RowPitch;
  pMapped->DepthPitch = map.DepthPitch;
  return S_OK;
}

template <typename = void>
void APIENTRY StagingResourceUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, UINT subresource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || static_cast<uint32_t>(subresource) != res->mapped_subresource) {
    return;
  }
  unmap_resource_locked(hDevice, dev, res, static_cast<uint32_t>(subresource));
}

template <typename = void>
HRESULT APIENTRY DynamicIABufferMapDiscard(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, void** ppData) {
  if (!ppData) {
    return E_INVALIDARG;
  }
  *ppData = nullptr;
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer ||
      (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  D3D10DDIARG_MAP map{};
  map.hResource = hResource;
  __if_exists(D3D10DDIARG_MAP::MapType) {
    map.MapType = static_cast<D3D10_DDI_MAP>(kD3DMapWriteDiscard);
  }
  const HRESULT hr = Map(hDevice, &map);
  if (FAILED(hr)) {
    return hr;
  }
  *ppData = map.pData;
  return S_OK;
}

template <typename = void>
HRESULT APIENTRY DynamicIABufferMapNoOverwrite(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, void** ppData) {
  if (!ppData) {
    return E_INVALIDARG;
  }
  *ppData = nullptr;
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer ||
      (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) == 0) {
    return E_INVALIDARG;
  }

  D3D10DDIARG_MAP map{};
  map.hResource = hResource;
  __if_exists(D3D10DDIARG_MAP::MapType) {
    map.MapType = static_cast<D3D10_DDI_MAP>(kD3DMapWriteNoOverwrite);
  }
  const HRESULT hr = Map(hDevice, &map);
  if (FAILED(hr)) {
    return hr;
  }
  *ppData = map.pData;
  return S_OK;
}

template <typename = void>
void APIENTRY DynamicIABufferUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    return;
  }
  unmap_resource_locked(hDevice, dev, res, /*subresource=*/0);
}

template <typename = void>
HRESULT APIENTRY DynamicConstantBufferMapDiscard(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource, void** ppData) {
  if (!ppData) {
    return E_INVALIDARG;
  }
  *ppData = nullptr;
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Buffer || (res->bind_flags & kD3D10BindConstantBuffer) == 0) {
    return E_INVALIDARG;
  }

  D3D10DDIARG_MAP map{};
  map.hResource = hResource;
  __if_exists(D3D10DDIARG_MAP::MapType) {
    map.MapType = static_cast<D3D10_DDI_MAP>(kD3DMapWriteDiscard);
  }
  const HRESULT hr = Map(hDevice, &map);
  if (FAILED(hr)) {
    return hr;
  }
  *ppData = map.pData;
  return S_OK;
}

template <typename = void>
void APIENTRY DynamicConstantBufferUnmap(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    return;
  }
  unmap_resource_locked(hDevice, dev, res, /*subresource=*/0);
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
    if (pUpdate->DstSubresource != 0) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    uint64_t dst_off = 0;
    uint64_t bytes = res->size_bytes;
    if (pUpdate->pDstBox) {
      const auto* box = pUpdate->pDstBox;
      if (box->right < box->left || box->top != 0 || box->bottom != 1 || box->front != 0 || box->back != 1) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      dst_off = static_cast<uint64_t>(box->left);
      bytes = static_cast<uint64_t>(box->right - box->left);
    }

    if (dst_off > res->size_bytes || bytes > res->size_bytes - dst_off) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    if (bytes > static_cast<uint64_t>(SIZE_MAX)) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }

    const uint64_t storage_needed_u64 = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    if (res->storage.size() < static_cast<size_t>(storage_needed_u64)) {
      if (storage_needed_u64 > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
      try {
        res->storage.resize(static_cast<size_t>(storage_needed_u64), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }
    if (bytes) {
      std::memcpy(res->storage.data() + static_cast<size_t>(dst_off), pUpdate->pSysMemUP, static_cast<size_t>(bytes));
    }
  } else if (res->kind == ResourceKind::Texture2D) {
    if (pUpdate->DstSubresource != 0 || pUpdate->pDstBox) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    if (res->mip_levels != 1 || res->array_size != 1) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    const uint32_t row_pitch = res->row_pitch_bytes ? res->row_pitch_bytes : min_row_bytes;
    const uint64_t total = static_cast<uint64_t>(row_pitch) * static_cast<uint64_t>(rows);
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
    const size_t src_pitch =
        pUpdate->RowPitch ? static_cast<size_t>(pUpdate->RowPitch) : static_cast<size_t>(min_row_bytes);
    if (min_row_bytes == 0 || rows == 0 || row_pitch < min_row_bytes || src_pitch < min_row_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    for (uint32_t y = 0; y < rows; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * row_pitch,
                  src + static_cast<size_t>(y) * src_pitch,
                  min_row_bytes);
      if (row_pitch > min_row_bytes) {
        std::memset(res->storage.data() + static_cast<size_t>(y) * row_pitch + min_row_bytes,
                    0,
                    row_pitch - min_row_bytes);
      }
    }
  }

  if (res->kind == ResourceKind::Buffer) {
    uint64_t dst_off = 0;
    uint64_t bytes = res->size_bytes;
    if (pUpdate->pDstBox) {
      const auto* box = pUpdate->pDstBox;
      dst_off = static_cast<uint64_t>(box->left);
      bytes = static_cast<uint64_t>(box->right - box->left);
    }

    if (bytes) {
      const uint64_t end = dst_off + bytes;
      if (end < dst_off) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const uint64_t upload_offset = dst_off & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      const uint64_t upload_size = upload_end - upload_offset;
      if (upload_offset > res->storage.size()) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const size_t remaining = res->storage.size() - static_cast<size_t>(upload_offset);
      if (upload_size > remaining) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      EmitUploadLocked(hDevice, dev, res, upload_offset, upload_size);
    }
  } else if (res->kind == ResourceKind::Texture2D) {
    if (!res->storage.empty()) {
      EmitUploadLocked(hDevice, dev, res, 0, res->storage.size());
    }
  }
}

void APIENTRY CopySubresourceRegion(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HRESOURCE hDst,
                                    UINT dst_subresource,
                                    UINT dstX,
                                    UINT dstY,
                                    UINT dstZ,
                                    D3D10DDI_HRESOURCE hSrc,
                                    UINT src_subresource,
                                    const D3D10_DDI_BOX* pSrcBox);

void APIENTRY CopyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hDst, D3D10DDI_HRESOURCE hSrc) {
  CopySubresourceRegion(hDevice, hDst, 0, 0, 0, 0, hSrc, 0, nullptr);
}

void APIENTRY CopySubresourceRegion(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HRESOURCE hDst,
                                    UINT dst_subresource,
                                    UINT dstX,
                                    UINT dstY,
                                    UINT dstZ,
                                    D3D10DDI_HRESOURCE hSrc,
                                    UINT src_subresource,
                                    const D3D10_DDI_BOX* pSrcBox) {
  if (!hDevice.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  if (dst_subresource != 0 || src_subresource != 0) {
    SetError(hDevice, E_NOTIMPL);
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

  if (dst->kind != src->kind) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  if (dst->kind == ResourceKind::Buffer) {
    if (dstY != 0 || dstZ != 0) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    const uint64_t dst_off = static_cast<uint64_t>(dstX);
    const uint64_t src_left = pSrcBox ? static_cast<uint64_t>(pSrcBox->left) : 0;
    const uint64_t src_right = pSrcBox ? static_cast<uint64_t>(pSrcBox->right) : src->size_bytes;

    if (src_right < src_left) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint64_t requested = src_right - src_left;
    const uint64_t max_src = (src_left < src->size_bytes) ? (src->size_bytes - src_left) : 0;
    const uint64_t max_dst = (dst_off < dst->size_bytes) ? (dst->size_bytes - dst_off) : 0;
    const uint64_t bytes = std::min(std::min(requested, max_src), max_dst);

    const uint64_t dst_storage_u64 = AlignUpU64(dst->size_bytes ? dst->size_bytes : 1, 4);
    if (dst_storage_u64 <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t dst_size = static_cast<size_t>(dst_storage_u64);
      if (dst->storage.size() < dst_size) {
        try {
          dst->storage.resize(dst_size, 0);
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
          return;
        }
      }
    }
    const uint64_t src_storage_u64 = AlignUpU64(src->size_bytes ? src->size_bytes : 1, 4);
    if (src_storage_u64 <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t src_size = static_cast<size_t>(src_storage_u64);
      if (src->storage.size() < src_size) {
        try {
          src->storage.resize(src_size, 0);
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
          return;
        }
      }
    }

    if (bytes && dst_off + bytes <= dst->storage.size() && src_left + bytes <= src->storage.size()) {
      std::memmove(dst->storage.data() + static_cast<size_t>(dst_off),
                   src->storage.data() + static_cast<size_t>(src_left),
                   static_cast<size_t>(bytes));
    }

    if (bytes) {
      const uint64_t end = dst_off + bytes;
      if (end < dst_off) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const uint64_t upload_offset = dst_off & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      const uint64_t upload_size = upload_end - upload_offset;
      if (upload_size > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
      if (upload_offset > static_cast<uint64_t>(dst->storage.size())) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const size_t remaining = dst->storage.size() - static_cast<size_t>(upload_offset);
      if (upload_size > static_cast<uint64_t>(remaining)) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }

      const uint8_t* payload = dst->storage.data() + static_cast<size_t>(upload_offset);
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, payload, static_cast<size_t>(upload_size));
      if (!upload) {
        SetError(hDevice, E_FAIL);
        return;
      }
      upload->resource_handle = dst->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = upload_offset;
      upload->size_bytes = upload_size;
    }

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
    if (dst->backing_alloc_id != 0 &&
        dst->usage == static_cast<uint32_t>(D3D10_USAGE_STAGING) &&
        (dst->cpu_access_flags & static_cast<uint32_t>(D3D10_CPU_ACCESS_READ)) != 0) {
      copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
    }
    cmd->flags = copy_flags;
    cmd->reserved0 = 0;
    TrackStagingWriteLocked(dev, dst);
    return;
  }

  if (dst->kind == ResourceKind::Texture2D) {
    if (dstZ != 0) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    if (dst->dxgi_format != src->dxgi_format) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !SupportsBcFormats(dev)) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    const uint32_t src_left = pSrcBox ? static_cast<uint32_t>(pSrcBox->left) : 0;
    const uint32_t src_top = pSrcBox ? static_cast<uint32_t>(pSrcBox->top) : 0;
    const uint32_t src_right = pSrcBox ? static_cast<uint32_t>(pSrcBox->right) : src->width;
    const uint32_t src_bottom = pSrcBox ? static_cast<uint32_t>(pSrcBox->bottom) : src->height;

    if (pSrcBox) {
      // Only support 2D boxes.
      if (pSrcBox->front != 0 || pSrcBox->back != 1) {
        SetError(hDevice, E_NOTIMPL);
        return;
      }
      if (src_right < src_left || src_bottom < src_top) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
    }

    const uint32_t copy_width = std::min(src_right - src_left, dst->width > dstX ? (dst->width - dstX) : 0u);
    const uint32_t copy_height = std::min(src_bottom - src_top, dst->height > dstY ? (dst->height - dstY) : 0u);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aer_fmt);
    const uint32_t dst_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, dst->width);
    const uint32_t src_min_row = aerogpu_texture_min_row_pitch_bytes(aer_fmt, src->width);
    const uint32_t dst_rows_total = aerogpu_texture_num_rows(aer_fmt, dst->height);
    const uint32_t src_rows_total = aerogpu_texture_num_rows(aer_fmt, src->height);
    if (!layout.valid ||
        dst_min_row == 0 ||
        src_min_row == 0 ||
        dst_rows_total == 0 ||
        src_rows_total == 0) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    auto ensure_row_pitch = [&](AeroGpuResource* res) -> bool {
      if (res->row_pitch_bytes != 0) {
        return true;
      }
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      if (row_bytes == 0) {
        return false;
      }
      res->row_pitch_bytes = AlignUpU32(row_bytes, 256);
      return (res->row_pitch_bytes != 0);
    };
    const bool has_row_pitch = ensure_row_pitch(dst) && ensure_row_pitch(src);

    if (dst->row_pitch_bytes < dst_min_row || src->row_pitch_bytes < src_min_row) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint64_t dst_total = aerogpu_texture_required_size_bytes(aer_fmt, dst->row_pitch_bytes, dst->height);
    const uint64_t src_total = aerogpu_texture_required_size_bytes(aer_fmt, src->row_pitch_bytes, src->height);
    if (dst_total <= static_cast<uint64_t>(SIZE_MAX) && dst->storage.size() < static_cast<size_t>(dst_total)) {
      try {
        dst->storage.resize(static_cast<size_t>(dst_total), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }
    if (src_total <= static_cast<uint64_t>(SIZE_MAX) && src->storage.size() < static_cast<size_t>(src_total)) {
      try {
        src->storage.resize(static_cast<size_t>(src_total), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }

    const uint32_t src_copy_right = src_left + copy_width;
    const uint32_t src_copy_bottom = src_top + copy_height;
    const uint32_t dst_copy_right = dstX + copy_width;
    const uint32_t dst_copy_bottom = dstY + copy_height;
    if (src_copy_right < src_left ||
        src_copy_bottom < src_top ||
        dst_copy_right < dstX ||
        dst_copy_bottom < dstY) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    if (layout.block_width > 1 || layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((src_left % layout.block_width) != 0 ||
          (src_top % layout.block_height) != 0 ||
          (dstX % layout.block_width) != 0 ||
          (dstY % layout.block_height) != 0 ||
          !aligned_or_edge(src_copy_right, layout.block_width, src->width) ||
          !aligned_or_edge(src_copy_bottom, layout.block_height, src->height) ||
          !aligned_or_edge(dst_copy_right, layout.block_width, dst->width) ||
          !aligned_or_edge(dst_copy_bottom, layout.block_height, dst->height)) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
    }

    const uint32_t src_block_left = src_left / layout.block_width;
    const uint32_t src_block_top = src_top / layout.block_height;
    const uint32_t dst_block_left = dstX / layout.block_width;
    const uint32_t dst_block_top = dstY / layout.block_height;
    const uint32_t src_block_right = aerogpu_div_round_up_u32(src_copy_right, layout.block_width);
    const uint32_t src_block_bottom = aerogpu_div_round_up_u32(src_copy_bottom, layout.block_height);
    const uint32_t dst_block_right = aerogpu_div_round_up_u32(dst_copy_right, layout.block_width);
    const uint32_t dst_block_bottom = aerogpu_div_round_up_u32(dst_copy_bottom, layout.block_height);
    if (src_block_right < src_block_left ||
        src_block_bottom < src_block_top ||
        dst_block_right < dst_block_left ||
        dst_block_bottom < dst_block_top) {
      SetError(hDevice, E_INVALIDARG);
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

    const uint64_t dst_row_needed =
        static_cast<uint64_t>(dst_block_left) * static_cast<uint64_t>(layout.bytes_per_block) +
        static_cast<uint64_t>(row_bytes);
    const uint64_t src_row_needed =
        static_cast<uint64_t>(src_block_left) * static_cast<uint64_t>(layout.bytes_per_block) +
        static_cast<uint64_t>(row_bytes);

    if (has_row_pitch &&
        row_bytes &&
        copy_height_blocks &&
        dst_row_needed <= dst->row_pitch_bytes &&
        src_row_needed <= src->row_pitch_bytes &&
        dst_block_top + copy_height_blocks <= dst_rows_total &&
        src_block_top + copy_height_blocks <= src_rows_total) {
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        const uint64_t dst_off_u64 =
            static_cast<uint64_t>(dst_block_top + y) * static_cast<uint64_t>(dst->row_pitch_bytes) +
            static_cast<uint64_t>(dst_block_left) * static_cast<uint64_t>(layout.bytes_per_block);
        const uint64_t src_off_u64 =
            static_cast<uint64_t>(src_block_top + y) * static_cast<uint64_t>(src->row_pitch_bytes) +
            static_cast<uint64_t>(src_block_left) * static_cast<uint64_t>(layout.bytes_per_block);
        if (dst_off_u64 + row_bytes_u64 <= dst->storage.size() &&
            src_off_u64 + row_bytes_u64 <= src->storage.size()) {
          std::memcpy(dst->storage.data() + static_cast<size_t>(dst_off_u64),
                      src->storage.data() + static_cast<size_t>(src_off_u64),
                      row_bytes);
        }
      }
    }

    // Keep guest-backed staging allocations coherent for CPU readback when the
    // transfer backend is unavailable or stubbed out.
    if (copy_width && copy_height &&
        dst->backing_alloc_id != 0 &&
        dst->usage == static_cast<uint32_t>(D3D10_USAGE_STAGING) &&
        (dst->cpu_access_flags == 0 ||
         (dst->cpu_access_flags & static_cast<uint32_t>(D3D10_CPU_ACCESS_READ)) != 0)) {
      EmitUploadLocked(hDevice, dev, dst, 0, dst->storage.size());
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
    cmd->dst_x = dstX;
    cmd->dst_y = dstY;
    cmd->src_x = src_left;
    cmd->src_y = src_top;
    cmd->width = copy_width;
    cmd->height = copy_height;
    uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
    if (dst->backing_alloc_id != 0 &&
        dst->usage == static_cast<uint32_t>(D3D10_USAGE_STAGING) &&
        (dst->cpu_access_flags & static_cast<uint32_t>(D3D10_CPU_ACCESS_READ)) != 0) {
      copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
    }
    cmd->flags = copy_flags;
    cmd->reserved0 = 0;
    TrackStagingWriteLocked(dev, dst);
    return;
  }

  SetError(hDevice, E_NOTIMPL);
}

SIZE_T APIENTRY CalcPrivateRenderTargetViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT APIENTRY CreateRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                        D3D10DDI_HRENDERTARGETVIEW hView,
                                        D3D10DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  D3D10DDI_HRESOURCE hRes{};
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::hDrvResource) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hRes);
  auto* rtv = new (hView.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  rtv->resource = res;
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
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  D3D10DDI_HRESOURCE hRes{};
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::hDrvResource) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hRes);
  auto* dsv = new (hView.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  dsv->resource = res;
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
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  D3D10DDI_HRESOURCE hRes{};
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    hRes = pDesc->hDrvResource;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::hDrvResource) {
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::hResource) {
      hRes = pDesc->hResource;
    }
  }
  if (!hRes.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hRes);
  auto* srv = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  srv->texture = res ? res->handle : 0;
  srv->resource = res;
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
  sh->handle = allocate_global_handle(dev->adapter);
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
  layout->handle = allocate_global_handle(dev->adapter);

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
                               const D3D10DDIARG_CREATESAMPLER* pDesc,
                               D3D10DDI_HSAMPLER hSampler,
                               D3D10DDI_HRTSAMPLER) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  auto* sampler = new (hSampler.pDrvPrivate) AeroGpuSampler();
  sampler->handle = allocate_global_handle(dev->adapter);
  if (!sampler->handle) {
    sampler->~AeroGpuSampler();
    return E_FAIL;
  }

  if (pDesc) {
    if constexpr (has_member_Desc<D3D10DDIARG_CREATESAMPLER>::value) {
      InitSamplerFromDesc(sampler, pDesc->Desc);
    } else if constexpr (has_member_SamplerDesc<D3D10DDIARG_CREATESAMPLER>::value) {
      InitSamplerFromDesc(sampler, pDesc->SamplerDesc);
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  cmd->sampler_handle = sampler->handle;
  cmd->filter = sampler->filter;
  cmd->address_u = sampler->address_u;
  cmd->address_v = sampler->address_v;
  cmd->address_w = sampler->address_w;
  return S_OK;
}

void APIENTRY DestroySampler(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSAMPLER hSampler) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sampler = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  if (!dev || !sampler) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (sampler->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_sampler>(AEROGPU_CMD_DESTROY_SAMPLER);
    cmd->sampler_handle = sampler->handle;
    cmd->reserved0 = 0;
  }
  sampler->~AeroGpuSampler();
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

  if (numBuffers == 0) {
    // We only model vertex buffer slot 0 in the minimal bring-up path. If the
    // runtime unbinds a different slot, ignore it rather than accidentally
    // clearing slot 0 state.
    if (startSlot != 0) {
      return;
    }
    dev->current_vb_res = nullptr;
    dev->current_vb_stride = 0;
    dev->current_vb_offset = 0;

    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
        AEROGPU_CMD_SET_VERTEX_BUFFERS, nullptr, 0);
    cmd->start_slot = 0;
    cmd->buffer_count = 0;
    return;
  }

  // Minimal bring-up: handle the common {start=0,count=1} case.
  if (startSlot != 0 || numBuffers != 1) {
    SetError(hDevice, E_NOTIMPL);
    return;
  }

  aerogpu_vertex_buffer_binding binding{};
  AeroGpuResource* vb_res = phBuffers[0].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phBuffers[0]) : nullptr;
  binding.buffer = vb_res ? vb_res->handle : 0;
  binding.stride_bytes = pStrides[0];
  binding.offset_bytes = pOffsets[0];
  binding.reserved0 = 0;

  dev->current_vb_res = vb_res;
  dev->current_vb_stride = pStrides[0];
  dev->current_vb_offset = pOffsets[0];

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

  auto* ib_res = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer) : nullptr;
  dev->current_ib_res = ib_res;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = ib_res ? ib_res->handle : 0;
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

static void SetConstantBuffersLocked(AeroGpuDevice* dev,
                                     D3D10DDI_HDEVICE hDevice,
                                     uint32_t shader_stage,
                                     UINT start_slot,
                                     UINT buffer_count,
                                     const D3D10DDI_HRESOURCE* phBuffers) {
  if (!dev || buffer_count == 0) {
    return;
  }
  if (!phBuffers) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (start_slot >= kMaxConstantBufferSlots || start_slot + buffer_count > kMaxConstantBufferSlots) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  aerogpu_constant_buffer_binding* table = ConstantBufferTableForStage(dev, shader_stage);
  if (!table) {
    return;
  }
  AeroGpuResource** bound_resources = nullptr;
  if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
    bound_resources = dev->current_vs_cb_resources;
  } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
    bound_resources = dev->current_ps_cb_resources;
  }

  std::vector<aerogpu_constant_buffer_binding> bindings;
  bindings.resize(buffer_count);
  for (UINT i = 0; i < buffer_count; i++) {
    aerogpu_constant_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.reserved0 = 0;

    auto* res = phBuffers[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phBuffers[i]) : nullptr;
    auto* buf_res = (res && res->kind == ResourceKind::Buffer) ? res : nullptr;
    if (res && res->kind == ResourceKind::Buffer) {
      b.buffer = res->handle;
      b.offset_bytes = 0;
      b.size_bytes = res->size_bytes > 0xFFFFFFFFull ? 0xFFFFFFFFu : static_cast<uint32_t>(res->size_bytes);
    }

    table[start_slot + i] = b;
    if (bound_resources) {
      bound_resources[start_slot + i] = buf_res;
    }
    bindings[i] = b;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings.data(), bindings.size() * sizeof(bindings[0]));
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->buffer_count = buffer_count;
  cmd->reserved0 = 0;
}

void APIENTRY VsSetConstantBuffers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numBuffers, const D3D10DDI_HRESOURCE* phBuffers) {
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
  SetConstantBuffersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, startSlot, numBuffers, phBuffers);
}

void APIENTRY PsSetConstantBuffers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numBuffers, const D3D10DDI_HRESOURCE* phBuffers) {
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
  SetConstantBuffersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, startSlot, numBuffers, phBuffers);
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
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (startSlot >= kMaxShaderResourceSlots || startSlot + numViews > kMaxShaderResourceSlots) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  for (UINT i = 0; i < numViews; i++) {
    const uint32_t slot = static_cast<uint32_t>(startSlot + i);
    aerogpu_handle_t tex = 0;
    AeroGpuResource* srv_res = nullptr;
    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i]);
      srv_res = view ? view->resource : nullptr;
      tex = srv_res ? srv_res->handle : (view ? view->texture : 0);
    }
    if (tex) {
      UnbindResourceFromOutputsLocked(dev, tex);
    }
    SetShaderResourceSlotLocked(dev, shader_stage, slot, tex);
    if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
      if (dev->vs_srvs[slot] == tex) {
        dev->current_vs_srv_resources[slot] = srv_res;
      }
    } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
      if (dev->ps_srvs[slot] == tex) {
        dev->current_ps_srv_resources[slot] = srv_res;
      }
    }
  }
}

void APIENTRY ClearState(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Clear shader resources.
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
    SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
  }
  std::memset(dev->current_vs_srv_resources, 0, sizeof(dev->current_vs_srv_resources));
  std::memset(dev->current_ps_srv_resources, 0, sizeof(dev->current_ps_srv_resources));

  auto clear_constant_buffers = [&](uint32_t shader_stage, aerogpu_constant_buffer_binding* table) {
    if (!table) {
      return;
    }
    bool any = false;
    for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
      if (table[slot].buffer != 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      return;
    }

    aerogpu_constant_buffer_binding zeros[kMaxConstantBufferSlots] = {};
    for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
      table[slot] = zeros[slot];
    }

    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(AEROGPU_CMD_SET_CONSTANT_BUFFERS,
                                                                              zeros,
                                                                              sizeof(zeros));
    cmd->shader_stage = shader_stage;
    cmd->start_slot = 0;
    cmd->buffer_count = kMaxConstantBufferSlots;
    cmd->reserved0 = 0;
  };

  clear_constant_buffers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_constant_buffers);
  clear_constant_buffers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_constant_buffers);
  std::memset(dev->current_vs_cb_resources, 0, sizeof(dev->current_vs_cb_resources));
  std::memset(dev->current_ps_cb_resources, 0, sizeof(dev->current_ps_cb_resources));

  auto clear_samplers = [&](uint32_t shader_stage, aerogpu_handle_t* table) {
    if (!table) {
      return;
    }
    bool any = false;
    for (uint32_t slot = 0; slot < kMaxSamplerSlots; ++slot) {
      if (table[slot] != 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      return;
    }

    aerogpu_handle_t zeros[kMaxSamplerSlots] = {};
    for (uint32_t slot = 0; slot < kMaxSamplerSlots; ++slot) {
      table[slot] = 0;
    }

    auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(AEROGPU_CMD_SET_SAMPLERS,
                                                                       zeros,
                                                                       sizeof(zeros));
    cmd->shader_stage = shader_stage;
    cmd->start_slot = 0;
    cmd->sampler_count = kMaxSamplerSlots;
    cmd->reserved0 = 0;
  };

  clear_samplers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_samplers);
  clear_samplers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_samplers);

  dev->current_rtv = 0;
  dev->current_rtv_res = nullptr;
  dev->current_dsv = 0;
  dev->current_dsv_res = nullptr;
  dev->viewport_width = 0;
  dev->viewport_height = 0;
  EmitSetRenderTargetsLocked(dev);

  dev->current_vs = 0;
  dev->current_ps = 0;
  EmitBindShadersLocked(dev);

  dev->current_input_layout = 0;
  auto* il_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  il_cmd->input_layout_handle = 0;
  il_cmd->reserved0 = 0;

  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  auto* topo_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  topo_cmd->topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  topo_cmd->reserved0 = 0;

  dev->current_vb_res = nullptr;
  dev->current_ib_res = nullptr;
  dev->current_vb_stride = 0;
  dev->current_vb_offset = 0;
  auto* vb_cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS, nullptr, 0);
  vb_cmd->start_slot = 0;
  vb_cmd->buffer_count = 0;

  auto* ib_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  ib_cmd->buffer = 0;
  ib_cmd->format = AEROGPU_INDEX_FORMAT_UINT16;
  ib_cmd->offset_bytes = 0;
  ib_cmd->reserved0 = 0;
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

static void SetSamplersLocked(AeroGpuDevice* dev,
                              D3D10DDI_HDEVICE hDevice,
                              uint32_t shader_stage,
                              UINT start_slot,
                              UINT sampler_count,
                              const D3D10DDI_HSAMPLER* phSamplers) {
  if (!dev || sampler_count == 0) {
    return;
  }
  if (!phSamplers) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (start_slot >= kMaxSamplerSlots || start_slot + sampler_count > kMaxSamplerSlots) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  aerogpu_handle_t* table = SamplerTableForStage(dev, shader_stage);
  if (!table) {
    return;
  }

  std::vector<aerogpu_handle_t> handles;
  handles.resize(sampler_count);
  for (UINT i = 0; i < sampler_count; i++) {
    aerogpu_handle_t handle = 0;
    if (phSamplers[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(phSamplers[i])->handle;
    }
    table[start_slot + i] = handle;
    handles[i] = handle;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles.data(), handles.size() * sizeof(handles[0]));
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->sampler_count = sampler_count;
  cmd->reserved0 = 0;
}

void APIENTRY VsSetSamplers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numSamplers, const D3D10DDI_HSAMPLER* phSamplers) {
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
  SetSamplersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, startSlot, numSamplers, phSamplers);
}

void APIENTRY PsSetSamplers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numSamplers, const D3D10DDI_HSAMPLER* phSamplers) {
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
  SetSamplersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, startSlot, numSamplers, phSamplers);
}
void APIENTRY GsSetSamplers(D3D10DDI_HDEVICE, UINT, UINT, const D3D10DDI_HSAMPLER*) {
  // Stub.
}

void APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice, UINT numViewports, const D3D10_DDI_VIEWPORT* pViewports) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (numViewports == 0) {
    // Some runtimes clear state by calling SetViewports(0, nullptr). Treat this
    // as a no-op for bring-up rather than returning E_INVALIDARG.
    return;
  }
  if (!pViewports) {
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

void APIENTRY SetScissorRects(D3D10DDI_HDEVICE hDevice, UINT numRects, const D3D10_DDI_RECT* pRects) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (numRects == 0) {
    // Some runtimes clear state by calling SetScissorRects(0, nullptr). Treat
    // this as a no-op for bring-up rather than returning E_INVALIDARG.
    return;
  }
  if (!pRects) {
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
  AeroGpuResource* rtv_res = nullptr;
  aerogpu_handle_t dsv_handle = 0;
  AeroGpuResource* dsv_res = nullptr;
  if (numViews && phViews && phViews[0].pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phViews[0]);
    rtv_res = view ? view->resource : nullptr;
    rtv_handle = rtv_res ? rtv_res->handle : (view ? view->texture : 0);
  }
  if (hDsv.pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
    dsv_res = view ? view->resource : nullptr;
    dsv_handle = dsv_res ? dsv_res->handle : (view ? view->texture : 0);
  }

  dev->current_rtv = rtv_handle;
  dev->current_rtv_res = rtv_res;
  dev->current_dsv = dsv_handle;
  dev->current_dsv_res = dsv_res;

  UnbindResourceFromSrvsLocked(dev, dev->current_rtv);
  UnbindResourceFromSrvsLocked(dev, dev->current_dsv);
  EmitSetRenderTargetsLocked(dev);
}

void APIENTRY ClearRenderTargetView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hView, const FLOAT color[4]) {
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

  AeroGpuResource* res = nullptr;
  if (hView.pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hView);
    res = view ? view->resource : nullptr;
  } else {
    res = dev->current_rtv_res;
  }

  if (res && res->kind == ResourceKind::Texture2D && res->width && res->height) {
    if (res->row_pitch_bytes == 0) {
      res->row_pitch_bytes = res->width * 4;
    }
    const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    if (total_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      try {
        if (res->storage.size() < static_cast<size_t>(total_bytes)) {
          res->storage.resize(static_cast<size_t>(total_bytes));
        }
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
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

      const uint8_t out_r = float_to_unorm8(color[0]);
      const uint8_t out_g = float_to_unorm8(color[1]);
      const uint8_t out_b = float_to_unorm8(color[2]);
      const uint8_t out_a = float_to_unorm8(color[3]);

      for (uint32_t y = 0; y < res->height; ++y) {
        uint8_t* row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        for (uint32_t x = 0; x < res->width; ++x) {
          uint8_t* dst = row + static_cast<size_t>(x) * 4;
          switch (res->dxgi_format) {
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

  TrackBoundTargetsForSubmitLocked(dev);
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

  TrackBoundTargetsForSubmitLocked(dev);
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
  TrackDrawStateLocked(dev);

  if (vertexCount == 3 && dev->current_topology == static_cast<uint32_t>(D3D10_DDI_PRIMITIVE_TOPOLOGY_TRIANGLELIST) &&
      dev->current_rtv_res && dev->current_vb_res) {
    auto* rt = dev->current_rtv_res;
    auto* vb = dev->current_vb_res;

    if (rt->kind == ResourceKind::Texture2D && vb->kind == ResourceKind::Buffer && rt->width && rt->height &&
        vb->storage.size() >= static_cast<size_t>(dev->current_vb_offset) +
                                static_cast<size_t>(startVertex + 3) * static_cast<size_t>(dev->current_vb_stride)) {
      if (rt->row_pitch_bytes == 0) {
        rt->row_pitch_bytes = rt->width * 4;
      }
      const uint64_t rt_bytes = static_cast<uint64_t>(rt->row_pitch_bytes) * static_cast<uint64_t>(rt->height);
      if (rt_bytes <= static_cast<uint64_t>(SIZE_MAX) && rt->storage.size() < static_cast<size_t>(rt_bytes)) {
        try {
          rt->storage.resize(static_cast<size_t>(rt_bytes));
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
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
                            static_cast<size_t>(startVertex + i) * static_cast<size_t>(dev->current_vb_stride);
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

  TrackDrawStateLocked(dev);
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
  HRESULT hr = S_OK;
  submit_locked(dev, /*want_present=*/false, &hr);
  if (FAILED(hr)) {
    SetError(hDevice, hr);
  }
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
  aerogpu_handle_t src_handle = 0;
  src_handle = src_res ? src_res->handle : 0;

  AEROGPU_D3D10_11_LOG("trace_resources: D3D10 Present sync=%u src_handle=%u",
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
  submit_locked(dev, /*want_present=*/true, &hr);
  if (FAILED(hr)) {
    return hr;
  }
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

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  AEROGPU_D3D10_11_LOG("trace_resources: D3D10 RotateResourceIdentities count=%u",
                       static_cast<unsigned>(numResources));
  for (UINT i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (phResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  + slot[%u]=%u",
                         static_cast<unsigned>(i),
                         static_cast<unsigned>(handle));
  }
#endif

  std::vector<AeroGpuResource*> resources;
  resources.reserve(numResources);
  for (UINT i = 0; i < numResources; ++i) {
    auto* res = phResources[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[i]) : nullptr;
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

  // Validate that we're rotating swapchain backbuffers (Texture2D render targets).
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
    uint32_t usage = 0;
    uint32_t cpu_access_flags = 0;
    AeroGpuResource::WddmIdentity wddm;
    std::vector<uint8_t> storage;
    uint64_t last_gpu_write_fence = 0;
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
    id.usage = res->usage;
    id.cpu_access_flags = res->cpu_access_flags;
    id.wddm = std::move(res->wddm);
    id.storage = std::move(res->storage);
    id.last_gpu_write_fence = res->last_gpu_write_fence;
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
    res->usage = id.usage;
    res->cpu_access_flags = id.cpu_access_flags;
    res->wddm = std::move(id.wddm);
    res->storage = std::move(id.storage);
    res->last_gpu_write_fence = id.last_gpu_write_fence;
  };

  std::vector<aerogpu_handle_t> old_handles;
  old_handles.reserve(resources.size());
  for (auto* res : resources) {
    old_handles.push_back(res ? res->handle : 0);
  }

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
      SetError(hDevice, E_OUTOFMEMORY);
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

  auto remap_handle = [&](aerogpu_handle_t handle) -> aerogpu_handle_t {
    for (size_t i = 0; i < old_handles.size(); ++i) {
      if (old_handles[i] == handle) {
        return resources[i] ? resources[i]->handle : 0;
      }
    }
    return handle;
  };

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    const aerogpu_handle_t new_vs = remap_handle(dev->vs_srvs[slot]);
    if (new_vs != dev->vs_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, new_vs);
    }
    const aerogpu_handle_t new_ps = remap_handle(dev->ps_srvs[slot]);
    if (new_ps != dev->ps_srvs[slot]) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, new_ps);
    }
  }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  for (UINT i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (phResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  -> slot[%u]=%u",
                         static_cast<unsigned>(i),
                         static_cast<unsigned>(handle));
  }
#endif
}

// -----------------------------------------------------------------------------
// Adapter DDI
// -----------------------------------------------------------------------------

template <typename T, typename = void>
struct has_FormatSupport2 : std::false_type {};

template <typename T>
struct has_FormatSupport2<T, std::void_t<decltype(&T::FormatSupport2)>> : std::true_type {};

HRESULT APIENTRY GetCaps(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_GETCAPS* pCaps) {
  if (!pCaps) {
    return E_INVALIDARG;
  }

  DebugLog("aerogpu-d3d10: GetCaps type=%u size=%u\n", (unsigned)pCaps->Type, (unsigned)pCaps->DataSize);

  if (!pCaps->pData || pCaps->DataSize == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    return S_OK;
  }

  DXGI_FORMAT in_format = DXGI_FORMAT_UNKNOWN;
  if (pCaps->Type == D3D10DDICAPS_TYPE_FORMAT_SUPPORT && pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
    in_format = reinterpret_cast<const D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData)->Format;
  }

  DXGI_FORMAT msaa_format = DXGI_FORMAT_UNKNOWN;
  UINT msaa_sample_count = 0;
  if (pCaps->Type == D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS && pCaps->DataSize >= sizeof(DXGI_FORMAT) + sizeof(UINT)) {
    const uint8_t* in_bytes = reinterpret_cast<const uint8_t*>(pCaps->pData);
    msaa_format = *reinterpret_cast<const DXGI_FORMAT*>(in_bytes);
    msaa_sample_count = *reinterpret_cast<const UINT*>(in_bytes + sizeof(DXGI_FORMAT));
  }

  if (pCaps->DataSize) {
    std::memset(pCaps->pData, 0, pCaps->DataSize);
  }
  const bool supports_bc = [&]() -> bool {
    auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
    if (!adapter || !adapter->umd_private_valid) {
      return false;
    }
    const aerogpu_umd_private_v1& blob = adapter->umd_private;
    const uint32_t major = blob.device_abi_version_u32 >> 16;
    const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
    return (major == AEROGPU_ABI_MAJOR) && (minor >= 2);
  }();
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
            support = supports_bc ? (D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_SHADER_SAMPLE |
                                     D3D10_FORMAT_SUPPORT_CPU_LOCKABLE)
                                  : 0;
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

    case D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
      // D3D10::CheckMultisampleQualityLevels. Treat 1x as supported (quality 1),
      // no MSAA yet.
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
  if (!pCreateDevice->pCallbacks) {
    device->~AeroGpuDevice();
    return E_INVALIDARG;
  }
  device->callbacks = *pCreateDevice->pCallbacks;
  __if_exists(D3D10DDIARG_CREATEDEVICE::hRTDevice) {
    device->hrt_device = pCreateDevice->hRTDevice;
  }
  if (!device->hrt_device.pDrvPrivate) {
    device->~AeroGpuDevice();
    return E_INVALIDARG;
  }
  __if_exists(D3D10DDIARG_CREATEDEVICE::pUMCallbacks) {
    device->um_callbacks = pCreateDevice->pUMCallbacks;
  }
  if (!device->um_callbacks) {
    device->um_callbacks = reinterpret_cast<const D3DDDI_DEVICECALLBACKS*>(pCreateDevice->pCallbacks);
  }

  // Ensure we have a kernel-mode device + context so we can wait/poll the
  // monitored fence sync object for Map READ / DO_NOT_WAIT semantics.
  HRESULT wddm_hr = InitKernelDeviceContext(device, hAdapter);
  if (FAILED(wddm_hr) || device->hSyncObject == 0) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return FAILED(wddm_hr) ? wddm_hr : E_FAIL;
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
  if constexpr (has_pfnOpenResource<D3D10DDI_DEVICEFUNCS>::value) {
    using Fn = decltype(funcs.pfnOpenResource);
    if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
      funcs.pfnOpenResource = &OpenResource;
    } else {
      funcs.pfnOpenResource = &NotImpl<Fn>::Fn;
    }
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
    funcs.pfnClearState = &ClearState;
  }
  if constexpr (has_pfnBegin<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnBegin = &NotImpl<decltype(funcs.pfnBegin)>::Fn;
  }
  if constexpr (has_pfnEnd<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnEnd = &NotImpl<decltype(funcs.pfnEnd)>::Fn;
  }
  if constexpr (has_pfnReadFromSubresource<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnReadFromSubresource = &NotImpl<decltype(funcs.pfnReadFromSubresource)>::Fn;
  }
  if constexpr (has_pfnWriteToSubresource<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnWriteToSubresource = &NotImpl<decltype(funcs.pfnWriteToSubresource)>::Fn;
  }
  if constexpr (has_pfnStagingResourceMap<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnStagingResourceMap = &StagingResourceMap<>;
  }
  if constexpr (has_pfnStagingResourceUnmap<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnStagingResourceUnmap = &StagingResourceUnmap<>;
  }
  if constexpr (has_pfnDynamicIABufferMapDiscard<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard<>;
  }
  if constexpr (has_pfnDynamicIABufferMapNoOverwrite<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite<>;
  }
  if constexpr (has_pfnDynamicIABufferUnmap<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDynamicIABufferUnmap = &DynamicIABufferUnmap<>;
  }
  if constexpr (has_pfnDynamicConstantBufferMapDiscard<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard<>;
  }
  if constexpr (has_pfnDynamicConstantBufferUnmap<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmap<>;
  }
  if constexpr (has_pfnCalcPrivateQuerySize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivateQuerySize = &NotImpl<decltype(funcs.pfnCalcPrivateQuerySize)>::Fn;
  }
  if constexpr (has_pfnCreateQuery<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreateQuery = &NotImpl<decltype(funcs.pfnCreateQuery)>::Fn;
  }
  if constexpr (has_pfnDestroyQuery<D3D10DDI_DEVICEFUNCS>::value) {
    // Destroy paths should be no-ops even for unsupported features so teardown
    // doesn't surface spurious device errors.
    funcs.pfnDestroyQuery = &Noop<decltype(funcs.pfnDestroyQuery)>::Fn;
  }
  if constexpr (has_pfnCalcPrivatePredicateSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivatePredicateSize = &NotImpl<decltype(funcs.pfnCalcPrivatePredicateSize)>::Fn;
  }
  if constexpr (has_pfnCreatePredicate<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreatePredicate = &NotImpl<decltype(funcs.pfnCreatePredicate)>::Fn;
  }
  if constexpr (has_pfnDestroyPredicate<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDestroyPredicate = &Noop<decltype(funcs.pfnDestroyPredicate)>::Fn;
  }
  if constexpr (has_pfnCalcPrivateCounterSize<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCalcPrivateCounterSize = &NotImpl<decltype(funcs.pfnCalcPrivateCounterSize)>::Fn;
  }
  if constexpr (has_pfnCreateCounter<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnCreateCounter = &NotImpl<decltype(funcs.pfnCreateCounter)>::Fn;
  }
  if constexpr (has_pfnDestroyCounter<D3D10DDI_DEVICEFUNCS>::value) {
    funcs.pfnDestroyCounter = &Noop<decltype(funcs.pfnDestroyCounter)>::Fn;
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
  DestroyKmtAdapterHandle(adapter);
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

  InitUmdPrivate(adapter);

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
