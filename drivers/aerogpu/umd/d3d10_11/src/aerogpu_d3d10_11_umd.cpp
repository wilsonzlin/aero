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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
// WDK build: keep this translation unit empty.
//
// On Win7, the exported UMD entrypoints are provided by the WDK-specific
// translation units instead:
//   - `aerogpu_d3d10_1_umd_wdk.cpp`   (OpenAdapter10 / OpenAdapter10_2)
//   - `aerogpu_d3d11_umd_wdk.cpp`     (OpenAdapter11)
// plus shared D3D10 helper code in `aerogpu_d3d10_umd_wdk.cpp`.
// which submit AeroGPU command streams via the shared Win7/WDDM backend in
// `aerogpu_d3d10_11_wddm_submit.{h,cpp}`.
//
// Keeping this file empty in WDK builds avoids compiling a second, unused WDDM
// submission path.
#else

#include <algorithm>
#include <cassert>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <new>
#include <tuple>
#include <type_traits>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../common/aerogpu_win32_security.h"

#ifndef FAILED
#define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

namespace {

#if defined(_WIN32)
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
#endif

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
const char* resource_dimension_name(AEROGPU_DDI_RESOURCE_DIMENSION dim) {
  switch (dim) {
    case AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER:
      return "BUFFER";
    case AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D:
      return "TEX2D";
    default:
      return "UNKNOWN";
  }
}

void trace_create_resource_desc(const AEROGPU_DDIARG_CREATERESOURCE* pDesc) {
  if (!pDesc) {
    return;
  }

  AEROGPU_D3D10_11_LOG(
      "trace_resources: CreateResource dim=%s(%u) fmt=%u bind=0x%08X usage=%u cpu=0x%08X misc=0x%08X "
      "sample=(%u,%u) rflags=0x%08X init=%p init_count=%u",
      resource_dimension_name(pDesc->Dimension),
      static_cast<unsigned>(pDesc->Dimension),
      static_cast<unsigned>(pDesc->Format),
      static_cast<unsigned>(pDesc->BindFlags),
      static_cast<unsigned>(pDesc->Usage),
      static_cast<unsigned>(pDesc->CPUAccessFlags),
      static_cast<unsigned>(pDesc->MiscFlags),
      static_cast<unsigned>(pDesc->SampleDescCount),
      static_cast<unsigned>(pDesc->SampleDescQuality),
      static_cast<unsigned>(pDesc->ResourceFlags),
      static_cast<const void*>(pDesc->pInitialData),
      static_cast<unsigned>(pDesc->InitialDataCount));

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    AEROGPU_D3D10_11_LOG("trace_resources:  + buffer: bytes=%u stride=%u",
                         static_cast<unsigned>(pDesc->ByteWidth),
                         static_cast<unsigned>(pDesc->StructureByteStride));
  } else if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    AEROGPU_D3D10_11_LOG("trace_resources:  + tex2d: %ux%u mips=%u array=%u",
                         static_cast<unsigned>(pDesc->Width),
                         static_cast<unsigned>(pDesc->Height),
                         static_cast<unsigned>(pDesc->MipLevels),
                         static_cast<unsigned>(pDesc->ArraySize));
  } else {
    AEROGPU_D3D10_11_LOG("trace_resources:  + raw: ByteWidth=%u Width=%u Height=%u Mips=%u Array=%u",
                         static_cast<unsigned>(pDesc->ByteWidth),
                         static_cast<unsigned>(pDesc->Width),
                         static_cast<unsigned>(pDesc->Height),
                         static_cast<unsigned>(pDesc->MipLevels),
                         static_cast<unsigned>(pDesc->ArraySize));
  }
}
#endif  // AEROGPU_UMD_TRACE_RESOURCES

constexpr aerogpu_handle_t kInvalidHandle = 0;
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr HRESULT kHrPending = static_cast<HRESULT>(0x8000000Au); // E_PENDING
constexpr HRESULT kHrWaitTimeout = static_cast<HRESULT>(0x80070102u); // HRESULT_FROM_WIN32(WAIT_TIMEOUT)
constexpr HRESULT kHrErrorTimeout = static_cast<HRESULT>(0x800705B4u); // HRESULT_FROM_WIN32(ERROR_TIMEOUT)
constexpr HRESULT kHrNtStatusTimeout = static_cast<HRESULT>(0x10000102u); // HRESULT_FROM_NT(STATUS_TIMEOUT) (SUCCEEDED)
constexpr HRESULT kHrNtStatusGraphicsGpuBusy =
    static_cast<HRESULT>(0xD01E0102u); // HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)
constexpr uint32_t kAeroGpuTimeoutMsInfinite = ~0u;


// -------------------------------------------------------------------------------------------------
// Optional bring-up logging for adapter caps queries.
// Define AEROGPU_D3D10_11_CAPS_LOG in the build to enable.
// -------------------------------------------------------------------------------------------------

#if defined(AEROGPU_D3D10_11_CAPS_LOG)
void CapsVLog(const char* fmt, va_list args) {
  char buf[2048];
  int n = vsnprintf(buf, sizeof(buf), fmt, args);
  if (n <= 0) {
    return;
  }
#if defined(_WIN32)
  OutputDebugStringA(buf);
#else
  fputs(buf, stderr);
#endif
}

void CapsLog(const char* fmt, ...) {
  va_list args;
  va_start(args, fmt);
  CapsVLog(fmt, args);
  va_end(args);
}
#define CAPS_LOG(...) CapsLog(__VA_ARGS__)
#else
#define CAPS_LOG(...) ((void)0)
#endif

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

// D3D11_FILTER subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11FilterMinMagMipPoint = 0;
constexpr uint32_t kD3D11FilterMinMagMipLinear = 0x15;
constexpr uint32_t kD3D11FilterAnisotropic = 0x55;

// D3D11_TEXTURE_ADDRESS_MODE subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11TextureAddressWrap = 1;
constexpr uint32_t kD3D11TextureAddressMirror = 2;
constexpr uint32_t kD3D11TextureAddressClamp = 3;
constexpr uint32_t kD3D11TextureAddressBorder = 4;
constexpr uint32_t kD3D11TextureAddressMirrorOnce = 5;

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

// D3D_FEATURE_LEVEL subset (numeric values from d3dcommon.h).
constexpr uint32_t kD3DFeatureLevel10_0 = 0xA000;

// D3D11_FORMAT_SUPPORT subset (numeric values from d3d11.h).
// These values are stable across Windows versions and are used by
// ID3D11Device::CheckFormatSupport.
constexpr uint32_t kD3D11FormatSupportBuffer = 0x1;
constexpr uint32_t kD3D11FormatSupportIaVertexBuffer = 0x2;
constexpr uint32_t kD3D11FormatSupportIaIndexBuffer = 0x4;
constexpr uint32_t kD3D11FormatSupportTexture2D = 0x20;
constexpr uint32_t kD3D11FormatSupportShaderLoad = 0x100;
constexpr uint32_t kD3D11FormatSupportShaderSample = 0x200;
constexpr uint32_t kD3D11FormatSupportRenderTarget = 0x4000;
constexpr uint32_t kD3D11FormatSupportBlendable = 0x8000;
constexpr uint32_t kD3D11FormatSupportDepthStencil = 0x10000;
constexpr uint32_t kD3D11FormatSupportCpuLockable = 0x20000;
constexpr uint32_t kD3D11FormatSupportDisplay = 0x80000;

// D3D11_RESOURCE_MISC_SHARED (numeric value from d3d11.h).
constexpr uint32_t kD3D11ResourceMiscShared = 0x2;

uint32_t d3d11_format_support_flags(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Typeless:
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8UnormSrgb:
    case kDxgiFormatB8G8R8X8Typeless:
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatR8G8B8A8Typeless:
      return kD3D11FormatSupportTexture2D | kD3D11FormatSupportRenderTarget | kD3D11FormatSupportShaderSample |
             kD3D11FormatSupportBlendable | kD3D11FormatSupportCpuLockable | kD3D11FormatSupportDisplay;
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
      return kD3D11FormatSupportTexture2D | kD3D11FormatSupportShaderSample | kD3D11FormatSupportCpuLockable;
    case kDxgiFormatD24UnormS8Uint:
    case kDxgiFormatD32Float:
      return kD3D11FormatSupportTexture2D | kD3D11FormatSupportDepthStencil;
    case kDxgiFormatR16Uint:
    case kDxgiFormatR32Uint:
      return kD3D11FormatSupportBuffer | kD3D11FormatSupportIaIndexBuffer;
    case kDxgiFormatR32G32Float:
    case kDxgiFormatR32G32B32Float:
    case kDxgiFormatR32G32B32A32Float:
      return kD3D11FormatSupportBuffer | kD3D11FormatSupportIaVertexBuffer;
    default:
      return 0;
  }
}

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

uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  if (alignment == 0) {
    return value;
  }
  const uint64_t mask = alignment - 1;
  return (value + mask) & ~mask;
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

struct AerogpuTextureFormatLayout {
  uint32_t block_width = 0;
  uint32_t block_height = 0;
  uint32_t bytes_per_block = 0;
  bool valid = false;
};

AerogpuTextureFormatLayout aerogpu_texture_format_layout(uint32_t aerogpu_format) {
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

bool aerogpu_format_is_block_compressed(uint32_t aerogpu_format) {
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  return layout.valid && (layout.block_width != 1 || layout.block_height != 1);
}

uint32_t aerogpu_div_round_up_u32(uint32_t value, uint32_t divisor) {
  return (value + divisor - 1) / divisor;
}

uint32_t aerogpu_texture_min_row_pitch_bytes(uint32_t aerogpu_format, uint32_t width) {
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

uint32_t aerogpu_texture_num_rows(uint32_t aerogpu_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return aerogpu_div_round_up_u32(height, layout.block_height);
}

uint64_t aerogpu_texture_required_size_bytes(uint32_t aerogpu_format, uint32_t row_pitch_bytes, uint32_t height) {
  if (row_pitch_bytes == 0) {
    return 0;
  }
  const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, height);
  return static_cast<uint64_t>(row_pitch_bytes) * static_cast<uint64_t>(rows);
}

uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
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

uint32_t d3d11_filter_to_aerogpu(uint32_t filter) {
  switch (filter) {
    case kD3D11FilterMinMagMipPoint:
      return AEROGPU_SAMPLER_FILTER_NEAREST;
    case kD3D11FilterMinMagMipLinear:
      return AEROGPU_SAMPLER_FILTER_LINEAR;
    case kD3D11FilterAnisotropic:
      return AEROGPU_SAMPLER_FILTER_LINEAR;
    default:
      return AEROGPU_SAMPLER_FILTER_LINEAR;
  }
}

uint32_t d3d11_address_mode_to_aerogpu(uint32_t mode) {
  switch (mode) {
    case kD3D11TextureAddressWrap:
      return AEROGPU_SAMPLER_ADDRESS_REPEAT;
    case kD3D11TextureAddressMirror:
      return AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT;
    case kD3D11TextureAddressClamp:
    case kD3D11TextureAddressBorder:
    case kD3D11TextureAddressMirrorOnce:
    default:
      return AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
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

};

#if defined(_WIN32)
static uint64_t splitmix64(uint64_t x) {
  x += 0x9E3779B97F4A7C15ULL;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
  return x ^ (x >> 31);
}

static bool fill_random_bytes(void* out, size_t len) {
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

static void log_global_handle_fallback_once() {
  static std::once_flag once;
  std::call_once(once, [] {
    OutputDebugStringA(
        "aerogpu-d3d10_11: GlobalHandleCounter mapping unavailable; using RNG fallback\n");
  });
}
#endif // defined(_WIN32)

static aerogpu_handle_t allocate_global_handle(AeroGpuAdapter* adapter) {
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
#endif

  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == kInvalidHandle) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
}

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible backing allocation ID (`alloc_id` / `backing_alloc_id`).
  //
  // This is a stable driver-defined `u32` used as the key in the per-submit
  // `aerogpu_alloc_table` (alloc_id -> {gpa, size}). It is intentionally *not*
  // a raw OS handle (and not the KMD-visible `DXGK_ALLOCATIONLIST::hAllocation`
  // pointer identity).
  //
  // On Win7/WDDM 1.1, the stable `alloc_id` is supplied to the KMD via WDDM
  // allocation private driver data (`aerogpu_wddm_alloc_priv.alloc_id`).
  //
  // 0 means "host allocated" (no allocation-table entry).
  //
  // IMPORTANT: On real Win7/WDDM 1.1 builds, do NOT use the numeric value of the
  // runtime's `hAllocation` handle as this ID: dxgkrnl does not preserve that
  // identity across UMD↔KMD. The stable cross-layer key is the driver-defined
  // `alloc_id` carried in WDDM allocation private driver data
  // (`drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
  //
  // The repository build's harness may choose to use `alloc_handle` as the
  // `backing_alloc_id`, but that is a harness contract, not a WDDM contract.
  uint32_t backing_alloc_id = 0;

  // Allocation backing this resource as understood by the repo-local harness
  // callback interface (0 if host allocated). In real WDDM builds, mapping is
  // done via the runtime LockCb/UnlockCb path using the UMD-visible allocation
  // handle returned by AllocateCb.
  AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
  uint32_t alloc_offset_bytes = 0;
  uint64_t alloc_size_bytes = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  bool is_shared = false;
  bool is_shared_alias = false;

  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;
  uint32_t usage = 0;
  uint32_t cpu_access_flags = 0;

  // WDDM identity (kernel-mode handles / allocation identities).
  //
  // DXGI swapchains on Win7 use pfnRotateResourceIdentities to "flip" buffers by
  // rotating the backing allocation identities between the runtime's resource
  // handles. Once resources are backed by real WDDM allocations, it's not enough
  // to rotate only the AeroGPU-side handle.
  //
  // These are stored as opaque values here to keep the repository build
  // self-contained; in a WDK build these correspond to the KM resource handle
  // and per-allocation KM handles.
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


  // CPU-visible backing storage for resource uploads.
  //
  // The initial milestone keeps resource data management very conservative:
  // - Buffers can be initialized at CreateResource time.
  // - Texture2D initial data is supported for the common {mips=1, array=1} case.
  //
  // A real WDDM build should map these updates onto real allocations.
  std::vector<uint8_t> storage;

  // Map/unmap tracking.
  bool mapped_via_allocation = false;
  void* mapped_ptr = nullptr;
  bool mapped = false;
  bool mapped_write = false;
  uint32_t mapped_subresource = 0;
  uint32_t mapped_map_type = 0;
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
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuDepthStencilView {
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuShaderResourceView {
  aerogpu_handle_t texture = 0;
};

struct AeroGpuSampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_NEAREST;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
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


struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // Portable build error reporting: some DDIs are void and report failure via a
  // runtime callback (pfnSetErrorCb). In the non-WDK build we track the last
  // error on the device for unit tests / bring-up logging.
  HRESULT last_error = S_OK;

  // Optional device callback table provided by the harness/real runtime.
  // Used by the portable UMD to allocate/map guest-backed resources and to pass
  // the list of referenced allocations alongside each submission.
  const AEROGPU_D3D10_11_DEVICECALLBACKS* device_callbacks = nullptr;
  std::vector<AEROGPU_WDDM_ALLOCATION_HANDLE> referenced_allocs;

  // Fence tracking for WDDM-backed synchronization. Higher-level D3D10/11 code (e.g. Map READ on
  // staging resources) can use these values to wait for GPU completion.
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  std::vector<AeroGpuResource*> live_resources;


  // Cached state.
  AeroGpuResource* current_rtv = nullptr;
  AeroGpuResource* current_dsv = nullptr;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_rtv_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_dsv_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_vb_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_ib_alloc = 0;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};

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

void AddLiveResourceLocked(AeroGpuDevice* dev, AeroGpuResource* res) {
  if (!dev || !res) {
    return;
  }
  dev->live_resources.push_back(res);
}

void RemoveLiveResourceLocked(AeroGpuDevice* dev, const AeroGpuResource* res) {
  if (!dev || !res) {
    return;
  }
  auto it = std::find(dev->live_resources.begin(), dev->live_resources.end(), res);
  if (it != dev->live_resources.end()) {
    dev->live_resources.erase(it);
  }
}

void track_alloc_for_submit_locked(AeroGpuDevice* dev, AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle) {
  if (!dev || alloc_handle == 0) {
    return;
  }

  auto& allocs = dev->referenced_allocs;
  if (std::find(allocs.begin(), allocs.end(), alloc_handle) == allocs.end()) {
    allocs.push_back(alloc_handle);
  }
}

void track_resource_alloc_for_submit_locked(AeroGpuDevice* dev, const AeroGpuResource* res) {
  if (!dev || !res) {
    return;
  }
  track_alloc_for_submit_locked(dev, res->alloc_handle);
}

static const AeroGpuResource* FindLiveResourceByHandleLocked(const AeroGpuDevice* dev, aerogpu_handle_t handle) {
  if (!dev || handle == kInvalidHandle) {
    return nullptr;
  }
  for (const auto* res : dev->live_resources) {
    if (res && res->handle == handle) {
      return res;
    }
  }
  return nullptr;
}

void track_current_state_allocs_for_submit_locked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  track_resource_alloc_for_submit_locked(dev, dev->current_rtv);
  track_resource_alloc_for_submit_locked(dev, dev->current_dsv);
  track_alloc_for_submit_locked(dev, dev->current_vb_alloc);
  track_alloc_for_submit_locked(dev, dev->current_ib_alloc);

  // Constant buffers and shader resources can be backed by guest allocations. Keep
  // them in the per-submit allocation list so the host can resolve alloc_id -> GPA
  // for resource bindings referenced by the command stream.
  for (uint32_t i = 0; i < kMaxConstantBufferSlots; i++) {
    const aerogpu_handle_t vs_handle = dev->vs_constant_buffers[i].buffer;
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, vs_handle));
    const aerogpu_handle_t ps_handle = dev->ps_constant_buffers[i].buffer;
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, ps_handle));
  }

  for (uint32_t i = 0; i < kMaxShaderResourceSlots; i++) {
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, dev->vs_srvs[i]));
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, dev->ps_srvs[i]));
  }
}


