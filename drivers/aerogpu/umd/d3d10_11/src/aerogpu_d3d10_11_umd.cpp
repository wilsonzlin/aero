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
//   - `aerogpu_d3d10_umd_wdk.cpp`     (OpenAdapter10)
//   - `aerogpu_d3d10_1_umd_wdk.cpp`   (OpenAdapter10_2)
//   - `aerogpu_d3d11_umd_wdk.cpp`     (OpenAdapter11)
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dkmthk.h>
  #include <d3dumddi.h>
#endif

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../common/aerogpu_win32_security.h"

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dkmthk.h>
  #include "../../../protocol/aerogpu_dbgctl_escape.h"
  #include "../../../protocol/aerogpu_umd_private.h"
 
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
constexpr uint32_t kAeroGpuTimeoutMsInfinite = ~0u;

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
// Win7 D3D11 runtime requests a specific user-mode DDI interface version. If we
// accept a version, we must fill function tables whose struct layout matches
// that version (otherwise the runtime can crash during device creation).
// `D3D10DDIARG_OPENADAPTER::Version` negotiation constant.
// Some WDKs expose `D3D11DDI_SUPPORTED`; others only provide `D3D11DDI_INTERFACE_VERSION`.
#if defined(D3D11DDI_SUPPORTED)
constexpr UINT kAeroGpuWin7D3D11DdiSupportedVersion = D3D11DDI_SUPPORTED;
#else
constexpr UINT kAeroGpuWin7D3D11DdiSupportedVersion = D3D11DDI_INTERFACE_VERSION;
#endif

// Compile-time sanity (avoid sizeof assertions; layouts vary across WDKs).
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICEFUNCS::pfnCreateResource)>,
              "Expected D3D11DDI_DEVICEFUNCS::pfnCreateResource");
static_assert(std::is_member_object_pointer_v<decltype(&D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw)>,
              "Expected D3D11DDI_DEVICECONTEXTFUNCS::pfnDraw");
#endif

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
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Typeless:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8UnormSrgb:
    case kDxgiFormatB8G8R8X8Typeless:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatR8G8B8A8Typeless:
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;

  D3D10DDI_HRTADAPTER hrt_adapter = {};
  const void* adapter_callbacks = nullptr;

  // Optional D3DKMT adapter handle for dev-only escapes (e.g. QUERY_FENCE).
  D3DKMT_HANDLE kmt_adapter = 0;
#endif
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  uint64_t wddm_allocation = 0;

  // When Map/Unmap is implemented via the runtime LockCb/UnlockCb path, keep the
  // lock pointer/pitch here so Unmap can copy the final bytes into `storage`
  // (used by the bring-up upload path) before unlocking.
  void* mapped_wddm_ptr = nullptr;
  uint32_t mapped_wddm_pitch = 0;
  uint32_t mapped_wddm_slice_pitch = 0;
#endif

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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // WDDM handles returned by the runtime allocation callback (kernel-mode
  // objects). The initial bring-up path uses a single allocation per resource.
  D3DKMT_HANDLE hkm_resource = 0;
  D3DKMT_HANDLE hkm_allocation = 0;
#endif
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

// Used to make DestroyDevice11 idempotent in the face of create-path failures.
// (The Win7 runtime typically does not invoke DestroyDevice on a failed CreateDevice,
// but keeping this crash-proof is cheap.)
constexpr uint32_t kAeroGpuDeviceMagic = 0x31444741u; // "AGD1"

struct AeroGpuDevice {
  uint32_t magic = kAeroGpuDeviceMagic;
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  // Runtime callbacks and handles (error reporting + WDDM submission).
  const D3D11DDI_DEVICECALLBACKS* callbacks = nullptr;
  const D3DDDI_DEVICECALLBACKS* ddi_callbacks = nullptr;
  D3D11DDI_HRTDEVICE hrt_device = {};
  D3D10DDI_HRTDEVICE hrt_device10 = {};
  D3D11DDI_HDEVICE hDevice = {};

  // Fence tracking for WDDM-backed synchronization.
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  // Kernel-mode WDDM objects used for fence wait/query.
  D3DKMT_HANDLE kmt_device = 0;
  D3DKMT_HANDLE kmt_fence_syncobj = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
  D3DKMT_HANDLE kmt_adapter = 0;

  AeroGpuImmediateContext* immediate = nullptr;
};

struct AeroGpuImmediateContext {
  AeroGpuDevice* device = nullptr;
  std::mutex mutex;
  aerogpu::CmdWriter cmd;

  // Cached state.
  aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* current_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  uint32_t current_rtv_count = 0;
  aerogpu_handle_t current_dsv = 0;
  AeroGpuResource* current_dsv_resource = nullptr;
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
    p.pfn_close_adapter = reinterpret_cast<decltype(&D3DKMTCloseAdapter)>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
    p.pfn_query_adapter_info =
        reinterpret_cast<decltype(&D3DKMTQueryAdapterInfo)>(GetProcAddress(gdi32, "D3DKMTQueryAdapterInfo"));
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
  if (!NT_SUCCESS(st) || !open.hAdapter) {
    return false;
  }

  bool found = false;

  aerogpu_umd_private_v1 blob;
  std::memset(&blob, 0, sizeof(blob));

  D3DKMT_QUERYADAPTERINFO q{};
  q.hAdapter = open.hAdapter;
  q.pPrivateDriverData = &blob;
  q.PrivateDriverDataSize = sizeof(blob);

  // Avoid relying on the WDK's numeric KMTQAITYPE_UMDRIVERPRIVATE constant by probing a
  // small range of values and looking for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  for (UINT type = 0; type < 256; ++type) {
    std::memset(&blob, 0, sizeof(blob));
    q.Type = static_cast<KMTQUERYADAPTERINFOTYPE>(type);

    const NTSTATUS qst = procs.pfn_query_adapter_info(&q);
    if (!NT_SUCCESS(qst)) {
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
    found = true;
    break;
  }

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
  if (!QueryUmdPrivateFromPrimaryDisplay(&blob)) {
    return;
  }

  adapter->umd_private = blob;
  adapter->umd_private_valid = true;
}

template <typename Fn, typename HandleA, typename HandleB, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, HandleA handle_a, HandleB handle_b, Args&&... args);

void SetError(AeroGpuDevice* dev, HRESULT hr) {
  if (!dev || !dev->callbacks) {
    return;
  }
  // Win7 D3D11 runtime expects pfnSetErrorCb for void-returning DDI failures.
  // Some WDK revisions disagree on whether `pfnSetErrorCb` takes a runtime device
  // handle (HRTDEVICE) or a driver device handle (HDEVICE). Support both.
  if (dev->callbacks->pfnSetErrorCb) {
    if constexpr (std::is_invocable_v<decltype(dev->callbacks->pfnSetErrorCb), D3D11DDI_HDEVICE, HRESULT>) {
      dev->callbacks->pfnSetErrorCb(dev->hDevice, hr);
    } else if constexpr (std::is_invocable_v<decltype(dev->callbacks->pfnSetErrorCb), D3D10DDI_HDEVICE, HRESULT>) {
      D3D10DDI_HDEVICE h10 = {};
      h10.pDrvPrivate = dev->hDevice.pDrvPrivate;
      dev->callbacks->pfnSetErrorCb(h10, hr);
    } else {
      CallCbMaybeHandle(dev->callbacks->pfnSetErrorCb, dev->hrt_device, dev->hrt_device10, hr);
    }
  }
}

// -------------------------------------------------------------------------------------------------
// D3D11 DDI function-table stubs
//
// Win7 D3D11 runtimes may call "unexpected" DDIs during device creation / validation even if the app
// never explicitly uses a feature. Leaving any function pointer as NULL is therefore a crash risk.
//
// Strategy:
// - HRESULT-returning DDIs: return E_NOTIMPL by default.
// - void-returning DDIs: report E_NOTIMPL via SetErrorCb (when we can reach the device).
// - CalcPrivate*Size DDIs (SIZE_T): return a small non-zero size.
//
// `__if_exists` is used at assignment sites so this stays buildable across WDK header revisions.
// -------------------------------------------------------------------------------------------------

constexpr SIZE_T kAeroGpuDdiStubPrivateSize = sizeof(uint64_t);

inline AeroGpuDevice* DeviceFromHandle(D3D11DDI_HDEVICE hDevice) {
  return hDevice.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
}

inline AeroGpuDevice* DeviceFromHandle(D3D11DDI_HDEVICECONTEXT hCtx) {
  auto* ctx = hCtx.pDrvPrivate ? FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx) : nullptr;
  return ctx ? ctx->device : nullptr;
}

template <typename T>
inline AeroGpuDevice* DeviceFromHandle(T) {
  return nullptr;
}

inline void ReportNotImpl() {}

template <typename Handle0, typename... Rest>
inline void ReportNotImpl(Handle0 handle0, Rest...) {
  SetError(DeviceFromHandle(handle0), E_NOTIMPL);
}

template <typename FnPtr>
struct AeroGpuDdiStub;

template <typename R, typename... Args>
struct AeroGpuDdiStub<R(AEROGPU_APIENTRY*)(Args...)> {
  static R AEROGPU_APIENTRY Func(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<R, void>) {
      ReportNotImpl(args...);
      return;
    } else if constexpr (std::is_same_v<R, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<R, SIZE_T>) {
      return kAeroGpuDdiStubPrivateSize;
    } else {
      return {};
    }
  }
};

template <typename FnPtr>
struct AeroGpuDdiNoopStub;

template <typename R, typename... Args>
struct AeroGpuDdiNoopStub<R(AEROGPU_APIENTRY*)(Args...)> {
  static R AEROGPU_APIENTRY Func(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<R, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<R, SIZE_T>) {
      return kAeroGpuDdiStubPrivateSize;
    } else if constexpr (std::is_same_v<R, void>) {
      return;
    } else {
      return {};
    }
  }
};

// Validates that the runtime will never see a NULL DDI function pointer.
//
// This is intentionally enabled in release builds. If our `__if_exists` field lists ever fall out of
// sync with the WDK's `d3d11umddi.h` layout, this check will fail fast (device creation returns
// `E_NOINTERFACE`) instead of allowing a later NULL-call crash inside the D3D11 runtime.
inline bool ValidateNoNullDdiTable(const char* name, const void* table, size_t bytes) {
  if (!table || bytes == 0) {
    return false;
  }

  // These tables are expected to contain only function pointers, densely packed.
  if ((bytes % sizeof(void*)) != 0) {
    return false;
  }

  const auto* raw = reinterpret_cast<const unsigned char*>(table);
  const size_t count = bytes / sizeof(void*);
  for (size_t i = 0; i < count; ++i) {
    const size_t offset = i * sizeof(void*);
    bool all_zero = true;
    for (size_t j = 0; j < sizeof(void*); ++j) {
      if (raw[offset + j] != 0) {
        all_zero = false;
        break;
      }
    }
    if (!all_zero) {
      continue;
    }

#if defined(_WIN32)
    char buf[256] = {};
    snprintf(buf, sizeof(buf), "aerogpu-d3d10_11: NULL DDI entry in %s at index=%zu\n", name ? name : "?", i);
    OutputDebugStringA(buf);
#endif

#if !defined(NDEBUG)
    assert(false && "NULL DDI function pointer");
#endif
    return false;
  }
  return true;
}

#define AEROGPU_D3D11_DEVICEFUNCS_FIELDS(X)                                                                    \
  X(pfnDestroyDevice)                                                                                           \
  X(pfnCalcPrivateResourceSize)                                                                                 \
  X(pfnCreateResource)                                                                                           \
  X(pfnOpenResource)                                                                                             \
  X(pfnDestroyResource)                                                                                          \
  X(pfnCalcPrivateShaderResourceViewSize)                                                                        \
  X(pfnCreateShaderResourceView)                                                                                 \
  X(pfnDestroyShaderResourceView)                                                                                \
  X(pfnCalcPrivateRenderTargetViewSize)                                                                          \
  X(pfnCreateRenderTargetView)                                                                                   \
  X(pfnDestroyRenderTargetView)                                                                                  \
  X(pfnCalcPrivateDepthStencilViewSize)                                                                          \
  X(pfnCreateDepthStencilView)                                                                                   \
  X(pfnDestroyDepthStencilView)                                                                                  \
  X(pfnCalcPrivateUnorderedAccessViewSize)                                                                       \
  X(pfnCreateUnorderedAccessView)                                                                                \
  X(pfnDestroyUnorderedAccessView)                                                                               \
  X(pfnCalcPrivateVertexShaderSize)                                                                              \
  X(pfnCreateVertexShader)                                                                                       \
  X(pfnDestroyVertexShader)                                                                                      \
  X(pfnCalcPrivatePixelShaderSize)                                                                               \
  X(pfnCreatePixelShader)                                                                                        \
  X(pfnDestroyPixelShader)                                                                                       \
  X(pfnCalcPrivateGeometryShaderSize)                                                                            \
  X(pfnCreateGeometryShader)                                                                                     \
  X(pfnDestroyGeometryShader)                                                                                    \
  X(pfnCalcPrivateGeometryShaderWithStreamOutputSize)                                                            \
  X(pfnCreateGeometryShaderWithStreamOutput)                                                                     \
  X(pfnCalcPrivateHullShaderSize)                                                                                \
  X(pfnCreateHullShader)                                                                                         \
  X(pfnDestroyHullShader)                                                                                        \
  X(pfnCalcPrivateDomainShaderSize)                                                                              \
  X(pfnCreateDomainShader)                                                                                       \
  X(pfnDestroyDomainShader)                                                                                      \
  X(pfnCalcPrivateComputeShaderSize)                                                                             \
  X(pfnCreateComputeShader)                                                                                      \
  X(pfnDestroyComputeShader)                                                                                     \
  X(pfnCalcPrivateElementLayoutSize)                                                                             \
  X(pfnCreateElementLayout)                                                                                      \
  X(pfnDestroyElementLayout)                                                                                     \
  X(pfnCalcPrivateSamplerSize)                                                                                   \
  X(pfnCreateSampler)                                                                                            \
  X(pfnDestroySampler)                                                                                           \
  X(pfnCalcPrivateBlendStateSize)                                                                                \
  X(pfnCreateBlendState)                                                                                         \
  X(pfnDestroyBlendState)                                                                                        \
  X(pfnCalcPrivateRasterizerStateSize)                                                                           \
  X(pfnCreateRasterizerState)                                                                                    \
  X(pfnDestroyRasterizerState)                                                                                   \
  X(pfnCalcPrivateDepthStencilStateSize)                                                                         \
  X(pfnCreateDepthStencilState)                                                                                  \
  X(pfnDestroyDepthStencilState)                                                                                 \
  X(pfnCalcPrivateQuerySize)                                                                                     \
  X(pfnCreateQuery)                                                                                               \
  X(pfnDestroyQuery)                                                                                              \
  X(pfnCalcPrivatePredicateSize)                                                                                 \
  X(pfnCreatePredicate)                                                                                           \
  X(pfnDestroyPredicate)                                                                                          \
  X(pfnCalcPrivateCounterSize)                                                                                   \
  X(pfnCreateCounter)                                                                                             \
  X(pfnDestroyCounter)                                                                                            \
  X(pfnCalcPrivateDeferredContextSize)                                                                           \
  X(pfnCreateDeferredContext)                                                                                     \
  X(pfnDestroyDeferredContext)                                                                                    \
  X(pfnCalcPrivateCommandListSize)                                                                               \
  X(pfnCreateCommandList)                                                                                         \
  X(pfnDestroyCommandList)                                                                                        \
  X(pfnCalcPrivateClassLinkageSize)                                                                              \
  X(pfnCreateClassLinkage)                                                                                        \
  X(pfnDestroyClassLinkage)                                                                                       \
  X(pfnCalcPrivateClassInstanceSize)                                                                             \
  X(pfnCreateClassInstance)                                                                                       \
  X(pfnDestroyClassInstance)                                                                                      \
  X(pfnCheckCounterInfo)                                                                                           \
  X(pfnCheckCounter)                                                                                               \
  X(pfnGetDeviceRemovedReason)                                                                                    \
  X(pfnGetExceptionMode)                                                                                           \
  X(pfnSetExceptionMode)                                                                                          \
  X(pfnPresent)                                                                                                    \
  X(pfnRotateResourceIdentities)                                                                                   \
  X(pfnCheckDeferredContextHandleSizes)                                                                           \
  X(pfnCalcPrivateDeviceContextSize)                                                                               \
  X(pfnCreateDeviceContext)                                                                                        \
  X(pfnDestroyDeviceContext)                                                                                       \
  X(pfnCalcPrivateDeviceContextStateSize)                                                                          \
  X(pfnCreateDeviceContextState)                                                                                   \
  X(pfnDestroyDeviceContextState)

