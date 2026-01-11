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

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cassert>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <limits>
#include <mutex>
#include <new>
#include <type_traits>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"
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
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr uint32_t kD3DMapFlagDoNotWait = 0x100000;

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

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;

  // Optional D3DKMT adapter handle for dev-only calls (e.g. QUERY_FENCE via Escape).
  // This is best-effort bring-up plumbing; the real submission path should use
  // runtime callbacks and context-owned sync objects instead.
  D3DKMT_HANDLE kmt_adapter = 0;
};

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

  // Map state (for UP resources backed by `storage`).
  bool mapped = false;
  bool mapped_write = false;
  uint32_t mapped_subresource = 0;
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;
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
  AeroGpuResource* resource = nullptr;
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

using SetErrorFn = void(AEROGPU_APIENTRY*)(D3D10DDI_HRTDEVICE, HRESULT);

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  D3D10DDI_HRTDEVICE hrt_device{};
  SetErrorFn pfn_set_error = nullptr;
  const D3DDDI_DEVICECALLBACKS* callbacks = nullptr;

  aerogpu::CmdWriter cmd;

  // Fence tracking for WDDM-backed synchronization (used by Map READ / DO_NOT_WAIT semantics).
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

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
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`, `d3d10_1_triangle`).
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

void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

template <typename Fn, typename Handle, typename... Args>
decltype(auto) CallCbMaybeHandle(Fn fn, Handle handle, Args&&... args) {
  if constexpr (std::is_invocable_v<Fn, Handle, Args...>) {
    return fn(handle, std::forward<Args>(args)...);
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
    p.pfn_close_adapter =
        reinterpret_cast<decltype(&D3DKMTCloseAdapter)>(GetProcAddress(gdi32, "D3DKMTCloseAdapter"));
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

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;

  if (dev->kmt_fence_syncobj) {
    __if_exists(D3DDDI_DEVICECALLBACKS::pfnDestroySynchronizationObjectCb) {
      if (cb && cb->pfnDestroySynchronizationObjectCb) {
        __if_exists(D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT) {
          D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT args{};
          __if_exists(D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT::hSyncObject) {
            args.hSyncObject = UintPtrToD3dHandle<decltype(args.hSyncObject)>(
                static_cast<std::uintptr_t>(dev->kmt_fence_syncobj));
          }
          (void)CallCbMaybeHandle(cb->pfnDestroySynchronizationObjectCb, dev->hrt_device, &args);
        }
      }
    }
    dev->kmt_fence_syncobj = 0;
  }

  if (dev->kmt_context) {
    __if_exists(D3DDDI_DEVICECALLBACKS::pfnDestroyContextCb) {
      if (cb && cb->pfnDestroyContextCb) {
        __if_exists(D3DDDICB_DESTROYCONTEXT) {
          D3DDDICB_DESTROYCONTEXT args{};
          __if_exists(D3DDDICB_DESTROYCONTEXT::hContext) {
            args.hContext = UintPtrToD3dHandle<decltype(args.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
          }
          (void)CallCbMaybeHandle(cb->pfnDestroyContextCb, dev->hrt_device, &args);
        }
      }
    }
    dev->kmt_context = 0;
  }

  if (dev->kmt_device) {
    __if_exists(D3DDDI_DEVICECALLBACKS::pfnDestroyDeviceCb) {
      if (cb && cb->pfnDestroyDeviceCb) {
        __if_exists(D3DDDICB_DESTROYDEVICE) {
          D3DDDICB_DESTROYDEVICE args{};
          __if_exists(D3DDDICB_DESTROYDEVICE::hDevice) {
            args.hDevice = UintPtrToD3dHandle<decltype(args.hDevice)>(static_cast<std::uintptr_t>(dev->kmt_device));
          }
          (void)CallCbMaybeHandle(cb->pfnDestroyDeviceCb, dev->hrt_device, &args);
        }
      }
    }
    dev->kmt_device = 0;
  }

  dev->dma_buffer_private_data = nullptr;
  dev->dma_buffer_private_data_size = 0;
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

  bool have_create_device = false;
  __if_exists(D3DDDI_DEVICECALLBACKS::pfnCreateDeviceCb) {
    have_create_device = (cb->pfnCreateDeviceCb != nullptr);
  }
  if (!have_create_device) {
    return S_OK;
  }

  bool have_create_context = false;
  bool use_create_context2 = false;
  __if_exists(D3DDDI_DEVICECALLBACKS::pfnCreateContextCb2) {
    if (cb->pfnCreateContextCb2) {
      have_create_context = true;
      use_create_context2 = true;
    }
  }
  __if_exists(D3DDDI_DEVICECALLBACKS::pfnCreateContextCb) {
    if (!have_create_context && cb->pfnCreateContextCb) {
      have_create_context = true;
      use_create_context2 = false;
    }
  }
  if (!have_create_context) {
    return S_OK;
  }

  __if_exists(D3DDDICB_CREATEDEVICE) {
    D3DDDICB_CREATEDEVICE create_device{};
    __if_exists(D3DDDICB_CREATEDEVICE::hAdapter) {
      create_device.hAdapter =
          UintPtrToD3dHandle<decltype(create_device.hAdapter)>(reinterpret_cast<std::uintptr_t>(hAdapter.pDrvPrivate));
    }
    HRESULT hr = CallCbMaybeHandle(cb->pfnCreateDeviceCb, dev->hrt_device, &create_device);
    if (FAILED(hr) || !create_device.hDevice) {
      return FAILED(hr) ? hr : E_FAIL;
    }
    dev->kmt_device = static_cast<D3DKMT_HANDLE>(D3dHandleToUintPtr(create_device.hDevice));
  }
  __if_not_exists(D3DDDICB_CREATEDEVICE) {
    return S_OK;
  }

  __if_exists(D3DDDICB_CREATECONTEXT) {
    D3DDDICB_CREATECONTEXT create_ctx{};
    __if_exists(D3DDDICB_CREATECONTEXT::hDevice) {
      create_ctx.hDevice = UintPtrToD3dHandle<decltype(create_ctx.hDevice)>(static_cast<std::uintptr_t>(dev->kmt_device));
    }
    __if_exists(D3DDDICB_CREATECONTEXT::NodeOrdinal) {
      create_ctx.NodeOrdinal = 0;
    }
    __if_exists(D3DDDICB_CREATECONTEXT::EngineAffinity) {
      create_ctx.EngineAffinity = 0;
    }
    __if_exists(D3DDDICB_CREATECONTEXT::pPrivateDriverData) {
      create_ctx.pPrivateDriverData = nullptr;
    }
    __if_exists(D3DDDICB_CREATECONTEXT::PrivateDriverDataSize) {
      create_ctx.PrivateDriverDataSize = 0;
    }

    HRESULT hr = E_FAIL;
    if (use_create_context2) {
      __if_exists(D3DDDI_DEVICECALLBACKS::pfnCreateContextCb2) {
        hr = CallCbMaybeHandle(cb->pfnCreateContextCb2, dev->hrt_device, &create_ctx);
      }
    } else {
      __if_exists(D3DDDI_DEVICECALLBACKS::pfnCreateContextCb) {
        hr = CallCbMaybeHandle(cb->pfnCreateContextCb, dev->hrt_device, &create_ctx);
      }
    }
    if (FAILED(hr) || !create_ctx.hContext || !create_ctx.hSyncObject) {
      DestroyKernelDeviceContext(dev);
      return FAILED(hr) ? hr : E_FAIL;
    }

    dev->kmt_context = static_cast<D3DKMT_HANDLE>(D3dHandleToUintPtr(create_ctx.hContext));
    dev->kmt_fence_syncobj = static_cast<D3DKMT_HANDLE>(D3dHandleToUintPtr(create_ctx.hSyncObject));
    __if_exists(D3DDDICB_CREATECONTEXT::pDmaBufferPrivateData) {
      dev->dma_buffer_private_data = create_ctx.pDmaBufferPrivateData;
    }
    __if_exists(D3DDDICB_CREATECONTEXT::DmaBufferPrivateDataSize) {
      dev->dma_buffer_private_data_size = create_ctx.DmaBufferPrivateDataSize;
    }
  }
  __if_not_exists(D3DDDICB_CREATECONTEXT) {
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

  if (dev->kmt_fence_syncobj) {
    D3DKMT_HANDLE handles[1] = {dev->kmt_fence_syncobj};
    UINT64 fence_values[1] = {fence};

    // Prefer the runtime's wait callback when available; it matches the Win7 DDI
    // contract and avoids direct-thunk WOW64 quirks.
    const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
    bool have_wait_cb = false;
    __if_exists(D3DDDI_DEVICECALLBACKS::pfnWaitForSynchronizationObjectCb) {
      have_wait_cb = (cb && cb->pfnWaitForSynchronizationObjectCb);
    }
    if (have_wait_cb) {
      D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
        args.hContext = UintPtrToD3dHandle<decltype(args.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::ObjectCount) {
        args.ObjectCount = 1;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::ObjectHandleArray) {
        args.ObjectHandleArray = handles;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hSyncObjects) {
        args.hSyncObjects = handles;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        args.FenceValueArray = fence_values;
      }
      __if_not_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValue) {
          args.FenceValue = fence;
        }
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::Timeout) {
        args.Timeout = timeout_ms ? static_cast<UINT64>(timeout_ms) : ~0ull;
      }

      const HRESULT hr = CallCbMaybeHandle(cb->pfnWaitForSynchronizationObjectCb, dev->hrt_device, &args);
      if (hr == kDxgiErrorWasStillDrawing || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) ||
          hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT)) {
        return kDxgiErrorWasStillDrawing;
      }
      if (FAILED(hr)) {
        return E_FAIL;
      }

      UpdateCompletedFence(dev, fence);
      (void)AeroGpuQueryCompletedFence(dev);
      return S_OK;
    }

    const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
    if (procs.pfn_wait_for_syncobj) {
      D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hAdapter) {
        args.hAdapter = dev->kmt_adapter;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
        args.hContext = dev->kmt_context;
      }
      args.ObjectCount = 1;
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::ObjectHandleArray) {
        args.ObjectHandleArray = handles;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hSyncObjects) {
        args.hSyncObjects = handles;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        args.FenceValueArray = fence_values;
      }
      __if_not_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValue) {
          args.FenceValue = fence;
        }
      }
      args.Timeout = timeout_ms ? static_cast<UINT64>(timeout_ms) : ~0ull;

      const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
      if (st == STATUS_TIMEOUT) {
        return kDxgiErrorWasStillDrawing;
      }
      if (!NT_SUCCESS(st)) {
        return E_FAIL;
      }

      UpdateCompletedFence(dev, fence);
      (void)AeroGpuQueryCompletedFence(dev);
      return S_OK;
    }

    return E_FAIL;
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

HRESULT AeroGpuPollFence(AeroGpuDevice* dev, uint64_t fence) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (fence == 0) {
    return S_OK;
  }

  if (AeroGpuQueryCompletedFence(dev) >= fence) {
    return S_OK;
  }

  if (dev->kmt_fence_syncobj) {
    const D3DKMT_HANDLE handles[1] = {dev->kmt_fence_syncobj};
    const UINT64 fence_values[1] = {fence};

    // Prefer the runtime's wait callback when available; it matches the Win7 DDI
    // contract and avoids direct-thunk WOW64 quirks.
    const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
    bool have_wait_cb = false;
    __if_exists(D3DDDI_DEVICECALLBACKS::pfnWaitForSynchronizationObjectCb) {
      have_wait_cb = (cb && cb->pfnWaitForSynchronizationObjectCb);
    }
    if (have_wait_cb) {
      D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT args{};
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
        args.hContext = UintPtrToD3dHandle<decltype(args.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::ObjectCount) {
        args.ObjectCount = 1;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::ObjectHandleArray) {
        args.ObjectHandleArray = handles;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::hSyncObjects) {
        args.hSyncObjects = handles;
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        args.FenceValueArray = fence_values;
      }
      __if_not_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::FenceValue) {
          args.FenceValue = fence;
        }
      }
      __if_exists(D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT::Timeout) {
        args.Timeout = 0; // poll
      }

      const HRESULT hr = CallCbMaybeHandle(cb->pfnWaitForSynchronizationObjectCb, dev->hrt_device, &args);
      if (hr == kDxgiErrorWasStillDrawing || hr == HRESULT_FROM_WIN32(WAIT_TIMEOUT) ||
          hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT)) {
        return kDxgiErrorWasStillDrawing;
      }
      if (FAILED(hr)) {
        return E_FAIL;
      }

      UpdateCompletedFence(dev, fence);
      (void)AeroGpuQueryCompletedFence(dev);
      return S_OK;
    }

    const AeroGpuD3dkmtProcs& procs = GetAeroGpuD3dkmtProcs();
    if (procs.pfn_wait_for_syncobj) {
      D3DKMT_WAITFORSYNCHRONIZATIONOBJECT args{};
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hAdapter) {
        args.hAdapter = dev->kmt_adapter;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hContext) {
        args.hContext = dev->kmt_context;
      }
      args.ObjectCount = 1;
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::ObjectHandleArray) {
        args.ObjectHandleArray = handles;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::hSyncObjects) {
        args.hSyncObjects = handles;
      }
      __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        args.FenceValueArray = fence_values;
      }
      __if_not_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValueArray) {
        __if_exists(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT::FenceValue) {
          args.FenceValue = fence;
        }
      }
      args.Timeout = 0; // poll

      const NTSTATUS st = procs.pfn_wait_for_syncobj(&args);
      if (st == STATUS_TIMEOUT) {
        return kDxgiErrorWasStillDrawing;
      }
      if (!NT_SUCCESS(st)) {
        return E_FAIL;
      }

      UpdateCompletedFence(dev, fence);
      (void)AeroGpuQueryCompletedFence(dev);
      return S_OK;
    }

    return E_FAIL;
  }

  if (!dev->adapter) {
    return E_FAIL;
  }

  uint64_t completed = 0;
  {
    std::lock_guard<std::mutex> lock(dev->adapter->fence_mutex);
    completed = dev->adapter->completed_fence;
  }
  UpdateCompletedFence(dev, completed);
  return (completed >= fence) ? S_OK : kDxgiErrorWasStillDrawing;
}

uint64_t submit_locked(AeroGpuDevice* dev, bool want_present, HRESULT* out_hr) {
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

  const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
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
  std::uintptr_t wddm_context = static_cast<std::uintptr_t>(dev->kmt_context);
  auto log_missing_context_once = [&] {
    static std::atomic<bool> logged = false;
    bool expected = false;
    if (logged.compare_exchange_strong(expected, true)) {
      AEROGPU_D3D10_11_LOG(
          "wddm_submit: D3DDDICB_* exposes hContext but the callback returned hContext=0; "
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
    __if_exists(D3DDDICB_ALLOCATE::hContext) {
      alloc.hContext = UintPtrToD3dHandle<decltype(alloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
    }

    HRESULT alloc_hr = CallCbMaybeHandle(cb->pfnAllocateCb, dev->hrt_device, &alloc);

    void* dma_ptr = nullptr;
    UINT dma_cap = 0;
    void* dma_priv_ptr = nullptr;
    size_t dma_priv_size = 0;
    bool dma_priv_ptr_present = false;
    bool dma_priv_size_present = false;
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
    __if_exists(D3DDDICB_ALLOCATE::pDmaBufferPrivateData) {
      dma_priv_ptr = alloc.pDmaBufferPrivateData;
      dma_priv_ptr_present = true;
    }
    __if_exists(D3DDDICB_ALLOCATE::DmaBufferPrivateDataSize) {
      dma_priv_size = static_cast<size_t>(alloc.DmaBufferPrivateDataSize);
      dma_priv_size_present = true;
    }

    __if_exists(D3DDDICB_ALLOCATE::hContext) {
      const std::uintptr_t ctx = D3dHandleToUintPtr(alloc.hContext);
      if (ctx) {
        wddm_context = ctx;
      } else {
        log_missing_context_once();
      }
    }

    if (FAILED(alloc_hr) || !dma_ptr || dma_cap == 0) {
      if (out_hr) {
        *out_hr = FAILED(alloc_hr) ? alloc_hr : E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      return 0;
    }

    if (dma_priv_size_present) {
      if (dma_priv_size != 0 && dma_priv_ptr == nullptr) {
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
        __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
          dealloc.pDmaBufferPrivateData = dma_priv_ptr;
        }
        CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

        if (out_hr) {
          *out_hr = E_FAIL;
        }
        dev->cmd.reset();
        return 0;
      }

      if (dma_priv_size < static_cast<size_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES)) {
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
        __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
          dealloc.pDmaBufferPrivateData = dma_priv_ptr;
        }
        CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

        if (out_hr) {
          *out_hr = E_FAIL;
        }
        dev->cmd.reset();
        return 0;
      }
    } else if (dma_priv_ptr_present && dma_priv_ptr == nullptr) {
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
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
        dealloc.pDmaBufferPrivateData = dma_priv_ptr;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

      if (out_hr) {
        *out_hr = E_FAIL;
      }
      dev->cmd.reset();
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
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
        dealloc.pDmaBufferPrivateData = dma_priv_ptr;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

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
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
        dealloc.pDmaBufferPrivateData = dma_priv_ptr;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);

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

    const bool is_last_chunk = (chunk_end == src_size);
    const bool do_present = want_present && is_last_chunk && cb->pfnPresentCb != nullptr;

    if (dma_priv_ptr && dma_priv_size_present) {
      const size_t clear_bytes = std::min(
          dma_priv_size,
          static_cast<size_t>(AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES));
      if (clear_bytes) {
        std::memset(dma_priv_ptr, 0, clear_bytes);
      }
    }

    HRESULT submit_hr = S_OK;
    uint64_t submission_fence = 0;
    if (do_present) {
      D3DDDICB_PRESENT present = {};
      __if_exists(D3DDDICB_PRESENT::hContext) {
        present.hContext = UintPtrToD3dHandle<decltype(present.hContext)>(wddm_context);
        if (!wddm_context) {
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
        present.DmaBufferPrivateDataSize = static_cast<UINT>(dma_priv_size);
      }

      submit_hr = CallCbMaybeHandle(cb->pfnPresentCb, dev->hrt_device, &present);
      __if_exists(D3DDDICB_PRESENT::NewFenceValue) {
        submission_fence = static_cast<uint64_t>(present.NewFenceValue);
      }
      __if_exists(D3DDDICB_PRESENT::SubmissionFenceId) {
        if (submission_fence == 0) {
          submission_fence = static_cast<uint64_t>(present.SubmissionFenceId);
        }
      }
    } else {
      D3DDDICB_RENDER render = {};
      __if_exists(D3DDDICB_RENDER::hContext) {
        render.hContext = UintPtrToD3dHandle<decltype(render.hContext)>(wddm_context);
        if (!wddm_context) {
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
        render.DmaBufferPrivateDataSize = static_cast<UINT>(dma_priv_size);
      }

      submit_hr = CallCbMaybeHandle(cb->pfnRenderCb, dev->hrt_device, &render);
      __if_exists(D3DDDICB_RENDER::NewFenceValue) {
        submission_fence = static_cast<uint64_t>(render.NewFenceValue);
      }
      __if_exists(D3DDDICB_RENDER::SubmissionFenceId) {
        if (submission_fence == 0) {
          submission_fence = static_cast<uint64_t>(render.SubmissionFenceId);
        }
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
      __if_exists(D3DDDICB_DEALLOCATE::pDmaBufferPrivateData) {
        dealloc.pDmaBufferPrivateData = dma_priv_ptr;
      }
      CallCbMaybeHandle(cb->pfnDeallocateCb, dev->hrt_device, &dealloc);
    }

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

  const bool complete_immediately = (dev->kmt_fence_syncobj == 0 && dev->monitored_fence_value == nullptr);
  if (last_fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, last_fence);
  }
  if (complete_immediately && last_fence != 0) {
    UpdateCompletedFence(dev, last_fence);
  }

  dev->cmd.reset();
  return last_fence;
}

void set_error(AeroGpuDevice* dev, HRESULT hr);

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

void set_error(AeroGpuDevice* dev, HRESULT hr) {
  // Many D3D10/DDI entrypoints are `void` and must signal failures via the
  // runtime callback instead of returning HRESULT. Log these so bring-up can
  // quickly correlate failures to the last DDI call.
  AEROGPU_D3D10_11_LOG("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
  AEROGPU_D3D10_TRACEF("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
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

// Minimal CPU-side CopyResource implementation used by the Win7 triangle tests.
// The runtime copies the swapchain backbuffer into a staging texture and then
// maps it for readback; until the full WDDM submission path is wired, emulate
// that flow by copying the CPU backing storage.
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

    try {
      if (dst->kind == ResourceKind::Buffer && src->kind == ResourceKind::Buffer) {
        const uint64_t copy_bytes = std::min<uint64_t>(dst->size_bytes, src->size_bytes);
        if (copy_bytes) {
          if (dst->storage.size() < static_cast<size_t>(dst->size_bytes)) {
            dst->storage.resize(static_cast<size_t>(dst->size_bytes), 0);
          }
          if (src->storage.size() < static_cast<size_t>(copy_bytes)) {
            src->storage.resize(static_cast<size_t>(copy_bytes), 0);
          }
          std::memcpy(dst->storage.data(), src->storage.data(), static_cast<size_t>(copy_bytes));
        }
      } else if (dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
        if (dst->row_pitch_bytes == 0) {
          dst->row_pitch_bytes = dst->width * 4;
        }
        if (src->row_pitch_bytes == 0) {
          src->row_pitch_bytes = src->width * 4;
        }

        const uint32_t copy_w = std::min(dst->width, src->width);
        const uint32_t copy_h = std::min(dst->height, src->height);
        const uint32_t row_bytes = copy_w * 4;

        const uint64_t dst_total = static_cast<uint64_t>(dst->row_pitch_bytes) * static_cast<uint64_t>(dst->height);
        const uint64_t src_total = static_cast<uint64_t>(src->row_pitch_bytes) * static_cast<uint64_t>(src->height);
        if (dst_total <= static_cast<uint64_t>(SIZE_MAX) && dst->storage.size() < static_cast<size_t>(dst_total)) {
          dst->storage.resize(static_cast<size_t>(dst_total), 0);
        }
        if (src_total <= static_cast<uint64_t>(SIZE_MAX) && src->storage.size() < static_cast<size_t>(src_total)) {
          src->storage.resize(static_cast<size_t>(src_total), 0);
        }

        for (uint32_t y = 0; y < copy_h; ++y) {
          const uint8_t* src_row = src->storage.data() + static_cast<size_t>(y) * src->row_pitch_bytes;
          uint8_t* dst_row = dst->storage.data() + static_cast<size_t>(y) * dst->row_pitch_bytes;
          std::memcpy(dst_row, src_row, row_bytes);
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
    bool nonzero_u32 = false;
    bool has_src_box = false;

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
        if (v != 0) {
          nonzero_u32 = true;
        }
      } else if constexpr (std::is_pointer_v<T> &&
                           std::is_same_v<std::remove_cv_t<std::remove_pointer_t<T>>, D3D10_DDI_BOX>) {
        has_src_box = (v != nullptr);
      }
    };
    (capture(args), ...);

    auto* dev = has_device ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
    if (count < 2) {
      set_error(dev, E_INVALIDARG);
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_INVALIDARG;
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    }
    if (nonzero_u32 || has_src_box) {
      set_error(dev, E_NOTIMPL);
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_NOTIMPL;
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    }

    // Delegate to the CopyResource CPU implementation.
    return CopyResourceImpl<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
  }
};

// -------------------------------------------------------------------------------------------------
// D3D10.1 Device DDI (minimal subset + conservative stubs)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_TRACEF("DestroyDevice hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
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
                                        D3D10DDI_HRTRESOURCE) {
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
    std::memcpy(&resource_flags_bits, &pDesc->ResourceFlags, n);
  }

  const uint32_t tex_w =
      (pDesc && pDesc->pMipInfoList) ? static_cast<uint32_t>(pDesc->pMipInfoList[0].TexelWidth) : 0;
  const uint32_t tex_h =
      (pDesc && pDesc->pMipInfoList) ? static_cast<uint32_t>(pDesc->pMipInfoList[0].TexelHeight) : 0;

  AEROGPU_D3D10_11_LOG(
      "trace_resources: D3D10.1 CreateResource dim=%u bind=0x%08X usage=%u cpu=0x%08X misc=0x%08X fmt=%u "
      "byteWidth=%u w=%u h=%u mips=%u array=%u sample=(%u,%u) rflags=0x%llX rflags_size=%u mipInfoList=%p init=%p",
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
      pDesc ? pDesc->pMipInfoList : nullptr,
      init_ptr);
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
  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = AllocateGlobalHandle(dev->adapter);
    res->kind = ResourceKind::Buffer;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u size=%llu",
                         static_cast<unsigned>(res->handle),
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
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(init_hr);
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
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_TEXTURE2D) {
    if (pDesc->ArraySize != 1) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = AllocateGlobalHandle(dev->adapter);
    res->kind = ResourceKind::Texture2D;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    if (!pDesc->pMipInfoList) {
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    res->width = pDesc->pMipInfoList[0].TexelWidth;
    res->height = pDesc->pMipInfoList[0].TexelHeight;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);
    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created tex2d handle=%u size=%ux%u row_pitch=%u",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->width),
                         static_cast<unsigned>(res->height),
                         static_cast<unsigned>(res->row_pitch_bytes));
#endif

    auto copy_initial_data = [&](auto init_data) -> HRESULT {
      if (!init_data) {
        return S_OK;
      }
      if (res->mip_levels != 1 || res->array_size != 1) {
        return E_NOTIMPL;
      }

      const auto& init = init_data[0];
      if (!init.pSysMem) {
        return E_INVALIDARG;
      }

      const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
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
                                                : static_cast<size_t>(res->row_pitch_bytes);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    res->row_pitch_bytes);
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
      res->~AeroGpuResource();
      AEROGPU_D3D10_RET_HR(init_hr);
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
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  AEROGPU_D3D10_RET_HR(E_NOTIMPL);
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

  if (dev->current_rtv_res == res) {
    dev->current_rtv_res = nullptr;
    dev->current_rtv = 0;
  }
  if (dev->current_vb_res == res) {
    dev->current_vb_res = nullptr;
    dev->current_vb_stride = 0;
    dev->current_vb_offset = 0;
  }

  if (res->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
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

uint64_t resource_total_bytes(const AeroGpuResource* res) {
  if (!res) {
    return 0;
  }
  switch (res->kind) {
    case ResourceKind::Buffer:
      return res->size_bytes;
    case ResourceKind::Texture2D:
      return static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    default:
      return 0;
  }
}

HRESULT ensure_resource_storage(AeroGpuResource* res, uint64_t bytes) {
  if (!res) {
    return E_INVALIDARG;
  }
  if (bytes > static_cast<uint64_t>(std::numeric_limits<size_t>::max())) {
    return E_OUTOFMEMORY;
  }
  if (res->storage.size() >= static_cast<size_t>(bytes)) {
    return S_OK;
  }
  try {
    res->storage.resize(static_cast<size_t>(bytes), 0);
  } catch (...) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
}

HRESULT map_resource_locked(AeroGpuResource* res,
                            uint32_t subresource,
                            uint32_t map_type,
                            D3D10DDI_MAPPED_SUBRESOURCE* pMapped) {
  if (!res || !pMapped) {
    return E_INVALIDARG;
  }
  if (res->mapped) {
    return E_FAIL;
  }
  if (subresource != 0) {
    return E_INVALIDARG;
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
  res->mapped_offset = 0;
  res->mapped_size = total;
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

  if (res->mapped_write) {
    emit_upload_resource_locked(dev, res, res->mapped_offset, res->mapped_size);
  }

  res->mapped = false;
  res->mapped_write = false;
  res->mapped_subresource = 0;
  res->mapped_offset = 0;
  res->mapped_size = 0;
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
  res->mapped_offset = 0;
  res->mapped_size = total;
  *ppData = res->storage.data();
  return S_OK;
}

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
  const uint32_t map_type_u = static_cast<uint32_t>(map_type);
  if (map_type_u == kD3DMapRead || map_type_u == kD3DMapReadWrite) {
    // STAGING READ must observe results of prior GPU work (CopyResource, etc).
    const uint64_t fence = dev->last_submitted_fence.load(std::memory_order_relaxed);
    HRESULT wait = (map_flags & kD3DMapFlagDoNotWait) ? AeroGpuPollFence(dev, fence) : AeroGpuWaitForFence(dev, fence, 0);
    if (FAILED(wait)) {
      return wait;
    }
  }
  if (res->kind != ResourceKind::Texture2D) {
    return E_INVALIDARG;
  }
  return map_resource_locked(res, subresource, map_type_u, pMapped);
}

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
  if ((res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer)) == 0) {
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
  if ((res->bind_flags & kD3D10BindConstantBuffer) == 0) {
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
      HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, &data);
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
      HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, &data);
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

  // Conservative: only support generic map on buffers and staging textures for now.
  if (map_type_u == kD3DMapRead || map_type_u == kD3DMapReadWrite) {
    const uint64_t fence = dev->last_submitted_fence.load(std::memory_order_relaxed);
    HRESULT wait = (map_flags & kD3DMapFlagDoNotWait) ? AeroGpuPollFence(dev, fence) : AeroGpuWaitForFence(dev, fence, 0);
    if (FAILED(wait)) {
      return wait;
    }
  }
  if (res->kind == ResourceKind::Texture2D && res->bind_flags == 0) {
    return map_resource_locked(res, subresource, map_type_u, pMapped);
  }
  if (res->kind == ResourceKind::Buffer) {
    return map_resource_locked(res, subresource, map_type_u, pMapped);
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

    if (res->row_pitch_bytes == 0) {
      res->row_pitch_bytes = res->width * 4;
    }
    const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
    if (total_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
      if (res->storage.size() < static_cast<size_t>(total_bytes)) {
        try {
          res->storage.resize(static_cast<size_t>(total_bytes));
        } catch (...) {
          set_error(dev, E_OUTOFMEMORY);
          return;
        }
      }

      const uint32_t row_bytes = res->width * 4;
      for (uint32_t y = 0; y < res->height; ++y) {
        uint8_t* row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        for (uint32_t x = 0; x < res->width; ++x) {
          uint8_t* px = row + static_cast<size_t>(x) * 4;
          switch (res->dxgi_format) {
            case kDxgiFormatR8G8B8A8Unorm:
              px[0] = r;
              px[1] = g;
              px[2] = b;
              px[3] = a;
              break;
            case kDxgiFormatB8G8R8X8Unorm:
              px[0] = b;
              px[1] = g;
              px[2] = r;
              px[3] = 255;
              break;
            case kDxgiFormatB8G8R8A8Unorm:
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
  if (num_views && !phViews) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  for (UINT i = 0; i < num_views; i++) {
    aerogpu_handle_t tex = 0;
    if (phViews[i].pDrvPrivate) {
      tex = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(phViews[i])->texture;
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    cmd->shader_stage = shader_stage;
    cmd->slot = start_slot + i;
    cmd->texture = tex;
    cmd->reserved0 = 0;
  }
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
  if (hDsv.pDrvPrivate) {
    dsv_handle = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture;
  }

  dev->current_rtv = rtv_handle;
  dev->current_rtv_res = rtv_res;
  dev->current_dsv = dsv_handle;

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

  if (vertex_count == 3 && dev->current_topology == static_cast<uint32_t>(D3D10_DDI_PRIMITIVE_TOPOLOGY_TRIANGLELIST) &&
      dev->current_rtv_res && dev->current_vb_res) {
    auto* rt = dev->current_rtv_res;
    auto* vb = dev->current_vb_res;

    if (rt->kind == ResourceKind::Texture2D && vb->kind == ResourceKind::Buffer && rt->width && rt->height &&
        vb->storage.size() >= static_cast<size_t>(dev->current_vb_offset) +
                                static_cast<size_t>(start_vertex + 3) * static_cast<size_t>(dev->current_vb_stride)) {
      if (rt->row_pitch_bytes == 0) {
        rt->row_pitch_bytes = rt->width * 4;
      }
      const uint64_t rt_bytes = static_cast<uint64_t>(rt->row_pitch_bytes) * static_cast<uint64_t>(rt->height);
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
  AEROGPU_D3D10_TRACEF_VERBOSE("Map2 hDevice=%p hResource=%p sub=%u type=%u flags=0x%X",
                               hDevice.pDrvPrivate,
                               (pMap && pMap->hResource.pDrvPrivate) ? pMap->hResource.pDrvPrivate : nullptr,
                               pMap ? static_cast<unsigned>(pMap->Subresource) : 0u,
                               pMap ? static_cast<unsigned>(pMap->MapType) : 0u,
                               pMap ? static_cast<unsigned>(pMap->Flags) : 0u);
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
  const uint32_t map_flags_u = static_cast<uint32_t>(pMap->Flags);

  if (pMap->Subresource != 0) {
    set_error(dev, E_NOTIMPL);
    return;
  }

  if (map_type_u == kD3DMapWriteDiscard) {
    if (res->bind_flags & (kD3D10BindVertexBuffer | kD3D10BindIndexBuffer | kD3D10BindConstantBuffer)) {
      void* data = nullptr;
      const HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/true, &data);
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
      const HRESULT hr = map_dynamic_buffer_locked(res, /*discard=*/false, &data);
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

  if (map_type_u == kD3DMapRead || map_type_u == kD3DMapReadWrite) {
    const uint64_t fence = dev->last_submitted_fence.load(std::memory_order_relaxed);
    HRESULT wait =
        (map_flags_u & kD3DMapFlagDoNotWait) ? AeroGpuPollFence(dev, fence) : AeroGpuWaitForFence(dev, fence, 0);
    if (FAILED(wait)) {
      set_error(dev, wait);
      return;
    }
  }

  const HRESULT hr = map_resource_locked(res, pMap->Subresource, map_type_u, pOut);
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
    AeroGpuResource::WddmIdentity wddm;
    std::vector<uint8_t> storage;
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
    id.wddm = std::move(res->wddm);
    id.storage = std::move(res->storage);
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
    res->wddm = std::move(id.wddm);
    res->storage = std::move(id.storage);
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
  if (FAILED(init_hr)) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return init_hr;
  }

  InitDeviceFuncsWithStubs(pCreateDevice->pDeviceFuncs);

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

  // Map/unmap. Win7 D3D11 runtimes may use specialized entrypoints.
  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
  if constexpr (HasStagingResourceMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnStagingResourceMap = &StagingResourceMap;
    pCreateDevice->pDeviceFuncs->pfnStagingResourceUnmap = &StagingResourceUnmap;
  }
  if constexpr (HasDynamicIABufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapDiscard = &DynamicIABufferMapDiscard;
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapNoOverwrite = &DynamicIABufferMapNoOverwrite;
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferUnmap = &DynamicIABufferUnmap;
  }
  if constexpr (HasDynamicConstantBufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferMapDiscard = &DynamicConstantBufferMapDiscard;
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferUnmap = &DynamicConstantBufferUnmap;
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
  if (FAILED(init_hr)) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return init_hr;
  }

  InitDeviceFuncsWithStubs(pCreateDevice->pDeviceFuncs);

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

  pCreateDevice->pDeviceFuncs->pfnMap = &Map;
  pCreateDevice->pDeviceFuncs->pfnUnmap = &Unmap;
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = &UpdateSubresourceUP;
  pCreateDevice->pDeviceFuncs->pfnCopyResource =
      &CopyResourceImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call;
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      &CopySubresourceRegionImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call;

  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT AEROGPU_APIENTRY GetCaps10(D3D10DDI_HADAPTER, const D3D10DDIARG_GETCAPS* pCaps) {
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
  if (!pCaps || !pCaps->pData) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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
        __if_exists(D3D10DDIARG_FORMAT_SUPPORT::FormatSupport2) {
          fmt->FormatSupport2 = 0;
        }
        AEROGPU_D3D10_TRACEF("GetCaps10 FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
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

  AEROGPU_D3D10_RET_HR(S_OK);
}

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER, const D3D10_1DDIARG_GETCAPS* pCaps) {
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
  if (!pCaps || !pCaps->pData) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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

  switch (pCaps->Type) {
    case D3D10_1DDICAPS_TYPE_D3D10_FEATURE_LEVEL:
      if (pCaps->DataSize >= sizeof(D3D10_FEATURE_LEVEL1)) {
        *reinterpret_cast<D3D10_FEATURE_LEVEL1*>(pCaps->pData) = D3D10_FEATURE_LEVEL_10_0;
      }
      break;

    case D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10_1DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10_1DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
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
        fmt->FormatSupport2 = 0;
        AEROGPU_D3D10_TRACEF("GetCaps FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
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