uint64_t AeroGpuQueryCompletedFence(AeroGpuDevice* dev) {
  if (!dev) {
    return 0;
  }

  if (!dev->adapter) {
    return dev->last_completed_fence.load(std::memory_order_relaxed);
  }

  const AEROGPU_D3D10_11_DEVICECALLBACKS* cb = dev->device_callbacks;
  const uint64_t observed = (cb && cb->pfnQueryCompletedFence) ? cb->pfnQueryCompletedFence(cb->pUserContext) : 0;

  uint64_t completed = 0;
  bool notify = false;
  {
    std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
    if (observed > dev->adapter->completed_fence) {
      dev->adapter->completed_fence = observed;
      notify = true;
    }
    completed = dev->adapter->completed_fence;
  }

  if (notify) {
    dev->adapter->fence_cv.notify_all();
  }
  atomic_max_u64(&dev->last_completed_fence, completed);
  return completed;
}

// Waits for `fence` to be completed.
//
// `timeout_ms` semantics match D3D11 / DXGI Map expectations:
// - 0: non-blocking poll
// - kAeroGpuTimeoutMsInfinite: infinite wait
//
// On timeout/poll miss, returns `DXGI_ERROR_WAS_STILL_DRAWING` (useful for D3D11 Map DO_NOT_WAIT).
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

  // Portable build: prefer an injected wait callback when available (unit tests
  // use this to model Win7/WDDM-style asynchronous fence completion).
  if (dev->device_callbacks && dev->device_callbacks->pfnWaitForFence) {
    const auto* cb = dev->device_callbacks;
    const HRESULT hr = cb->pfnWaitForFence(cb->pUserContext, fence, timeout_ms);
    // Mirror Win7/WDDM wait behavior: several "not ready" / timeout HRESULTs can
    // be returned for DO_NOT_WAIT polling, including `HRESULT_FROM_NT(STATUS_TIMEOUT)`
    // which is a SUCCEEDED() HRESULT.
    if (hr == kDxgiErrorWasStillDrawing || hr == kHrWaitTimeout || hr == kHrErrorTimeout ||
        hr == kHrNtStatusTimeout || hr == kHrNtStatusGraphicsGpuBusy ||
        (timeout_ms == 0 && hr == kHrPending)) {
      return kDxgiErrorWasStillDrawing;
    }
    if (FAILED(hr)) {
      return hr;
    }

    if (dev->adapter) {
      {
        std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
        dev->adapter->completed_fence = std::max(dev->adapter->completed_fence, fence);
      }
      dev->adapter->fence_cv.notify_all();
    }

    atomic_max_u64(&dev->last_completed_fence, fence);
    return S_OK;
  }

  if (!dev->adapter) {
    return E_FAIL;
  }

  const AEROGPU_D3D10_11_DEVICECALLBACKS* cb = dev->device_callbacks;
  if (cb && cb->pfnWaitForFence) {
    const HRESULT hr = cb->pfnWaitForFence(cb->pUserContext, fence, timeout_ms);
    if (!FAILED(hr)) {
      bool notify = false;
      {
        std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
        if (fence > dev->adapter->completed_fence) {
          dev->adapter->completed_fence = fence;
          notify = true;
        }
      }
      if (notify) {
        dev->adapter->fence_cv.notify_all();
      }
      atomic_max_u64(&dev->last_completed_fence, fence);
    }
    return hr;
  }

  // If the harness supplies an explicit completed-fence query callback, poll it
  // while waiting so portable (non-WDK) builds can model asynchronous
  // completions.
  if (cb && cb->pfnQueryCompletedFence) {
    if (timeout_ms == 0) {
      return kDxgiErrorWasStillDrawing;
    }

    const auto start = std::chrono::steady_clock::now();
    AeroGpuAdapter* adapter = dev->adapter;
    for (;;) {
      if (AeroGpuQueryCompletedFence(dev) >= fence) {
        return S_OK;
      }

      if (timeout_ms != kAeroGpuTimeoutMsInfinite) {
        const auto elapsed =
            std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start).count();
        if (elapsed >= static_cast<int64_t>(timeout_ms)) {
          return kDxgiErrorWasStillDrawing;
        }
      }

      std::unique_lock<std::mutex> lock(adapter->fence_mutex);
      adapter->fence_cv.wait_for(lock, std::chrono::milliseconds(1));
    }
  }

  AeroGpuAdapter* adapter = dev->adapter;
  std::unique_lock<std::mutex> lock(adapter->fence_mutex);
  auto ready = [&] { return adapter->completed_fence >= fence; };

  if (ready()) {
    atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
    return S_OK;
  }

  if (timeout_ms == 0) {
    return kDxgiErrorWasStillDrawing;
  }

  if (timeout_ms == kAeroGpuTimeoutMsInfinite) {
    adapter->fence_cv.wait(lock, ready);
  } else if (!adapter->fence_cv.wait_for(lock, std::chrono::milliseconds(timeout_ms), ready)) {
    return kDxgiErrorWasStillDrawing;
  }

  atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
  return S_OK;
}

inline void SetErrorIfPossible(AeroGpuDevice*, D3D10DDI_HDEVICE, HRESULT) {}
inline HRESULT DeallocateResourceIfNeeded(AeroGpuDevice*, D3D10DDI_HDEVICE, AeroGpuResource*) {
  return S_OK;
}

inline void ReportDeviceErrorLocked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  if (dev) {
    dev->last_error = hr;
    if (dev->device_callbacks && dev->device_callbacks->pfnSetError) {
      const auto* cb = dev->device_callbacks;
      cb->pfnSetError(cb->pUserContext, hr);
    }
  }
  SetErrorIfPossible(dev, hDevice, hr);
}

bool set_texture_locked(AeroGpuDevice* dev,
                        D3D10DDI_HDEVICE hDevice,
                        uint32_t shader_stage,
                        uint32_t slot,
                        aerogpu_handle_t texture) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->slot = slot;
  cmd->texture = texture;
  cmd->reserved0 = 0;
  return true;
}

bool unbind_resource_from_srvs_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return true;
  }

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if (dev->vs_srvs[slot] == resource) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, 0)) {
        return false;
      }
      dev->vs_srvs[slot] = 0;
    }
    if (dev->ps_srvs[slot] == resource) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, 0)) {
        return false;
      }
      dev->ps_srvs[slot] = 0;
    }
  }
  return true;
}

bool emit_set_render_targets_locked(AeroGpuDevice* dev);

bool set_render_targets_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, AeroGpuResource* rtv_res, AeroGpuResource* dsv_res) {
  if (!dev) {
    return false;
  }

  const aerogpu_handle_t rtv_handle = rtv_res ? rtv_res->handle : 0;
  const aerogpu_handle_t dsv_handle = dsv_res ? dsv_res->handle : 0;
  if (!unbind_resource_from_srvs_locked(dev, hDevice, rtv_handle)) {
    return false;
  }
  if (dsv_handle != rtv_handle && !unbind_resource_from_srvs_locked(dev, hDevice, dsv_handle)) {
    return false;
  }

  AeroGpuResource* prev_rtv = dev->current_rtv;
  AeroGpuResource* prev_dsv = dev->current_dsv;

  dev->current_rtv = rtv_res;
  dev->current_dsv = dsv_res;
  if (!emit_set_render_targets_locked(dev)) {
    dev->current_rtv = prev_rtv;
    dev->current_dsv = prev_dsv;
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }

  track_resource_alloc_for_submit_locked(dev, rtv_res);
  track_resource_alloc_for_submit_locked(dev, dsv_res);
  return true;
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

  // Portable build: optionally hand the command stream + referenced allocations
  // to a harness/runtime callback (used to model WDDM allocation lists in
  // non-WDK builds).
  if (dev->device_callbacks && dev->device_callbacks->pfnSubmitCmdStream) {
    track_current_state_allocs_for_submit_locked(dev);

    const auto* cb = dev->device_callbacks;
    const AEROGPU_WDDM_ALLOCATION_HANDLE* allocs = dev->referenced_allocs.empty() ? nullptr : dev->referenced_allocs.data();
    const uint32_t alloc_count = static_cast<uint32_t>(dev->referenced_allocs.size());

    uint64_t fence = 0;
    const HRESULT hr = cb->pfnSubmitCmdStream(cb->pUserContext,
                                              dev->cmd.data(),
                                              static_cast<uint32_t>(dev->cmd.size()),
                                              allocs,
                                              alloc_count,
                                              &fence);
    dev->referenced_allocs.clear();

    if (FAILED(hr)) {
      if (out_hr) {
        *out_hr = hr;
      }
      dev->cmd.reset();
      return 0;
    }

    const bool fence_provided = (fence != 0);

    // Repository build: default to a synchronous in-process fence unless the
    // harness provides a real fence value (and completion is tracked separately
    // via `pfnQueryCompletedFence`).
    if (!fence_provided) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      fence = adapter->next_fence++;
    }

    const bool external_completion = (cb->pfnWaitForFence != nullptr) || (cb->pfnQueryCompletedFence != nullptr);
    const bool mark_complete = !external_completion || !fence_provided;

    {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      adapter->next_fence = std::max(adapter->next_fence, fence + 1);
      if (mark_complete && fence > adapter->completed_fence) {
        adapter->completed_fence = fence;
      }
    }
    if (mark_complete) {
      adapter->fence_cv.notify_all();
      atomic_max_u64(&dev->last_completed_fence, fence);
    } else if (cb->pfnQueryCompletedFence) {
      // Refresh cached completion so DO_NOT_WAIT polls observe the harness state.
      (void)AeroGpuQueryCompletedFence(dev);
    }

    atomic_max_u64(&dev->last_submitted_fence, fence);

    dev->cmd.reset();
    return fence;
  }

  // No submission callback: keep the legacy synchronous in-process fence.
  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  atomic_max_u64(&dev->last_submitted_fence, fence);
  atomic_max_u64(&dev->last_completed_fence, fence);

  dev->referenced_allocs.clear();
  dev->cmd.reset();
  return fence;
}

HRESULT flush_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice) {
  HRESULT hr = S_OK;
  if (dev) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      hr = E_OUTOFMEMORY;
    } else {
      cmd->reserved0 = 0;
      cmd->reserved1 = 0;
    }
  }

  HRESULT submit_hr = S_OK;
  submit_locked(dev, &submit_hr);
  if (FAILED(submit_hr)) {
    return submit_hr;
  }
  return hr;
}

bool emit_set_render_targets_locked(AeroGpuDevice* dev) {
  if (!dev) {
    return false;
  }

  const aerogpu_handle_t rtv_handle = dev->current_rtv ? dev->current_rtv->handle : 0;
  const aerogpu_handle_t dsv_handle = dev->current_dsv ? dev->current_dsv->handle : 0;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }
  cmd->color_count = 1;
  cmd->depth_stencil = dsv_handle;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  cmd->colors[0] = rtv_handle;
  return true;
}

