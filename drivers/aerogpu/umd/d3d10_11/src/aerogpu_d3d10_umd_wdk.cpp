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
#include <utility>
#include <vector>

#include <d3d10.h>
#include <d3d10_1.h>
#include <d3dkmthk.h>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"
#include "../../../protocol/aerogpu_win7_abi.h"

namespace {

constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
}

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
      "byteWidth=%u w=%u h=%u mips=%u array=%u sample=(%u,%u) rflags=0x%llX rflags_size=%u init=%p",
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
      init_ptr);
}
#endif  // AEROGPU_UMD_TRACE_RESOURCES

constexpr aerogpu_handle_t kInvalidHandle = 0;

constexpr uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

constexpr uint32_t AlignUpU32(uint32_t value, uint32_t alignment) {
  return static_cast<uint32_t>((value + alignment - 1) & ~(alignment - 1));
}

static uint64_t AllocateGlobalToken() {
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";
    HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, sizeof(uint64_t), name);
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

static bool AllocateSharedAllocIds(uint32_t* out_alloc_id, uint64_t* out_share_token) {
  if (!out_alloc_id || !out_share_token) {
    return false;
  }

  const uint64_t token = AllocateGlobalToken();
  if (!token) {
    return false;
  }
  const uint32_t alloc_id = static_cast<uint32_t>(token & 0x7FFFFFFFULL);
  if (!alloc_id) {
    return false;
  }
  *out_alloc_id = alloc_id;
  *out_share_token = token;
  return true;
}

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

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