#define AEROGPU_D3D11_DEVICECONTEXTFUNCS_FIELDS(X)                                                               \
  X(pfnVsSetShader)                                                                                               \
  X(pfnVsSetConstantBuffers)                                                                                      \
  X(pfnVsSetShaderResources)                                                                                      \
  X(pfnVsSetSamplers)                                                                                             \
  X(pfnGsSetShader)                                                                                               \
  X(pfnGsSetConstantBuffers)                                                                                      \
  X(pfnGsSetShaderResources)                                                                                      \
  X(pfnGsSetSamplers)                                                                                             \
  X(pfnPsSetShader)                                                                                               \
  X(pfnPsSetConstantBuffers)                                                                                      \
  X(pfnPsSetShaderResources)                                                                                      \
  X(pfnPsSetSamplers)                                                                                             \
  X(pfnHsSetShader)                                                                                               \
  X(pfnHsSetConstantBuffers)                                                                                      \
  X(pfnHsSetShaderResources)                                                                                      \
  X(pfnHsSetSamplers)                                                                                             \
  X(pfnDsSetShader)                                                                                               \
  X(pfnDsSetConstantBuffers)                                                                                      \
  X(pfnDsSetShaderResources)                                                                                      \
  X(pfnDsSetSamplers)                                                                                             \
  X(pfnCsSetShader)                                                                                               \
  X(pfnCsSetConstantBuffers)                                                                                      \
  X(pfnCsSetShaderResources)                                                                                      \
  X(pfnCsSetSamplers)                                                                                             \
  X(pfnCsSetUnorderedAccessViews)                                                                                 \
  X(pfnIaSetInputLayout)                                                                                           \
  X(pfnIaSetVertexBuffers)                                                                                        \
  X(pfnIaSetIndexBuffer)                                                                                           \
  X(pfnIaSetTopology)                                                                                              \
  X(pfnSoSetTargets)                                                                                               \
  X(pfnSetViewports)                                                                                               \
  X(pfnSetScissorRects)                                                                                            \
  X(pfnSetRasterizerState)                                                                                         \
  X(pfnSetBlendState)                                                                                              \
  X(pfnSetDepthStencilState)                                                                                       \
  X(pfnSetRenderTargets)                                                                                           \
  X(pfnSetRenderTargetsAndUnorderedAccessViews)                                                                    \
  X(pfnSetRenderTargetsAndUnorderedAccessViews11_1)                                                                \
  X(pfnDraw)                                                                                                       \
  X(pfnDrawIndexed)                                                                                                \
  X(pfnDrawInstanced)                                                                                              \
  X(pfnDrawIndexedInstanced)                                                                                       \
  X(pfnDrawAuto)                                                                                                   \
  X(pfnDrawInstancedIndirect)                                                                                      \
  X(pfnDrawIndexedInstancedIndirect)                                                                               \
  X(pfnDispatch)                                                                                                   \
  X(pfnDispatchIndirect)                                                                                           \
  X(pfnStagingResourceMap)                                                                                        \
  X(pfnStagingResourceUnmap)                                                                                      \
  X(pfnDynamicIABufferMapDiscard)                                                                                 \
  X(pfnDynamicIABufferMapNoOverwrite)                                                                             \
  X(pfnDynamicIABufferUnmap)                                                                                      \
  X(pfnDynamicConstantBufferMapDiscard)                                                                           \
  X(pfnDynamicConstantBufferUnmap)                                                                                \
  X(pfnMap)                                                                                                        \
  X(pfnUnmap)                                                                                                      \
  X(pfnUpdateSubresourceUP)                                                                                        \
  X(pfnUpdateSubresource)                                                                                           \
  X(pfnCopySubresourceRegion)                                                                                      \
  X(pfnCopyResource)                                                                                               \
  X(pfnCopyStructureCount)                                                                                          \
  X(pfnResolveSubresource)                                                                                          \
  X(pfnGenerateMips)                                                                                               \
  X(pfnSetResourceMinLOD)                                                                                           \
  X(pfnClearRenderTargetView)                                                                                      \
  X(pfnClearUnorderedAccessViewUint)                                                                               \
  X(pfnClearUnorderedAccessViewFloat)                                                                              \
  X(pfnClearDepthStencilView)                                                                                      \
  X(pfnBegin)                                                                                                      \
  X(pfnEnd)                                                                                                        \
  X(pfnQueryGetData)                                                                                               \
  X(pfnGetData)                                                                                                    \
  X(pfnSetPredication)                                                                                              \
  X(pfnExecuteCommandList)                                                                                         \
  X(pfnFinishCommandList)                                                                                          \
  X(pfnClearState)                                                                                                 \
  X(pfnFlush)                                                                                                      \
  X(pfnPresent)                                                                                                    \
  X(pfnRotateResourceIdentities)                                                                                   \
  X(pfnDiscardResource)                                                                                            \
  X(pfnDiscardView)                                                                                                \
  X(pfnSetMarker)                                                                                                  \
  X(pfnBeginEvent)                                                                                                 \
  X(pfnEndEvent)                                                                                                   \
  X(pfnGetResourceMinLOD)

#define AEROGPU_D3D11_DEVICECONTEXTFUNCS_NOOP_FIELDS(X)                                                           \
  X(pfnClearState)                                                                                                 \
  X(pfnSetPredication)                                                                                              \
  X(pfnSoSetTargets)                                                                                               \
  X(pfnVsSetShader)                                                                                                 \
  X(pfnVsSetConstantBuffers)                                                                                      \
  X(pfnVsSetShaderResources)                                                                                      \
  X(pfnVsSetSamplers)                                                                                             \
  X(pfnHsSetShader)                                                                                               \
  X(pfnHsSetConstantBuffers)                                                                                      \
  X(pfnHsSetShaderResources)                                                                                      \
  X(pfnHsSetSamplers)                                                                                             \
  X(pfnDsSetShader)                                                                                               \
  X(pfnDsSetConstantBuffers)                                                                                      \
  X(pfnDsSetShaderResources)                                                                                      \
  X(pfnDsSetSamplers)                                                                                             \
  X(pfnGsSetShader)                                                                                               \
  X(pfnGsSetConstantBuffers)                                                                                      \
  X(pfnGsSetShaderResources)                                                                                      \
  X(pfnGsSetSamplers)                                                                                             \
  X(pfnPsSetShader)                                                                                               \
  X(pfnPsSetConstantBuffers)                                                                                      \
  X(pfnPsSetShaderResources)                                                                                      \
  X(pfnPsSetSamplers)                                                                                             \
  X(pfnCsSetShader)                                                                                               \
  X(pfnCsSetConstantBuffers)                                                                                      \
  X(pfnCsSetShaderResources)                                                                                      \
  X(pfnCsSetSamplers)                                                                                             \
  X(pfnCsSetUnorderedAccessViews)                                                                                 \
  X(pfnSetRenderTargetsAndUnorderedAccessViews)                                                                    \
  X(pfnSetRenderTargetsAndUnorderedAccessViews11_1)                                                                \
  X(pfnUnmap)                                                                                                      \
  X(pfnStagingResourceUnmap)                                                                                      \
  X(pfnDynamicIABufferUnmap)                                                                                      \
  X(pfnDynamicConstantBufferUnmap)                                                                                \
  X(pfnDiscardResource)                                                                                            \
  X(pfnDiscardView)                                                                                                \
  X(pfnSetMarker)                                                                                                  \
  X(pfnBeginEvent)                                                                                                 \
  X(pfnEndEvent)                                                                                                   \
  X(pfnSetResourceMinLOD)

// Device-level functions that should never trip the runtime error state when stubbed.
// These are primarily Destroy* entrypoints that may be called during cleanup/reset even after
// a higher-level failure.
#define AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS(X)                                                                  \
  X(pfnDestroyDevice)                                                                                             \
  X(pfnDestroyResource)                                                                                           \
  X(pfnDestroyShaderResourceView)                                                                                 \
  X(pfnDestroyRenderTargetView)                                                                                   \
  X(pfnDestroyDepthStencilView)                                                                                   \
  X(pfnDestroyUnorderedAccessView)                                                                                \
  X(pfnDestroyVertexShader)                                                                                       \
  X(pfnDestroyPixelShader)                                                                                        \
  X(pfnDestroyGeometryShader)                                                                                     \
  X(pfnDestroyHullShader)                                                                                         \
  X(pfnDestroyDomainShader)                                                                                       \
  X(pfnDestroyComputeShader)                                                                                      \
  X(pfnDestroyClassLinkage)                                                                                       \
  X(pfnDestroyClassInstance)                                                                                      \
  X(pfnDestroyElementLayout)                                                                                      \
  X(pfnDestroySampler)                                                                                            \
  X(pfnDestroyBlendState)                                                                                         \
  X(pfnDestroyRasterizerState)                                                                                    \
  X(pfnDestroyDepthStencilState)                                                                                  \
  X(pfnDestroyQuery)                                                                                               \
  X(pfnDestroyPredicate)                                                                                           \
  X(pfnDestroyCounter)                                                                                             \
  X(pfnDestroyDeviceContext)                                                                                       \
  X(pfnDestroyDeferredContext)                                                                                    \
  X(pfnDestroyCommandList)                                                                                        \
  X(pfnDestroyDeviceContextState)

inline void InitDeviceFuncsWithStubs(D3D11DDI_DEVICEFUNCS* out) {
  if (!out) {
    return;
  }
  std::memset(out, 0, sizeof(*out));
#define AEROGPU_ASSIGN_DEVICE_STUB(field)                                                                          \
  __if_exists(D3D11DDI_DEVICEFUNCS::field) { out->field = &AeroGpuDdiStub<decltype(out->field)>::Func; }
  AEROGPU_D3D11_DEVICEFUNCS_FIELDS(AEROGPU_ASSIGN_DEVICE_STUB)
#undef AEROGPU_ASSIGN_DEVICE_STUB

  // Ensure benign cleanup paths never spam SetErrorCb.
#define AEROGPU_ASSIGN_DEVICE_NOOP(field)                                                                          \
  __if_exists(D3D11DDI_DEVICEFUNCS::field) { out->field = &AeroGpuDdiNoopStub<decltype(out->field)>::Func; }
  AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS(AEROGPU_ASSIGN_DEVICE_NOOP)
#undef AEROGPU_ASSIGN_DEVICE_NOOP
}

inline void InitDeviceContextFuncsWithStubs(D3D11DDI_DEVICECONTEXTFUNCS* out) {
  if (!out) {
    return;
  }
  std::memset(out, 0, sizeof(*out));
#define AEROGPU_ASSIGN_CONTEXT_STUB(field)                                                                         \
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::field) { out->field = &AeroGpuDdiStub<decltype(out->field)>::Func; }
  AEROGPU_D3D11_DEVICECONTEXTFUNCS_FIELDS(AEROGPU_ASSIGN_CONTEXT_STUB)
#undef AEROGPU_ASSIGN_CONTEXT_STUB

  // Avoid spamming SetErrorCb for benign ClearState/unbind sequences.
#define AEROGPU_ASSIGN_CONTEXT_NOOP(field)                                                                         \
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::field) { out->field = &AeroGpuDdiNoopStub<decltype(out->field)>::Func; }
  AEROGPU_D3D11_DEVICECONTEXTFUNCS_NOOP_FIELDS(AEROGPU_ASSIGN_CONTEXT_NOOP)
#undef AEROGPU_ASSIGN_CONTEXT_NOOP
}

static const D3D11DDI_DEVICEFUNCS kStubDeviceFuncs = [] {
  D3D11DDI_DEVICEFUNCS f{};
  InitDeviceFuncsWithStubs(&f);
  return f;
}();

static const D3D11DDI_DEVICECONTEXTFUNCS kStubCtxFuncs = [] {
  D3D11DDI_DEVICECONTEXTFUNCS f{};
  InitDeviceContextFuncsWithStubs(&f);
  return f;
}();

#undef AEROGPU_D3D11_DEVICEFUNCS_FIELDS
#undef AEROGPU_D3D11_DEVICECONTEXTFUNCS_FIELDS
#undef AEROGPU_D3D11_DEVICECONTEXTFUNCS_NOOP_FIELDS
#undef AEROGPU_D3D11_DEVICEFUNCS_NOOP_FIELDS

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

void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

uint64_t AeroGpuQueryCompletedFence(AeroGpuDevice* dev) {
  if (!dev) {
    return 0;
  }

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
        return static_cast<uint64_t>(q.last_completed_fence);
      }
    }
  }

  return dev->last_completed_fence.load(std::memory_order_relaxed);
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

  if (!dev->kmt_fence_syncobj) {
    return E_FAIL;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_wait_for_syncobj) {
    return E_FAIL;
  }

  const D3DKMT_HANDLE handles[1] = {dev->kmt_fence_syncobj};
  const UINT64 fence_values[1] = {fence};
  const UINT64 timeout = (timeout_ms == kAeroGpuTimeoutMsInfinite) ? ~0ull : static_cast<UINT64>(timeout_ms);

  D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
  args.ObjectCount = 1;
  args.ObjectHandleArray = handles;
  args.FenceValueArray = fence_values;
  args.Timeout = timeout;

  const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
  if (st == STATUS_TIMEOUT) {
    return kDxgiErrorWasStillDrawing;
  }
  if (!NT_SUCCESS(st)) {
    return E_FAIL;
  }

  // Waiting succeeded => the fence is at least complete even if we cannot query a monitored value.
  atomic_max_u64(&dev->last_completed_fence, fence);
  (void)AeroGpuQueryCompletedFence(dev);
  return S_OK;
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

  if (last_fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, last_fence);
  }
  ctx->cmd.reset();
  return last_fence;
}

void flush_locked(AeroGpuImmediateContext* ctx) {
  if (ctx) {
    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
    if (!cmd) {
      SetError(ctx->device, E_OUTOFMEMORY);
    } else {
      cmd->reserved0 = 0;
      cmd->reserved1 = 0;
    }
  }
  HRESULT hr = S_OK;
  submit_locked(ctx, false, &hr);
  if (FAILED(hr)) {
    SetError(ctx->device, hr);
  }
}

// Some DDIs (notably Present/RotateResourceIdentities) historically move between
// the device and context tables across D3D11 DDI interface versions. Bind them
// opportunistically based on whether the field exists and the pointer type
// matches.
template <typename T, typename = void>
struct HasPresent : std::false_type {};

template <typename T>
struct HasPresent<T, std::void_t<decltype(std::declval<T>().pfnPresent)>> : std::true_type {};

template <typename T, typename = void>
struct HasRotateResourceIdentities : std::false_type {};

template <typename T>
struct HasRotateResourceIdentities<T, std::void_t<decltype(std::declval<T>().pfnRotateResourceIdentities)>> : std::true_type {};

HRESULT AEROGPU_APIENTRY Present11(D3D11DDI_HDEVICECONTEXT hCtx, const D3D10DDIARG_PRESENT* pPresent);
void AEROGPU_APIENTRY RotateResourceIdentities11(D3D11DDI_HDEVICECONTEXT hCtx, D3D11DDI_HRESOURCE* pResources, UINT numResources);

HRESULT AEROGPU_APIENTRY Present11Device(D3D11DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent);
void AEROGPU_APIENTRY RotateResourceIdentities11Device(D3D11DDI_HDEVICE hDevice, D3D11DDI_HRESOURCE* pResources, UINT numResources);