// -------------------------------------------------------------------------------------------------
// Device DDI (plain functions to ensure the correct calling convention)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyDevice hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERESOURCE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateResourceSize");
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                          const AEROGPU_DDIARG_CREATERESOURCE* pDesc,
                                          D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateResource dim=%u bind=0x%x misc=0x%x byteWidth=%u w=%u h=%u mips=%u array=%u fmt=%u initCount=%u",
                       pDesc ? static_cast<uint32_t>(pDesc->Dimension) : 0,
                       pDesc ? pDesc->BindFlags : 0,
                       pDesc ? pDesc->MiscFlags : 0,
                       pDesc ? pDesc->ByteWidth : 0,
                       pDesc ? pDesc->Width : 0,
                       pDesc ? pDesc->Height : 0,
                       pDesc ? pDesc->MipLevels : 0,
                       pDesc ? pDesc->ArraySize : 0,
                       pDesc ? pDesc->Format : 0,
                       pDesc ? pDesc->InitialDataCount : 0);
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  trace_create_resource_desc(pDesc);
#endif

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = allocate_global_handle(dev->adapter);
    res->kind = ResourceKind::Buffer;
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

    if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }

    // Prefer allocation-backed resources when the harness provides callbacks.
    const auto* cb = dev->device_callbacks;
    const bool can_alloc_backing = cb && cb->pfnAllocateBacking && cb->pfnMapAllocation && cb->pfnUnmapAllocation;
    if (can_alloc_backing) {
      AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
      uint64_t alloc_size_bytes = 0;
      uint32_t unused_row_pitch = 0;
      const HRESULT hr = cb->pfnAllocateBacking(cb->pUserContext,
                                                pDesc,
                                                &alloc_handle,
                                                &alloc_size_bytes,
                                                &unused_row_pitch);
      (void)unused_row_pitch;
      if (FAILED(hr) || alloc_handle == 0) {
        res->~AeroGpuResource();
        return FAILED(hr) ? hr : E_FAIL;
      }

      res->alloc_handle = alloc_handle;
      res->backing_alloc_id = static_cast<uint32_t>(alloc_handle);
      res->alloc_offset_bytes = 0;
      res->alloc_size_bytes = alloc_size_bytes ? alloc_size_bytes : res->size_bytes;
      track_alloc_for_submit_locked(dev, alloc_handle);
    } else {
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes));
      } catch (...) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
    }

    const bool has_initial_data = (pDesc->pInitialData && pDesc->InitialDataCount);
    const bool is_guest_backed = (res->backing_alloc_id != 0);
    bool wddm_initial_upload = false;
    if (has_initial_data) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem || res->size_bytes == 0) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }


      if (!res->storage.empty() && res->storage.size() >= static_cast<size_t>(res->size_bytes)) {
        std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
      }

      if (!wddm_initial_upload && res->alloc_handle != 0) {
        void* cpu_ptr = nullptr;
        HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &cpu_ptr);
        if (FAILED(hr) || !cpu_ptr) {
          res->~AeroGpuResource();
          return FAILED(hr) ? hr : E_FAIL;
        }
        std::memcpy(static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes,
                    init.pSysMem,
                    static_cast<size_t>(res->size_bytes));
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        wddm_initial_upload = true;
      }
    }

    AddLiveResourceLocked(dev, res);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      RemoveLiveResourceLocked(dev, res);
      res->handle = kInvalidHandle;
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->alloc_offset_bytes;
    cmd->reserved0 = 0;

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u size=%llu",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned long long>(res->size_bytes));
#endif

    if (has_initial_data) {
      if (is_guest_backed) {
        if (!wddm_initial_upload) {
          // Guest-backed resources must be initialized via the WDDM allocation +
          // RESOURCE_DIRTY_RANGE path; inline UPLOAD_RESOURCE is only valid for
          // host-owned resources.
          res->~AeroGpuResource();
          return E_FAIL;
        }

        auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!dirty) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          dirty->resource_handle = res->handle;
          dirty->reserved0 = 0;
          dirty->offset_bytes = 0;
          dirty->size_bytes = res->size_bytes;
          track_resource_alloc_for_submit_locked(dev, res);
        }
      } else {
        auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
            AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
        if (!upload) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          upload->resource_handle = res->handle;
          upload->reserved0 = 0;
          upload->offset_bytes = 0;
          upload->size_bytes = res->size_bytes;
        }
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
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
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(pDesc->Format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = allocate_global_handle(dev->adapter);
    res->kind = ResourceKind::Texture2D;
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = mip_levels;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = pDesc->Format;
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (!min_row_bytes) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    res->row_pitch_bytes = min_row_bytes;

    const auto* cb = dev->device_callbacks;
    const bool can_alloc_backing = cb && cb->pfnAllocateBacking && cb->pfnMapAllocation && cb->pfnUnmapAllocation;
    if (can_alloc_backing) {
      AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
      uint64_t alloc_size_bytes = 0;
      uint32_t row_pitch_bytes = 0;
      const HRESULT hr = cb->pfnAllocateBacking(cb->pUserContext,
                                                pDesc,
                                                &alloc_handle,
                                                &alloc_size_bytes,
                                                &row_pitch_bytes);
      if (FAILED(hr) || alloc_handle == 0) {
        res->~AeroGpuResource();
        return FAILED(hr) ? hr : E_FAIL;
      }

      if (row_pitch_bytes) {
        res->row_pitch_bytes = row_pitch_bytes;
      }

      res->alloc_handle = alloc_handle;
      res->backing_alloc_id = static_cast<uint32_t>(alloc_handle);
      res->alloc_offset_bytes = 0;
      res->alloc_size_bytes = alloc_size_bytes;
      track_alloc_for_submit_locked(dev, alloc_handle);
    }

    uint32_t level_w = res->width ? res->width : 1u;
    uint32_t level_h = res->height ? res->height : 1u;
    uint64_t total_bytes = 0;
    for (uint32_t level = 0; level < res->mip_levels; ++level) {
      const uint32_t level_pitch = (level == 0) ? res->row_pitch_bytes : aerogpu_texture_min_row_pitch_bytes(aer_fmt, level_w);
      const uint32_t level_rows = aerogpu_texture_num_rows(aer_fmt, level_h);
      if (level_pitch == 0 || level_rows == 0) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      total_bytes += static_cast<uint64_t>(level_pitch) * static_cast<uint64_t>(level_rows);
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }

    if (res->alloc_handle != 0) {
      if (res->alloc_size_bytes == 0) {
        res->alloc_size_bytes = total_bytes;
      }
    } else {
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
    }

    const bool has_initial_data = (pDesc->pInitialData && pDesc->InitialDataCount);
    const bool is_guest_backed = (res->backing_alloc_id != 0);
    bool wddm_initial_upload = false;
    if (has_initial_data) {
      if (res->mip_levels != 1 || res->array_size != 1) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        AEROGPU_D3D10_RET_HR(E_NOTIMPL);
      }

      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
      const uint32_t src_pitch = init.SysMemPitch ? init.SysMemPitch : row_bytes;
      if (row_bytes == 0 || rows == 0 || src_pitch < row_bytes || res->row_pitch_bytes < row_bytes) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }


      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      uint8_t* dst = res->storage.empty() ? nullptr : res->storage.data();
      void* mapped = nullptr;
      if (!wddm_initial_upload && res->alloc_handle != 0) {
        HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &mapped);
        if (FAILED(hr) || !mapped) {
          res->~AeroGpuResource();
          AEROGPU_D3D10_RET_HR(FAILED(hr) ? hr : E_FAIL);
        }
        dst = static_cast<uint8_t*>(mapped) + res->alloc_offset_bytes;
      }
      if (!dst) {
        res->~AeroGpuResource();
        return E_FAIL;
      }

      for (uint32_t y = 0; y < rows; y++) {
        uint8_t* dst_row = dst + static_cast<size_t>(y) * res->row_pitch_bytes;
        std::memcpy(dst_row,
                    src + static_cast<size_t>(y) * src_pitch,
                    static_cast<size_t>(row_bytes));
        if (res->row_pitch_bytes > row_bytes) {
          std::memset(dst_row + static_cast<size_t>(row_bytes),
                      0,
                      static_cast<size_t>(res->row_pitch_bytes - row_bytes));
        }
      }
      if (mapped) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        wddm_initial_upload = true;
      }
    }

    AddLiveResourceLocked(dev, res);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      RemoveLiveResourceLocked(dev, res);
      res->handle = kInvalidHandle;
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = res->alloc_offset_bytes;
    cmd->reserved0 = 0;

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created tex2d handle=%u size=%ux%u row_pitch=%u",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->width),
                         static_cast<unsigned>(res->height),
                          static_cast<unsigned>(res->row_pitch_bytes));
#endif

    if (has_initial_data) {
      const uint64_t dirty_size = aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
      if (is_guest_backed) {
        if (!wddm_initial_upload) {
          res->~AeroGpuResource();
          return E_FAIL;
        }

        auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!dirty) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          dirty->resource_handle = res->handle;
          dirty->reserved0 = 0;
          dirty->offset_bytes = 0;
          dirty->size_bytes = dirty_size;
          track_resource_alloc_for_submit_locked(dev, res);
        }
      } else {
        auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
            AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
        if (!upload) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          upload->resource_handle = res->handle;
          upload->reserved0 = 0;
          upload->offset_bytes = 0;
          upload->size_bytes = res->storage.size();
        }
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  AEROGPU_D3D10_RET_HR(E_NOTIMPL);
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
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

  if (res->handle == kInvalidHandle) {
    return;
  }


  if (res->handle != kInvalidHandle) {
    // NOTE: For now we emit DESTROY_RESOURCE for both original resources and
    // shared-surface aliases. The host command processor is expected to
    // normalize alias lifetimes, but proper cross-process refcounting may be
    // needed later.
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    } else {
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
    }
  }
  RemoveLiveResourceLocked(dev, res);
  res->handle = kInvalidHandle;
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
    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return 0;
    }

    uint32_t level_w = res->width ? res->width : 1u;
    uint32_t level_h = res->height ? res->height : 1u;
    uint64_t total_bytes = 0;
    for (uint32_t level = 0; level < res->mip_levels; ++level) {
      const uint32_t level_pitch =
          (level == 0) ? res->row_pitch_bytes : aerogpu_texture_min_row_pitch_bytes(aer_fmt, level_w);
      const uint32_t level_rows = aerogpu_texture_num_rows(aer_fmt, level_h);
      if (level_pitch == 0 || level_rows == 0) {
        return 0;
      }
      const uint64_t level_size = static_cast<uint64_t>(level_pitch) * static_cast<uint64_t>(level_rows);
      if (level_size > UINT64_MAX - total_bytes) {
        return 0;
      }
      total_bytes += level_size;
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }

    const uint64_t array_layers = res->array_size ? static_cast<uint64_t>(res->array_size) : 1ull;
    if (total_bytes > UINT64_MAX / array_layers) {
      return 0;
    }
    return total_bytes * array_layers;
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