static aerogpu_handle_t allocate_global_handle(AeroGpuAdapter* adapter) {
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";
    HANDLE mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, sizeof(uint64_t), name);
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

  if (!adapter) {
    return 0;
  }
  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == 0) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
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

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

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
  AeroGpuResource* resource = nullptr;
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
  D3D10DDI_HRTDEVICE hrt_device = {};
  D3D10DDI_DEVICECALLBACKS callbacks = {};
  const D3DDDI_DEVICECALLBACKS* um_callbacks = nullptr;
  uint64_t last_submitted_fence = 0;
  // Best-effort WDDM context propagation for WDK/OS callback struct variants
  // that include `hContext` in D3DDDICB_* submission structs.
  D3DKMT_HANDLE hContext = 0;

  std::mutex mutex;
  aerogpu::CmdWriter cmd;

  // Cached state.
  aerogpu_handle_t current_rtv = 0;
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`).
  AeroGpuResource* current_rtv_res = nullptr;
  AeroGpuResource* current_vb_res = nullptr;
  uint32_t current_vb_stride = 0;
  uint32_t current_vb_offset = 0;
  uint32_t viewport_width = 0;
  uint32_t viewport_height = 0;

  AeroGpuDevice() {
    cmd.reset();
  }
};

template <typename THandle, typename TObject>
TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

template <typename Fn, typename Handle, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, Handle handle, Args&&... args) {
  if constexpr (std::is_invocable_v<Fn, Handle, Args...>) {
    return fn(handle, std::forward<Args>(args)...);
  } else {
    return fn(std::forward<Args>(args)...);
  }
}

void SetError(D3D10DDI_HDEVICE hDevice, HRESULT hr) {
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->callbacks.pfnSetErrorCb) {
    return;
  }
  dev->callbacks.pfnSetErrorCb(hDevice, hr);
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
AEROGPU_DEFINE_HAS_MEMBER(CPUAccessFlags)
AEROGPU_DEFINE_HAS_MEMBER(CpuAccessFlags)
AEROGPU_DEFINE_HAS_MEMBER(Usage)

#undef AEROGPU_DEFINE_HAS_MEMBER

uint64_t submit_locked(AeroGpuDevice* dev, bool want_present, HRESULT* out_hr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev || dev->cmd.empty()) {
    return 0;
  }
  if (!dev->adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->cmd.reset();
    return 0;
  }

  dev->cmd.finalize();

  const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
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

  auto deallocate = [&](const D3DDDICB_ALLOCATE& alloc, void* dma_priv_ptr, UINT dma_priv_size) {
    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBuffer) {
      __if_exists(D3DDDICB_ALLOCATE::pDmaBuffer) {
        dealloc.pDmaBuffer = alloc.pDmaBuffer;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pCommandBuffer) {
      __if_exists(D3DDDICB_ALLOCATE::pCommandBuffer) {
        dealloc.pCommandBuffer = alloc.pCommandBuffer;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pAllocationList) {
      __if_exists(D3DDDICB_ALLOCATE::pAllocationList) {
        dealloc.pAllocationList = alloc.pAllocationList;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pPatchLocationList) {
      __if_exists(D3DDDICB_ALLOCATE::pPatchLocationList) {
        dealloc.pPatchLocationList = alloc.pPatchLocationList;
      }
    }
    __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
      dealloc.pDmaBufferPrivateData = dma_priv_ptr;
    }
    __if_exists(D3DDDICB_DEALLOCATE::DmaBufferPrivateDataSize) {
      dealloc.DmaBufferPrivateDataSize = dma_priv_size;
    }
    CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
  };

  uint64_t last_fence = 0;
  auto log_missing_context_once = [&] {
    static std::atomic<bool> logged = false;
    bool expected = false;
    if (logged.compare_exchange_strong(expected, true)) {
      AEROGPU_D3D10_11_LOG(
          "d3d10_wdk_submit: D3DDDICB_* exposes hContext but submissions are using hContext=0; "
          "this may require creating a WDDM context via pfnCreateContextCb2");
    }
  };

  // Chunk at packet boundaries if the runtime returns a smaller-than-requested DMA buffer.
  size_t cur = sizeof(aerogpu_cmd_stream_header);
  while (cur < src_size) {
    const size_t remaining_packets_bytes = src_size - cur;
    const UINT request_bytes =
        static_cast<UINT>(remaining_packets_bytes + sizeof(aerogpu_cmd_stream_header));

    D3DDDICB_ALLOCATE alloc = {};
    __if_exists(D3DDDICB_ALLOCATE::hContext) {
      alloc.hContext = dev->hContext;
    }
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

    HRESULT alloc_hr = CallCbMaybeHandle(cb->pfnAllocateCb, dev->hrt_device, &alloc);
    __if_exists(D3DDDICB_ALLOCATE::hContext) {
      if (alloc.hContext != 0) {
        dev->hContext = alloc.hContext;
      } else if (dev->hContext == 0) {
        log_missing_context_once();
      }
    }

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

    void* dma_priv_ptr = nullptr;
    UINT dma_priv_size = 0;
    __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
      dma_priv_ptr = alloc.pDmaBufferPrivateData;
    }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
      dma_priv_size = alloc.DmaBufferPrivateDataSize;
    }

    if (FAILED(alloc_hr) || !dma_ptr || dma_cap == 0) {
      if (out_hr) {
        *out_hr = FAILED(alloc_hr) ? alloc_hr : E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    bool require_dma_priv = false;
    __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
      require_dma_priv = true;
    }
    if (require_dma_priv) {
      if (!dma_priv_ptr) {
        deallocate(alloc, dma_priv_ptr, dma_priv_size);
        if (out_hr) {
          *out_hr = E_FAIL;
        }
        dev->cmd.reset();
        return 0;
      }

      bool has_dma_priv_size = false;
      __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
        has_dma_priv_size = true;
      }
      if (has_dma_priv_size && dma_priv_size < AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
        deallocate(alloc, dma_priv_ptr, dma_priv_size);
        if (out_hr) {
          *out_hr = E_FAIL;
        }
        dev->cmd.reset();
        return 0;
      }
      if (!has_dma_priv_size) {
        dma_priv_size = AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
      }
    }

    if (dma_cap < sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_hdr)) {
      deallocate(alloc, dma_priv_ptr, dma_priv_size);
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
    bool stream_invalid = false;

    while (chunk_end < src_size) {
      const auto* pkt = reinterpret_cast<const aerogpu_cmd_hdr*>(src + chunk_end);
      const size_t pkt_size = static_cast<size_t>(pkt->size_bytes);
      if (pkt_size < sizeof(aerogpu_cmd_hdr) || (pkt_size & 3u) != 0 || chunk_end + pkt_size > src_size) {
        stream_invalid = true;
        break;
      }
      if (chunk_size + pkt_size > dma_cap) {
        break;
      }
      chunk_end += pkt_size;
      chunk_size += pkt_size;
    }

    if (stream_invalid) {
      deallocate(alloc, dma_priv_ptr, dma_priv_size);
      if (out_hr) {
        *out_hr = E_FAIL;
      }
      dev->cmd.reset();
      return 0;
    }

    if (chunk_end == chunk_begin) {
      deallocate(alloc, dma_priv_ptr, dma_priv_size);
      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    auto* dst = static_cast<uint8_t*>(dma_ptr);
    std::memcpy(dst, src, sizeof(aerogpu_cmd_stream_header));
    std::memcpy(dst + sizeof(aerogpu_cmd_stream_header),
                src + chunk_begin,
                chunk_size - sizeof(aerogpu_cmd_stream_header));
    auto* hdr = reinterpret_cast<aerogpu_cmd_stream_header*>(dst);
    hdr->size_bytes = static_cast<uint32_t>(chunk_size);

    if (require_dma_priv && dma_priv_ptr && dma_priv_size) {
      std::memset(dma_priv_ptr, 0, static_cast<size_t>(dma_priv_size));
    }

    const bool is_last_chunk = (chunk_end == src_size);
    const bool do_present = want_present && is_last_chunk && cb->pfnPresentCb != nullptr;

    HRESULT submit_hr = S_OK;
    uint64_t submission_fence = 0;
    if (do_present) {
      D3DDDICB_PRESENT present = {};
      __if_exists(D3DDDICB_PRESENT::hContext) {
        present.hContext = dev->hContext;
        if (present.hContext == 0) {
          log_missing_context_once();
        }
      }
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
      __if_exists(D3DDDICB_PRESENT::pDmaBufferPrivateData) {
        present.pDmaBufferPrivateData = dma_priv_ptr;
      }
      __if_exists(D3DDDICB_PRESENT::DmaBufferPrivateDataSize) {
        present.DmaBufferPrivateDataSize = dma_priv_size;
      }

      submit_hr = CallCbMaybeHandle(cb->pfnPresentCb, dev->hrt_device, &present);
      __if_exists(D3DDDICB_PRESENT::NewFenceValue) {
        submission_fence = static_cast<uint64_t>(present.NewFenceValue);
      }
      __if_not_exists(D3DDDICB_PRESENT::NewFenceValue) {
        __if_exists(D3DDDICB_PRESENT::SubmissionFenceId) {
          submission_fence = static_cast<uint64_t>(present.SubmissionFenceId);
        }
      }
    } else {
      D3DDDICB_RENDER render = {};
      __if_exists(D3DDDICB_RENDER::hContext) {
        render.hContext = dev->hContext;
        if (render.hContext == 0) {
          log_missing_context_once();
        }
      }
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
      __if_exists(D3DDDICB_RENDER::pDmaBufferPrivateData) {
        render.pDmaBufferPrivateData = dma_priv_ptr;
      }
      __if_exists(D3DDDICB_RENDER::DmaBufferPrivateDataSize) {
        render.DmaBufferPrivateDataSize = dma_priv_size;
      }

      submit_hr = CallCbMaybeHandle(cb->pfnRenderCb, dev->hrt_device, &render);
      __if_exists(D3DDDICB_RENDER::NewFenceValue) {
        submission_fence = static_cast<uint64_t>(render.NewFenceValue);
      }
      __if_not_exists(D3DDDICB_RENDER::NewFenceValue) {
        __if_exists(D3DDDICB_RENDER::SubmissionFenceId) {
          submission_fence = static_cast<uint64_t>(render.SubmissionFenceId);
        }
      }
    }

    // Always return submission buffers to the runtime.
    deallocate(alloc, dma_priv_ptr, dma_priv_size);

    if (FAILED(submit_hr)) {
      if (out_hr) {
        *out_hr = submit_hr;
      }
      dev->cmd.reset();
      return 0;
    }

    if (submission_fence != 0) {
      last_fence = submission_fence;
    }

    cur = chunk_end;
  }

  if (last_fence != 0) {
    dev->last_submitted_fence = last_fence;
  }

  dev->cmd.reset();
  return last_fence;
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
    dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    dealloc.NumAllocations = static_cast<UINT>(km_allocs.size());
    dealloc.HandleList = km_allocs.empty() ? nullptr : km_allocs.data();
    dev->callbacks.pfnDeallocateCb(dev->hrt_device, &dealloc);
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  };

  const auto allocate_one = [&](uint64_t size_bytes, bool cpu_visible, bool is_rt, bool is_ds, bool is_shared) -> HRESULT {
    if (!pDesc->pAllocationInfo) {
      return E_INVALIDARG;
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
    alloc_info[0].SupportedReadSegmentSet = 1;
    alloc_info[0].SupportedWriteSegmentSet = 1;

    aerogpu_wddm_alloc_priv priv = {};
    if (is_shared) {
      uint32_t alloc_id = 0;
      uint64_t share_token = 0;
      if (!AllocateSharedAllocIds(&alloc_id, &share_token)) {
        return E_FAIL;
      }

      priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
      priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
      priv.alloc_id = alloc_id;
      priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED;
      priv.share_token = share_token;
      priv.size_bytes = static_cast<aerogpu_wddm_u64>(size_bytes);
      priv.reserved0 = 0;

      alloc_info[0].pPrivateDriverData = &priv;
      alloc_info[0].PrivateDriverDataSize = sizeof(priv);
    }

    D3DDDICB_ALLOCATE alloc = {};
    alloc.hResource = hRTResource;
    alloc.NumAllocations = 1;
    alloc.pAllocationInfo = alloc_info;
    alloc.Flags.Value = 0;
    alloc.Flags.CreateResource = 1;
    if (is_shared) {
      alloc.Flags.CreateShared = 1;
    }
    alloc.ResourceFlags.Value = 0;
    alloc.ResourceFlags.RenderTarget = is_rt ? 1u : 0u;
    alloc.ResourceFlags.ZBuffer = is_ds ? 1u : 0u;

    const HRESULT hr = dev->callbacks.pfnAllocateCb(dev->hrt_device, &alloc);
    if (FAILED(hr)) {
      return hr;
    }

    res->wddm.km_resource_handle = static_cast<uint64_t>(alloc.hKMResource);
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_allocation_handles.push_back(static_cast<uint64_t>(alloc_info[0].hKMAllocation));
    return S_OK;
  };

  const uint32_t dim = static_cast<uint32_t>(pDesc->ResourceDimension);
  if (dim == 1u /* buffer */) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pDesc->ByteWidth;
    const uint64_t alloc_size = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 256);
    bool cpu_visible = false;
    if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING));
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
#ifdef D3D10_DDI_RESOURCE_MISC_SHARED
    is_shared = (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED) != 0;
#else
    is_shared = (res->misc_flags & D3D10_RESOURCE_MISC_SHARED) != 0;
#endif

    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      res->~AeroGpuResource();
      return hr;
    }

    auto copy_initial_data = [&](auto init_data) -> HRESULT {
      if (!init_data) {
        return S_OK;
      }
      const auto& init = init_data[0];
      if (!init.pSysMem) {
        return E_INVALIDARG;
      }
      if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes));
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

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u size=%llu",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned long long>(res->size_bytes));
#endif
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = res->storage.size();
    }
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

    if (res->mip_levels != 1 || res->array_size != 1) {
      res->~AeroGpuResource();
      return E_NOTIMPL;
    }

    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    const uint64_t row_bytes_u64 = static_cast<uint64_t>(res->width) * static_cast<uint64_t>(bpp);
    if (bpp == 0 || row_bytes_u64 == 0 || row_bytes_u64 > UINT32_MAX) {
      res->~AeroGpuResource();
      return E_OUTOFMEMORY;
    }
    const uint32_t row_bytes = static_cast<uint32_t>(row_bytes_u64);
    res->row_pitch_bytes = AlignUpU32(row_bytes, 256);

    const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    bool cpu_visible = false;
    if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->Usage) == static_cast<uint32_t>(D3D10_USAGE_STAGING));
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
#ifdef D3D10_DDI_RESOURCE_MISC_SHARED
    is_shared = (res->misc_flags & D3D10_DDI_RESOURCE_MISC_SHARED) != 0;
#else
    is_shared = (res->misc_flags & D3D10_RESOURCE_MISC_SHARED) != 0;
#endif
    HRESULT hr = allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      res->~AeroGpuResource();
      return hr;
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
      for (uint32_t y = 0; y < res->height; y++) {
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

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created tex2d handle=%u size=%ux%u row_pitch=%u",
                         static_cast<unsigned>(res->handle),
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
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), res->storage.size());
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = res->storage.size();
    }
    return S_OK;
  }

  deallocate_if_needed();
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

  if (dev->current_rtv_res == res) {
    dev->current_rtv_res = nullptr;
    dev->current_rtv = 0;
  }
  if (dev->current_vb_res == res) {
    dev->current_vb_res = nullptr;
    dev->current_vb_stride = 0;
    dev->current_vb_offset = 0;
  }

  if (res->wddm.km_resource_handle != 0 || !res->wddm.km_allocation_handles.empty()) {
    std::vector<D3DKMT_HANDLE> km_allocs;
    km_allocs.reserve(res->wddm.km_allocation_handles.size());
    for (uint64_t h : res->wddm.km_allocation_handles) {
      km_allocs.push_back(static_cast<D3DKMT_HANDLE>(h));
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    dealloc.NumAllocations = static_cast<UINT>(km_allocs.size());
    dealloc.HandleList = km_allocs.empty() ? nullptr : km_allocs.data();
    const HRESULT hr = dev->callbacks.pfnDeallocateCb(dev->hrt_device, &dealloc);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
    }
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  }

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

    if (dst->size_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t dst_size = static_cast<size_t>(dst->size_bytes);
      if (dst->storage.size() < dst_size) {
        try {
          dst->storage.resize(dst_size, 0);
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
          return;
        }
      }
    }
    if (src->size_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      const size_t src_size = static_cast<size_t>(src->size_bytes);
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
      std::memcpy(dst->storage.data() + static_cast<size_t>(dst_off),
                  src->storage.data() + static_cast<size_t>(src_left),
                  static_cast<size_t>(bytes));
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = dst_off;
    cmd->src_offset_bytes = src_left;
    cmd->size_bytes = bytes;
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
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

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(dst->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);

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
    const uint64_t row_bytes_u64 = static_cast<uint64_t>(copy_width) * static_cast<uint64_t>(bpp);

    auto ensure_row_pitch = [&](AeroGpuResource* res) -> bool {
      if (res->row_pitch_bytes != 0) {
        return true;
      }
      const uint64_t pitch = static_cast<uint64_t>(res->width) * static_cast<uint64_t>(bpp);
      if (pitch > UINT32_MAX) {
        return false;
      }
      res->row_pitch_bytes = static_cast<uint32_t>(pitch);
      return true;
    };
    const bool has_row_pitch = ensure_row_pitch(dst) && ensure_row_pitch(src);

    const uint64_t dst_total = static_cast<uint64_t>(dst->row_pitch_bytes) * static_cast<uint64_t>(dst->height);
    const uint64_t src_total = static_cast<uint64_t>(src->row_pitch_bytes) * static_cast<uint64_t>(src->height);
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

    if (has_row_pitch && row_bytes_u64 && row_bytes_u64 <= static_cast<uint64_t>(SIZE_MAX)) {
      const uint64_t dst_row_needed = static_cast<uint64_t>(dstX) * static_cast<uint64_t>(bpp) + row_bytes_u64;
      const uint64_t src_row_needed = static_cast<uint64_t>(src_left) * static_cast<uint64_t>(bpp) + row_bytes_u64;
      if (dst_row_needed <= static_cast<uint64_t>(dst->row_pitch_bytes) && src_row_needed <= static_cast<uint64_t>(src->row_pitch_bytes)) {
        for (uint32_t y = 0; y < copy_height; y++) {
          const uint64_t dst_off_u64 =
              static_cast<uint64_t>(dstY + y) * static_cast<uint64_t>(dst->row_pitch_bytes) +
              static_cast<uint64_t>(dstX) * static_cast<uint64_t>(bpp);
          const uint64_t src_off_u64 =
              static_cast<uint64_t>(src_top + y) * static_cast<uint64_t>(src->row_pitch_bytes) +
              static_cast<uint64_t>(src_left) * static_cast<uint64_t>(bpp);
          if (dst_off_u64 + row_bytes_u64 <= dst->storage.size() && src_off_u64 + row_bytes_u64 <= src->storage.size()) {
            std::memcpy(dst->storage.data() + static_cast<size_t>(dst_off_u64),
                        src->storage.data() + static_cast<size_t>(src_off_u64),
                        static_cast<size_t>(row_bytes_u64));
          }
        }
      }
    }

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
    cmd->flags = AEROGPU_COPY_FLAG_NONE;
    cmd->reserved0 = 0;
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

  // Unbind path (e.g. IASetVertexBuffers(0,0,NULL,NULL,NULL)).
  if (startSlot == 0 && numBuffers == 0) {
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
  AeroGpuResource* rtv_res = nullptr;
  aerogpu_handle_t dsv_handle = 0;
  if (numViews && phViews && phViews[0].pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phViews[0]);
    rtv_res = view ? view->resource : nullptr;
    rtv_handle = rtv_res ? rtv_res->handle : (view ? view->texture : 0);
  }
  if (hDsv.pDrvPrivate) {
    dsv_handle = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture;
  }

  dev->current_rtv = rtv_handle;
  dev->current_rtv_res = rtv_res;
  dev->current_dsv = dsv_handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = numViews ? 1 : 0;
  cmd->depth_stencil = dsv_handle;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  cmd->colors[0] = rtv_handle;
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
              dst[0] = out_r;
              dst[1] = out_g;
              dst[2] = out_b;
              dst[3] = out_a;
              break;
            case kDxgiFormatB8G8R8X8Unorm:
              dst[0] = out_b;
              dst[1] = out_g;
              dst[2] = out_r;
              dst[3] = 255;
              break;
            case kDxgiFormatB8G8R8A8Unorm:
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
                dst[0] = out_r;
                dst[1] = out_g;
                dst[2] = out_b;
                dst[3] = out_a;
                break;
              case kDxgiFormatB8G8R8X8Unorm:
                dst[0] = out_b;
                dst[1] = out_g;
                dst[2] = out_r;
                dst[3] = 255;
                break;
              case kDxgiFormatB8G8R8A8Unorm:
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

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
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

  aerogpu_handle_t src_handle = 0;
  if (hsrc.pDrvPrivate) {
    src_handle = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hsrc)->handle;
  }

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
    if (!res) {
      return;
    }
    if (std::find(resources.begin(), resources.end(), res) != resources.end()) {
      // Reject duplicates: RotateResourceIdentities expects distinct resources.
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
    AeroGpuResource::WddmIdentity wddm;
    std::vector<uint8_t> storage;
  };

  auto take_identity = [](AeroGpuResource* res) -> ResourceIdentity {
    ResourceIdentity id{};
    if (!res) {
      return id;
    }
    id.handle = res->handle;
    id.wddm = std::move(res->wddm);
    id.storage = std::move(res->storage);
    return id;
  };

  auto put_identity = [](AeroGpuResource* res, ResourceIdentity&& id) {
    if (!res) {
      return;
    }
    res->handle = id.handle;
    res->wddm = std::move(id.wddm);
    res->storage = std::move(id.storage);
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

HRESULT APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pCaps) {
  if (!pCaps || !pCaps->pData) {
    return E_INVALIDARG;
  }

  DebugLog("aerogpu-d3d10: GetCaps type=%u size=%u\n", (unsigned)pCaps->Type, (unsigned)pCaps->DataSize);

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

  switch (pCaps->Type) {
    case D3D10DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    case D3D10DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        fmt->Format = in_format;
        const uint32_t format = static_cast<uint32_t>(in_format);

        UINT support = 0;
        switch (format) {
          case kDxgiFormatB8G8R8A8Unorm:
          case kDxgiFormatB8G8R8X8Unorm:
          case kDxgiFormatR8G8B8A8Unorm:
            support = D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                      D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY | D3D10_FORMAT_SUPPORT_BLENDABLE |
                      D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
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
        uint8_t* out_bytes = reinterpret_cast<uint8_t*>(pCaps->pData);
        *reinterpret_cast<DXGI_FORMAT*>(out_bytes) = msaa_format;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT)) = msaa_sample_count;
        *reinterpret_cast<UINT*>(out_bytes + sizeof(DXGI_FORMAT) + sizeof(UINT)) =
            (msaa_sample_count == 1) ? 1u : 0u;
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