template <typename TFuncs>
void BindPresentAndRotate(TFuncs* funcs) {
  if (!funcs) {
    return;
  }

  if constexpr (HasPresent<TFuncs>::value) {
    using Fn = decltype(funcs->pfnPresent);
    if constexpr (std::is_convertible_v<decltype(&Present11), Fn>) {
      funcs->pfnPresent = &Present11;
    } else if constexpr (std::is_convertible_v<decltype(&Present11Device), Fn>) {
      funcs->pfnPresent = &Present11Device;
    } else {
      funcs->pfnPresent = &AeroGpuDdiStub<Fn>::Func;
    }
  }

  if constexpr (HasRotateResourceIdentities<TFuncs>::value) {
    using Fn = decltype(funcs->pfnRotateResourceIdentities);
    if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11), Fn>) {
      funcs->pfnRotateResourceIdentities = &RotateResourceIdentities11;
    } else if constexpr (std::is_convertible_v<decltype(&RotateResourceIdentities11Device), Fn>) {
      funcs->pfnRotateResourceIdentities = &RotateResourceIdentities11Device;
    } else {
      funcs->pfnRotateResourceIdentities = &AeroGpuDdiStub<Fn>::Func;
    }
  }
}

// -------------------------------------------------------------------------------------------------
// Device DDI (D3D11DDI_DEVICEFUNCS)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice11(D3D11DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }

  uint32_t magic = 0;
  std::memcpy(&magic, hDevice.pDrvPrivate, sizeof(magic));
  if (magic != kAeroGpuDeviceMagic) {
    return;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  // Mark destroyed early so a re-entrant or duplicate DestroyDevice call becomes a no-op.
  dev->magic = 0;

  // Immediate context is owned by the device allocation (allocated via new below).
  if (dev->immediate) {
    dev->immediate->~AeroGpuImmediateContext();
    dev->immediate = nullptr;
  }

  dev->~AeroGpuDevice();
}

HRESULT AEROGPU_APIENTRY GetDeviceRemovedReason11(D3D11DDI_HDEVICE) {
  // The runtime expects S_OK when the device is healthy. Returning E_NOTIMPL
  // here can cause higher-level API calls like ID3D11Device::GetDeviceRemovedReason
  // to fail unexpectedly.
  return S_OK;
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

template <typename T, typename = void>
struct HasMember_pInitialDataUP : std::false_type {};
template <typename T>
struct HasMember_pInitialDataUP<T, std::void_t<decltype(std::declval<T>().pInitialDataUP)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_pInitialData : std::false_type {};
template <typename T>
struct HasMember_pInitialData<T, std::void_t<decltype(std::declval<T>().pInitialData)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_pSysMem : std::false_type {};
template <typename T>
struct HasMember_pSysMem<T, std::void_t<decltype(std::declval<T>().pSysMem)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_SysMemPitch : std::false_type {};
template <typename T>
struct HasMember_SysMemPitch<T, std::void_t<decltype(std::declval<T>().SysMemPitch)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_pSysMemUP : std::false_type {};
template <typename T>
struct HasMember_pSysMemUP<T, std::void_t<decltype(std::declval<T>().pSysMemUP)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_RowPitch : std::false_type {};
template <typename T>
struct HasMember_RowPitch<T, std::void_t<decltype(std::declval<T>().RowPitch)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_DepthPitch : std::false_type {};
template <typename T>
struct HasMember_DepthPitch<T, std::void_t<decltype(std::declval<T>().DepthPitch)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_SrcPitch : std::false_type {};
template <typename T>
struct HasMember_SrcPitch<T, std::void_t<decltype(std::declval<T>().SrcPitch)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_SrcSlicePitch : std::false_type {};
template <typename T>
struct HasMember_SrcSlicePitch<T, std::void_t<decltype(std::declval<T>().SrcSlicePitch)>> : std::true_type {};

template <typename T, typename = void>
struct HasMember_SysMemSlicePitch : std::false_type {};
template <typename T>
struct HasMember_SysMemSlicePitch<T, std::void_t<decltype(std::declval<T>().SysMemSlicePitch)>> : std::true_type {};

HRESULT AEROGPU_APIENTRY CreateResource11(D3D11DDI_HDEVICE hDevice,
                                          const D3D11DDIARG_CREATERESOURCE* pDesc,
                                          D3D11DDI_HRESOURCE hResource,
                                          D3D11DDI_HRTRESOURCE) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter || !dev->immediate) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  res->handle = allocate_global_handle(dev->adapter);
  res->bind_flags = static_cast<uint32_t>(pDesc->BindFlags);
  res->misc_flags = static_cast<uint32_t>(pDesc->MiscFlags);
  res->usage = static_cast<uint32_t>(pDesc->Usage);
  res->cpu_access_flags = static_cast<uint32_t>(pDesc->CPUAccessFlags);

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);

  const auto emit_upload_locked = [&](const uint64_t offset, const uint64_t size) -> HRESULT {
    if (offset + size > static_cast<uint64_t>(res->storage.size())) {
      return E_INVALIDARG;
    }
    if (size == 0) {
      return S_OK;
    }

    auto* upload = dev->immediate->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE,
        res->storage.data() + static_cast<size_t>(offset),
        static_cast<size_t>(size));
    if (!upload) {
      return E_OUTOFMEMORY;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = offset;
    upload->size_bytes = size;
    return S_OK;
  };

  const auto copy_initial_bytes = [&](const void* src, size_t bytes) -> HRESULT {
    if (!src || bytes == 0) {
      return S_OK;
    }
    bytes = std::min(bytes, res->storage.size());
    if (!bytes) {
      return S_OK;
    }
    std::memcpy(res->storage.data(), src, bytes);
    return emit_upload_locked(/*offset=*/0, /*size=*/bytes);
  };

  const auto copy_initial_tex2d = [&](const void* src, UINT src_pitch) -> HRESULT {
    if (!src || res->row_pitch_bytes == 0 || res->height == 0) {
      return S_OK;
    }
    if (res->storage.empty()) {
      return S_OK;
    }
    const uint8_t* src_bytes = reinterpret_cast<const uint8_t*>(src);
    const uint32_t pitch = src_pitch ? src_pitch : res->row_pitch_bytes;
    for (uint32_t y = 0; y < res->height; y++) {
      std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                  src_bytes + static_cast<size_t>(y) * pitch,
                  res->row_pitch_bytes);
    }
    return emit_upload_locked(/*offset=*/0, /*size=*/res->storage.size());
  };

  const auto maybe_copy_initial = [&](auto init_ptr) -> HRESULT {
    if (!init_ptr) {
      return S_OK;
    }

    using ElemT = std::remove_pointer_t<decltype(init_ptr)>;
    if constexpr (std::is_void_v<ElemT>) {
      if (res->kind == ResourceKind::Buffer) {
        return copy_initial_bytes(init_ptr, static_cast<size_t>(res->size_bytes));
      }
      return copy_initial_bytes(init_ptr, res->storage.size());
    } else if constexpr (HasMember_pSysMem<ElemT>::value) {
      const void* sys = init_ptr[0].pSysMem;
      UINT pitch = 0;
      if constexpr (HasMember_SysMemPitch<ElemT>::value) {
        pitch = init_ptr[0].SysMemPitch;
      }
      if (res->kind == ResourceKind::Buffer) {
        return copy_initial_bytes(sys, static_cast<size_t>(res->size_bytes));
      }
      if (res->kind == ResourceKind::Texture2D) {
        return copy_initial_tex2d(sys, pitch);
      }
      return S_OK;
    } else {
      return S_OK;
    }
  };

  if (dim == D3D10DDIRESOURCE_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(pDesc->ByteWidth);
    try {
      res->storage.resize(static_cast<size_t>(res->size_bytes));
    } catch (...) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }

    auto* cmd = dev->immediate->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    HRESULT hr = S_OK;
    if constexpr (HasMember_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      hr = maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (HasMember_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      hr = maybe_copy_initial(pDesc->pInitialData);
    }
    if (FAILED(hr)) {
      res->~AeroGpuResource();
      return hr;
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
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);
    const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    try {
      res->storage.resize(static_cast<size_t>(total_bytes));
    } catch (...) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }

    auto* cmd = dev->immediate->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = 1;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    HRESULT hr = S_OK;
    if constexpr (HasMember_pInitialDataUP<D3D11DDIARG_CREATERESOURCE>::value) {
      hr = maybe_copy_initial(pDesc->pInitialDataUP);
    } else if constexpr (HasMember_pInitialData<D3D11DDIARG_CREATERESOURCE>::value) {
      hr = maybe_copy_initial(pDesc->pInitialData);
    }
    if (FAILED(hr)) {
      res->~AeroGpuResource();
      return hr;
    }

    return S_OK;
  }

  res->~AeroGpuResource();
  return E_NOTIMPL;
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
  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource) : nullptr;
  auto* rtv = new (hView.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->resource = res;
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
  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource) : nullptr;
  auto* dsv = new (hView.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->resource = res;
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
  auto* res = pDesc->hResource.pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource) : nullptr;
  srv->texture = res ? res->handle : 0;
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
                                              const D3D11DDIARG_CREATEVERTEXSHADER* pDesc,
                                              D3D11DDI_HVERTEXSHADER hShader,
                                              D3D11DDI_HRTVERTEXSHADER) {
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter || !dev->immediate) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = AEROGPU_SHADER_STAGE_VERTEX;
  if (!pDesc->pShaderCode || pDesc->ShaderCodeSize == 0) {
    sh->~AeroGpuShader();
    return E_INVALIDARG;
  }
  try {
    sh->dxbc.resize(static_cast<size_t>(pDesc->ShaderCodeSize));
  } catch (...) {
    sh->~AeroGpuShader();
    return E_OUTOFMEMORY;
  }
  std::memcpy(sh->dxbc.data(), pDesc->pShaderCode, static_cast<size_t>(pDesc->ShaderCodeSize));
  auto* cmd = dev->immediate->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  if (!cmd) {
    sh->~AeroGpuShader();
    return E_OUTOFMEMORY;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = sh->stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyVertexShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HVERTEXSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sh = FromHandle<D3D11DDI_HVERTEXSHADER, AeroGpuShader>(hShader);
  if (!dev || !sh) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->immediate && sh->handle != kInvalidHandle) {
    std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
    auto* cmd = dev->immediate->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    if (cmd) {
      cmd->shader_handle = sh->handle;
      cmd->reserved0 = 0;
    }
  }
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivatePixelShaderSize11(D3D11DDI_HDEVICE, const D3D11DDIARG_CREATEPIXELSHADER*) {
  return sizeof(AeroGpuShader);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader11(D3D11DDI_HDEVICE hDevice,
                                             const D3D11DDIARG_CREATEPIXELSHADER* pDesc,
                                             D3D11DDI_HPIXELSHADER hShader,
                                             D3D11DDI_HRTPIXELSHADER) {
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter || !dev->immediate) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = allocate_global_handle(dev->adapter);
  sh->stage = AEROGPU_SHADER_STAGE_PIXEL;
  if (!pDesc->pShaderCode || pDesc->ShaderCodeSize == 0) {
    sh->~AeroGpuShader();
    return E_INVALIDARG;
  }
  try {
    sh->dxbc.resize(static_cast<size_t>(pDesc->ShaderCodeSize));
  } catch (...) {
    sh->~AeroGpuShader();
    return E_OUTOFMEMORY;
  }
  std::memcpy(sh->dxbc.data(), pDesc->pShaderCode, static_cast<size_t>(pDesc->ShaderCodeSize));
  auto* cmd = dev->immediate->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  if (!cmd) {
    sh->~AeroGpuShader();
    return E_OUTOFMEMORY;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = sh->stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyPixelShader11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HPIXELSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sh = FromHandle<D3D11DDI_HPIXELSHADER, AeroGpuShader>(hShader);
  if (!dev || !sh) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->immediate && sh->handle != kInvalidHandle) {
    std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
    auto* cmd = dev->immediate->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    if (cmd) {
      cmd->shader_handle = sh->handle;
      cmd->reserved0 = 0;
    }
  }
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
  sh->handle = allocate_global_handle(dev->adapter);
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
                                               const D3D11DDIARG_CREATEELEMENTLAYOUT* pDesc,
                                               D3D11DDI_HELEMENTLAYOUT hLayout,
                                               D3D11DDI_HRTELEMENTLAYOUT) {
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter || !dev->immediate) {
    return E_FAIL;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = allocate_global_handle(dev->adapter);

  const UINT elem_count = pDesc->NumElements;
  if (!pDesc->pVertexElements || elem_count == 0) {
    layout->~AeroGpuInputLayout();
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
    layout->~AeroGpuInputLayout();
    return E_OUTOFMEMORY;
  }
  std::memcpy(layout->blob.data(), &header, sizeof(header));
  std::memcpy(layout->blob.data() + sizeof(header), elems.data(), elems.size() * sizeof(elems[0]));

  auto* cmd = dev->immediate->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, layout->blob.data(), layout->blob.size());
  if (!cmd) {
    layout->~AeroGpuInputLayout();
    return E_OUTOFMEMORY;
  }
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyElementLayout11(D3D11DDI_HDEVICE hDevice, D3D11DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate || !hLayout.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* layout = FromHandle<D3D11DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  if (!dev || !layout) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (dev->immediate && layout->handle != kInvalidHandle) {
    std::lock_guard<std::mutex> ctx_lock(dev->immediate->mutex);
    auto* cmd = dev->immediate->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    if (cmd) {
      cmd->input_layout_handle = layout->handle;
      cmd->reserved0 = 0;
    }
  }
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
  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
  ctx->current_input_layout = handle;
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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
  const uint32_t new_topology = static_cast<uint32_t>(topology);
  if (ctx->current_topology == new_topology) {
    return;
  }
  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
  ctx->current_topology = new_topology;
  cmd->topology = new_topology;
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
  const aerogpu_handle_t vs = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HVERTEXSHADER, AeroGpuShader>(hShader)->handle : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
  ctx->current_vs = vs;
  cmd->vs = vs;
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
  const aerogpu_handle_t ps = hShader.pDrvPrivate ? FromHandle<D3D11DDI_HPIXELSHADER, AeroGpuShader>(hShader)->handle : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
  cmd->vs = ctx->current_vs;
  cmd->ps = ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
  ctx->current_ps = ps;
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
  // Geometry shaders are currently ignored (no GS stage in the AeroGPU command
  // stream / WebGPU backend). Binding/unbinding must not fail so apps can use
  // pass-through GS shaders (e.g. to rename varyings).
}

template <typename THandle>
static bool AnyNonNullHandles(const THandle* handles, UINT count);

void AEROGPU_APIENTRY VsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY PsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY GsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

template <typename THandle>
static bool AnyNonNullHandles(const THandle* handles, UINT count) {
  if (!handles || count == 0) {
    return false;
  }
  for (UINT i = 0; i < count; ++i) {
    if (handles[i].pDrvPrivate) {
      return true;
    }
  }
  return false;
}

template <typename FnPtr>
struct SoSetTargetsThunk;

template <typename Ret, typename... Args>
struct SoSetTargetsThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_void_v<Ret>) {
      return;
    } else {
      return {};
    }
  }
};

// Stream-output is unsupported for bring-up. Treat unbind (all-null handles) as a
// no-op but report E_NOTIMPL if an app attempts to bind real targets.
template <typename Ret, typename TargetsPtr, typename... Tail>
struct SoSetTargetsThunk<Ret(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT, UINT, TargetsPtr, Tail...)> {
  static Ret AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx, UINT NumTargets, TargetsPtr phTargets, Tail... tail) {
    ((void)tail, ...);
    if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phTargets, NumTargets)) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return S_OK;
      } else if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return {};
      }
    }
    SetError(DeviceFromHandle(hCtx), E_NOTIMPL);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (!std::is_void_v<Ret>) {
      return {};
    }
  }
};

template <typename FnPtr>
struct SetPredicationThunk;

template <typename Ret, typename... Args>
struct SetPredicationThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_void_v<Ret>) {
      return;
    } else {
      return {};
    }
  }
};

// Predication is optional. Treat clearing/unbinding as a no-op but report
// E_NOTIMPL when a non-null predicate is set.
template <typename Ret, typename PredicateHandle, typename... Tail>
struct SetPredicationThunk<Ret(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT, PredicateHandle, Tail...)> {
  static Ret AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx, PredicateHandle hPredicate, Tail... tail) {
    ((void)tail, ...);
    if (!hCtx.pDrvPrivate || !hPredicate.pDrvPrivate) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return S_OK;
      } else if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return {};
      }
    }
    SetError(DeviceFromHandle(hCtx), E_NOTIMPL);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (!std::is_void_v<Ret>) {
      return {};
    }
  }
};