namespace {

template <typename T, typename = void>
struct HasField_Value : std::false_type {};
template <typename T>
struct HasField_Value<T, std::void_t<decltype(std::declval<T>().Value)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_ReadOnly : std::false_type {};
template <typename T>
struct HasField_ReadOnly<T, std::void_t<decltype(std::declval<T>().ReadOnly)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_WriteOnly : std::false_type {};
template <typename T>
struct HasField_WriteOnly<T, std::void_t<decltype(std::declval<T>().WriteOnly)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_Write : std::false_type {};
template <typename T>
struct HasField_Write<T, std::void_t<decltype(std::declval<T>().Write)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_Discard : std::false_type {};
template <typename T>
struct HasField_Discard<T, std::void_t<decltype(std::declval<T>().Discard)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_NoOverwrite : std::false_type {};
template <typename T>
struct HasField_NoOverwrite<T, std::void_t<decltype(std::declval<T>().NoOverwrite)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_NoOverWrite : std::false_type {};
template <typename T>
struct HasField_NoOverWrite<T, std::void_t<decltype(std::declval<T>().NoOverWrite)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_DoNotWait : std::false_type {};
template <typename T>
struct HasField_DoNotWait<T, std::void_t<decltype(std::declval<T>().DoNotWait)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_DonotWait : std::false_type {};
template <typename T>
struct HasField_DonotWait<T, std::void_t<decltype(std::declval<T>().DonotWait)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_Subresource : std::false_type {};
template <typename T>
struct HasField_Subresource<T, std::void_t<decltype(std::declval<T>().Subresource)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_SubresourceIndex : std::false_type {};
template <typename T>
struct HasField_SubresourceIndex<T, std::void_t<decltype(std::declval<T>().SubresourceIndex)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_SubResourceIndex : std::false_type {};
template <typename T>
struct HasField_SubResourceIndex<T, std::void_t<decltype(std::declval<T>().SubResourceIndex)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_Offset : std::false_type {};
template <typename T>
struct HasField_Offset<T, std::void_t<decltype(std::declval<T>().Offset)>> : std::true_type {};

template <typename T, typename = void>
struct HasField_Size : std::false_type {};
template <typename T>
struct HasField_Size<T, std::void_t<decltype(std::declval<T>().Size)>> : std::true_type {};

template <typename TLockFlags>
void SetLockFlagsFromMap(TLockFlags* flags, uint32_t map_type, uint32_t map_flags) {
  if (!flags) {
    return;
  }

  const bool do_not_wait = (map_flags & AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;

  if constexpr (std::is_integral_v<TLockFlags>) {
    *flags = static_cast<TLockFlags>(map_type | map_flags);
    return;
  }

  constexpr bool kHasAnyKnownFields =
      HasField_ReadOnly<TLockFlags>::value || HasField_WriteOnly<TLockFlags>::value || HasField_Write<TLockFlags>::value ||
      HasField_Discard<TLockFlags>::value || HasField_NoOverwrite<TLockFlags>::value || HasField_NoOverWrite<TLockFlags>::value ||
      HasField_DoNotWait<TLockFlags>::value || HasField_DonotWait<TLockFlags>::value;

  // If we don't understand the flag layout, fall back to writing a raw value
  // (some header revisions expose `Value`).
  if constexpr (!kHasAnyKnownFields) {
    if constexpr (HasField_Value<TLockFlags>::value) {
      flags->Value = map_type | map_flags;
    }
    return;
  }

  // Translate D3D11/D3D10 MapType to the runtime's LockCb flags.
  // See docs/graphics/win7-d3d11-map-unmap.md (§3).
  const bool read_only = (map_type == AEROGPU_DDI_MAP_READ);
  const bool write_only = (map_type == AEROGPU_DDI_MAP_WRITE ||
                           map_type == AEROGPU_DDI_MAP_WRITE_DISCARD ||
                           map_type == AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE);
  const bool discard = (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD);
  const bool no_overwrite = (map_type == AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE);

  if constexpr (HasField_ReadOnly<TLockFlags>::value) {
    flags->ReadOnly = read_only ? 1 : 0;
  }
  if constexpr (HasField_WriteOnly<TLockFlags>::value) {
    flags->WriteOnly = write_only ? 1 : 0;
  }
  if constexpr (HasField_Write<TLockFlags>::value) {
    flags->Write = write_only ? 1 : 0;
  }
  if constexpr (HasField_Discard<TLockFlags>::value) {
    flags->Discard = discard ? 1 : 0;
  }
  if constexpr (HasField_NoOverwrite<TLockFlags>::value) {
    flags->NoOverwrite = no_overwrite ? 1 : 0;
  }
  if constexpr (HasField_NoOverWrite<TLockFlags>::value) {
    flags->NoOverWrite = no_overwrite ? 1 : 0;
  }
  if constexpr (HasField_DoNotWait<TLockFlags>::value) {
    flags->DoNotWait = do_not_wait ? 1 : 0;
  }
  if constexpr (HasField_DonotWait<TLockFlags>::value) {
    flags->DonotWait = do_not_wait ? 1 : 0;
  }
}

template <typename TLock>
void SetLockSubresource(TLock* lock, uint32_t subresource) {
  if (!lock) {
    return;
  }
  if constexpr (HasField_Subresource<TLock>::value) {
    lock->Subresource = subresource;
  } else if constexpr (HasField_SubresourceIndex<TLock>::value) {
    lock->SubresourceIndex = subresource;
  } else if constexpr (HasField_SubResourceIndex<TLock>::value) {
    lock->SubResourceIndex = subresource;
  }
}

template <typename TUnlock>
void SetUnlockSubresource(TUnlock* unlock, uint32_t subresource) {
  if (!unlock) {
    return;
  }
  if constexpr (HasField_Subresource<TUnlock>::value) {
    unlock->Subresource = subresource;
  } else if constexpr (HasField_SubresourceIndex<TUnlock>::value) {
    unlock->SubresourceIndex = subresource;
  } else if constexpr (HasField_SubResourceIndex<TUnlock>::value) {
    unlock->SubResourceIndex = subresource;
  }
}

template <typename TLock>
void SetLockRange(TLock* lock, uint32_t offset, uint32_t size) {
  if (!lock) {
    return;
  }
  if constexpr (HasField_Offset<TLock>::value) {
    lock->Offset = offset;
  }
  if constexpr (HasField_Size<TLock>::value) {
    lock->Size = size;
  }
}

} // namespace

template <typename TMappedSubresource>
HRESULT map_resource_locked(AeroGpuDevice* dev,
                            AeroGpuResource* res,
                            uint32_t subresource,
                            uint32_t map_type,
                            uint32_t map_flags,
                              TMappedSubresource* pMapped) {
  if (!dev || !res || !pMapped) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }
  if (subresource != 0) {
    return E_INVALIDARG;
  }
  if ((map_flags & ~static_cast<uint32_t>(AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT)) != 0) {
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

  // Enforce D3D11 usage rules (mirrors the Win7 runtime validation). This keeps
  // the portable UMD's behavior aligned with the WDK build and the documented
  // contract in docs/graphics/win7-d3d11-map-unmap.md.
  switch (res->usage) {
    case kD3D11UsageDynamic:
      if (map_type != AEROGPU_DDI_MAP_WRITE_DISCARD && map_type != AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE) {
        return E_INVALIDARG;
      }
      break;
    case kD3D11UsageStaging: {
      const uint32_t access_mask = kD3D11CpuAccessRead | kD3D11CpuAccessWrite;
      const uint32_t access = res->cpu_access_flags & access_mask;
      if (access == kD3D11CpuAccessRead) {
        if (map_type != AEROGPU_DDI_MAP_READ) {
          return E_INVALIDARG;
        }
      } else if (access == kD3D11CpuAccessWrite) {
        if (map_type != AEROGPU_DDI_MAP_WRITE) {
          return E_INVALIDARG;
        }
      } else if (access == access_mask) {
        if (map_type != AEROGPU_DDI_MAP_READ && map_type != AEROGPU_DDI_MAP_WRITE &&
            map_type != AEROGPU_DDI_MAP_READ_WRITE) {
          return E_INVALIDARG;
        }
      } else {
        return E_INVALIDARG;
      }
      break;
    }
    default:
      return E_INVALIDARG;
  }

  if (want_read && (res->cpu_access_flags & kD3D11CpuAccessRead) == 0) {
    return E_INVALIDARG;
  }
  if (want_write && (res->cpu_access_flags & kD3D11CpuAccessWrite) == 0) {
    return E_INVALIDARG;
  }

  // Staging readback maps are synchronization points. For bring-up we conservatively
  // submit and wait for the latest fence whenever the CPU requests a read.
  if (want_read) {
    const bool do_not_wait = (map_flags & AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;
    HRESULT submit_hr = S_OK;
    const uint64_t submitted_fence = submit_locked(dev, &submit_hr);
    if (FAILED(submit_hr)) {
      return submit_hr;
    }
    const uint64_t last_fence = dev->last_submitted_fence.load(std::memory_order_relaxed);
    const uint64_t fence = submitted_fence > last_fence ? submitted_fence : last_fence;
    if (fence != 0) {
      if (do_not_wait) {
        const HRESULT wait_hr = AeroGpuWaitForFence(dev, fence, /*timeout_ms=*/0);
        if (wait_hr == kDxgiErrorWasStillDrawing) {
          return kDxgiErrorWasStillDrawing;
        }
        if (FAILED(wait_hr)) {
          return wait_hr;
        }
      } else {
        HRESULT wait_hr = AeroGpuWaitForFence(dev, fence, /*timeout_ms=*/kAeroGpuTimeoutMsInfinite);
        if (FAILED(wait_hr)) {
          return wait_hr;
        }
      }
    }
  }

  const uint64_t total = resource_total_bytes(res);
  if (!total) {
    return E_INVALIDARG;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);


  // Prefer mapping guest-backed resources via their WDDM allocation.
  if (is_guest_backed && res->alloc_handle != 0 && dev->device_callbacks && dev->device_callbacks->pfnMapAllocation &&
      dev->device_callbacks->pfnUnmapAllocation) {
    const auto* cb = dev->device_callbacks;
    void* cpu_ptr = nullptr;
    const HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &cpu_ptr);
    if (FAILED(hr) || !cpu_ptr) {
      return FAILED(hr) ? hr : E_FAIL;
    }

    res->mapped_via_allocation = true;
    res->mapped_ptr = cpu_ptr;

    uint8_t* data = static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes;
    if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD && total <= static_cast<uint64_t>(SIZE_MAX)) {
      // Discard contents are undefined; clear for deterministic tests.
      std::memset(data, 0, static_cast<size_t>(total));
    }

    pMapped->pData = data;
    if (res->kind == ResourceKind::Texture2D) {
      pMapped->RowPitch = res->row_pitch_bytes;
      const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
      pMapped->DepthPitch = static_cast<uint32_t>(aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height));
    } else {
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
    }

    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_map_type = map_type;
    res->mapped_offset_bytes = 0;
    res->mapped_size_bytes = total;
    return S_OK;
  }


  if (is_guest_backed) {
    // Guest-backed resources must be mapped via their backing allocation.
    return E_FAIL;
  }

  HRESULT hr = ensure_resource_storage(res, total);
  if (FAILED(hr)) {
    return hr;
  }

  if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD) {
    // Discard contents are undefined; clear for deterministic tests.
    std::memset(res->storage.data(), 0, res->storage.size());
  }

  res->mapped_via_allocation = false;
  res->mapped_ptr = nullptr;

  pMapped->pData = res->storage.data();
  if (res->kind == ResourceKind::Texture2D) {
    pMapped->RowPitch = res->row_pitch_bytes;
    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    pMapped->DepthPitch = static_cast<uint32_t>(aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height));
  } else {
    pMapped->RowPitch = 0;
    pMapped->DepthPitch = 0;
  }

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_map_type = map_type;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = total;
  return S_OK;
}

void unmap_resource_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, AeroGpuResource* res, uint32_t subresource) {
  if (!dev || !res) {
    return;
  }
  if (!res->mapped || subresource != res->mapped_subresource) {
    ReportDeviceErrorLocked(dev, hDevice, E_INVALIDARG);
    return;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);


  if (res->mapped_via_allocation) {
    if (dev->device_callbacks && dev->device_callbacks->pfnUnmapAllocation) {
      const auto* cb = dev->device_callbacks;
      cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
    }
  }


  if (res->mapped_write && res->handle != kInvalidHandle) {
    if (is_guest_backed) {
      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!dirty) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      } else {
        dirty->resource_handle = res->handle;
        dirty->reserved0 = 0;
        dirty->offset_bytes = res->mapped_offset_bytes;
        dirty->size_bytes = res->mapped_size_bytes;
        track_resource_alloc_for_submit_locked(dev, res);
      }
    } else {
      // Host-owned resource: inline the bytes into the command stream.
      if (res->mapped_offset_bytes + res->mapped_size_bytes <= static_cast<uint64_t>(res->storage.size())) {
        const auto offset = static_cast<size_t>(res->mapped_offset_bytes);
        const auto size = static_cast<size_t>(res->mapped_size_bytes);
        auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
            AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + offset, size);
        if (!upload) {
          ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        } else {
          upload->resource_handle = res->handle;
          upload->reserved0 = 0;
          upload->offset_bytes = res->mapped_offset_bytes;
          upload->size_bytes = res->mapped_size_bytes;
        }
      }
    }
  }


  res->mapped_via_allocation = false;
  res->mapped_ptr = nullptr;
  res->mapped = false;
  res->mapped_write = false;
  res->mapped_subresource = 0;
  res->mapped_map_type = 0;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = 0;
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
  if (res->usage != kD3D11UsageDynamic) {
    return E_INVALIDARG;
  }
  if ((res->cpu_access_flags & kD3D11CpuAccessWrite) == 0) {
    return E_INVALIDARG;
  }

  const uint64_t total = res->size_bytes;
  if (res->alloc_handle != 0 && dev->device_callbacks && dev->device_callbacks->pfnMapAllocation &&
      dev->device_callbacks->pfnUnmapAllocation) {
    const auto* cb = dev->device_callbacks;
    void* cpu_ptr = nullptr;
    HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &cpu_ptr);
    if (FAILED(hr) || !cpu_ptr) {
      return FAILED(hr) ? hr : E_FAIL;
    }
    res->mapped_via_allocation = true;
    res->mapped_ptr = cpu_ptr;

    auto* data = static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes;
    if (discard && total <= static_cast<uint64_t>(SIZE_MAX)) {
      // Discard contents are undefined; clear for deterministic tests.
      std::memset(data, 0, static_cast<size_t>(total));
    }
    *ppData = data;
  } else {
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

    res->mapped_via_allocation = false;
    res->mapped_ptr = nullptr;
    *ppData = res->storage.data();
  }

  res->mapped = true;
  res->mapped_write = true;
  res->mapped_subresource = 0;
  res->mapped_map_type = discard ? AEROGPU_DDI_MAP_WRITE_DISCARD : AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE;
  res->mapped_offset_bytes = 0;
  res->mapped_size_bytes = total;
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
  return map_resource_locked(dev, res, subresource, map_type, map_flags, pMapped);
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
  unmap_resource_locked(dev, hDevice, res, subresource);
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
  return map_dynamic_buffer_locked(dev, res, /*discard=*/true, ppData);
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
  return map_dynamic_buffer_locked(dev, res, /*discard=*/false, ppData);
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
  unmap_resource_locked(dev, hDevice, res, /*subresource=*/0);
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
  return map_dynamic_buffer_locked(dev, res, /*discard=*/true, ppData);
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
  unmap_resource_locked(dev, hDevice, res, /*subresource=*/0);
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
  if ((map_flags & ~static_cast<uint32_t>(AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT)) != 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
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
    if (res->bind_flags & kD3D11BindConstantBuffer) {
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
  } else if (map_type == AEROGPU_DDI_MAP_WRITE_NO_OVERWRITE) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
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

  if (res->kind == ResourceKind::Texture2D && res->bind_flags == 0) {
    return map_resource_locked(dev, res, subresource, map_type, map_flags, pMapped);
  }

  // Conservative: only support generic map on buffers and staging textures for now.
  if (res->kind == ResourceKind::Buffer) {
    return map_resource_locked(dev, res, subresource, map_type, map_flags, pMapped);
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
  unmap_resource_locked(dev, hDevice, res, subresource);
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

  if (dst_subresource != 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle == kInvalidHandle) {
    return;
  }

  const auto* cb = dev->device_callbacks;
  const bool allocation_backed = res->alloc_handle != 0 && cb && cb->pfnMapAllocation && cb->pfnUnmapAllocation;
  if (allocation_backed) {
    void* mapped = nullptr;
    const HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &mapped);
    if (FAILED(hr) || !mapped) {
      return;
    }

    uint8_t* dst = static_cast<uint8_t*>(mapped) + res->alloc_offset_bytes;

    if (res->kind == ResourceKind::Buffer) {
      if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }

      if (pDstBox) {
        if (pDstBox->top != 0 || pDstBox->bottom != 1 || pDstBox->front != 0 || pDstBox->back != 1) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        if (pDstBox->left >= pDstBox->right) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        const uint64_t offset = pDstBox->left;
        const uint64_t size = static_cast<uint64_t>(pDstBox->right) - static_cast<uint64_t>(pDstBox->left);
        if (offset + size > res->size_bytes || size > static_cast<uint64_t>(SIZE_MAX)) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        std::memcpy(dst + static_cast<size_t>(offset), pSysMem, static_cast<size_t>(size));
      } else {
        std::memcpy(dst, pSysMem, static_cast<size_t>(res->size_bytes));
      }
    } else if (res->kind == ResourceKind::Texture2D) {
      const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
      if (aer_fmt == AEROGPU_FORMAT_INVALID) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aer_fmt);
      const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      if (!layout.valid || min_row_bytes == 0 || res->row_pitch_bytes < min_row_bytes) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }

      uint32_t copy_left = 0;
      uint32_t copy_top = 0;
      uint32_t copy_right = res->width;
      uint32_t copy_bottom = res->height;
      if (pDstBox) {
        if (pDstBox->front != 0 || pDstBox->back != 1) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        if (pDstBox->left >= pDstBox->right || pDstBox->top >= pDstBox->bottom) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        if (pDstBox->right > res->width || pDstBox->bottom > res->height) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        copy_left = pDstBox->left;
        copy_top = pDstBox->top;
        copy_right = pDstBox->right;
        copy_bottom = pDstBox->bottom;
      }

      if (layout.block_width > 1 || layout.block_height > 1) {
        const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
          return (v % align) == 0 || v == extent;
        };
        if ((copy_left % layout.block_width) != 0 || (copy_top % layout.block_height) != 0 ||
            !aligned_or_edge(copy_right, layout.block_width, res->width) ||
            !aligned_or_edge(copy_bottom, layout.block_height, res->height)) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
      }

      const uint32_t block_left = copy_left / layout.block_width;
      const uint32_t block_top = copy_top / layout.block_height;
      const uint32_t block_right = aerogpu_div_round_up_u32(copy_right, layout.block_width);
      const uint32_t block_bottom = aerogpu_div_round_up_u32(copy_bottom, layout.block_height);
      if (block_right < block_left || block_bottom < block_top) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }

      const uint32_t copy_width_blocks = block_right - block_left;
      const uint32_t copy_height_blocks = block_bottom - block_top;
      const uint64_t row_bytes_u64 =
          static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(layout.bytes_per_block);
      if (row_bytes_u64 == 0 || row_bytes_u64 > UINT32_MAX || copy_height_blocks == 0) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const uint32_t row_bytes = static_cast<uint32_t>(row_bytes_u64);
      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(row_bytes);
      if (static_cast<size_t>(row_bytes) > src_pitch) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
      const uint64_t dst_x_bytes_u64 =
          static_cast<uint64_t>(block_left) * static_cast<uint64_t>(layout.bytes_per_block);
      if (dst_x_bytes_u64 > static_cast<uint64_t>(res->row_pitch_bytes) ||
          static_cast<uint64_t>(res->row_pitch_bytes) - dst_x_bytes_u64 < static_cast<uint64_t>(row_bytes)) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const size_t dst_x_bytes = static_cast<size_t>(dst_x_bytes_u64);
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        uint8_t* dst_row = dst + (static_cast<size_t>(block_top) + y) * res->row_pitch_bytes + dst_x_bytes;
        std::memcpy(dst_row, src + y * src_pitch, row_bytes);
      }

      // If this is a full upload, also clear any per-row padding to keep guest
      // memory deterministic for host-side uploads.
      if (!pDstBox && res->row_pitch_bytes > row_bytes) {
        const uint32_t total_rows = aerogpu_texture_num_rows(aer_fmt, res->height);
        for (uint32_t y = 0; y < total_rows; y++) {
          uint8_t* dst_row = dst + static_cast<size_t>(y) * res->row_pitch_bytes;
          std::memset(dst_row + row_bytes, 0, res->row_pitch_bytes - row_bytes);
        }
      }
    } else {
      cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
      return;
    }

    cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);

    auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!dirty) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
    dirty->resource_handle = res->handle;
    dirty->reserved0 = 0;
    dirty->offset_bytes = 0;
    dirty->size_bytes = resource_total_bytes(res);
    track_resource_alloc_for_submit_locked(dev, res);
    return;
  }

  // Host-owned resources: inline data into the command stream.
  if (!pDstBox) {
    if (res->kind == ResourceKind::Buffer) {
      if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return;
      }
      HRESULT hr = ensure_resource_storage(res, res->size_bytes);
      if (FAILED(hr) || res->storage.size() < static_cast<size_t>(res->size_bytes)) {
        return;
      }
      std::memcpy(res->storage.data(), pSysMem, static_cast<size_t>(res->size_bytes));

      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, pSysMem, static_cast<size_t>(res->size_bytes));
      if (!upload) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        return;
      }
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = res->size_bytes;
      return;
    }

    if (res->kind == ResourceKind::Texture2D) {
      const uint32_t aerogpu_format = dxgi_format_to_aerogpu(res->dxgi_format);
      const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, res->width);
      const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, res->height);
      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(row_bytes);
      if (!layout.valid || row_bytes == 0 || rows == 0 || static_cast<size_t>(row_bytes) > src_pitch ||
          row_bytes > res->row_pitch_bytes) {
        return;
      }

      const uint64_t total = resource_total_bytes(res);
      if (!total || total > static_cast<uint64_t>(SIZE_MAX)) {
        return;
      }
      HRESULT hr = ensure_resource_storage(res, total);
      if (FAILED(hr) || res->storage.size() < static_cast<size_t>(total)) {
        return;
      }

      const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
      for (uint32_t y = 0; y < rows; y++) {
        uint8_t* dst_row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        std::memcpy(dst_row, src + static_cast<size_t>(y) * src_pitch, static_cast<size_t>(row_bytes));
        if (res->row_pitch_bytes > row_bytes) {
          std::memset(dst_row + static_cast<size_t>(row_bytes), 0, res->row_pitch_bytes - row_bytes);
        }
      }

      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), static_cast<size_t>(total));
      if (!upload) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        return;
      }
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = total;
      return;
    }

    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    if (pDstBox->top != 0 || pDstBox->bottom != 1 || pDstBox->front != 0 || pDstBox->back != 1) {
      return;
    }
    if (pDstBox->left >= pDstBox->right) {
      return;
    }
    const uint64_t offset = pDstBox->left;
    const uint64_t size = static_cast<uint64_t>(pDstBox->right) - static_cast<uint64_t>(pDstBox->left);
    if (offset + size > res->size_bytes) {
      return;
    }

    HRESULT hr = ensure_resource_storage(res, res->size_bytes);
    if (FAILED(hr) || res->storage.size() < static_cast<size_t>(res->size_bytes)) {
      return;
    }
    std::memcpy(res->storage.data() + static_cast<size_t>(offset), pSysMem, static_cast<size_t>(size));

    auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, pSysMem, static_cast<size_t>(size));
    if (!upload) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = offset;
    upload->size_bytes = size;
    return;
  }

  if (res->kind == ResourceKind::Texture2D) {
    if (pDstBox->front != 0 || pDstBox->back != 1) {
      return;
    }
    if (pDstBox->left >= pDstBox->right || pDstBox->top >= pDstBox->bottom) {
      return;
    }
    if (pDstBox->right > res->width || pDstBox->bottom > res->height) {
      return;
    }

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(res->dxgi_format);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, res->width);
    if (!layout.valid || min_row_bytes == 0 || res->row_pitch_bytes < min_row_bytes) {
      return;
    }

    if (layout.block_width > 1 || layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((pDstBox->left % layout.block_width) != 0 || (pDstBox->top % layout.block_height) != 0 ||
          !aligned_or_edge(pDstBox->right, layout.block_width, res->width) ||
          !aligned_or_edge(pDstBox->bottom, layout.block_height, res->height)) {
        return;
      }
    }

    const uint32_t block_left = pDstBox->left / layout.block_width;
    const uint32_t block_top = pDstBox->top / layout.block_height;
    const uint32_t block_right = aerogpu_div_round_up_u32(pDstBox->right, layout.block_width);
    const uint32_t block_bottom = aerogpu_div_round_up_u32(pDstBox->bottom, layout.block_height);
    if (block_right < block_left || block_bottom < block_top) {
      return;
    }

    const uint32_t copy_width_blocks = block_right - block_left;
    const uint32_t copy_height_blocks = block_bottom - block_top;
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > SIZE_MAX || row_bytes_u64 > UINT32_MAX || copy_height_blocks == 0) {
      return;
    }
    const size_t row_bytes = static_cast<size_t>(row_bytes_u64);

    const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : row_bytes;
    if (row_bytes > src_pitch) {
      return;
    }

    const uint64_t total = resource_total_bytes(res);
    if (!total) {
      return;
    }
    HRESULT hr = ensure_resource_storage(res, total);
    if (FAILED(hr) || res->storage.size() < static_cast<size_t>(total)) {
      return;
    }

    const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
    const size_t dst_pitch = static_cast<size_t>(res->row_pitch_bytes);
    const size_t dst_x_bytes = static_cast<size_t>(block_left) * static_cast<size_t>(layout.bytes_per_block);
    for (uint32_t y = 0; y < copy_height_blocks; ++y) {
      const size_t dst_offset = (static_cast<size_t>(block_top) + y) * dst_pitch + dst_x_bytes;
      std::memcpy(res->storage.data() + dst_offset, src + y * src_pitch, row_bytes);
    }

    // The browser executor currently only supports partial UPLOAD_RESOURCE updates for
    // tightly packed textures (row_pitch_bytes == width*4). When the texture has per-row
    // padding, keep the command stream compatible by uploading the entire texture.
    const size_t tight_row_bytes = static_cast<size_t>(min_row_bytes);
    size_t upload_offset = static_cast<size_t>(block_top) * dst_pitch;
    size_t upload_size = static_cast<size_t>(copy_height_blocks) * dst_pitch;
    if (dst_pitch != tight_row_bytes) {
      upload_offset = 0;
      upload_size = res->storage.size();
    }
    if (upload_offset > res->storage.size() || upload_size > res->storage.size() - upload_offset) {
      return;
    }
    auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + upload_offset, upload_size);
    if (!upload) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = upload_offset;
    upload->size_bytes = upload_size;
    return;
  }
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

  track_resource_alloc_for_submit_locked(dev, dst);
  track_resource_alloc_for_submit_locked(dev, src);

  struct CopySimMapping {
    uint8_t* data = nullptr;
    bool mapped_allocation = false;
    AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
  };

  auto map_copy_sim = [&](AeroGpuResource* r, uint64_t required_bytes) -> CopySimMapping {
    CopySimMapping m{};
    if (!dev || !r || !required_bytes) {
      return m;
    }

    const auto* cb = dev->device_callbacks;
    const bool can_map_allocation = cb && cb->pfnMapAllocation && cb->pfnUnmapAllocation;
    if (can_map_allocation && r->alloc_handle != 0) {
      void* base = nullptr;
      const HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, r->alloc_handle, &base);
      if (SUCCEEDED(hr) && base) {
        const uint64_t offset = static_cast<uint64_t>(r->alloc_offset_bytes);
        if (r->alloc_size_bytes != 0 && required_bytes + offset > r->alloc_size_bytes) {
          cb->pfnUnmapAllocation(cb->pUserContext, r->alloc_handle);
          return m;
        }

        m.data = static_cast<uint8_t*>(base) + r->alloc_offset_bytes;
        m.mapped_allocation = true;
        m.alloc_handle = r->alloc_handle;
        return m;
      }
      if (SUCCEEDED(hr) && !base) {
        cb->pfnUnmapAllocation(cb->pUserContext, r->alloc_handle);
      }
    }

    HRESULT hr = ensure_resource_storage(r, required_bytes);
    if (FAILED(hr) || r->storage.size() < static_cast<size_t>(required_bytes)) {
      return m;
    }
    m.data = r->storage.data();
    return m;
  };

  auto unmap_copy_sim = [&](const CopySimMapping& m) {
    if (!m.mapped_allocation || m.alloc_handle == 0) {
      return;
    }
    const auto* cb = dev->device_callbacks;
    if (cb && cb->pfnUnmapAllocation) {
      cb->pfnUnmapAllocation(cb->pUserContext, m.alloc_handle);
    }
  };

  // Repository builds keep a conservative CPU backing store; simulate the copy
  // immediately so a subsequent staging Map(READ) sees the bytes. For
  // allocation-backed resources, write directly into the backing allocation.
  if (dst->kind == ResourceKind::Buffer) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    cmd->size_bytes = std::min(dst->size_bytes, src->size_bytes);
    cmd->flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint64_t copy_bytes_u64 = cmd->size_bytes;
    if (copy_bytes_u64 != 0 && copy_bytes_u64 <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t copy_bytes = static_cast<size_t>(copy_bytes_u64);
      CopySimMapping src_map = map_copy_sim(src, copy_bytes_u64);
      CopySimMapping dst_map = map_copy_sim(dst, copy_bytes_u64);
      if (src_map.data && dst_map.data) {
        std::memcpy(dst_map.data, src_map.data, copy_bytes);
      }
      unmap_copy_sim(dst_map);
      unmap_copy_sim(src_map);
    }
  } else if (dst->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0) {
      return;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
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
    cmd->flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(src->dxgi_format);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
    const uint32_t row_bytes_u32 = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, cmd->width);
    const uint32_t copy_rows_u32 = aerogpu_texture_num_rows(aerogpu_format, cmd->height);
    if (!layout.valid || row_bytes_u32 == 0 || copy_rows_u32 == 0) {
      return;
    }
    const size_t row_bytes = static_cast<size_t>(row_bytes_u32);
    const size_t copy_rows = static_cast<size_t>(copy_rows_u32);

    if (row_bytes > dst->row_pitch_bytes || row_bytes > src->row_pitch_bytes) {
      return;
    }

    const uint64_t dst_required_u64 = static_cast<uint64_t>(dst->row_pitch_bytes) * static_cast<uint64_t>(copy_rows);
    const uint64_t src_required_u64 = static_cast<uint64_t>(src->row_pitch_bytes) * static_cast<uint64_t>(copy_rows);
    if (dst_required_u64 == 0 || src_required_u64 == 0 || dst_required_u64 > static_cast<uint64_t>(SIZE_MAX) ||
        src_required_u64 > static_cast<uint64_t>(SIZE_MAX)) {
      return;
    }

    CopySimMapping src_map = map_copy_sim(src, src_required_u64);
    CopySimMapping dst_map = map_copy_sim(dst, dst_required_u64);
    if (src_map.data && dst_map.data) {
      const size_t dst_pitch = static_cast<size_t>(dst->row_pitch_bytes);
      const size_t src_pitch = static_cast<size_t>(src->row_pitch_bytes);
      const size_t dst_tight_row_bytes =
          static_cast<size_t>(aerogpu_texture_min_row_pitch_bytes(aerogpu_format, dst->width));
      for (size_t y = 0; y < copy_rows; y++) {
        uint8_t* dst_row = dst_map.data + y * dst_pitch;
        const uint8_t* src_row = src_map.data + y * src_pitch;
        std::memcpy(dst_row, src_row, row_bytes);
        if (dst_pitch > dst_tight_row_bytes) {
          std::memset(dst_row + dst_tight_row_bytes, 0, dst_pitch - dst_tight_row_bytes);
        }
      }
    }
    unmap_copy_sim(dst_map);
    unmap_copy_sim(src_map);
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

  track_resource_alloc_for_submit_locked(dev, dst);
  track_resource_alloc_for_submit_locked(dev, src);

  struct CopySimMapping {
    uint8_t* data = nullptr;
    bool mapped_allocation = false;
    AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
  };

  auto map_copy_sim = [&](AeroGpuResource* r, uint64_t required_bytes) -> CopySimMapping {
    CopySimMapping m{};
    if (!dev || !r || !required_bytes) {
      return m;
    }

    const auto* cb = dev->device_callbacks;
    const bool can_map_allocation = cb && cb->pfnMapAllocation && cb->pfnUnmapAllocation;
    if (can_map_allocation && r->alloc_handle != 0) {
      void* base = nullptr;
      const HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, r->alloc_handle, &base);
      if (SUCCEEDED(hr) && base) {
        const uint64_t offset = static_cast<uint64_t>(r->alloc_offset_bytes);
        if (r->alloc_size_bytes != 0 && required_bytes + offset > r->alloc_size_bytes) {
          cb->pfnUnmapAllocation(cb->pUserContext, r->alloc_handle);
          return m;
        }

        m.data = static_cast<uint8_t*>(base) + r->alloc_offset_bytes;
        m.mapped_allocation = true;
        m.alloc_handle = r->alloc_handle;
        return m;
      }
      if (SUCCEEDED(hr) && !base) {
        cb->pfnUnmapAllocation(cb->pUserContext, r->alloc_handle);
      }
    }

    HRESULT hr = ensure_resource_storage(r, required_bytes);
    if (FAILED(hr) || r->storage.size() < static_cast<size_t>(required_bytes)) {
      return m;
    }
    m.data = r->storage.data();
    return m;
  };

  auto unmap_copy_sim = [&](const CopySimMapping& m) {
    if (!m.mapped_allocation || m.alloc_handle == 0) {
      return;
    }
    const auto* cb = dev->device_callbacks;
    if (cb && cb->pfnUnmapAllocation) {
      cb->pfnUnmapAllocation(cb->pUserContext, m.alloc_handle);
    }
  };

  if (dst->kind == ResourceKind::Buffer) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    cmd->size_bytes = std::min(dst->size_bytes, src->size_bytes);
    cmd->flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint64_t copy_bytes_u64 = cmd->size_bytes;
    if (copy_bytes_u64 != 0 && copy_bytes_u64 <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t copy_bytes = static_cast<size_t>(copy_bytes_u64);
      CopySimMapping src_map = map_copy_sim(src, copy_bytes_u64);
      CopySimMapping dst_map = map_copy_sim(dst, copy_bytes_u64);
      if (src_map.data && dst_map.data) {
        std::memcpy(dst_map.data, src_map.data, copy_bytes);
      }
      unmap_copy_sim(dst_map);
      unmap_copy_sim(src_map);
    }
  } else if (dst->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0) {
      return E_INVALIDARG;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
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
    cmd->flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(src->dxgi_format);
    const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
    const uint32_t row_bytes_u32 = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, cmd->width);
    const uint32_t copy_rows_u32 = aerogpu_texture_num_rows(aerogpu_format, cmd->height);
    if (!layout.valid || row_bytes_u32 == 0 || copy_rows_u32 == 0) {
      return S_OK;
    }
    const size_t row_bytes = static_cast<size_t>(row_bytes_u32);
    const size_t copy_rows = static_cast<size_t>(copy_rows_u32);

    if (row_bytes > dst->row_pitch_bytes || row_bytes > src->row_pitch_bytes) {
      return S_OK;
    }

    const uint64_t dst_required_u64 = static_cast<uint64_t>(dst->row_pitch_bytes) * static_cast<uint64_t>(copy_rows);
    const uint64_t src_required_u64 = static_cast<uint64_t>(src->row_pitch_bytes) * static_cast<uint64_t>(copy_rows);
    if (dst_required_u64 == 0 || src_required_u64 == 0 || dst_required_u64 > static_cast<uint64_t>(SIZE_MAX) ||
        src_required_u64 > static_cast<uint64_t>(SIZE_MAX)) {
      return S_OK;
    }

    CopySimMapping src_map = map_copy_sim(src, src_required_u64);
    CopySimMapping dst_map = map_copy_sim(dst, dst_required_u64);
    if (src_map.data && dst_map.data) {
      const size_t dst_pitch = static_cast<size_t>(dst->row_pitch_bytes);
      const size_t src_pitch = static_cast<size_t>(src->row_pitch_bytes);
      const size_t dst_tight_row_bytes =
          static_cast<size_t>(aerogpu_texture_min_row_pitch_bytes(aerogpu_format, dst->width));
      for (size_t y = 0; y < copy_rows; y++) {
        uint8_t* dst_row = dst_map.data + y * dst_pitch;
        const uint8_t* src_row = src_map.data + y * src_pitch;
        std::memcpy(dst_row, src_row, row_bytes);
        if (dst_pitch > dst_tight_row_bytes) {
          std::memset(dst_row + dst_tight_row_bytes, 0, dst_pitch - dst_tight_row_bytes);
        }
      }
    }
    unmap_copy_sim(dst_map);
    unmap_copy_sim(src_map);
  }

  return S_OK;
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADER*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateShaderSize");
  return sizeof(AeroGpuShader);
}