// Tessellation and compute stages are unsupported in the current FL10_0 bring-up implementation.
// These entrypoints must behave like no-ops when clearing/unbinding (runtime ClearState), but
// should still report E_NOTIMPL when an app attempts to bind real state.
void AEROGPU_APIENTRY HsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HHULLSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY HsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HDOMAINSHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY DsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetShader11(D3D11DDI_HDEVICECONTEXT hCtx,
                                   D3D11DDI_HCOMPUTESHADER hShader,
                                   const D3D11DDI_HCLASSINSTANCE*,
                                   UINT) {
  if (!hCtx.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetConstantBuffers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumBuffers,
                                             const D3D11DDI_HRESOURCE* phBuffers,
                                             const UINT*,
                                             const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phBuffers, NumBuffers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY CsSetUnorderedAccessViews11(D3D11DDI_HDEVICECONTEXT hCtx,
                                                  UINT,
                                                  UINT NumUavs,
                                                  const D3D11DDI_HUNORDEREDACCESSVIEW* phUavs,
                                                  const UINT*) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phUavs, NumUavs)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

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
    if (!cmd) {
      SetError(ctx->device, E_OUTOFMEMORY);
      return;
    }
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
    if (!cmd) {
      SetError(ctx->device, E_OUTOFMEMORY);
      return;
    }
    cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->slot = StartSlot + i;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
}

void AEROGPU_APIENTRY GsSetShaderResources11(D3D11DDI_HDEVICECONTEXT hCtx,
                                             UINT,
                                             UINT NumViews,
                                             const D3D11DDI_HSHADERRESOURCEVIEW* phViews) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phViews, NumViews)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY VsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY PsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

void AEROGPU_APIENTRY GsSetSamplers11(D3D11DDI_HDEVICECONTEXT hCtx,
                                      UINT,
                                      UINT NumSamplers,
                                      const D3D11DDI_HSAMPLER* phSamplers) {
  if (!hCtx.pDrvPrivate || !AnyNonNullHandles(phSamplers, NumSamplers)) {
    return;
  }
  auto* ctx = FromHandle<D3D11DDI_HDEVICECONTEXT, AeroGpuImmediateContext>(hCtx);
  if (!ctx || !ctx->device) {
    return;
  }
  SetError(ctx->device, E_NOTIMPL);
}

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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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

  const uint32_t new_rtv_count = (NumViews > AEROGPU_MAX_RENDER_TARGETS) ? AEROGPU_MAX_RENDER_TARGETS : NumViews;
  aerogpu_handle_t new_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* new_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < new_rtv_count; i++) {
    if (phRtvs && phRtvs[i].pDrvPrivate) {
      auto* view = FromHandle<D3D11DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phRtvs[i]);
      AeroGpuResource* res = view ? view->resource : nullptr;
      new_rtv_resources[i] = res;
      new_rtvs[i] = res ? res->handle : 0;
    }
  }
  AeroGpuResource* new_dsv_resource =
      hDsv.pDrvPrivate ? FromHandle<D3D11DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->resource : nullptr;
  const aerogpu_handle_t new_dsv = new_dsv_resource ? new_dsv_resource->handle : 0;

  auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }

  ctx->current_rtv_count = new_rtv_count;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    ctx->current_rtvs[i] = new_rtvs[i];
    ctx->current_rtv_resources[i] = new_rtv_resources[i];
  }
  ctx->current_dsv_resource = new_dsv_resource;
  ctx->current_dsv = new_dsv;

  cmd->color_count = new_rtv_count;
  cmd->depth_stencil = new_dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = new_rtvs[i];
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = IndexCount;
  cmd->instance_count = 1;
  cmd->first_index = StartIndexLocation;
  cmd->base_vertex = BaseVertexLocation;
  cmd->first_instance = 0;
}

// -------------------------------------------------------------------------------------------------
// Resource update/copy/map DDIs (Win7 WDK D3D11 immediate context)
// -------------------------------------------------------------------------------------------------

template <typename T>
uint32_t to_u32(T v) {
  if constexpr (std::is_enum_v<T>) {
    return static_cast<uint32_t>(v);
  } else {
    return static_cast<uint32_t>(v);
  }
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

template <typename TMappedSubresource>
HRESULT map_resource_locked(AeroGpuResource* res,
                            uint32_t subresource,
                            uint32_t map_type,
                            uint32_t map_flags,
                            TMappedSubresource* pMapped) {
  (void)map_flags;
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
    case kD3D11MapRead:
      want_read = true;
      break;
    case kD3D11MapWrite:
    case kD3D11MapWriteDiscard:
    case kD3D11MapWriteNoOverwrite:
      want_write = true;
      break;
    case kD3D11MapReadWrite:
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

void unmap_resource_locked(AeroGpuImmediateContext* ctx, AeroGpuResource* res, uint32_t subresource) {
  if (!ctx || !res) {
    return;
  }
  if (!res->mapped) {
    return;
  }
  if (subresource != res->mapped_subresource) {
    return;
  }

  if (res->mapped_write && res->handle != kInvalidHandle) {
    // Inline updated bytes into the DMA buffer so the host does not have to
    // dereference guest pointers.
    if (res->mapped_offset_bytes + res->mapped_size_bytes <= static_cast<uint64_t>(res->storage.size())) {
      const auto offset = static_cast<size_t>(res->mapped_offset_bytes);
      const auto size = static_cast<size_t>(res->mapped_size_bytes);
      auto* upload = ctx->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + offset, size);
      if (!upload) {
        SetError(ctx->device, E_OUTOFMEMORY);
      } else {
        upload->resource_handle = res->handle;
        upload->reserved0 = 0;
        upload->offset_bytes = res->mapped_offset_bytes;
        upload->size_bytes = res->mapped_size_bytes;
      }
    }
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

  res->mapped_via_allocation = false;
  res->mapped_ptr = nullptr;
  *ppData = res->storage.data();
  return S_OK;
}

template <typename TMappedSubresource>
HRESULT MapImpl(AeroGpuImmediateContext* ctx,
                AeroGpuResource* res,
                uint32_t subresource,
                uint32_t map_type,
                uint32_t map_flags,
                TMappedSubresource* pMapped) {
  if (!ctx || !ctx->device || !res) {
    return E_INVALIDARG;
  }

  if (!pMapped) {
    return E_INVALIDARG;
  }

  if (map_type == kD3D11MapWriteDiscard) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }

    if (res->kind == ResourceKind::Buffer) {
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
  } else if (map_type == kD3D11MapWriteNoOverwrite) {
    if (subresource != 0) {
      return E_INVALIDARG;
    }

    if (res->kind == ResourceKind::Buffer) {
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

  // Conservative: only support generic map on buffers and staging textures.
  if (res->kind == ResourceKind::Texture2D && res->bind_flags == 0) {
    return map_resource_locked(res, subresource, map_type, map_flags, pMapped);
  }
  if (res->kind == ResourceKind::Buffer) {
    return map_resource_locked(res, subresource, map_type, map_flags, pMapped);
  }
  return E_NOTIMPL;
}

HRESULT UpdateSubresourceUPImpl(AeroGpuImmediateContext* ctx,
                                AeroGpuResource* res,
                                uint32_t dst_subresource,
                                const void* pDstBox,
                                const void* pSysMem,
                                uint32_t SysMemPitch,
                                uint32_t SysMemSlicePitch) {
  (void)SysMemSlicePitch;
  if (!ctx || !ctx->device || !res || !pSysMem) {
    return E_INVALIDARG;
  }
  if (dst_subresource != 0) {
    return E_NOTIMPL;
  }

  if (res->handle == kInvalidHandle) {
    return E_FAIL;
  }

  struct Box {
    uint32_t left;
    uint32_t top;
    uint32_t front;
    uint32_t right;
    uint32_t bottom;
    uint32_t back;
  };

  if (!pDstBox) {
    if (res->kind == ResourceKind::Buffer) {
      HRESULT hr = ensure_resource_storage(res, res->size_bytes);
      if (FAILED(hr)) {
        return hr;
      }
      if (res->storage.size() < static_cast<size_t>(res->size_bytes)) {
        return E_FAIL;
      }
      std::memcpy(res->storage.data(), pSysMem, static_cast<size_t>(res->size_bytes));
    } else if (res->kind == ResourceKind::Texture2D) {
      const uint64_t total = resource_total_bytes(res);
      if (!total) {
        return E_FAIL;
      }
      HRESULT hr = ensure_resource_storage(res, total);
      if (FAILED(hr)) {
        return hr;
      }

      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(res->row_pitch_bytes);
      if (src_pitch < static_cast<size_t>(res->row_pitch_bytes)) {
        return E_INVALIDARG;
      }
      const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    res->row_pitch_bytes);
      }
    } else {
      return E_NOTIMPL;
    }

    auto* upload = ctx->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
    if (!upload) {
      return E_OUTOFMEMORY;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = 0;
    upload->size_bytes = res->storage.size();
    return S_OK;
  }

  const auto* box = reinterpret_cast<const Box*>(pDstBox);
  if (!box) {
    return E_INVALIDARG;
  }

  if (res->kind == ResourceKind::Buffer) {
    if (box->top != 0 || box->bottom != 1 || box->front != 0 || box->back != 1) {
      return E_INVALIDARG;
    }
    if (box->left >= box->right) {
      return E_INVALIDARG;
    }
    const uint64_t offset = box->left;
    const uint64_t size = static_cast<uint64_t>(box->right) - static_cast<uint64_t>(box->left);
    if (offset + size > res->size_bytes) {
      return E_INVALIDARG;
    }

    HRESULT hr = ensure_resource_storage(res, res->size_bytes);
    if (FAILED(hr)) {
      return hr;
    }
    if (res->storage.size() < static_cast<size_t>(res->size_bytes)) {
      return E_FAIL;
    }
    std::memcpy(res->storage.data() + static_cast<size_t>(offset), pSysMem, static_cast<size_t>(size));

    auto* upload = ctx->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, pSysMem, static_cast<size_t>(size));
    if (!upload) {
      return E_OUTOFMEMORY;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = offset;
    upload->size_bytes = size;
    return S_OK;
  }

  if (res->kind == ResourceKind::Texture2D) {
    if (box->front != 0 || box->back != 1) {
      return E_INVALIDARG;
    }
    if (box->left >= box->right || box->top >= box->bottom) {
      return E_INVALIDARG;
    }
    if (box->right > res->width || box->bottom > res->height) {
      return E_INVALIDARG;
    }

    const uint32_t aerogpu_format = dxgi_format_to_aerogpu(res->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aerogpu_format);
    const size_t row_bytes = static_cast<size_t>(box->right - box->left) * static_cast<size_t>(bpp);
    const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : row_bytes;
    if (row_bytes == 0 || row_bytes > src_pitch) {
      return E_INVALIDARG;
    }
    if (row_bytes > res->row_pitch_bytes) {
      return E_INVALIDARG;
    }

    const uint64_t total = resource_total_bytes(res);
    if (!total) {
      return E_FAIL;
    }
    HRESULT hr = ensure_resource_storage(res, total);
    if (FAILED(hr)) {
      return hr;
    }

    const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
    const size_t dst_pitch = static_cast<size_t>(res->row_pitch_bytes);
    const size_t dst_x_bytes = static_cast<size_t>(box->left) * static_cast<size_t>(bpp);
    for (uint32_t y = 0; y < (box->bottom - box->top); ++y) {
      const size_t dst_offset = (static_cast<size_t>(box->top) + y) * dst_pitch + dst_x_bytes;
      std::memcpy(res->storage.data() + dst_offset, src + y * src_pitch, row_bytes);
    }

    // Keep browser executor compatibility: partial UPLOAD_RESOURCE ranges are only
    // supported for tightly packed textures (row_pitch_bytes == width*4).
    const size_t tight_row_bytes = static_cast<size_t>(res->width) * static_cast<size_t>(bpp);
    size_t upload_offset = static_cast<size_t>(box->top) * dst_pitch;
    size_t upload_size = static_cast<size_t>(box->bottom - box->top) * dst_pitch;
    if (dst_pitch != tight_row_bytes) {
      upload_offset = 0;
      upload_size = res->storage.size();
    }
    if (upload_offset > res->storage.size() || upload_size > res->storage.size() - upload_offset) {
      return E_FAIL;
    }
    auto* upload = ctx->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + upload_offset, upload_size);
    if (!upload) {
      return E_OUTOFMEMORY;
    }
    upload->resource_handle = res->handle;
    upload->reserved0 = 0;
    upload->offset_bytes = upload_offset;
    upload->size_bytes = upload_size;
    return S_OK;
  }

  return E_NOTIMPL;
}

HRESULT CopyResourceImpl(AeroGpuImmediateContext* ctx, AeroGpuResource* dst, AeroGpuResource* src) {
  if (!ctx || !ctx->device || !dst || !src) {
    return E_INVALIDARG;
  }
  if (dst->kind != src->kind) {
    return E_INVALIDARG;
  }

  if (dst->kind == ResourceKind::Buffer) {
    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
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
    return S_OK;
  }

  if (dst->kind == ResourceKind::Texture2D) {
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0) {
      return E_INVALIDARG;
    }

    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
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
    return S_OK;
  }

  return E_NOTIMPL;
}

HRESULT CopySubresourceRegionImpl(AeroGpuImmediateContext* ctx,
                                  AeroGpuResource* dst,
                                  uint32_t dst_subresource,
                                  uint32_t dst_x,
                                  uint32_t dst_y,
                                  uint32_t dst_z,
                                  AeroGpuResource* src,
                                  uint32_t src_subresource,
                                  const void* pSrcBox) {
  if (!ctx || !ctx->device || !dst || !src) {
    return E_INVALIDARG;
  }
  if (dst_subresource != 0 || src_subresource != 0 || dst_x != 0 || dst_y != 0 || dst_z != 0 || pSrcBox) {
    return E_NOTIMPL;
  }
  return CopyResourceImpl(ctx, dst, src);
}

template <typename FnPtr>
struct Thunk;

template <typename Ret, typename... Args>
struct Thunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  using Return = Ret;
  static constexpr size_t Arity = sizeof...(Args);
};

template <typename FnPtr>
struct MapThunk;