static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                  D3D10DDI_HSHADER hShader,
                                  uint32_t stage) {
  AEROGPU_D3D10_TRACEF("CreateShader stage=%u codeSize=%u", stage, pDesc ? pDesc->CodeSize : 0);
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate || !pDesc->pCode || !pDesc->CodeSize) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = stage;
  try {
    sh->dxbc.resize(pDesc->CodeSize);
  } catch (...) {
    sh->handle = kInvalidHandle;
    sh->~AeroGpuShader();
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  std::memcpy(sh->dxbc.data(), pDesc->pCode, pDesc->CodeSize);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  if (!cmd) {
    sh->handle = kInvalidHandle;
    sh->~AeroGpuShader();
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                            D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateVertexShader codeSize=%u", pDesc ? pDesc->CodeSize : 0);
  const HRESULT hr = CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_VERTEX);
  AEROGPU_D3D10_RET_HR(hr);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                           D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreatePixelShader codeSize=%u", pDesc ? pDesc->CodeSize : 0);
  const HRESULT hr = CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_PIXEL);
  AEROGPU_D3D10_RET_HR(hr);
}

void AEROGPU_APIENTRY DestroyShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
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
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    } else {
      cmd->shader_handle = sh->handle;
      cmd->reserved0 = 0;
    }
  }
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateInputLayoutSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEINPUTLAYOUT*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateInputLayoutSize");
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateInputLayout(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATEINPUTLAYOUT* pDesc,
                                           D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateInputLayout elements=%u", pDesc ? pDesc->NumElements : 0);
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate || (!pDesc->NumElements && pDesc->pElements)) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = allocate_global_handle(dev->adapter);

  const size_t blob_size = sizeof(aerogpu_input_layout_blob_header) +
                           static_cast<size_t>(pDesc->NumElements) * sizeof(aerogpu_input_layout_element_dxgi);
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    layout->handle = kInvalidHandle;
    layout->~AeroGpuInputLayout();
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }

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
  if (!cmd) {
    layout->handle = kInvalidHandle;
    layout->~AeroGpuInputLayout();
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyInputLayout hDevice=%p hLayout=%p", hDevice.pDrvPrivate, hLayout.pDrvPrivate);
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
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    } else {
      cmd->input_layout_handle = layout->handle;
      cmd->reserved0 = 0;
    }
  }
  layout->~AeroGpuInputLayout();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERENDERTARGETVIEW*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateRTVSize");
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRTV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                   D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateRTV hDevice=%p hResource=%p",
                       hDevice.pDrvPrivate,
                       pDesc ? pDesc->hResource.pDrvPrivate : nullptr);
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->resource = res;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyRTV(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyRTV hRtv=%p", hRtv.pDrvPrivate);
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateDSVSize");
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDSV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                   D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateDSV hDevice=%p hResource=%p",
                       hDevice.pDrvPrivate,
                       pDesc ? pDesc->hResource.pDrvPrivate : nullptr);
  if (!hDevice.pDrvPrivate || !pDesc || !hDsv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->resource = res;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyDSV(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyDSV hDsv=%p", hDsv.pDrvPrivate);
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  dsv->~AeroGpuDepthStencilView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
                                                  const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                  D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hDevice.pDrvPrivate || !pDesc || !hView.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  if (!res || res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }
  if (pDesc->ViewDimension && pDesc->ViewDimension != AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D) {
    return E_NOTIMPL;
  }
  if (pDesc->MostDetailedMip != 0) {
    return E_NOTIMPL;
  }
  const uint32_t mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
  if (mip_levels != 1 || res->mip_levels != 1 || res->array_size != 1) {
    return E_NOTIMPL;
  }

  auto* view = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  view->texture = res->handle;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView(D3D10DDI_HDEVICE, D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  view->~AeroGpuShaderResourceView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                                       const AEROGPU_DDIARG_CREATESAMPLER* pDesc,
                                       D3D10DDI_HSAMPLER hSampler) {
  if (!hDevice.pDrvPrivate || !pDesc || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* s = new (hSampler.pDrvPrivate) AeroGpuSampler();
  s->handle = dev->adapter->next_handle.fetch_add(1);
  s->filter = d3d11_filter_to_aerogpu(pDesc->Filter);
  s->address_u = d3d11_address_mode_to_aerogpu(pDesc->AddressU);
  s->address_v = d3d11_address_mode_to_aerogpu(pDesc->AddressV);
  s->address_w = d3d11_address_mode_to_aerogpu(pDesc->AddressW);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    s->handle = 0;
    s->~AeroGpuSampler();
    return E_OUTOFMEMORY;
  }
  cmd->sampler_handle = s->handle;
  cmd->filter = s->filter;
  cmd->address_u = s->address_u;
  cmd->address_v = s->address_v;
  cmd->address_w = s->address_w;
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSAMPLER hSampler) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  if (!dev || !s) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (s->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_sampler>(AEROGPU_CMD_DESTROY_SAMPLER);
    if (!cmd) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    } else {
      cmd->sampler_handle = s->handle;
      cmd->reserved0 = 0;
    }
  }
  s->~AeroGpuSampler();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEBLENDSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateBlendStateSize");
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const AEROGPU_DDIARG_CREATEBLENDSTATE*,
                                          D3D10DDI_HBLENDSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateBlendState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyBlendState hState=%p", hState.pDrvPrivate);
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERASTERIZERSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateRasterizerStateSize");
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const AEROGPU_DDIARG_CREATERASTERIZERSTATE*,
                                               D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateRasterizerState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyRasterizerState hState=%p", hState.pDrvPrivate);
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE,
                                                         const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateDepthStencilStateSize");
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateDepthStencilState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyDepthStencilState hState=%p", hState.pDrvPrivate);
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
  AEROGPU_D3D10_TRACEF_VERBOSE("SetRenderTargets hDevice=%p hRtv=%p hDsv=%p",
                               hDevice.pDrvPrivate,
                               hRtv.pDrvPrivate,
                               hDsv.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  AeroGpuResource* rtv_res = nullptr;
  AeroGpuResource* dsv_res = nullptr;
  if (hRtv.pDrvPrivate) {
    rtv_res = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv)->resource;
  }
  if (hDsv.pDrvPrivate) {
    dsv_res = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->resource;
  }

  if (!set_render_targets_locked(dev, hDevice, rtv_res, dsv_res)) {
    return;
  }
}

void AEROGPU_APIENTRY ClearRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW, const float rgba[4]) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("ClearRTV hDevice=%p rgba=[%f %f %f %f]",
                               hDevice.pDrvPrivate,
                               rgba ? rgba[0] : 0.0f,
                               rgba ? rgba[1] : 0.0f,
                               rgba ? rgba[2] : 0.0f,
                               rgba ? rgba[3] : 0.0f);
  if (!hDevice.pDrvPrivate || !rgba) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
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
  AEROGPU_D3D10_TRACEF_VERBOSE("ClearDSV hDevice=%p flags=0x%x depth=%f stencil=%u",
                               hDevice.pDrvPrivate,
                               clear_flags,
                               depth,
                               static_cast<uint32_t>(stencil));
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
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
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
  AEROGPU_D3D10_TRACEF_VERBOSE("SetInputLayout hDevice=%p hLayout=%p",
                               hDevice.pDrvPrivate,
                               hLayout.pDrvPrivate);
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

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
  dev->current_input_layout = handle;
}

void AEROGPU_APIENTRY SetVertexBuffer(D3D10DDI_HDEVICE hDevice,
                                      D3D10DDI_HRESOURCE hBuffer,
                                      uint32_t stride,
                                      uint32_t offset) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetVertexBuffer hDevice=%p hBuffer=%p stride=%u offset=%u",
                               hDevice.pDrvPrivate,
                               hBuffer.pDrvPrivate,
                               stride,
                               offset);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_vertex_buffer_binding binding{};
  AEROGPU_WDDM_ALLOCATION_HANDLE vb_alloc = 0;
  if (hBuffer.pDrvPrivate) {
    auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer);
    binding.buffer = res ? res->handle : 0;
    vb_alloc = res ? res->alloc_handle : 0;
  } else {
    binding.buffer = 0;
  }
  binding.stride_bytes = stride;
  binding.offset_bytes = offset;
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS,
                                                                           &binding,
                                                                           sizeof(binding));
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->start_slot = 0;
  cmd->buffer_count = 1;
  dev->current_vb_alloc = vb_alloc;
  track_alloc_for_submit_locked(dev, vb_alloc);
}

void AEROGPU_APIENTRY SetIndexBuffer(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hBuffer, uint32_t format, uint32_t offset) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetIndexBuffer hDevice=%p hBuffer=%p fmt=%u offset=%u",
                               hDevice.pDrvPrivate,
                               hBuffer.pDrvPrivate,
                               format,
                               offset);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  AEROGPU_WDDM_ALLOCATION_HANDLE ib_alloc = 0;
  aerogpu_handle_t ib_handle = 0;
  if (hBuffer.pDrvPrivate) {
    auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer);
    ib_handle = res ? res->handle : 0;
    ib_alloc = res ? res->alloc_handle : 0;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->buffer = ib_handle;
  cmd->format = dxgi_index_format_to_aerogpu(format);
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
  dev->current_ib_alloc = ib_alloc;
  track_alloc_for_submit_locked(dev, ib_alloc);
}

void AEROGPU_APIENTRY SetViewport(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDI_VIEWPORT* pVp) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetViewport hDevice=%p x=%f y=%f w=%f h=%f",
                               hDevice.pDrvPrivate,
                               pVp ? pVp->TopLeftX : 0.0f,
                               pVp ? pVp->TopLeftY : 0.0f,
                               pVp ? pVp->Width : 0.0f,
                               pVp ? pVp->Height : 0.0f);
  if (!hDevice.pDrvPrivate || !pVp) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->x_f32 = f32_bits(pVp->TopLeftX);
  cmd->y_f32 = f32_bits(pVp->TopLeftY);
  cmd->width_f32 = f32_bits(pVp->Width);
  cmd->height_f32 = f32_bits(pVp->Height);
  cmd->min_depth_f32 = f32_bits(pVp->MinDepth);
  cmd->max_depth_f32 = f32_bits(pVp->MaxDepth);
}

void AEROGPU_APIENTRY SetDrawState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hVs, D3D10DDI_HSHADER hPs) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetDrawState hDevice=%p hVs=%p hPs=%p",
                               hDevice.pDrvPrivate,
                               hVs.pDrvPrivate,
                               hPs.pDrvPrivate);
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

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vs = vs;
  cmd->ps = ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
  dev->current_vs = vs;
  dev->current_ps = ps;
}

void AEROGPU_APIENTRY SetBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetBlendState");
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetRasterizerState");
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetDepthStencilState");
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetPrimitiveTopology(D3D10DDI_HDEVICE hDevice, uint32_t topology) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetPrimitiveTopology hDevice=%p topology=%u", hDevice.pDrvPrivate, topology);
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

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->topology = topology;
  cmd->reserved0 = 0;
  dev->current_topology = topology;
}

void AEROGPU_APIENTRY VsSetConstantBuffers(D3D10DDI_HDEVICE hDevice,
                                           uint32_t start_slot,
                                           uint32_t buffer_count,
                                           const D3D10DDI_HRESOURCE* pBuffers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxConstantBufferSlots) {
    return;
  }
  uint32_t count = buffer_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxConstantBufferSlots) {
    count = kMaxConstantBufferSlots - start_slot;
  }

  std::vector<aerogpu_constant_buffer_binding> bindings(count);
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_constant_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.reserved0 = 0;

    if (pBuffers && pBuffers[i].pDrvPrivate) {
      auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pBuffers[i]);
      if (res && res->kind == ResourceKind::Buffer) {
        b.buffer = res->handle;
        b.offset_bytes = 0;
        b.size_bytes = (res->size_bytes > 0xFFFFFFFFull) ? 0xFFFFFFFFu : static_cast<uint32_t>(res->size_bytes);
      }
    }

    bindings[i] = b;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings.data(), bindings.size() * sizeof(bindings[0]));
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
  cmd->start_slot = start_slot;
  cmd->buffer_count = count;
  cmd->reserved0 = 0;
  for (uint32_t i = 0; i < count; i++) {
    dev->vs_constant_buffers[start_slot + i] = bindings[i];
  }
}

void AEROGPU_APIENTRY PsSetConstantBuffers(D3D10DDI_HDEVICE hDevice,
                                           uint32_t start_slot,
                                           uint32_t buffer_count,
                                           const D3D10DDI_HRESOURCE* pBuffers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxConstantBufferSlots) {
    return;
  }
  uint32_t count = buffer_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxConstantBufferSlots) {
    count = kMaxConstantBufferSlots - start_slot;
  }

  std::vector<aerogpu_constant_buffer_binding> bindings(count);
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_constant_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.reserved0 = 0;

    if (pBuffers && pBuffers[i].pDrvPrivate) {
      auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pBuffers[i]);
      if (res && res->kind == ResourceKind::Buffer) {
        b.buffer = res->handle;
        b.offset_bytes = 0;
        b.size_bytes = (res->size_bytes > 0xFFFFFFFFull) ? 0xFFFFFFFFu : static_cast<uint32_t>(res->size_bytes);
      }
    }

    bindings[i] = b;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings.data(), bindings.size() * sizeof(bindings[0]));
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->start_slot = start_slot;
  cmd->buffer_count = count;
  cmd->reserved0 = 0;
  for (uint32_t i = 0; i < count; i++) {
    dev->ps_constant_buffers[start_slot + i] = bindings[i];
  }
}

void AEROGPU_APIENTRY VsSetShaderResources(D3D10DDI_HDEVICE hDevice,
                                           uint32_t start_slot,
                                           uint32_t view_count,
                                           const D3D10DDI_HSHADERRESOURCEVIEW* pViews) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxShaderResourceSlots) {
    return;
  }
  uint32_t count = view_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxShaderResourceSlots) {
    count = kMaxShaderResourceSlots - start_slot;
  }

  AeroGpuResource* new_rtv = dev->current_rtv;
  AeroGpuResource* new_dsv = dev->current_dsv;
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      tex = view ? view->texture : 0;
    }
    if (tex && new_rtv && tex == new_rtv->handle) {
      new_rtv = nullptr;
    }
    if (tex && new_dsv && tex == new_dsv->handle) {
      new_dsv = nullptr;
    }
  }
  if (new_rtv != dev->current_rtv || new_dsv != dev->current_dsv) {
    if (!set_render_targets_locked(dev, hDevice, new_rtv, new_dsv)) {
      return;
    }
  }

  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      tex = view ? view->texture : 0;
    }
    const uint32_t slot = start_slot + i;
    if (dev->vs_srvs[slot] == tex) {
      continue;
    }
    if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, tex)) {
      return;
    }
    dev->vs_srvs[slot] = tex;
  }
}

void AEROGPU_APIENTRY PsSetShaderResources(D3D10DDI_HDEVICE hDevice,
                                           uint32_t start_slot,
                                           uint32_t view_count,
                                           const D3D10DDI_HSHADERRESOURCEVIEW* pViews) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxShaderResourceSlots) {
    return;
  }
  uint32_t count = view_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxShaderResourceSlots) {
    count = kMaxShaderResourceSlots - start_slot;
  }

  AeroGpuResource* new_rtv = dev->current_rtv;
  AeroGpuResource* new_dsv = dev->current_dsv;
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      tex = view ? view->texture : 0;
    }
    if (tex && new_rtv && tex == new_rtv->handle) {
      new_rtv = nullptr;
    }
    if (tex && new_dsv && tex == new_dsv->handle) {
      new_dsv = nullptr;
    }
  }
  if (new_rtv != dev->current_rtv || new_dsv != dev->current_dsv) {
    if (!set_render_targets_locked(dev, hDevice, new_rtv, new_dsv)) {
      return;
    }
  }

  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      tex = view ? view->texture : 0;
    }
    const uint32_t slot = start_slot + i;
    if (dev->ps_srvs[slot] == tex) {
      continue;
    }
    if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, tex)) {
      return;
    }
    dev->ps_srvs[slot] = tex;
  }
}

void AEROGPU_APIENTRY VsSetSamplers(D3D10DDI_HDEVICE hDevice,
                                    uint32_t start_slot,
                                    uint32_t sampler_count,
                                    const D3D10DDI_HSAMPLER* pSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxSamplerSlots) {
    return;
  }
  uint32_t count = sampler_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxSamplerSlots) {
    count = kMaxSamplerSlots - start_slot;
  }

  std::vector<aerogpu_handle_t> handles(count);
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t h = 0;
    if (pSamplers && pSamplers[i].pDrvPrivate) {
      auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(pSamplers[i]);
      h = s ? s->handle : 0;
    }
    handles[i] = h;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles.data(), handles.size() * sizeof(handles[0]));
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
  cmd->start_slot = start_slot;
  cmd->sampler_count = count;
  cmd->reserved0 = 0;
  for (uint32_t i = 0; i < count; i++) {
    dev->vs_samplers[start_slot + i] = handles[i];
  }
}

void AEROGPU_APIENTRY PsSetSamplers(D3D10DDI_HDEVICE hDevice,
                                    uint32_t start_slot,
                                    uint32_t sampler_count,
                                    const D3D10DDI_HSAMPLER* pSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot >= kMaxSamplerSlots) {
    return;
  }
  uint32_t count = sampler_count;
  if (count == 0) {
    return;
  }
  if (start_slot + count > kMaxSamplerSlots) {
    count = kMaxSamplerSlots - start_slot;
  }

  std::vector<aerogpu_handle_t> handles(count);
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t h = 0;
    if (pSamplers && pSamplers[i].pDrvPrivate) {
      auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(pSamplers[i]);
      h = s ? s->handle : 0;
    }
    handles[i] = h;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles.data(), handles.size() * sizeof(handles[0]));
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->start_slot = start_slot;
  cmd->sampler_count = count;
  cmd->reserved0 = 0;
  for (uint32_t i = 0; i < count; i++) {
    dev->ps_samplers[start_slot + i] = handles[i];
  }
}

void AEROGPU_APIENTRY Draw(D3D10DDI_HDEVICE hDevice, uint32_t vertex_count, uint32_t start_vertex) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("Draw hDevice=%p vc=%u start=%u", hDevice.pDrvPrivate, vertex_count, start_vertex);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, uint32_t index_count, uint32_t start_index, int32_t base_vertex) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawIndexed hDevice=%p ic=%u start=%u base=%d",
                               hDevice.pDrvPrivate,
                               index_count,
                               start_index,
                               base_vertex);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY Map(D3D10DDI_HDEVICE hDevice, const AEROGPU_D3D11DDIARG_MAP* pMap) {
  if (!hDevice.pDrvPrivate || !pMap || !pMap->hResource.pDrvPrivate || !pMap->pMappedSubresource) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pMap->hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->last_error = S_OK;

  const HRESULT hr = map_resource_locked(dev,
                                         res,
                                         static_cast<uint32_t>(pMap->Subresource),
                                         static_cast<uint32_t>(pMap->MapType),
                                         static_cast<uint32_t>(pMap->MapFlags),
                                         pMap->pMappedSubresource);
  if (FAILED(hr)) {
    ReportDeviceErrorLocked(dev, hDevice, hr);
  }
}

void AEROGPU_APIENTRY Unmap(D3D10DDI_HDEVICE hDevice, const AEROGPU_D3D11DDIARG_UNMAP* pUnmap) {
  if (!hDevice.pDrvPrivate || !pUnmap || !pUnmap->hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pUnmap->hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  dev->last_error = S_OK;
  unmap_resource_locked(dev, hDevice, res, static_cast<uint32_t>(pUnmap->Subresource));
}

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDIARG_PRESENT* pPresent) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("Present hDevice=%p syncInterval=%u backbuffer=%p",
                       hDevice.pDrvPrivate,
                       pPresent ? pPresent->SyncInterval : 0,
                       pPresent ? pPresent->hBackBuffer.pDrvPrivate : nullptr);
  if (!hDevice.pDrvPrivate || !pPresent) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  aerogpu_handle_t bb_handle = 0;
  if (pPresent->hBackBuffer.pDrvPrivate) {
    bb_handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pPresent->hBackBuffer)->handle;
  }
  AEROGPU_D3D10_11_LOG("trace_resources: Present sync=%u backbuffer_handle=%u",
                       static_cast<unsigned>(pPresent->SyncInterval),
                       static_cast<unsigned>(bb_handle));
#endif

  if (pPresent->hBackBuffer.pDrvPrivate) {
    auto* backbuffer = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pPresent->hBackBuffer);
    track_resource_alloc_for_submit_locked(dev, backbuffer);
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    HRESULT submit_hr = S_OK;
    submit_locked(dev, &submit_hr);
    AEROGPU_D3D10_RET_HR(FAILED(submit_hr) ? submit_hr : E_OUTOFMEMORY);
  }
  cmd->scanout_id = 0;
  cmd->flags = (pPresent->SyncInterval != 0) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
 
  HRESULT hr = S_OK;
  submit_locked(dev, &hr);
  AEROGPU_D3D10_RET_HR(hr);
}

HRESULT AEROGPU_APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("Flush hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  const HRESULT hr = flush_locked(dev, hDevice);
  AEROGPU_D3D10_RET_HR(hr);
}