template <typename Ret, typename... Args>
struct MapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 6) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      const uint32_t subresource = to_u32(std::get<2>(tup));
      const uint32_t map_type = to_u32(std::get<3>(tup));
      const uint32_t map_flags = to_u32(std::get<4>(tup));
      auto* pMapped = std::get<5>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      std::lock_guard<std::mutex> lock(ctx->mutex);
      const HRESULT hr = MapImpl(ctx, res, subresource, map_type, map_flags, pMapped);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else {
      (void)tup;
      if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct UnmapThunk;

template <typename Ret, typename... Args>
struct UnmapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      const uint32_t subresource = to_u32(std::get<2>(tup));

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !res) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      std::lock_guard<std::mutex> lock(ctx->mutex);
      unmap_resource_locked(ctx, res, subresource);
      if constexpr (!std::is_void_v<Ret>) {
        return S_OK;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct StagingResourceMapThunk;

template <typename Ret, typename... Args>
struct StagingResourceMapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 6) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      const uint32_t subresource = to_u32(std::get<2>(tup));
      const uint32_t map_type = to_u32(std::get<3>(tup));
      const uint32_t map_flags = to_u32(std::get<4>(tup));
      auto* pMapped = std::get<5>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      const HRESULT hr = map_resource_locked(res, subresource, map_type, map_flags, pMapped);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else {
      (void)tup;
      if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct StagingResourceUnmapThunk;

template <typename Ret, typename... Args>
struct StagingResourceUnmapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      const uint32_t subresource = to_u32(std::get<2>(tup));

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !res) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      std::lock_guard<std::mutex> lock(ctx->mutex);
      unmap_resource_locked(ctx, res, subresource);
      if constexpr (!std::is_void_v<Ret>) {
        return S_OK;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct DynamicIABufferMapDiscardThunk;

template <typename Ret, typename... Args>
struct DynamicIABufferMapDiscardThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      auto* ppData = std::get<2>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      const HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, ppData);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else {
      (void)tup;
      if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct DynamicIABufferMapNoOverwriteThunk;

template <typename Ret, typename... Args>
struct DynamicIABufferMapNoOverwriteThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      auto* ppData = std::get<2>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      const HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/false, ppData);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else {
      (void)tup;
      if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct DynamicIABufferUnmapThunk;

template <typename Ret, typename... Args>
struct DynamicIABufferUnmapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 2) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !res) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      unmap_resource_locked(ctx, res, /*subresource=*/0);
      if constexpr (!std::is_void_v<Ret>) {
        return S_OK;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct DynamicConstantBufferMapDiscardThunk;

template <typename Ret, typename... Args>
struct DynamicConstantBufferMapDiscardThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      auto* ppData = std::get<2>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          return;
        } else {
          return E_INVALIDARG;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      const HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, ppData);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else {
      (void)tup;
      if constexpr (std::is_void_v<Ret>) {
        return;
      } else {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct DynamicConstantBufferUnmapThunk;

template <typename Ret, typename... Args>
struct DynamicConstantBufferUnmapThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 2) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);

      if (!hCtx.pDrvPrivate || !hRes.pDrvPrivate) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }
      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !res) {
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      unmap_resource_locked(ctx, res, /*subresource=*/0);
      if constexpr (!std::is_void_v<Ret>) {
        return S_OK;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct SetRenderTargetsAndUnorderedAccessViewsThunk;

template <typename Ret, typename... Args>
struct SetRenderTargetsAndUnorderedAccessViewsThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_void_v<Ret>) {
      return;
    } else {
      return {};
    }
  }
};

template <typename Ret, typename NumViewsT, typename... Tail>
struct SetRenderTargetsAndUnorderedAccessViewsThunk<
    Ret(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT,
                           NumViewsT,
                           const D3D11DDI_HRENDERTARGETVIEW*,
                           D3D11DDI_HDEPTHSTENCILVIEW,
                           Tail...)> {
  static Ret AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx,
                                  NumViewsT numViews,
                                  const D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                  D3D11DDI_HDEPTHSTENCILVIEW hDsv,
                                  Tail... tail) {
    // Always bind render targets (supported subset). UAV binding is unsupported, but unbinding is benign.
    SetRenderTargets11(hCtx, to_u32(numViews), phRtvs, hDsv);

    if constexpr (sizeof...(Tail) >= 3) {
      using TailTuple = std::tuple<Tail...>;
      using CountT = std::tuple_element_t<1, TailTuple>;
      using UavPtrT = std::tuple_element_t<2, TailTuple>;
      if constexpr ((std::is_integral_v<std::decay_t<CountT>> || std::is_enum_v<std::decay_t<CountT>>) &&
                    std::is_pointer_v<std::decay_t<UavPtrT>> &&
                    std::is_same_v<std::remove_cv_t<std::remove_pointer_t<std::decay_t<UavPtrT>>>, D3D11DDI_HUNORDEREDACCESSVIEW>) {
        auto tail_tup = std::forward_as_tuple(tail...);
        const uint32_t num_uavs = to_u32(std::get<1>(tail_tup));
        const auto* ph_uavs = std::get<2>(tail_tup);
        if (AnyNonNullHandles(ph_uavs, num_uavs)) {
          SetError(DeviceFromHandle(hCtx), E_NOTIMPL);
          if constexpr (std::is_same_v<Ret, HRESULT>) {
            return E_NOTIMPL;
          } else if constexpr (std::is_void_v<Ret>) {
            return;
          } else {
            return {};
          }
        }
      }
    }

    ((void)tail, ...);
    if constexpr (std::is_same_v<Ret, HRESULT>) {
      return S_OK;
    } else if constexpr (std::is_void_v<Ret>) {
      return;
    } else {
      return {};
    }
  }
};

template <typename Ret, typename NumViewsT, typename... Tail>
struct SetRenderTargetsAndUnorderedAccessViewsThunk<
    Ret(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT,
                           NumViewsT,
                           D3D11DDI_HRENDERTARGETVIEW*,
                           D3D11DDI_HDEPTHSTENCILVIEW,
                           Tail...)> {
  static Ret AEROGPU_APIENTRY Impl(D3D11DDI_HDEVICECONTEXT hCtx,
                                  NumViewsT numViews,
                                  D3D11DDI_HRENDERTARGETVIEW* phRtvs,
                                  D3D11DDI_HDEPTHSTENCILVIEW hDsv,
                                  Tail... tail) {
    return SetRenderTargetsAndUnorderedAccessViewsThunk<
        Ret(AEROGPU_APIENTRY*)(D3D11DDI_HDEVICECONTEXT,
                               NumViewsT,
                               const D3D11DDI_HRENDERTARGETVIEW*,
                               D3D11DDI_HDEPTHSTENCILVIEW,
                               Tail...)>::Impl(hCtx, numViews, phRtvs, hDsv, tail...);
  }
};

template <typename FnPtr>
struct UpdateSubresourceUPThunk;

template <typename Ret, typename... Args>
struct UpdateSubresourceUPThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 7) {
      const auto hCtx = std::get<0>(tup);
      const auto hRes = std::get<1>(tup);
      const uint32_t dst_subresource = to_u32(std::get<2>(tup));
      const void* pDstBox = std::get<3>(tup);
      const void* pSysMem = std::get<4>(tup);
      const uint32_t sys_pitch = to_u32(std::get<5>(tup));
      const uint32_t sys_slice_pitch = to_u32(std::get<6>(tup));

      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* res = FromHandle<decltype(hRes), AeroGpuResource>(hRes);
      if (!ctx || !ctx->device || !res || !pSysMem) {
        if (ctx && ctx->device) {
          SetError(ctx->device, E_INVALIDARG);
        }
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      HRESULT hr = UpdateSubresourceUPImpl(ctx, res, dst_subresource, pDstBox, pSysMem, sys_pitch, sys_slice_pitch);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
        return;
      } else {
        return hr;
      }
    } else if constexpr (sizeof...(Args) == 2) {
      using ArgTuple = std::tuple<Args...>;
      using Arg1 = std::tuple_element_t<1, ArgTuple>;
      if constexpr (std::is_pointer_v<std::decay_t<Arg1>> &&
                    std::is_same_v<std::remove_cv_t<std::remove_pointer_t<std::decay_t<Arg1>>>, D3D11DDIARG_UPDATESUBRESOURCEUP>) {
        const auto hCtx = std::get<0>(tup);
        const auto* pArgs = std::get<1>(tup);

        const void* pSysMem = nullptr;
        if constexpr (HasMember_pSysMemUP<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          pSysMem = pArgs ? pArgs->pSysMemUP : nullptr;
        } else if constexpr (HasMember_pSysMem<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          pSysMem = pArgs ? pArgs->pSysMem : nullptr;
        }

        uint32_t sys_pitch = 0;
        if constexpr (HasMember_SrcPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->SrcPitch) : 0;
        } else if constexpr (HasMember_RowPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->RowPitch) : 0;
        } else if constexpr (HasMember_SysMemPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->SysMemPitch) : 0;
        }

        uint32_t sys_slice_pitch = 0;
        if constexpr (HasMember_SrcSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->SrcSlicePitch) : 0;
        } else if constexpr (HasMember_DepthPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->DepthPitch) : 0;
        } else if constexpr (HasMember_SysMemSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->SysMemSlicePitch) : 0;
        }

        if (!pArgs) {
          if constexpr (!std::is_void_v<Ret>) {
            return E_INVALIDARG;
          } else {
            return;
          }
        }

        auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
        auto* res = FromHandle<decltype(pArgs->hDstResource), AeroGpuResource>(pArgs->hDstResource);
        if (!ctx || !ctx->device || !res || !pSysMem) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          if constexpr (!std::is_void_v<Ret>) {
            return E_INVALIDARG;
          } else {
            return;
          }
        }

        std::lock_guard<std::mutex> lock(ctx->mutex);
        HRESULT hr = UpdateSubresourceUPImpl(ctx,
                                            res,
                                            to_u32(pArgs->DstSubresource),
                                            pArgs->pDstBox,
                                            pSysMem,
                                            sys_pitch,
                                            sys_slice_pitch);
        if constexpr (std::is_void_v<Ret>) {
          if (FAILED(hr)) {
            SetError(ctx->device, hr);
          }
          return;
        } else {
          return hr;
        }
      } else {
        (void)tup;
        if constexpr (!std::is_void_v<Ret>) {
          return E_NOTIMPL;
        }
      }
    } else if constexpr (sizeof...(Args) == 3) {
      using ArgTuple = std::tuple<Args...>;
      using Arg1 = std::tuple_element_t<1, ArgTuple>;
      if constexpr (std::is_pointer_v<std::decay_t<Arg1>> &&
                    std::is_same_v<std::remove_cv_t<std::remove_pointer_t<std::decay_t<Arg1>>>, D3D11DDIARG_UPDATESUBRESOURCEUP>) {
        const auto hCtx = std::get<0>(tup);
        const auto* pArgs = std::get<1>(tup);
        const void* pSysMem = std::get<2>(tup);

        uint32_t sys_pitch = 0;
        if constexpr (HasMember_SrcPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->SrcPitch) : 0;
        } else if constexpr (HasMember_RowPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->RowPitch) : 0;
        } else if constexpr (HasMember_SysMemPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_pitch = pArgs ? to_u32(pArgs->SysMemPitch) : 0;
        }

        uint32_t sys_slice_pitch = 0;
        if constexpr (HasMember_SrcSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->SrcSlicePitch) : 0;
        } else if constexpr (HasMember_DepthPitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->DepthPitch) : 0;
        } else if constexpr (HasMember_SysMemSlicePitch<D3D11DDIARG_UPDATESUBRESOURCEUP>::value) {
          sys_slice_pitch = pArgs ? to_u32(pArgs->SysMemSlicePitch) : 0;
        }

        if (!pArgs) {
          if constexpr (!std::is_void_v<Ret>) {
            return E_INVALIDARG;
          } else {
            return;
          }
        }

        auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
        auto* res = FromHandle<decltype(pArgs->hDstResource), AeroGpuResource>(pArgs->hDstResource);
        if (!ctx || !ctx->device || !res || !pSysMem) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
          if constexpr (!std::is_void_v<Ret>) {
            return E_INVALIDARG;
          } else {
            return;
          }
        }

        std::lock_guard<std::mutex> lock(ctx->mutex);
        HRESULT hr = UpdateSubresourceUPImpl(ctx,
                                            res,
                                            to_u32(pArgs->DstSubresource),
                                            pArgs->pDstBox,
                                            pSysMem,
                                            sys_pitch,
                                            sys_slice_pitch);
        if constexpr (std::is_void_v<Ret>) {
          if (FAILED(hr)) {
            SetError(ctx->device, hr);
          }
          return;
        } else {
          return hr;
        }
      } else {
        (void)tup;
        if constexpr (!std::is_void_v<Ret>) {
          return E_NOTIMPL;
        }
      }
    } else {
      (void)tup;
      // If the signature does not match what we expect, fail cleanly.
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

template <typename FnPtr>
struct CopyResourceThunk;

template <typename Ret, typename... Args>
struct CopyResourceThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 3) {
      const auto hCtx = std::get<0>(tup);
      const auto hDst = std::get<1>(tup);
      const auto hSrc = std::get<2>(tup);

      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* dst = FromHandle<decltype(hDst), AeroGpuResource>(hDst);
      auto* src = FromHandle<decltype(hSrc), AeroGpuResource>(hSrc);
      if (!ctx || !ctx->device || !dst || !src) {
        if (ctx && ctx->device) {
          SetError(ctx->device, E_INVALIDARG);
        }
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      HRESULT hr = CopyResourceImpl(ctx, dst, src);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
      } else {
        return hr;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      } else {
        return;
      }
    }
  }
};

template <typename FnPtr>
struct CopySubresourceRegionThunk;

template <typename Ret, typename... Args>
struct CopySubresourceRegionThunk<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Impl(Args... args) {
    auto tup = std::forward_as_tuple(args...);
    if constexpr (sizeof...(Args) == 9) {
      const auto hCtx = std::get<0>(tup);
      const auto hDst = std::get<1>(tup);
      const uint32_t dst_subresource = to_u32(std::get<2>(tup));
      const uint32_t dst_x = to_u32(std::get<3>(tup));
      const uint32_t dst_y = to_u32(std::get<4>(tup));
      const uint32_t dst_z = to_u32(std::get<5>(tup));
      const auto hSrc = std::get<6>(tup);
      const uint32_t src_subresource = to_u32(std::get<7>(tup));
      const void* pSrcBox = std::get<8>(tup);

      if (!hCtx.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
        if constexpr (std::is_void_v<Ret>) {
          if (hCtx.pDrvPrivate) {
            auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
            if (ctx && ctx->device) {
              SetError(ctx->device, E_INVALIDARG);
            }
          }
        }
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      auto* ctx = FromHandle<decltype(hCtx), AeroGpuImmediateContext>(hCtx);
      auto* dst = FromHandle<decltype(hDst), AeroGpuResource>(hDst);
      auto* src = FromHandle<decltype(hSrc), AeroGpuResource>(hSrc);
      if (!ctx || !dst || !src) {
        if constexpr (std::is_void_v<Ret>) {
          if (ctx && ctx->device) {
            SetError(ctx->device, E_INVALIDARG);
          }
        }
        if constexpr (!std::is_void_v<Ret>) {
          return E_INVALIDARG;
        } else {
          return;
        }
      }

      std::lock_guard<std::mutex> lock(ctx->mutex);
      HRESULT hr = CopySubresourceRegionImpl(ctx, dst, dst_subresource, dst_x, dst_y, dst_z, src, src_subresource, pSrcBox);
      if constexpr (std::is_void_v<Ret>) {
        if (FAILED(hr)) {
          SetError(ctx->device, hr);
        }
      } else {
        return hr;
      }
    } else {
      if constexpr (!std::is_void_v<Ret>) {
        return E_NOTIMPL;
      }
    }
  }
};

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
  if (!cmd) {
    SetError(ctx->device, E_OUTOFMEMORY);
    HRESULT submit_hr = S_OK;
    submit_locked(ctx, true, &submit_hr);
    return FAILED(submit_hr) ? submit_hr : E_OUTOFMEMORY;
  }
  cmd->scanout_id = 0;
  bool vsync = (pPresent->SyncInterval != 0);
  if (vsync && ctx->device && ctx->device->adapter && ctx->device->adapter->umd_private_valid) {
    vsync = (ctx->device->adapter->umd_private.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0;
  }
  cmd->flags = vsync ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

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

  std::vector<AeroGpuResource*> resources;
  resources.reserve(numResources);
  for (UINT i = 0; i < numResources; ++i) {
    auto* res = pResources[i].pDrvPrivate ? FromHandle<D3D11DDI_HRESOURCE, AeroGpuResource>(pResources[i]) : nullptr;
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
  for (UINT i = 1; i < numResources; ++i) {
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
    uint64_t share_token = 0;
    bool is_shared = false;
    bool is_shared_alias = false;
    AeroGpuResource::WddmIdentity wddm;
    uint64_t wddm_allocation = 0;
    void* mapped_wddm_ptr = nullptr;
    uint32_t mapped_wddm_pitch = 0;
    uint32_t mapped_wddm_slice_pitch = 0;
    std::vector<uint8_t> storage;
    bool mapped = false;
    bool mapped_write = false;
    uint32_t mapped_subresource = 0;
    uint32_t mapped_map_type = 0;
    uint64_t mapped_offset_bytes = 0;
    uint64_t mapped_size_bytes = 0;
    D3DKMT_HANDLE hkm_resource = 0;
    D3DKMT_HANDLE hkm_allocation = 0;
  };

  auto take_identity = [](AeroGpuResource* res) -> ResourceIdentity {
    ResourceIdentity id{};
    if (!res) {
      return id;
    }
    id.handle = res->handle;
    id.backing_alloc_id = res->backing_alloc_id;
    id.share_token = res->share_token;
    id.is_shared = res->is_shared;
    id.is_shared_alias = res->is_shared_alias;
    id.wddm = std::move(res->wddm);
    id.wddm_allocation = res->wddm_allocation;
    id.mapped_wddm_ptr = res->mapped_wddm_ptr;
    id.mapped_wddm_pitch = res->mapped_wddm_pitch;
    id.mapped_wddm_slice_pitch = res->mapped_wddm_slice_pitch;
    id.storage = std::move(res->storage);
    id.mapped = res->mapped;
    id.mapped_write = res->mapped_write;
    id.mapped_subresource = res->mapped_subresource;
    id.mapped_map_type = res->mapped_map_type;
    id.mapped_offset_bytes = res->mapped_offset_bytes;
    id.mapped_size_bytes = res->mapped_size_bytes;
    id.hkm_resource = res->hkm_resource;
    id.hkm_allocation = res->hkm_allocation;
    return id;
  };

  auto put_identity = [](AeroGpuResource* res, ResourceIdentity&& id) {
    if (!res) {
      return;
    }
    res->handle = id.handle;
    res->backing_alloc_id = id.backing_alloc_id;
    res->share_token = id.share_token;
    res->is_shared = id.is_shared;
    res->is_shared_alias = id.is_shared_alias;
    res->wddm = std::move(id.wddm);
    res->wddm_allocation = id.wddm_allocation;
    res->mapped_wddm_ptr = id.mapped_wddm_ptr;
    res->mapped_wddm_pitch = id.mapped_wddm_pitch;
    res->mapped_wddm_slice_pitch = id.mapped_wddm_slice_pitch;
    res->storage = std::move(id.storage);
    res->mapped = id.mapped;
    res->mapped_write = id.mapped_write;
    res->mapped_subresource = id.mapped_subresource;
    res->mapped_map_type = id.mapped_map_type;
    res->mapped_offset_bytes = id.mapped_offset_bytes;
    res->mapped_size_bytes = id.mapped_size_bytes;
    res->hkm_resource = id.hkm_resource;
    res->hkm_allocation = id.hkm_allocation;
  };

  ResourceIdentity saved = take_identity(resources[0]);
  for (UINT i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  bool needs_rebind = false;
  for (AeroGpuResource* r : resources) {
    if (ctx->current_dsv_resource == r) {
      needs_rebind = true;
      break;
    }
    for (uint32_t i = 0; i < ctx->current_rtv_count; ++i) {
      if (ctx->current_rtv_resources[i] == r) {
        needs_rebind = true;
        break;
      }
    }
    if (needs_rebind) {
      break;
    }
  }

  if (needs_rebind) {
    aerogpu_handle_t new_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
    for (uint32_t i = 0; i < ctx->current_rtv_count; ++i) {
      new_rtvs[i] = ctx->current_rtv_resources[i] ? ctx->current_rtv_resources[i]->handle : 0;
    }
    const aerogpu_handle_t new_dsv = ctx->current_dsv_resource ? ctx->current_dsv_resource->handle : 0;

    auto* cmd = ctx->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!cmd) {
      // Undo the rotation (rotate right by one).
      ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
      for (UINT i = numResources - 1; i > 0; --i) {
        put_identity(resources[i], take_identity(resources[i - 1]));
      }
      put_identity(resources[0], std::move(undo_saved));
      SetError(ctx->device, E_OUTOFMEMORY);
      return;
    }

    // Update cached handles after the command buffer append succeeds so the
    // cached state stays consistent even if the append fails (e.g. small DMA
    // buffer).
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      ctx->current_rtvs[i] = new_rtvs[i];
    }
    ctx->current_dsv = new_dsv;
    cmd->color_count = ctx->current_rtv_count;
    cmd->depth_stencil = ctx->current_dsv;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
      cmd->colors[i] = ctx->current_rtvs[i];
    }
  }
}

HRESULT AEROGPU_APIENTRY Present11Device(D3D11DDI_HDEVICE hDevice, const D3D10DDIARG_PRESENT* pPresent) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->immediate) {
    return E_FAIL;
  }
  D3D11DDI_HDEVICECONTEXT hCtx = {};
  hCtx.pDrvPrivate = dev->immediate;
  return Present11(hCtx, pPresent);
}

void AEROGPU_APIENTRY RotateResourceIdentities11Device(D3D11DDI_HDEVICE hDevice,
                                                      D3D11DDI_HRESOURCE* pResources,
                                                      UINT numResources) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D11DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->immediate) {
    return;
  }
  D3D11DDI_HDEVICECONTEXT hCtx = {};
  hCtx.pDrvPrivate = dev->immediate;
  RotateResourceIdentities11(hCtx, pResources, numResources);
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI (D3D11DDI_ADAPTERFUNCS)
// -------------------------------------------------------------------------------------------------

HRESULT AEROGPU_APIENTRY GetCaps11(D3D10DDI_HADAPTER, const D3D11DDIARG_GETCAPS* pGetCaps) {
  AEROGPU_D3D10_11_LOG_CALL();
  if (!pGetCaps) {
    return E_INVALIDARG;
  }
  if (!pGetCaps->pData || pGetCaps->DataSize == 0) {
    // Be conservative and avoid failing the runtime during bring-up: treat
    // missing/empty output buffers as a no-op query.
    return S_OK;
  }

  const uint32_t type = static_cast<uint32_t>(pGetCaps->Type);
  void* data = pGetCaps->pData;
  const uint32_t data_size = static_cast<uint32_t>(pGetCaps->DataSize);
  CAPS_LOG("aerogpu-d3d10_11: GetCaps11 type=%u size=%u\n", (unsigned)type, (unsigned)data_size);

  auto log_unknown_type_once = [&](uint32_t unknown_type) {
    if (!aerogpu_d3d10_11_log_enabled()) {
      return;
    }

    // Track a small, common range of D3D11DDICAPS_TYPE values without any heap
    // allocations (UMD-friendly).
    static std::atomic<uint64_t> logged[4] = {}; // 256 bits.
    if (unknown_type < 256) {
      const uint32_t idx = unknown_type / 64;
      const uint64_t bit = 1ull << (unknown_type % 64);
      const uint64_t prev = logged[idx].fetch_or(bit, std::memory_order_relaxed);
      if ((prev & bit) != 0) {
        return;
      }
    }
    AEROGPU_D3D10_11_LOG("GetCaps11 unknown type=%u (size=%u) -> zero-fill + S_OK",
                         (unsigned)unknown_type,
                         (unsigned)data_size);
  };

  // Mirror the non-WDK bring-up behavior: unknown cap types are treated as
  // "supported but with everything disabled" to avoid runtime crashes.
  //
  // NOTE: We avoid blanket zero-fill for in/out cap structs (e.g. format support)
  // until after we've read the input fields.
  switch (type) {
    // D3D11_FEATURE_* values (Win7 D3D11 runtime routes CheckFeatureSupport via these).
    case 0: // THREADING
    case 1: // DOUBLES
    case 4: // D3D10_X_HARDWARE_OPTIONS
    case 5: // D3D11_OPTIONS
    case 6: // ARCHITECTURE_INFO
    case 7: // D3D9_OPTIONS
      std::memset(data, 0, data_size);
      return S_OK;

    case 8: { // FEATURE_LEVELS
      std::memset(data, 0, data_size);
      static const D3D_FEATURE_LEVEL kLevels[] = {D3D_FEATURE_LEVEL_10_0};

      // Win7 D3D11 runtime generally expects "count + inline list", but some
      // header/runtime combinations treat this as a {count, pointer} struct.
      // Populate both layouts when possible to avoid mismatched interpretation.
      struct FeatureLevelsCapsPtr {
        UINT NumFeatureLevels;
        const D3D_FEATURE_LEVEL* pFeatureLevels;
      };

      constexpr size_t kInlineLevelsOffset = sizeof(UINT);
      constexpr size_t kPtrOffset = offsetof(FeatureLevelsCapsPtr, pFeatureLevels);

      // On 32-bit builds the pointer field overlaps the first inline element
      // (both start at offset 4), so we cannot populate both layouts. Prefer
      // the {count, pointer} layout to avoid returning a bogus pointer value
      // (e.g. 0xA000) that could crash the runtime if it expects the pointer
      // interpretation.
      if (kPtrOffset == kInlineLevelsOffset && data_size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      if (data_size >= sizeof(UINT) + sizeof(D3D_FEATURE_LEVEL)) {
        auto* out_count = reinterpret_cast<UINT*>(data);
        *out_count = 1;
        auto* out_levels = reinterpret_cast<D3D_FEATURE_LEVEL*>(out_count + 1);
        out_levels[0] = kLevels[0];
        if (data_size >= sizeof(FeatureLevelsCapsPtr) && kPtrOffset >= kInlineLevelsOffset + sizeof(D3D_FEATURE_LEVEL)) {
          auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
          out_ptr->pFeatureLevels = kLevels;
        }
        return S_OK;
      }

      if (data_size >= sizeof(FeatureLevelsCapsPtr)) {
        auto* out_ptr = reinterpret_cast<FeatureLevelsCapsPtr*>(data);
        out_ptr->NumFeatureLevels = 1;
        out_ptr->pFeatureLevels = kLevels;
        return S_OK;
      }

      if (data_size >= sizeof(D3D_FEATURE_LEVEL)) {
        *reinterpret_cast<D3D_FEATURE_LEVEL*>(data) = kLevels[0];
        return S_OK;
      }

      return E_INVALIDARG;
    }

    case 2: { // FORMAT_SUPPORT
      struct FormatSupport {
        UINT InFormat;        // DXGI_FORMAT
        UINT OutFormatSupport; // D3D11_FORMAT_SUPPORT
      };
      if (data_size < sizeof(FormatSupport)) {
        return E_INVALIDARG;
      }
      const UINT format = reinterpret_cast<FormatSupport*>(data)->InFormat;
      std::memset(data, 0, data_size);
      auto* fs = reinterpret_cast<FormatSupport*>(data);
      fs->InFormat = format;
      fs->OutFormatSupport = d3d11_format_support_flags(static_cast<uint32_t>(format));
      return S_OK;
    }

    case 3: { // FORMAT_SUPPORT2
      struct FormatSupport2 {
        UINT InFormat;
        UINT OutFormatSupport2;
      };
      if (data_size < sizeof(FormatSupport2)) {
        return E_INVALIDARG;
      }
      const UINT format = reinterpret_cast<FormatSupport2*>(data)->InFormat;
      std::memset(data, 0, data_size);
      auto* fs = reinterpret_cast<FormatSupport2*>(data);
      fs->InFormat = format;
      fs->OutFormatSupport2 = 0;
      return S_OK;
    }

    case 9: { // MULTISAMPLE_QUALITY_LEVELS
      struct MsaaQualityLevels {
        UINT Format;
        UINT SampleCount;
        UINT Flags;
        UINT NumQualityLevels;
      };
      if (data_size < sizeof(MsaaQualityLevels)) {
        return E_INVALIDARG;
      }
      auto* in = reinterpret_cast<MsaaQualityLevels*>(data);
      const UINT format = in->Format;
      const UINT sample_count = in->SampleCount;
      std::memset(data, 0, data_size);
      auto* ms = reinterpret_cast<MsaaQualityLevels*>(data);
      ms->Format = format;
      ms->SampleCount = sample_count;
      ms->Flags = 0;
      const uint32_t support = d3d11_format_support_flags(static_cast<uint32_t>(format));
      const bool supported_format = (support & kD3D11FormatSupportTexture2D) != 0 &&
                                    (support & (kD3D11FormatSupportRenderTarget | kD3D11FormatSupportDepthStencil)) != 0;
      ms->NumQualityLevels = (sample_count == 1 && supported_format) ? 1u : 0u;
      return S_OK;
    }

    case 10: { // SHADER
      // Shader model caps for FL10_0: VS/GS/PS are SM4.0; HS/DS/CS are unsupported.
      //
      // Layout begins with a sequence of UINT "version tokens" matching the D3D
      // shader bytecode token format:
      //   (program_type << 16) | (major << 4) | minor
      //
      // Be careful about overrunning DataSize: only write fields that fit.
      std::memset(data, 0, data_size);

      constexpr auto ver_token = [](uint32_t program_type, uint32_t major, uint32_t minor) -> uint32_t {
        return (program_type << 16) | (major << 4) | minor;
      };

      constexpr uint32_t kShaderTypePixel = 0;
      constexpr uint32_t kShaderTypeVertex = 1;
      constexpr uint32_t kShaderTypeGeometry = 2;

      auto write_u32 = [&](size_t offset, uint32_t value) {
        if (data_size < offset + sizeof(uint32_t)) {
          return;
        }
        *reinterpret_cast<uint32_t*>(reinterpret_cast<uint8_t*>(data) + offset) = value;
      };

      write_u32(0, ver_token(kShaderTypePixel, 4, 0));
      write_u32(sizeof(uint32_t), ver_token(kShaderTypeVertex, 4, 0));
      write_u32(sizeof(uint32_t) * 2, ver_token(kShaderTypeGeometry, 4, 0));
      return S_OK;
    }

    default:
      log_unknown_type_once(type);
      std::memset(data, 0, data_size);
      return S_OK;
  }
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize11(D3D10DDI_HADAPTER, const D3D11DDIARG_CREATEDEVICE*) {
  __if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
    return sizeof(AeroGpuDevice);
  }
  // Device allocation includes the immediate context object.
  return sizeof(AeroGpuDevice) + sizeof(AeroGpuImmediateContext);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceContextSize11(D3D10DDI_HADAPTER, const D3D11DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuImmediateContext);
}

HRESULT AEROGPU_APIENTRY CreateDevice11(D3D10DDI_HADAPTER hAdapter, D3D11DDIARG_CREATEDEVICE* pCreate) {
  if (!pCreate || !pCreate->hDevice.pDrvPrivate || !pCreate->pDeviceFuncs || !pCreate->pDeviceContextFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }
  if (adapter->d3d11_ddi_interface_version != kAeroGpuWin7D3D11DdiSupportedVersion) {
    return E_NOINTERFACE;
  }

  auto* dev = new (pCreate->hDevice.pDrvPrivate) AeroGpuDevice();
  dev->adapter = adapter;
  dev->kmt_adapter = adapter->kmt_adapter;
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

  void* ctx_mem = pCreate->hImmediateContext.pDrvPrivate;
  if (!ctx_mem) {
    __if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
      return E_INVALIDARG;
    }
    ctx_mem = reinterpret_cast<uint8_t*>(pCreate->hDevice.pDrvPrivate) + sizeof(AeroGpuDevice);
    pCreate->hImmediateContext.pDrvPrivate = ctx_mem;
  }

  auto* ctx = new (ctx_mem) AeroGpuImmediateContext();
  ctx->device = dev;
  dev->immediate = ctx;

  // The Win7 runtime may call a much larger portion of the DDI surface during
  // device creation / initialization than a simple triangle sample would
  // suggest. Ensure we never leave NULL function pointers in the tables by
  // starting from fully-stubbed defaults and overriding implemented entrypoints.
  D3D11DDI_DEVICEFUNCS device_funcs = kStubDeviceFuncs;
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

  __if_exists(D3D11DDI_DEVICEFUNCS::pfnGetDeviceRemovedReason) { device_funcs.pfnGetDeviceRemovedReason = &GetDeviceRemovedReason11; }

  BindPresentAndRotate(&device_funcs);
  if (!ValidateNoNullDdiTable("D3D11DDI_DEVICEFUNCS", &device_funcs, sizeof(device_funcs))) {
    pCreate->hImmediateContext.pDrvPrivate = nullptr;
    DestroyDevice11(pCreate->hDevice);
    return E_NOINTERFACE;
  }
  *pCreate->pDeviceFuncs = device_funcs;

  D3D11DDI_DEVICECONTEXTFUNCS ctx_funcs = kStubCtxFuncs;
  ctx_funcs.pfnIaSetInputLayout = &IaSetInputLayout11;
  ctx_funcs.pfnIaSetVertexBuffers = &IaSetVertexBuffers11;
  ctx_funcs.pfnIaSetIndexBuffer = &IaSetIndexBuffer11;
  ctx_funcs.pfnIaSetTopology = &IaSetTopology11;
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSoSetTargets) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnSoSetTargets);
    ctx_funcs.pfnSoSetTargets = &SoSetTargetsThunk<Fn>::Impl;
  }

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

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShader) { ctx_funcs.pfnHsSetShader = &HsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetConstantBuffers) {
    ctx_funcs.pfnHsSetConstantBuffers = &HsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetShaderResources) {
    ctx_funcs.pfnHsSetShaderResources = &HsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnHsSetSamplers) { ctx_funcs.pfnHsSetSamplers = &HsSetSamplers11; }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShader) { ctx_funcs.pfnDsSetShader = &DsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetConstantBuffers) {
    ctx_funcs.pfnDsSetConstantBuffers = &DsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetShaderResources) {
    ctx_funcs.pfnDsSetShaderResources = &DsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDsSetSamplers) { ctx_funcs.pfnDsSetSamplers = &DsSetSamplers11; }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShader) { ctx_funcs.pfnCsSetShader = &CsSetShader11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetConstantBuffers) {
    ctx_funcs.pfnCsSetConstantBuffers = &CsSetConstantBuffers11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetShaderResources) {
    ctx_funcs.pfnCsSetShaderResources = &CsSetShaderResources11;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetSamplers) { ctx_funcs.pfnCsSetSamplers = &CsSetSamplers11; }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCsSetUnorderedAccessViews) {
    ctx_funcs.pfnCsSetUnorderedAccessViews = &CsSetUnorderedAccessViews11;
  }

  ctx_funcs.pfnSetViewports = &SetViewports11;
  ctx_funcs.pfnSetScissorRects = &SetScissorRects11;
  ctx_funcs.pfnSetRasterizerState = &SetRasterizerState11;
  ctx_funcs.pfnSetBlendState = &SetBlendState11;
  ctx_funcs.pfnSetDepthStencilState = &SetDepthStencilState11;
  ctx_funcs.pfnSetRenderTargets = &SetRenderTargets11;

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnSetRenderTargetsAndUnorderedAccessViews);
    ctx_funcs.pfnSetRenderTargetsAndUnorderedAccessViews = &SetRenderTargetsAndUnorderedAccessViewsThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetRenderTargetsAndUnorderedAccessViews11_1) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnSetRenderTargetsAndUnorderedAccessViews11_1);
    ctx_funcs.pfnSetRenderTargetsAndUnorderedAccessViews11_1 = &SetRenderTargetsAndUnorderedAccessViewsThunk<Fn>::Impl;
  }

  ctx_funcs.pfnClearRenderTargetView = &ClearRenderTargetView11;
  ctx_funcs.pfnClearDepthStencilView = &ClearDepthStencilView11;
  ctx_funcs.pfnDraw = &Draw11;
  ctx_funcs.pfnDrawIndexed = &DrawIndexed11;

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnSetPredication) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnSetPredication);
    ctx_funcs.pfnSetPredication = &SetPredicationThunk<Fn>::Impl;
  }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnUpdateSubresourceUP) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnUpdateSubresourceUP);
    ctx_funcs.pfnUpdateSubresourceUP = &UpdateSubresourceUPThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnUpdateSubresource) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnUpdateSubresource);
    ctx_funcs.pfnUpdateSubresource = &UpdateSubresourceUPThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCopyResource) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnCopyResource);
    ctx_funcs.pfnCopyResource = &CopyResourceThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnCopySubresourceRegion) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnCopySubresourceRegion);
    ctx_funcs.pfnCopySubresourceRegion = &CopySubresourceRegionThunk<Fn>::Impl;
  }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnStagingResourceMap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnStagingResourceMap);
    ctx_funcs.pfnStagingResourceMap = &StagingResourceMapThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnStagingResourceUnmap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnStagingResourceUnmap);
    ctx_funcs.pfnStagingResourceUnmap = &StagingResourceUnmapThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferMapDiscard) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnDynamicIABufferMapDiscard);
    ctx_funcs.pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscardThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferMapNoOverwrite) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnDynamicIABufferMapNoOverwrite);
    ctx_funcs.pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwriteThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferUnmap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnDynamicIABufferUnmap);
    ctx_funcs.pfnDynamicIABufferUnmap = &DynamicIABufferUnmapThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicConstantBufferMapDiscard) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnDynamicConstantBufferMapDiscard);
    ctx_funcs.pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscardThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicConstantBufferUnmap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnDynamicConstantBufferUnmap);
    ctx_funcs.pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmapThunk<Fn>::Impl;
  }

  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnMap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnMap);
    ctx_funcs.pfnMap = &MapThunk<Fn>::Impl;
  }
  __if_exists(D3D11DDI_DEVICECONTEXTFUNCS::pfnUnmap) {
    using Fn = decltype(std::declval<D3D11DDI_DEVICECONTEXTFUNCS>().pfnUnmap);
    ctx_funcs.pfnUnmap = &UnmapThunk<Fn>::Impl;
  }

  ctx_funcs.pfnFlush = &Flush11;

  BindPresentAndRotate(&ctx_funcs);
  if (!ValidateNoNullDdiTable("D3D11DDI_DEVICECONTEXTFUNCS", &ctx_funcs, sizeof(ctx_funcs))) {
    pCreate->hImmediateContext.pDrvPrivate = nullptr;
    DestroyDevice11(pCreate->hDevice);
    return E_NOINTERFACE;
  }
  *pCreate->pDeviceContextFuncs = ctx_funcs;
  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter11(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  DestroyKmtAdapterHandle(adapter);
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
  bool interface_ok = (pOpenData->Interface == D3D11DDI_INTERFACE_VERSION);
#ifdef D3D11DDI_INTERFACE
  interface_ok = interface_ok || (pOpenData->Interface == D3D11DDI_INTERFACE);
#endif
  if (!interface_ok) {
    return E_INVALIDARG;
  }

  if (pOpenData->Version == 0) {
    pOpenData->Version = kAeroGpuWin7D3D11DdiSupportedVersion;
  } else if (pOpenData->Version < kAeroGpuWin7D3D11DdiSupportedVersion) {
    return E_NOINTERFACE;
  } else if (pOpenData->Version > kAeroGpuWin7D3D11DdiSupportedVersion) {
    pOpenData->Version = kAeroGpuWin7D3D11DdiSupportedVersion;
  }

  auto* adapter = new (std::nothrow) AeroGpuAdapter();
  if (!adapter) {
    return E_OUTOFMEMORY;
  }
  adapter->d3d11_ddi_interface_version = pOpenData->Version;
  InitKmtAdapterHandle(adapter);
  InitUmdPrivate(adapter);
  pOpenData->hAdapter.pDrvPrivate = adapter;
  __if_exists(D3D10DDIARG_OPENADAPTER::hRTAdapter) {
    adapter->hrt_adapter = pOpenData->hRTAdapter;
  }
  __if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
    adapter->adapter_callbacks = pOpenData->pAdapterCallbacks;
  }

  auto* funcs = reinterpret_cast<D3D11DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  D3D11DDI_ADAPTERFUNCS stub = {};
  stub.pfnGetCaps = &AeroGpuDdiStub<decltype(stub.pfnGetCaps)>::Func;
  stub.pfnCalcPrivateDeviceSize = &AeroGpuDdiStub<decltype(stub.pfnCalcPrivateDeviceSize)>::Func;
  __if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
    stub.pfnCalcPrivateDeviceContextSize = &AeroGpuDdiStub<decltype(stub.pfnCalcPrivateDeviceContextSize)>::Func;
  }
  stub.pfnCreateDevice = &AeroGpuDdiStub<decltype(stub.pfnCreateDevice)>::Func;
  stub.pfnCloseAdapter = &AeroGpuDdiStub<decltype(stub.pfnCloseAdapter)>::Func;
  *funcs = stub;
  funcs->pfnGetCaps = &GetCaps11;
  funcs->pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize11;
  __if_exists(D3D11DDI_ADAPTERFUNCS::pfnCalcPrivateDeviceContextSize) {
    funcs->pfnCalcPrivateDeviceContextSize = &CalcPrivateDeviceContextSize11;
  }
  funcs->pfnCreateDevice = &CreateDevice11;
  funcs->pfnCloseAdapter = &CloseAdapter11;
  if (!ValidateNoNullDdiTable("D3D11DDI_ADAPTERFUNCS", funcs, sizeof(*funcs))) {
    pOpenData->hAdapter.pDrvPrivate = nullptr;
    DestroyKmtAdapterHandle(adapter);
    delete adapter;
    return E_NOINTERFACE;
  }
  return S_OK;
}

} // namespace