void AEROGPU_APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE* pResources, uint32_t numResources) {
  AEROGPU_D3D10_11_LOG_CALL();
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
  AEROGPU_D3D10_11_LOG("trace_resources: RotateResourceIdentities count=%u", static_cast<unsigned>(numResources));
  for (uint32_t i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  + slot[%u]=%u", static_cast<unsigned>(i), static_cast<unsigned>(handle));
  }
#endif

  // Validate that we're rotating swapchain backbuffers (Texture2D render targets).
  std::vector<AeroGpuResource*> resources;
  resources.reserve(numResources);
  for (uint32_t i = 0; i < numResources; ++i) {
    auto* res = pResources[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i]) : nullptr;
    if (!res) {
      return;
    }
    if (std::find(resources.begin(), resources.end(), res) != resources.end()) {
      // Reject duplicates: RotateResourceIdentities expects distinct resources.
      return;
    }
    if (res->mapped) {
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
  if (!ref || ref->kind != ResourceKind::Texture2D || !(ref->bind_flags & kD3D11BindRenderTarget)) {
    return;
  }

  for (uint32_t i = 1; i < numResources; ++i) {
    const AeroGpuResource* r = resources[i];
    if (!r || r->kind != ResourceKind::Texture2D || !(r->bind_flags & kD3D11BindRenderTarget) ||
        r->width != ref->width || r->height != ref->height || r->dxgi_format != ref->dxgi_format ||
        r->mip_levels != ref->mip_levels || r->array_size != ref->array_size) {
      return;
    }
  }

  struct ResourceIdentity {
    aerogpu_handle_t handle = 0;
    uint32_t backing_alloc_id = 0;
    AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle = 0;
    uint32_t alloc_offset_bytes = 0;
    uint64_t alloc_size_bytes = 0;
    uint64_t share_token = 0;
    bool is_shared = false;
    bool is_shared_alias = false;
    AeroGpuResource::WddmIdentity wddm;
    std::vector<uint8_t> storage;
    bool mapped = false;
    bool mapped_write = false;
    uint32_t mapped_subresource = 0;
    uint32_t mapped_map_type = 0;
    uint64_t mapped_offset_bytes = 0;
    uint64_t mapped_size_bytes = 0;
  };

  auto take_identity = [](AeroGpuResource* res) -> ResourceIdentity {
    ResourceIdentity id{};
    if (!res) {
      return id;
    }
    id.handle = res->handle;
    id.backing_alloc_id = res->backing_alloc_id;
    id.alloc_handle = res->alloc_handle;
    id.alloc_offset_bytes = res->alloc_offset_bytes;
    id.alloc_size_bytes = res->alloc_size_bytes;
    id.share_token = res->share_token;
    id.is_shared = res->is_shared;
    id.is_shared_alias = res->is_shared_alias;
    id.wddm = std::move(res->wddm);
    id.storage = std::move(res->storage);
    id.mapped = res->mapped;
    id.mapped_write = res->mapped_write;
    id.mapped_subresource = res->mapped_subresource;
    id.mapped_map_type = res->mapped_map_type;
    id.mapped_offset_bytes = res->mapped_offset_bytes;
    id.mapped_size_bytes = res->mapped_size_bytes;
    return id;
  };

  auto put_identity = [](AeroGpuResource* res, ResourceIdentity&& id) {
    if (!res) {
      return;
    }
    res->handle = id.handle;
    res->backing_alloc_id = id.backing_alloc_id;
    res->alloc_handle = id.alloc_handle;
    res->alloc_offset_bytes = id.alloc_offset_bytes;
    res->alloc_size_bytes = id.alloc_size_bytes;
    res->share_token = id.share_token;
    res->is_shared = id.is_shared;
    res->is_shared_alias = id.is_shared_alias;
    res->wddm = std::move(id.wddm);
    res->storage = std::move(id.storage);
    res->mapped = id.mapped;
    res->mapped_write = id.mapped_write;
    res->mapped_subresource = id.mapped_subresource;
    res->mapped_map_type = id.mapped_map_type;
    res->mapped_offset_bytes = id.mapped_offset_bytes;
    res->mapped_size_bytes = id.mapped_size_bytes;
  };

  // Rotate the full resource identity bundle. This matches Win7 DXGI's
  // expectation that the *logical* backbuffer resource (buffer[0]) continues to
  // be used by the app across frames while the underlying allocation identity
  // flips.
  ResourceIdentity saved = take_identity(resources[0]);
  for (uint32_t i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  // If the current render targets refer to a rotated resource, re-emit the bind
  // command so the next frame targets the new backbuffer identity.
  bool needs_rebind = false;
  for (AeroGpuResource* r : resources) {
    if (dev->current_rtv == r || dev->current_dsv == r) {
      needs_rebind = true;
      break;
    }
  }
  if (needs_rebind) {
    if (!emit_set_render_targets_locked(dev)) {
      // Undo the rotation (rotate right by one).
      ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
      for (uint32_t i = numResources - 1; i > 0; --i) {
        put_identity(resources[i], take_identity(resources[i - 1]));
      }
      put_identity(resources[0], std::move(undo_saved));
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
  }

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  for (uint32_t i = 0; i < numResources; ++i) {
    aerogpu_handle_t handle = 0;
    if (pResources[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i])->handle;
    }
    AEROGPU_D3D10_11_LOG("trace_resources:  -> slot[%u]=%u", static_cast<unsigned>(i), static_cast<unsigned>(handle));
  }
#endif
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateDeviceSize");
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateDevice hAdapter=%p hDevice=%p",
                       hAdapter.pDrvPrivate,
                       pCreateDevice ? pCreateDevice->hDevice.pDrvPrivate : nullptr);
  if (!pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* out_funcs = reinterpret_cast<AEROGPU_D3D10_11_DEVICEFUNCS*>(pCreateDevice->pDeviceFuncs);
  if (!out_funcs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  auto* device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;
  device->device_callbacks = pCreateDevice->pDeviceCallbacks;

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

  funcs.pfnCalcPrivateShaderResourceViewSize = &CalcPrivateShaderResourceViewSize;
  funcs.pfnCreateShaderResourceView = &CreateShaderResourceView;
  funcs.pfnDestroyShaderResourceView = &DestroyShaderResourceView;

  funcs.pfnCalcPrivateSamplerSize = &CalcPrivateSamplerSize;
  funcs.pfnCreateSampler = &CreateSampler;
  funcs.pfnDestroySampler = &DestroySampler;

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

  funcs.pfnVsSetConstantBuffers = &VsSetConstantBuffers;
  funcs.pfnPsSetConstantBuffers = &PsSetConstantBuffers;
  funcs.pfnVsSetShaderResources = &VsSetShaderResources;
  funcs.pfnPsSetShaderResources = &PsSetShaderResources;
  funcs.pfnVsSetSamplers = &VsSetSamplers;
  funcs.pfnPsSetSamplers = &PsSetSamplers;

  funcs.pfnDraw = &Draw;
  funcs.pfnDrawIndexed = &DrawIndexed;
  funcs.pfnMap = &Map;
  funcs.pfnUnmap = &Unmap;
  funcs.pfnPresent = &Present;
  funcs.pfnFlush = &Flush;
  funcs.pfnRotateResourceIdentities = &RotateResourceIdentities;
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
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CloseAdapter hAdapter=%p", hAdapter.pDrvPrivate);
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// D3D11 adapter caps (pfnGetCaps)
// -------------------------------------------------------------------------------------------------

// The real Win7 D3D11 runtime calls D3D11DDI_ADAPTERFUNCS::pfnGetCaps during
// device creation and to service API calls like CheckFeatureSupport and
// CheckFormatSupport.
//
// For repository builds we do not depend on the WDK headers, so we model only
// the subset of D3D11DDIARG_GETCAPS / cap types that are exercised by Win7 at
// FL10_0 and by the guest-side smoke tests.
//
// Unknown cap types are treated as "supported but with everything disabled":
// we zero-fill the caller-provided buffer (when present), log the type, and
// return S_OK. This is intentionally conservative; the runtime generally
// interprets missing capabilities as unsupported feature paths.
//
// Note: Win7 uses the same layout for D3D10/DDI and D3D11/DDI cap queries, so we
// model this entrypoint using the shared `D3D10DDIARG_GETCAPS` container from
// `include/aerogpu_d3d10_11_umd.h`.

// NOTE: These numeric values intentionally match the D3D11_FEATURE enum values
// for the common CheckFeatureSupport queries on Windows 7. Win7-specific DDI
// cap queries (feature levels, multisample quality) are assigned consecutive
// values and may need to be extended as more types are observed in the wild.
enum AEROGPU_D3D11DDICAPS_TYPE : uint32_t {
  AEROGPU_D3D11DDICAPS_THREADING = 0,
  AEROGPU_D3D11DDICAPS_DOUBLES = 1,
  AEROGPU_D3D11DDICAPS_FORMAT_SUPPORT = 2,
  AEROGPU_D3D11DDICAPS_FORMAT_SUPPORT2 = 3,
  AEROGPU_D3D11DDICAPS_D3D10_X_HARDWARE_OPTIONS = 4,
  AEROGPU_D3D11DDICAPS_D3D11_OPTIONS = 5,
  AEROGPU_D3D11DDICAPS_ARCHITECTURE_INFO = 6,
  AEROGPU_D3D11DDICAPS_D3D9_OPTIONS = 7,
  AEROGPU_D3D11DDICAPS_FEATURE_LEVELS = 8,
  AEROGPU_D3D11DDICAPS_MULTISAMPLE_QUALITY_LEVELS = 9,
};

struct AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT {
  uint32_t InFormat;
  uint32_t OutFormatSupport;
};

struct AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT2 {
  uint32_t InFormat;
  uint32_t OutFormatSupport2;
};

struct AEROGPU_D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS {
  uint32_t Format;
  uint32_t SampleCount;
  uint32_t NumQualityLevels;
};

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pGetCaps) {
  if (!pGetCaps) {
    return E_INVALIDARG;
  }

  const uint32_t type = static_cast<uint32_t>(pGetCaps->Type);
  void* data = pGetCaps->pData;
  const uint32_t data_size = static_cast<uint32_t>(pGetCaps->DataSize);
  CAPS_LOG("aerogpu-d3d10_11: GetCaps type=%u size=%u\n", (unsigned)type, (unsigned)data_size);

  if (!data || data_size == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    return S_OK;
  }

  switch (type) {
    case AEROGPU_D3D11DDICAPS_FEATURE_LEVELS: {
      // The Win7 runtime uses this to determine which feature levels to attempt.
      // We advertise only FL10_0 until CS/UAV/etc are implemented.
      // Win7 D3D11 uses a "count + inline list" layout:
      //   { UINT NumFeatureLevels; D3D_FEATURE_LEVEL FeatureLevels[NumFeatureLevels]; }
      //
      // But some header/runtime combinations treat this as a {count, pointer}
      // struct. Populate both layouts when we have enough space so we avoid
      // mismatched interpretation (in particular on 64-bit where the pointer
      // lives at a different offset than the inline list element). On 32-bit the
      // pointer field overlaps the first inline element, so we prefer the
      // pointer layout to avoid returning a bogus pointer value (0xA000).
      static const uint32_t kLevels[] = {kD3DFeatureLevel10_0};
      struct FeatureLevelsCapsPtr {
        uint32_t NumFeatureLevels;
        const uint32_t* pFeatureLevels;
      };

      std::memset(data, 0, data_size);
      constexpr size_t kInlineLevelsOffset = sizeof(uint32_t);
      constexpr size_t kPtrOffset = offsetof(FeatureLevelsCapsPtr, pFeatureLevels);

      // 32-bit: the pointer field overlaps the first inline element. Prefer the
      // {count, pointer} layout to avoid returning a bogus pointer value
      // (e.g. 0xA000) that could crash the runtime if it expects the pointer
      // interpretation.
      if (kPtrOffset == kInlineLevelsOffset && data_size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      if (data_size >= sizeof(uint32_t) * 2) {
        auto* out = reinterpret_cast<uint32_t*>(data);
        out[0] = 1;
        out[1] = kD3DFeatureLevel10_0;
        if (data_size >= sizeof(FeatureLevelsCapsPtr) && kPtrOffset >= kInlineLevelsOffset + sizeof(uint32_t)) {
          reinterpret_cast<FeatureLevelsCapsPtr*>(data)->pFeatureLevels = kLevels;
        }
        return S_OK;
      }

      if (data_size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      // Fallback: treat the buffer as a single feature-level value.
      if (data_size >= sizeof(uint32_t)) {
        reinterpret_cast<uint32_t*>(data)[0] = kD3DFeatureLevel10_0;
        return S_OK;
      }

      return E_INVALIDARG;
    }

    case AEROGPU_D3D11DDICAPS_THREADING:
    case AEROGPU_D3D11DDICAPS_DOUBLES:
    case AEROGPU_D3D11DDICAPS_D3D10_X_HARDWARE_OPTIONS:
    case AEROGPU_D3D11DDICAPS_D3D11_OPTIONS:
    case AEROGPU_D3D11DDICAPS_ARCHITECTURE_INFO:
    case AEROGPU_D3D11DDICAPS_D3D9_OPTIONS: {
      // Conservative: report "not supported" for everything (all fields zero).
      std::memset(data, 0, data_size);
      return S_OK;
    }

    case AEROGPU_D3D11DDICAPS_FORMAT_SUPPORT: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT)) {
        return E_INVALIDARG;
      }
      auto* fs = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT*>(data);
      fs->OutFormatSupport = d3d11_format_support_flags(fs->InFormat);
      return S_OK;
    }

    case AEROGPU_D3D11DDICAPS_FORMAT_SUPPORT2: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT2)) {
        return E_INVALIDARG;
      }
      auto* fs = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT2*>(data);
      fs->OutFormatSupport2 = 0;
      return S_OK;
    }

    case AEROGPU_D3D11DDICAPS_MULTISAMPLE_QUALITY_LEVELS: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS)) {
        return E_INVALIDARG;
      }
      auto* ms = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS*>(data);
      // No MSAA support yet; report only the implicit 1x case.
      const uint32_t support = d3d11_format_support_flags(ms->Format);
      const bool supported_format = (support & kD3D11FormatSupportTexture2D) != 0 &&
                                    (support & (kD3D11FormatSupportRenderTarget | kD3D11FormatSupportDepthStencil)) != 0;
      ms->NumQualityLevels = (ms->SampleCount == 1 && supported_format) ? 1u : 0u;
      return S_OK;
    }

    default:
      AEROGPU_D3D10_11_LOG("GetCaps unknown type=%u (size=%u) -> zero-fill + S_OK",
                           (unsigned)type,
                           (unsigned)data_size);
      std::memset(data, 0, data_size);
      return S_OK;
  }
}

// -------------------------------------------------------------------------------------------------
// Exported OpenAdapter entrypoints
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapterCommon(D3D10DDIARG_OPENADAPTER* pOpenData) {
#if defined(_WIN32)
  // Always emit the module path once. This is the quickest way to confirm the
  // correct UMD bitness was loaded on Win7 x64 (System32 vs SysWOW64).
  LogModulePathOnce();
#endif

  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapterCommon iface=%u ver=%u",
                       pOpenData ? pOpenData->Interface : 0,
                       pOpenData ? pOpenData->Version : 0);
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* adapter = new (std::nothrow) AeroGpuAdapter();
  if (!adapter) {
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  pOpenData->hAdapter.pDrvPrivate = adapter;


  D3D10DDI_ADAPTERFUNCS funcs = {};
  funcs.pfnGetCaps = &GetCaps;
  funcs.pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
  funcs.pfnCreateDevice = &CreateDevice;
  funcs.pfnCloseAdapter = &CloseAdapter;

  auto* out_funcs = reinterpret_cast<D3D10DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  if (!out_funcs) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  *out_funcs = funcs;
  AEROGPU_D3D10_RET_HR(S_OK);
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter10");
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter10_2");
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter11");
  return OpenAdapterCommon(pOpenData);
}

} // extern "C"


#endif // WDK build exclusion guard (this TU is portable-only)