// NOTE: The real WDK-backed D3D11 `OpenAdapter11` export lives in
// `aerogpu_d3d11_umd_wdk.cpp`. This TU retains the portable (non-WDK) fallback
// implementation under the `#else` below.

#else

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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
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
  D3DDDI_DEVICECALLBACKS callbacks = {};

  // Mark that the next submission is triggered by Present; used to route the
  // final chunk through the Present callback so the KMD hits DxgkDdiPresent.
  bool next_submit_is_present = false;
#endif

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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
static HRESULT UploadInitialDataToWddmAllocation(AeroGpuDevice* dev,
                                                 uint64_t allocation,
                                                 const void* src,
                                                 size_t bytes) {
  if (!dev || !dev->callbacks || !dev->callbacks->pfnLockCb || !dev->callbacks->pfnUnlockCb || !src || !bytes ||
      !allocation) {
    return E_INVALIDARG;
  }

  D3DDDICB_LOCK lock = {};
  lock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
  HRESULT hr = dev->callbacks->pfnLockCb(dev->hrt_device, &lock);
  if (FAILED(hr)) {
    return hr;
  }
  if (!lock.pData) {
    D3DDDICB_UNLOCK unlock = {};
    unlock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
    dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
    return E_FAIL;
  }

  std::memcpy(lock.pData, src, bytes);

  D3DDDICB_UNLOCK unlock = {};
  unlock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
  hr = dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
  return hr;
}

static HRESULT UploadInitialDataTex2DToWddmAllocation(AeroGpuDevice* dev,
                                                      uint64_t allocation,
                                                      const void* src,
                                                      uint32_t width,
                                                      uint32_t height,
                                                      uint32_t bytes_per_pixel,
                                                      uint32_t src_pitch,
                                                      uint32_t dst_pitch) {
  if (!dev || !dev->callbacks || !dev->callbacks->pfnLockCb || !dev->callbacks->pfnUnlockCb || !src || !width ||
      !height || !bytes_per_pixel || !allocation) {
    return E_INVALIDARG;
  }

  const uint64_t bytes_per_row = static_cast<uint64_t>(width) * static_cast<uint64_t>(bytes_per_pixel);
  if (bytes_per_row > UINT32_MAX) {
    return E_INVALIDARG;
  }
  if (src_pitch && src_pitch < bytes_per_row) {
    return E_INVALIDARG;
  }
  if (dst_pitch < bytes_per_row) {
    return E_INVALIDARG;
  }

  D3DDDICB_LOCK lock = {};
  lock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
  HRESULT hr = dev->callbacks->pfnLockCb(dev->hrt_device, &lock);
  if (FAILED(hr)) {
    return hr;
  }
  if (!lock.pData) {
    D3DDDICB_UNLOCK unlock = {};
    unlock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
    dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
    return E_FAIL;
  }

  const uint8_t* src_bytes = static_cast<const uint8_t*>(src);
  uint8_t* dst_bytes = static_cast<uint8_t*>(lock.pData);
  const uint32_t effective_src_pitch = src_pitch ? src_pitch : static_cast<uint32_t>(bytes_per_row);
  for (uint32_t y = 0; y < height; y++) {
    std::memcpy(dst_bytes + static_cast<size_t>(y) * dst_pitch,
                src_bytes + static_cast<size_t>(y) * effective_src_pitch,
                static_cast<size_t>(bytes_per_row));
  }

  D3DDDICB_UNLOCK unlock = {};
  unlock.hAllocation = static_cast<D3DKMT_HANDLE>(allocation);
  hr = dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
  return hr;
}
#endif

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

void track_current_state_allocs_for_submit_locked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  track_resource_alloc_for_submit_locked(dev, dev->current_rtv);
  track_resource_alloc_for_submit_locked(dev, dev->current_dsv);
  track_alloc_for_submit_locked(dev, dev->current_vb_alloc);
  track_alloc_for_submit_locked(dev, dev->current_ib_alloc);
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (!dev->kmt_fence_syncobj) {
    return E_FAIL;
  }

  const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
  if (!procs.pfn_wait_for_syncobj) {
    return E_FAIL;
  }

  const D3DKMT_HANDLE handles[1] = {dev->kmt_fence_syncobj};
  const UINT64 fence_values[1] = {fence};
  const UINT64 timeout = (timeout_ms == kAeroGpuTimeoutMsInfinite) ? ~0ull : static_cast<UINT64>(timeout_ms);

  D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
  args.ObjectCount = 1;
  args.ObjectHandleArray = handles;
  args.FenceValueArray = fence_values;
  args.Timeout = timeout;

  const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
  if (st == STATUS_TIMEOUT) {
    return kDxgiErrorWasStillDrawing;
  }
  if (!NT_SUCCESS(st)) {
    return E_FAIL;
  }

  // Waiting succeeded => the fence is at least complete even if we cannot query a monitored value.
  atomic_max_u64(&dev->last_completed_fence, fence);

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
    return kDxgiErrorWasStillDrawing;
  }

  if (timeout_ms == kAeroGpuTimeoutMsInfinite) {
    adapter->fence_cv.wait(lock, ready);
  } else if (!adapter->fence_cv.wait_for(lock, std::chrono::milliseconds(timeout_ms), ready)) {
    return kDxgiErrorWasStillDrawing;
  }

  atomic_max_u64(&dev->last_completed_fence, adapter->completed_fence);
  return S_OK;
#endif
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
void SetErrorIfPossible(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  if (!dev) {
    return;
  }
  if (dev->callbacks.pfnSetErrorCb) {
    dev->callbacks.pfnSetErrorCb(hDevice, hr);
  }
}

HRESULT DeallocateResourceIfNeeded(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, AeroGpuResource* res) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  if (!dev->callbacks.pfnDeallocateCb) {
    return S_OK;
  }
  if (res->hkm_resource == 0 && res->hkm_allocation == 0) {
    return S_OK;
  }

  D3DDDICB_DEALLOCATE dealloc = {};
  dealloc.hKMResource = res->hkm_resource;
  dealloc.NumAllocations = (res->hkm_allocation != 0) ? 1u : 0u;
  dealloc.HandleList = (res->hkm_allocation != 0) ? &res->hkm_allocation : nullptr;

  const HRESULT hr = dev->callbacks.pfnDeallocateCb(dev->hrt_device, &dealloc);
  if (FAILED(hr)) {
    SetErrorIfPossible(dev, hDevice, hr);
    return hr;
  }

  res->hkm_allocation = 0;
  res->hkm_resource = 0;
  return S_OK;
}
#else
inline void SetErrorIfPossible(AeroGpuDevice*, D3D10DDI_HDEVICE, HRESULT) {}
inline HRESULT DeallocateResourceIfNeeded(AeroGpuDevice*, D3D10DDI_HDEVICE, AeroGpuResource*) {
  return S_OK;
}
#endif

inline void ReportDeviceErrorLocked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  if (dev) {
    dev->last_error = hr;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
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

    // Repository build: treat submissions as synchronous unless the harness
    // integrates a real fence completion path.
    if (fence == 0) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      fence = adapter->next_fence++;
    }

    {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      adapter->next_fence = std::max(adapter->next_fence, fence + 1);
      adapter->completed_fence = fence;
    }
    adapter->fence_cv.notify_all();

    atomic_max_u64(&dev->last_submitted_fence, fence);
    atomic_max_u64(&dev->last_completed_fence, fence);

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
#endif
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
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    for (AeroGpuResource* res : dev->live_resources) {
      if (res) {
        DeallocateResourceIfNeeded(dev, hDevice, res);
      }
    }
    dev->live_resources.clear();
  }
#endif
  dev->~AeroGpuDevice();
}

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
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

  if (!dev->callbacks.pfnAllocateCb) {
    SetErrorIfPossible(dev, hDevice, E_FAIL);
    return E_FAIL;
  }

  auto allocate_one = [&](uint64_t size_bytes, bool cpu_visible, bool is_rt, bool is_ds, AeroGpuResource* res) -> HRESULT {
    if (!pDesc->pAllocationInfo || !res) {
      return E_INVALIDARG;
    }

    auto* alloc_info = pDesc->pAllocationInfo;
    std::memset(alloc_info, 0, sizeof(*alloc_info));
    alloc_info[0].Size = static_cast<SIZE_T>(size_bytes);
    alloc_info[0].Alignment = 0;
    alloc_info[0].Flags.Value = 0;
    alloc_info[0].Flags.CpuVisible = cpu_visible ? 1u : 0u;
    alloc_info[0].SupportedReadSegmentSet = 1;
    alloc_info[0].SupportedWriteSegmentSet = 1;

    D3DDDICB_ALLOCATE alloc = {};
    alloc.hResource = hRTResource;
    alloc.NumAllocations = 1;
    alloc.pAllocationInfo = alloc_info;
    alloc.Flags.Value = 0;
    alloc.Flags.CreateResource = 1;
    alloc.ResourceFlags.Value = 0;
    alloc.ResourceFlags.RenderTarget = is_rt ? 1u : 0u;
    alloc.ResourceFlags.ZBuffer = is_ds ? 1u : 0u;

    const HRESULT hr = dev->callbacks.pfnAllocateCb(dev->hrt_device, &alloc);
    if (FAILED(hr)) {
      SetErrorIfPossible(dev, hDevice, hr);
      return hr;
    }

    res->hkm_resource = alloc.hKMResource;
    res->hkm_allocation = alloc_info[0].hKMAllocation;
    return S_OK;
  };

  if (pDesc->Dimension == 1) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Buffer;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

    const uint64_t alloc_size = AlignUpU64(static_cast<uint64_t>(res->size_bytes), 256);
    const bool cpu_visible = pDesc->CPUAccessFlags != 0;
    const bool is_rt = (pDesc->BindFlags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (pDesc->BindFlags & kD3D11BindDepthStencil) != 0;
    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, res);
    if (FAILED(hr)) {
      res->handle = kInvalidHandle;
      res->~AeroGpuResource();
      return hr;
    }

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes));
      } catch (...) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
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
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
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
    return S_OK;
  }

  if (pDesc->Dimension == 3) {
    if (pDesc->MipLevels != 1 || pDesc->ArraySize != 1 || pDesc->SampleDesc.Count != 1) {
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
    res->mip_levels = pDesc->MipLevels;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = pDesc->Format;

    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    const uint64_t tight_row_bytes = static_cast<uint64_t>(res->width) * static_cast<uint64_t>(bpp);
    const uint64_t row_pitch_bytes = AlignUpU64(tight_row_bytes, 256);
    res->row_pitch_bytes = static_cast<uint32_t>(row_pitch_bytes);
    const uint64_t alloc_size = row_pitch_bytes * static_cast<uint64_t>(res->height);

    const bool cpu_visible = pDesc->CPUAccessFlags != 0;
    const bool is_rt = (pDesc->BindFlags & kD3D11BindRenderTarget) != 0;
    const bool is_ds = (pDesc->BindFlags & kD3D11BindDepthStencil) != 0;
    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, res);
    if (FAILED(hr)) {
      res->handle = kInvalidHandle;
      res->~AeroGpuResource();
      return hr;
    }

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }

      if (alloc_size > static_cast<uint64_t>(SIZE_MAX)) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(alloc_size));
      } catch (...) {
        res->handle = kInvalidHandle;
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }

      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch) : static_cast<size_t>(tight_row_bytes);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * static_cast<size_t>(row_pitch_bytes),
                    src + static_cast<size_t>(y) * src_pitch,
                    static_cast<size_t>(tight_row_bytes));
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
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
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
    return S_OK;
  }

  return E_NOTIMPL;
}
#else
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
      if (is_guest_backed && res->wddm_allocation) {
        const HRESULT hr = UploadInitialDataToWddmAllocation(
            dev, res->wddm_allocation, init.pSysMem, static_cast<size_t>(res->size_bytes));
        if (FAILED(hr)) {
          res->~AeroGpuResource();
          return hr;
        }
        wddm_initial_upload = true;
      }
#endif

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
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    const uint32_t row_bytes = res->width * bpp;
    res->row_pitch_bytes = row_bytes;

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
      const uint32_t level_pitch = (level == 0) ? res->row_pitch_bytes : (level_w * bpp);
      total_bytes += static_cast<uint64_t>(level_pitch) * static_cast<uint64_t>(level_h);
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
      const uint64_t bytes_per_row = static_cast<uint64_t>(res->width) * static_cast<uint64_t>(bpp);
      const uint32_t src_pitch = init.SysMemPitch ? init.SysMemPitch : static_cast<uint32_t>(bytes_per_row);
      if (bytes_per_row > UINT32_MAX || src_pitch < bytes_per_row || res->row_pitch_bytes < bytes_per_row) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
      if (is_guest_backed && res->wddm_allocation) {
        const HRESULT hr = UploadInitialDataTex2DToWddmAllocation(dev,
                                                                  res->wddm_allocation,
                                                                  init.pSysMem,
                                                                  res->width,
                                                                  res->height,
                                                                  bpp,
                                                                  src_pitch,
                                                                  res->row_pitch_bytes);
        if (FAILED(hr)) {
          res->~AeroGpuResource();
          return hr;
        }
        wddm_initial_upload = true;
      }
#endif

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

      for (uint32_t y = 0; y < res->height; y++) {
        uint8_t* dst_row = dst + static_cast<size_t>(y) * res->row_pitch_bytes;
        std::memcpy(dst_row,
                    src + static_cast<size_t>(y) * src_pitch,
                    static_cast<size_t>(bytes_per_row));
        if (static_cast<uint64_t>(res->row_pitch_bytes) > bytes_per_row) {
          std::memset(dst_row + static_cast<size_t>(bytes_per_row),
                      0,
                      static_cast<size_t>(static_cast<uint64_t>(res->row_pitch_bytes) - bytes_per_row));
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
      const uint64_t dirty_size =
          static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
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
#endif

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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  DeallocateResourceIfNeeded(dev, hDevice, res);
#endif

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
        if (AeroGpuQueryCompletedFence(dev) < fence) {
          return kDxgiErrorWasStillDrawing;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;
#endif

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
      pMapped->DepthPitch = res->row_pitch_bytes * res->height;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Win7 WDK build: Map via LockCb/UnlockCb when a runtime-managed allocation exists.
  if (res->wddm_allocation && dev->callbacks && dev->callbacks->pfnLockCb && dev->callbacks->pfnUnlockCb) {
    D3DDDICB_LOCK lock = {};
    lock.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation);
    SetLockSubresource(&lock, subresource);
    SetLockRange(&lock, /*offset=*/0, /*size=*/0);
    SetLockFlagsFromMap(&lock.Flags, map_type, map_flags);
    HRESULT hr = dev->callbacks->pfnLockCb(dev->hrt_device, &lock);
    if (FAILED(hr)) {
      return hr;
    }
    if (!lock.pData) {
      D3DDDICB_UNLOCK unlock = {};
      unlock.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation);
      SetUnlockSubresource(&unlock, subresource);
      dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
      return E_FAIL;
    }

    res->mapped_wddm_ptr = lock.pData;
    res->mapped_wddm_pitch = lock.Pitch;
    res->mapped_wddm_slice_pitch = lock.SlicePitch;

    if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD) {
      // Discard contents are undefined; clear for deterministic tests.
      if (res->kind == ResourceKind::Buffer) {
        if (res->size_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(lock.pData, 0, static_cast<size_t>(res->size_bytes));
        }
      } else if (res->kind == ResourceKind::Texture2D) {
        const uint32_t pitch = lock.Pitch ? lock.Pitch : res->row_pitch_bytes;
        const uint64_t bytes = static_cast<uint64_t>(pitch) * static_cast<uint64_t>(res->height);
        if (bytes <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(lock.pData, 0, static_cast<size_t>(bytes));
        }
      }
    }

    pMapped->pData = lock.pData;
    if (res->kind == ResourceKind::Texture2D) {
      const uint32_t pitch = lock.Pitch ? lock.Pitch : res->row_pitch_bytes;
      pMapped->RowPitch = pitch;
      const uint32_t slice = lock.SlicePitch ? lock.SlicePitch : pitch * res->height;
      pMapped->DepthPitch = slice;
    } else {
      pMapped->RowPitch = 0;
      pMapped->DepthPitch = 0;
    }

    res->mapped_via_allocation = false;
    res->mapped_ptr = nullptr;

    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_map_type = map_type;
    res->mapped_offset_bytes = 0;
    res->mapped_size_bytes = total;
    return S_OK;
  }
#endif

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
    pMapped->DepthPitch = res->row_pitch_bytes * res->height;
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
  if (!res->mapped) {
    return;
  }
  if (subresource != res->mapped_subresource) {
    return;
  }

  const bool is_guest_backed = (res->backing_alloc_id != 0);

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  const bool had_wddm_lock = res->mapped_wddm_ptr != nullptr;
#endif

  if (res->mapped_via_allocation) {
    if (dev->device_callbacks && dev->device_callbacks->pfnUnmapAllocation) {
      const auto* cb = dev->device_callbacks;
      cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
    }
  }

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (had_wddm_lock && res->mapped_write && !is_guest_backed) {
    // Host-owned bring-up path: copy the updated bytes from the locked WDDM
    // allocation back into shadow storage so UPLOAD_RESOURCE uses a tightly
    // packed layout.
    if (res->kind == ResourceKind::Buffer) {
      const uint64_t bytes = res->size_bytes;
      if (SUCCEEDED(ensure_resource_storage(res, bytes)) && bytes <= static_cast<uint64_t>(res->storage.size())) {
        std::memcpy(res->storage.data(), res->mapped_wddm_ptr, static_cast<size_t>(bytes));
      }
    } else if (res->kind == ResourceKind::Texture2D) {
      const uint64_t bytes = resource_total_bytes(res);
      const uint32_t bytes_per_row = res->row_pitch_bytes;
      const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : bytes_per_row;
      if (bytes_per_row != 0 && src_pitch >= bytes_per_row && bytes != 0 &&
          SUCCEEDED(ensure_resource_storage(res, bytes)) && bytes <= static_cast<uint64_t>(res->storage.size())) {
        const uint8_t* src = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
        for (uint32_t y = 0; y < res->height; y++) {
          std::memcpy(res->storage.data() + static_cast<size_t>(y) * bytes_per_row,
                      src + static_cast<size_t>(y) * src_pitch,
                      bytes_per_row);
        }
      }
    }
  }
#endif

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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  if (had_wddm_lock && res->wddm_allocation && dev->callbacks && dev->callbacks->pfnUnlockCb) {
    D3DDDICB_UNLOCK unlock = {};
    unlock.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation);
    SetUnlockSubresource(&unlock, subresource);
    dev->callbacks->pfnUnlockCb(dev->hrt_device, &unlock);
  }
  res->mapped_wddm_ptr = nullptr;
  res->mapped_wddm_pitch = 0;
  res->mapped_wddm_slice_pitch = 0;
#endif

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
    *ppData = static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes;
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
      const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);

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

      const uint32_t row_bytes = (copy_right - copy_left) * bpp;
      if (row_bytes > res->row_pitch_bytes) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(row_bytes);
      if (!row_bytes || static_cast<size_t>(row_bytes) > src_pitch) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const uint8_t* src = static_cast<const uint8_t*>(pSysMem);
      const size_t dst_x_bytes = static_cast<size_t>(copy_left) * static_cast<size_t>(bpp);
      for (uint32_t y = 0; y < (copy_bottom - copy_top); y++) {
        uint8_t* dst_row = dst + (static_cast<size_t>(copy_top) + y) * res->row_pitch_bytes + dst_x_bytes;
        std::memcpy(dst_row, src + y * src_pitch, row_bytes);
      }

      // If this is a full upload, also clear any per-row padding to keep guest
      // memory deterministic for host-side uploads.
      if (!pDstBox && res->row_pitch_bytes > row_bytes) {
        for (uint32_t y = 0; y < res->height; y++) {
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
      const uint32_t bpp = bytes_per_pixel_aerogpu(aerogpu_format);
      const size_t row_bytes = static_cast<size_t>(res->width) * static_cast<size_t>(bpp);
      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : row_bytes;
      if (!row_bytes || row_bytes > src_pitch || row_bytes > res->row_pitch_bytes) {
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
      for (uint32_t y = 0; y < res->height; y++) {
        uint8_t* dst_row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        std::memcpy(dst_row, src + static_cast<size_t>(y) * src_pitch, row_bytes);
        if (res->row_pitch_bytes > row_bytes) {
          std::memset(dst_row + row_bytes, 0, res->row_pitch_bytes - row_bytes);
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
    const uint32_t bpp = bytes_per_pixel_aerogpu(aerogpu_format);
    const size_t row_bytes =
        static_cast<size_t>(pDstBox->right - pDstBox->left) * static_cast<size_t>(bpp);
    const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : row_bytes;
    if (!row_bytes || row_bytes > src_pitch || row_bytes > res->row_pitch_bytes) {
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
    const size_t dst_x_bytes = static_cast<size_t>(pDstBox->left) * static_cast<size_t>(bpp);
    for (uint32_t y = 0; y < (pDstBox->bottom - pDstBox->top); ++y) {
      const size_t dst_offset = (static_cast<size_t>(pDstBox->top) + y) * dst_pitch + dst_x_bytes;
      std::memcpy(res->storage.data() + dst_offset, src + y * src_pitch, row_bytes);
    }

    // The browser executor currently only supports partial UPLOAD_RESOURCE updates for
    // tightly packed textures (row_pitch_bytes == width*4). When the texture has per-row
    // padding, keep the command stream compatible by uploading the entire texture.
    const size_t tight_row_bytes = static_cast<size_t>(res->width) * static_cast<size_t>(bpp);
    size_t upload_offset = static_cast<size_t>(pDstBox->top) * dst_pitch;
    size_t upload_size = static_cast<size_t>(pDstBox->bottom - pDstBox->top) * dst_pitch;
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

  // Repository builds keep a conservative CPU backing store; simulate the copy
  // immediately so a subsequent staging Map(READ) sees the bytes.
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

  track_resource_alloc_for_submit_locked(dev, dst);
  track_resource_alloc_for_submit_locked(dev, src);

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

  HRESULT hr = map_resource_locked(dev,
                                   res,
                                   static_cast<uint32_t>(pMap->Subresource),
                                   static_cast<uint32_t>(pMap->MapType),
                                   static_cast<uint32_t>(pMap->MapFlags),
                                   pMap->pMappedSubresource);
  if (FAILED(hr)) {
    dev->last_error = hr;
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
  if (!res->mapped || res->mapped_subresource != pUnmap->Subresource) {
    dev->last_error = E_INVALIDARG;
    return;
  }
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
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  dev->next_submit_is_present = true;
#endif
 
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
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
    uint64_t wddm_allocation = 0;
#endif
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
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
    id.wddm_allocation = res->wddm_allocation;
#endif
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
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
    res->wddm_allocation = id.wddm_allocation;
#endif
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Field names vary slightly across WDK versions; use MSVC's `__if_exists` to
  // tolerate both.
  __if_exists(D3D10DDIARG_CREATEDEVICE::hRTDevice) {
    device->hrt_device = pCreateDevice->hRTDevice;
  }
  __if_exists(D3D10DDIARG_CREATEDEVICE::pCallbacks) {
    device->callbacks = pCreateDevice->pCallbacks;
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

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  __if_exists(D3D10DDIARG_OPENADAPTER::hRTAdapter) {
    adapter->hrt_adapter = pOpenData->hRTAdapter;
  }
  __if_exists(D3D10DDIARG_OPENADAPTER::pAdapterCallbacks) {
    adapter->adapter_callbacks = pOpenData->pAdapterCallbacks;
  }
#endif

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

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS

#endif // WDK build exclusion guard (this TU is portable-only)
