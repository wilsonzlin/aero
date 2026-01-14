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

#include <array>
#include <atomic>
#include <algorithm>
#include <cassert>
#include <condition_variable>
#include <cstdarg>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <cmath>
#include <excpt.h>
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
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_blend_state_validate.h"
#include "aerogpu_legacy_d3d9_format_fixup.h"
#include "aerogpu_d3d10_11_log.h"
#include "../../common/aerogpu_win32_security.h"
#include "aerogpu_d3d10_11_wddm_submit.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"
#include "../../../protocol/aerogpu_win7_abi.h"

namespace {

using aerogpu::d3d10_11::kStatusTimeout;
using aerogpu::d3d10_11::NtSuccess;
using aerogpu::d3d10_11::kDxgiErrorWasStillDrawing;
using aerogpu::d3d10_11::kHrPending;
using aerogpu::d3d10_11::kHrWaitTimeout;
using aerogpu::d3d10_11::kHrErrorTimeout;
using aerogpu::d3d10_11::kHrNtStatusTimeout;
using aerogpu::d3d10_11::kHrNtStatusGraphicsGpuBusy;
using aerogpu::d3d10_11::kD3DMapFlagDoNotWait;
using aerogpu::d3d10_11::kAeroGpuTimeoutMsInfinite;
using aerogpu::d3d10_11::kD3D10UsageDynamic;
using aerogpu::d3d10_11::kD3D10UsageStaging;
using aerogpu::d3d10_11::kD3D10CpuAccessRead;
using aerogpu::d3d10_11::kD3D10CpuAccessWrite;
using aerogpu::d3d10_11::kD3D10ResourceMiscShared;
using aerogpu::d3d10_11::ResetObject;
using aerogpu::d3d10_11::kD3D10DeviceLiveCookie;
using aerogpu::d3d10_11::HasLiveCookie;
using aerogpu::d3d10_11::ConsumeWddmAllocPrivV2;
using aerogpu::d3d10_11::ValidateNoNullDdiTable;
using aerogpu::d3d10_11::AnyNonNullHandles;
using aerogpu::d3d10_11::D3dViewDimensionIsTexture2D;
using aerogpu::d3d10_11::D3dViewDimensionIsTexture2DArray;
using aerogpu::d3d10_11::D3dViewCountToRemaining;
using aerogpu::d3d10_11::ClampU64ToU32;
using aerogpu::d3d10_11::InitSamplerFromCreateSamplerArg;
using aerogpu::d3d10_11::InitLockForWrite;
using aerogpu::d3d10_11::InitLockArgsForMap;
using aerogpu::d3d10_11::InitUnlockArgsForMap;
using aerogpu::d3d10_11::InitUnlockForWrite;
using aerogpu::d3d10_11::UintPtrToD3dHandle;
using aerogpu::d3d10_11::TrackStagingWriteLocked;
using aerogpu::d3d10_11::ResourcesAlias;
using aerogpu::d3d10_11::EmitSetRenderTargetsCmdLocked;
using aerogpu::d3d10_11::resource_total_bytes;

static bool IsDeviceLive(D3D10DDI_HDEVICE hDevice) {
  return HasLiveCookie(hDevice.pDrvPrivate, kD3D10DeviceLiveCookie);
}

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

using aerogpu::d3d10_11::kInvalidHandle;
using aerogpu::d3d10_11::kMaxConstantBufferSlots;
using aerogpu::d3d10_11::kMaxShaderResourceSlots;
using aerogpu::d3d10_11::kMaxSamplerSlots;
using aerogpu::d3d10_11::kD3DSampleMaskAll;
using aerogpu::d3d10_11::kD3DColorWriteMaskAll;
using aerogpu::d3d10_11::kD3DStencilMaskAll;
constexpr uint32_t kMaxVertexBufferSlots = aerogpu::d3d10_11::kD3D10IaVertexInputResourceSlotCount;

using aerogpu::d3d10_11::AlignUpU64;
using aerogpu::d3d10_11::AlignUpU32;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
using aerogpu::d3d10_11::kDxgiFormatR32G32B32A32Float;
using aerogpu::d3d10_11::kDxgiFormatR32G32B32Float;
using aerogpu::d3d10_11::kDxgiFormatR32G32Float;
using aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Typeless;
using aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Unorm;
using aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc1Typeless;
using aerogpu::d3d10_11::kDxgiFormatBc1Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc1UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc2Typeless;
using aerogpu::d3d10_11::kDxgiFormatBc2Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc2UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc3Typeless;
using aerogpu::d3d10_11::kDxgiFormatBc3Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc3UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatD32Float;
using aerogpu::d3d10_11::kDxgiFormatD24UnormS8Uint;
using aerogpu::d3d10_11::kDxgiFormatR16Uint;
using aerogpu::d3d10_11::kDxgiFormatR32Uint;
using aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm;
using aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Unorm;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Typeless;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Typeless;
using aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb;
using aerogpu::d3d10_11::kDxgiFormatBc7Typeless;
using aerogpu::d3d10_11::kDxgiFormatBc7Unorm;
using aerogpu::d3d10_11::kDxgiFormatBc7UnormSrgb;

using aerogpu::d3d10_11::f32_bits;
using aerogpu::d3d10_11::HashSemanticName;
using aerogpu::d3d10_11::FromHandle;
using aerogpu::d3d10_11::bind_flags_to_buffer_usage_flags;
using aerogpu::d3d10_11::aerogpu_div_round_up_u32;
using aerogpu::d3d10_11::aerogpu_format_is_block_compressed;
using aerogpu::d3d10_11::aerogpu_mip_dim;
using aerogpu::d3d10_11::aerogpu_texture_min_row_pitch_bytes;
using aerogpu::d3d10_11::aerogpu_texture_num_rows;
using aerogpu::d3d10_11::aerogpu_texture_required_size_bytes;
using aerogpu::d3d10_11::bind_flags_to_usage_flags;
using aerogpu::d3d10_11::build_texture2d_subresource_layouts;
using aerogpu::d3d10_11::bytes_per_pixel_aerogpu;
using aerogpu::d3d10_11::dxgi_index_format_to_aerogpu;
using aerogpu::shared_surface::D3d9FormatToDxgi;
using aerogpu::shared_surface::FixupLegacyPrivForOpenResource;

using AerogpuTextureFormatLayout = aerogpu::d3d10_11::AerogpuTextureFormatLayout;
using aerogpu::d3d10_11::aerogpu_texture_format_layout;

static uint32_t aerogpu_lock_pitch_bytes(const D3DDDICB_LOCK& lock) {
  uint32_t pitch = 0;
  __if_exists(D3DDDICB_LOCK::Pitch) {
    pitch = lock.Pitch;
  }
  return pitch;
}

static uint32_t aerogpu_lock_slice_pitch_bytes(const D3DDDICB_LOCK& lock) {
  uint32_t pitch = 0;
  __if_exists(D3DDDICB_LOCK::SlicePitch) {
    pitch = lock.SlicePitch;
  }
  return pitch;
}

// D3D10_BIND_* and D3D11_BIND_* share values for the common subset we care about.
using aerogpu::d3d10_11::kD3D10BindVertexBuffer;
using aerogpu::d3d10_11::kD3D10BindIndexBuffer;
using aerogpu::d3d10_11::kD3D10BindConstantBuffer;
using aerogpu::d3d10_11::kD3D10BindShaderResource;
using aerogpu::d3d10_11::kD3D10BindRenderTarget;
using aerogpu::d3d10_11::kD3D10BindDepthStencil;

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

using Texture2DSubresourceLayout = aerogpu::d3d10_11::Texture2DSubresourceLayout;

static void LogLockPitchMismatchMaybe(uint32_t dxgi_format,
                                      uint32_t subresource_index,
                                      const Texture2DSubresourceLayout& sub,
                                      uint32_t expected_pitch,
                                      uint32_t lock_pitch) {
  if (lock_pitch == 0 || lock_pitch == expected_pitch) {
    return;
  }
  static std::atomic<uint32_t> g_mismatch_logs{0};
  const uint32_t n = g_mismatch_logs.fetch_add(1, std::memory_order_relaxed);
  if (n < 32) {
    AEROGPU_D3D10_11_LOG(
        "D3D10 LockCb pitch mismatch: fmt=%u sub=%u (mip=%u layer=%u) expected_pitch=%u lock_pitch=%u",
        static_cast<unsigned>(dxgi_format),
        static_cast<unsigned>(subresource_index),
        static_cast<unsigned>(sub.mip_level),
        static_cast<unsigned>(sub.array_layer),
        static_cast<unsigned>(expected_pitch),
        static_cast<unsigned>(lock_pitch));
  } else if (n == 32) {
    AEROGPU_D3D10_11_LOG("D3D10 LockCb pitch mismatch: log limit reached; suppressing further messages");
  }
}

static bool ValidateTexture2DRowSpan(uint32_t aerogpu_format,
                                     const Texture2DSubresourceLayout& sub,
                                     uint32_t pitch_bytes,
                                     uint64_t allocation_size_bytes,
                                     uint32_t* out_row_bytes) {
  if (out_row_bytes) {
    *out_row_bytes = 0;
  }
  if (pitch_bytes == 0 || allocation_size_bytes == 0) {
    return false;
  }

  const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, sub.width);
  if (row_bytes == 0 || sub.rows_in_layout == 0) {
    return false;
  }
  if (pitch_bytes < row_bytes) {
    return false;
  }

  const uint64_t rows_minus_one = static_cast<uint64_t>(sub.rows_in_layout - 1u);
  const uint64_t pitch_u64 = static_cast<uint64_t>(pitch_bytes);
  const uint64_t row_bytes_u64 = static_cast<uint64_t>(row_bytes);

  // offset + (rows-1)*pitch + row_bytes must be in-bounds.
  uint64_t span = 0;
  if (rows_minus_one != 0) {
    const uint64_t step = rows_minus_one * pitch_u64;
    if (step / pitch_u64 != rows_minus_one) {
      return false;
    }
    span = step;
  }
  const uint64_t span_plus_row = span + row_bytes_u64;
  if (span_plus_row < span) {
    return false;
  }
  const uint64_t end = sub.offset_bytes + span_plus_row;
  if (end < sub.offset_bytes) {
    return false;
  }
  if (end > allocation_size_bytes) {
    return false;
  }

  if (out_row_bytes) {
    *out_row_bytes = row_bytes;
  }
  return true;
}

struct AeroGpuDevice;

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
  if (!aerogpu::d3d10_11::GetPrimaryDisplayName(displayName)) {
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
  if (!aerogpu::d3d10_11::GetPrimaryDisplayName(displayName)) {
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
  // Actual WDDM allocation size (bytes), as reported by AllocateCb/OpenResource
  // private driver data. Used for conservative bounds checks when the runtime
  // lock pitch differs from our expected layout pitch.
  uint64_t wddm_allocation_size_bytes = 0;

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
  uint32_t sample_count = 1;
  uint32_t sample_quality = 0;
  uint32_t dxgi_format = 0;
  uint32_t row_pitch_bytes = 0;
  std::vector<Texture2DSubresourceLayout> tex2d_subresources;

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
  aerogpu::d3d10_11::AerogpuBlendStateBase state;
};

struct AeroGpuRasterizerState {
  uint32_t fill_mode = static_cast<uint32_t>(D3D10_FILL_SOLID);
  uint32_t cull_mode = static_cast<uint32_t>(D3D10_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  int32_t depth_bias = 0;
  uint32_t depth_clip_enable = 1u;
};

struct AeroGpuDepthStencilState {
  uint32_t depth_enable = 1u;
  uint32_t depth_write_mask = static_cast<uint32_t>(D3D10_DEPTH_WRITE_MASK_ALL);
  uint32_t depth_func = static_cast<uint32_t>(D3D10_COMPARISON_LESS);
  uint32_t stencil_enable = 0u;
  uint8_t stencil_read_mask = kD3DStencilMaskAll;
  uint8_t stencil_write_mask = kD3DStencilMaskAll;
  uint8_t reserved0[2] = {0, 0};
};

template <typename T, typename = void>
struct has_member_AlphaToCoverageEnable : std::false_type {};
template <typename T>
struct has_member_AlphaToCoverageEnable<T, std::void_t<decltype(std::declval<T>().AlphaToCoverageEnable)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BlendEnable : std::false_type {};
template <typename T>
struct has_member_BlendEnable<T, std::void_t<decltype(std::declval<T>().BlendEnable)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_RenderTargetWriteMask : std::false_type {};
template <typename T>
struct has_member_RenderTargetWriteMask<T, std::void_t<decltype(std::declval<T>().RenderTargetWriteMask)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrcBlend : std::false_type {};
template <typename T>
struct has_member_SrcBlend<T, std::void_t<decltype(std::declval<T>().SrcBlend)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DestBlend : std::false_type {};
template <typename T>
struct has_member_DestBlend<T, std::void_t<decltype(std::declval<T>().DestBlend)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BlendOp : std::false_type {};
template <typename T>
struct has_member_BlendOp<T, std::void_t<decltype(std::declval<T>().BlendOp)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SrcBlendAlpha : std::false_type {};
template <typename T>
struct has_member_SrcBlendAlpha<T, std::void_t<decltype(std::declval<T>().SrcBlendAlpha)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DestBlendAlpha : std::false_type {};
template <typename T>
struct has_member_DestBlendAlpha<T, std::void_t<decltype(std::declval<T>().DestBlendAlpha)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_BlendOpAlpha : std::false_type {};
template <typename T>
struct has_member_BlendOpAlpha<T, std::void_t<decltype(std::declval<T>().BlendOpAlpha)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_RenderTarget : std::false_type {};
template <typename T>
struct has_member_RenderTarget<T, std::void_t<decltype(std::declval<T>().RenderTarget)>> : std::true_type {};

template <typename DescT>
static bool FillBlendRtDescsFromDesc(const DescT& desc,
                                    aerogpu::d3d10_11::D3dRtBlendDesc* rts,
                                    uint32_t rt_count,
                                    bool* out_alpha_to_coverage_enable) {
  if (!rts || rt_count == 0) {
    return false;
  }

  bool alpha_to_coverage = false;
  if constexpr (has_member_AlphaToCoverageEnable<DescT>::value) {
    alpha_to_coverage = desc.AlphaToCoverageEnable ? true : false;
  }
  if (out_alpha_to_coverage_enable) {
    *out_alpha_to_coverage_enable = alpha_to_coverage;
  }

  // D3D10_BLEND_DESC-style: global blend factors/ops + per-RT enable/write-mask arrays.
  if constexpr (has_member_BlendEnable<DescT>::value) {
    uint32_t src_blend = aerogpu::d3d10_11::kD3dBlendOne;
    uint32_t dest_blend = aerogpu::d3d10_11::kD3dBlendZero;
    uint32_t blend_op = aerogpu::d3d10_11::kD3dBlendOpAdd;
    uint32_t src_blend_alpha = aerogpu::d3d10_11::kD3dBlendOne;
    uint32_t dest_blend_alpha = aerogpu::d3d10_11::kD3dBlendZero;
    uint32_t blend_op_alpha = aerogpu::d3d10_11::kD3dBlendOpAdd;

    if constexpr (has_member_SrcBlend<DescT>::value) {
      src_blend = static_cast<uint32_t>(desc.SrcBlend);
    }
    if constexpr (has_member_DestBlend<DescT>::value) {
      dest_blend = static_cast<uint32_t>(desc.DestBlend);
    }
    if constexpr (has_member_BlendOp<DescT>::value) {
      blend_op = static_cast<uint32_t>(desc.BlendOp);
    }
    if constexpr (has_member_SrcBlendAlpha<DescT>::value) {
      src_blend_alpha = static_cast<uint32_t>(desc.SrcBlendAlpha);
    }
    if constexpr (has_member_DestBlendAlpha<DescT>::value) {
      dest_blend_alpha = static_cast<uint32_t>(desc.DestBlendAlpha);
    }
    if constexpr (has_member_BlendOpAlpha<DescT>::value) {
      blend_op_alpha = static_cast<uint32_t>(desc.BlendOpAlpha);
    }

    for (uint32_t i = 0; i < rt_count; ++i) {
      rts[i].blend_enable = desc.BlendEnable[i] ? true : false;
      if constexpr (has_member_RenderTargetWriteMask<DescT>::value) {
        rts[i].write_mask = static_cast<uint8_t>(desc.RenderTargetWriteMask[i]);
      }
      rts[i].src_blend = src_blend;
      rts[i].dest_blend = dest_blend;
      rts[i].blend_op = blend_op;
      rts[i].src_blend_alpha = src_blend_alpha;
      rts[i].dest_blend_alpha = dest_blend_alpha;
      rts[i].blend_op_alpha = blend_op_alpha;
    }
    return true;
  }

  // D3D10.1-style: per-RT blend desc array (including factors/ops).
  if constexpr (has_member_RenderTarget<DescT>::value) {
    using RtT = std::remove_reference_t<decltype(desc.RenderTarget[0])>;
    if constexpr (!has_member_BlendEnable<RtT>::value || !has_member_RenderTargetWriteMask<RtT>::value ||
                  !has_member_SrcBlend<RtT>::value || !has_member_DestBlend<RtT>::value || !has_member_BlendOp<RtT>::value ||
                  !has_member_SrcBlendAlpha<RtT>::value || !has_member_DestBlendAlpha<RtT>::value ||
                  !has_member_BlendOpAlpha<RtT>::value) {
      return false;
    } else {
      for (uint32_t i = 0; i < rt_count; ++i) {
        const auto& rt = desc.RenderTarget[i];
        rts[i].blend_enable = rt.BlendEnable ? true : false;
        rts[i].write_mask = static_cast<uint8_t>(rt.RenderTargetWriteMask);
        rts[i].src_blend = static_cast<uint32_t>(rt.SrcBlend);
        rts[i].dest_blend = static_cast<uint32_t>(rt.DestBlend);
        rts[i].blend_op = static_cast<uint32_t>(rt.BlendOp);
        rts[i].src_blend_alpha = static_cast<uint32_t>(rt.SrcBlendAlpha);
        rts[i].dest_blend_alpha = static_cast<uint32_t>(rt.DestBlendAlpha);
        rts[i].blend_op_alpha = static_cast<uint32_t>(rt.BlendOpAlpha);
      }
      return true;
    }
  }

  return false;
}

struct AeroGpuSampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_LINEAR;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
};

struct AeroGpuDevice {
  uint32_t live_cookie = kD3D10DeviceLiveCookie;
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
  std::vector<aerogpu::d3d10_11::WddmSubmitAllocation> wddm_submit_allocation_handles;
  bool wddm_submit_allocation_list_oom = false;
  std::vector<AeroGpuResource*> pending_staging_writes;

  // Cached state.
  uint32_t current_rtv_count = 0;
  aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  AeroGpuDepthStencilState* current_dss = nullptr;
  uint32_t current_stencil_ref = 0;
  AeroGpuRasterizerState* current_rs = nullptr;
  AeroGpuBlendState* current_bs = nullptr;
  float current_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  uint32_t current_sample_mask = kD3DSampleMaskAll;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding gs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t gs_srvs[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_vs_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_ps_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_gs_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* current_vs_cb_resources[kMaxConstantBufferSlots] = {};
  AeroGpuResource* current_ps_cb_resources[kMaxConstantBufferSlots] = {};
  AeroGpuResource* current_gs_cb_resources[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t gs_samplers[kMaxSamplerSlots] = {};

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`).
  AeroGpuResource* current_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* current_dsv_res = nullptr;
  AeroGpuResource* current_vb_res = nullptr;
  AeroGpuResource* current_vb_resources[kMaxVertexBufferSlots] = {};
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

void SetError(D3D10DDI_HDEVICE hDevice, HRESULT hr) noexcept {
  // Called from many hot/error paths, including from `noexcept` exception
  // barriers. Swallow any unexpected C++ exceptions from runtime callbacks.
  try {
    if (!IsDeviceLive(hDevice)) {
      return;
    }
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
  } catch (...) {
  }
}

// -----------------------------------------------------------------------------
// D3D10 WDK DDI exception barrier
// -----------------------------------------------------------------------------
//
// D3D10 DDIs are invoked through function tables filled during OpenAdapter/CreateDevice.
// The runtime expects these callbacks to be "C ABI safe": no C++ exceptions may
// escape. Even though individual entrypoints try to avoid allocations and catch
// OOM in hot paths, defensively wrap every exported DDI entrypoint so a stray
// exception (e.g. std::bad_alloc, std::system_error) cannot unwind into the
// runtime.
template <typename... Args>
inline void ReportExceptionForArgs(HRESULT hr, Args... args) noexcept {
  try {
    if constexpr (sizeof...(Args) == 0) {
      return;
    } else {
      using First = std::tuple_element_t<0, std::tuple<Args...>>;
      if constexpr (std::is_same_v<std::decay_t<First>, D3D10DDI_HDEVICE>) {
        const auto tup = std::forward_as_tuple(args...);
        const auto hDevice = std::get<0>(tup);
        SetError(hDevice, hr);
      }
    }
  } catch (...) {
  }
}

template <auto Impl>
struct aerogpu_d3d10_wdk_ddi_thunk;

template <typename Ret, typename... Args, Ret(APIENTRY* Impl)(Args...)>
struct aerogpu_d3d10_wdk_ddi_thunk<Impl> {
  static Ret APIENTRY thunk(Args... args) noexcept {
    try {
      if constexpr (std::is_void_v<Ret>) {
        Impl(args...);
        return;
      } else {
        return Impl(args...);
      }
    } catch (const std::bad_alloc&) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_OUTOFMEMORY;
      } else if constexpr (std::is_void_v<Ret>) {
        ReportExceptionForArgs(E_OUTOFMEMORY, args...);
        return;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        ReportExceptionForArgs(E_OUTOFMEMORY, args...);
        return sizeof(uint64_t);
      } else {
        return Ret{};
      }
    } catch (...) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_FAIL;
      } else if constexpr (std::is_void_v<Ret>) {
        ReportExceptionForArgs(E_FAIL, args...);
        return;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        ReportExceptionForArgs(E_FAIL, args...);
        return sizeof(uint64_t);
      } else {
        return Ret{};
      }
    }
  }
};

#define AEROGPU_D3D10_WDK_DDI(fn) aerogpu_d3d10_wdk_ddi_thunk<&fn>::thunk

static void TrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res, bool write);
static bool AerogpuFormatIsDepth(uint32_t aerogpu_format);

struct WddmAllocListCheckpoint {
  AeroGpuDevice* dev = nullptr;
  size_t size = 0;
  bool oom = false;

  explicit WddmAllocListCheckpoint(AeroGpuDevice* d) : dev(d) {
    if (!dev) {
      return;
    }
    size = dev->wddm_submit_allocation_handles.size();
    oom = dev->wddm_submit_allocation_list_oom;
  }

  void rollback() const {
    if (!dev) {
      return;
    }
    if (dev->wddm_submit_allocation_handles.size() > size) {
      dev->wddm_submit_allocation_handles.resize(size);
    }
    dev->wddm_submit_allocation_list_oom = oom;
  }
};

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

  // Guest-backed resources are updated by writing directly into the backing
  // allocation and emitting RESOURCE_DIRTY_RANGE. Ensure we can record the dirty
  // range before committing any bytes into the guest allocation (avoid
  // host/guest drift on OOM).
  aerogpu_cmd_resource_dirty_range* dirty_cmd = nullptr;
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  const auto restore_storage_from_allocation = [&]() {
    if (res->storage.empty()) {
      return;
    }
    uint64_t allocation_size = res->wddm_allocation_size_bytes;
    if (allocation_size == 0) {
      allocation_size = static_cast<uint64_t>(res->storage.size());
    }
    const uint64_t end = upload_offset + upload_size;
    if (end < upload_offset) {
      return;
    }
    if (end > allocation_size) {
      return;
    }
    if (upload_offset > static_cast<uint64_t>(SIZE_MAX) || upload_size > static_cast<uint64_t>(SIZE_MAX)) {
      return;
    }
    if (upload_offset > static_cast<uint64_t>(res->storage.size())) {
      return;
    }
    const size_t remaining = res->storage.size() - static_cast<size_t>(upload_offset);
    if (upload_size > static_cast<uint64_t>(remaining)) {
      return;
    }
    const size_t off_restore = static_cast<size_t>(upload_offset);
    const size_t sz_restore = static_cast<size_t>(upload_size);
    std::memcpy(res->storage.data() + off_restore, static_cast<const uint8_t*>(lock_args.pData) + off_restore, sz_restore);
  };

  HRESULT copy_hr = S_OK;
  uint64_t dirty_offset = upload_offset;
  uint64_t dirty_end = upload_offset + upload_size;

  if (dev->wddm_submit_allocation_list_oom) {
    restore_storage_from_allocation();
    dev->cmd.rollback(cmd_checkpoint);
    alloc_checkpoint.rollback();
    SetError(hDevice, E_OUTOFMEMORY);
    copy_hr = E_OUTOFMEMORY;
    goto Unlock;
  }

  TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  if (dev->wddm_submit_allocation_list_oom) {
    restore_storage_from_allocation();
    dev->cmd.rollback(cmd_checkpoint);
    alloc_checkpoint.rollback();
    copy_hr = E_OUTOFMEMORY;
    goto Unlock;
  }

  dirty_cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!dirty_cmd) {
    restore_storage_from_allocation();
    dev->cmd.rollback(cmd_checkpoint);
    alloc_checkpoint.rollback();
    SetError(hDevice, E_OUTOFMEMORY);
    copy_hr = E_OUTOFMEMORY;
    goto Unlock;
  }
  dirty_cmd->resource_handle = res->handle;
  dirty_cmd->reserved0 = 0;
  // Initialize to the requested upload range; this is refined after the copy
  // completes (Texture2D uploads may expand to cover row padding).
  dirty_cmd->offset_bytes = upload_offset;
  dirty_cmd->size_bytes = upload_size;

  if (dirty_end < upload_offset) {
    copy_hr = E_INVALIDARG;
    goto Unlock;
  }

  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }
    if (res->tex2d_subresources.empty()) {
      copy_hr = E_FAIL;
      goto Unlock;
    }

    uint64_t allocation_size = res->wddm_allocation_size_bytes;
    if (allocation_size == 0) {
      allocation_size = static_cast<uint64_t>(res->storage.size());
    }
    if (allocation_size == 0) {
      copy_hr = E_FAIL;
      goto Unlock;
    }

    // For safety, ensure we can address the highest byte we might touch using size_t pointer arithmetic.
    if (allocation_size > static_cast<uint64_t>(SIZE_MAX)) {
      copy_hr = E_OUTOFMEMORY;
      goto Unlock;
    }

    const uint32_t lock_pitch = aerogpu_lock_pitch_bytes(lock_args);

    uint64_t span_start = UINT64_MAX;
    uint64_t span_end = 0;
    uint8_t* dst_base = static_cast<uint8_t*>(lock_args.pData);
    const uint8_t* src_base = res->storage.data();
    const size_t storage_size = res->storage.size();
    const uint64_t storage_size_u64 = static_cast<uint64_t>(storage_size);

    const uint64_t upload_end = dirty_end;
    const bool full_resource_upload = (upload_offset == 0 && upload_size == res->storage.size());
    size_t exact_index = SIZE_MAX;
    if (!full_resource_upload) {
      for (size_t i = 0; i < res->tex2d_subresources.size(); ++i) {
        const Texture2DSubresourceLayout& sub = res->tex2d_subresources[i];
        if (sub.offset_bytes == upload_offset && sub.size_bytes == upload_size) {
          exact_index = i;
          break;
        }
      }
    }

    bool copied_any = false;

    for (size_t i = 0; i < res->tex2d_subresources.size(); ++i) {
      if (full_resource_upload) {
        // Copy all subresources.
      } else if (exact_index != SIZE_MAX) {
        if (i != exact_index) {
          continue;
        }
      } else {
        const Texture2DSubresourceLayout& sub = res->tex2d_subresources[i];
        const uint64_t sub_start = sub.offset_bytes;
        const uint64_t sub_end = sub.offset_bytes + sub.size_bytes;
        if (sub_end < sub_start) {
          copy_hr = E_FAIL;
          break;
        }
        if (upload_end <= sub_start || upload_offset >= sub_end) {
          continue;
        }
      }

      const Texture2DSubresourceLayout& sub = res->tex2d_subresources[i];
      const uint32_t expected_pitch = sub.row_pitch_bytes;
      // Guest-backed textures are interpreted by the host using the protocol
      // pitch (CREATE_TEXTURE2D.row_pitch_bytes). Ignore the runtime's LockCb
      // pitch here so the bytes written into the guest allocation match what the
      // host expects.
      if (sub.mip_level == 0) {
        LogLockPitchMismatchMaybe(res->dxgi_format, static_cast<uint32_t>(i), sub, expected_pitch, lock_pitch);
      }
      const uint32_t dst_pitch = expected_pitch;

      uint32_t row_bytes = 0;
      if (!ValidateTexture2DRowSpan(aer_fmt, sub, dst_pitch, allocation_size, &row_bytes)) {
        copy_hr = E_INVALIDARG;
        break;
      }
      if (expected_pitch < row_bytes) {
        copy_hr = E_INVALIDARG;
        break;
      }

      bool can_clear_padding = false;
      uint64_t full_row_end = 0;
      if (dst_pitch > row_bytes) {
        full_row_end =
            sub.offset_bytes +
            static_cast<uint64_t>(sub.rows_in_layout - 1u) * static_cast<uint64_t>(dst_pitch) +
            static_cast<uint64_t>(dst_pitch);
        can_clear_padding = (full_row_end >= sub.offset_bytes) &&
                            (full_row_end <= allocation_size) &&
                            (full_row_end <= static_cast<uint64_t>(SIZE_MAX));
      }

      for (uint32_t y = 0; y < sub.rows_in_layout; ++y) {
        const uint64_t src_off_u64 =
            sub.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(expected_pitch);
        const uint64_t dst_off_u64 =
            sub.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(dst_pitch);
        if (src_off_u64 > storage_size_u64 || src_off_u64 + row_bytes > storage_size_u64) {
          copy_hr = E_FAIL;
          break;
        }
        if (dst_off_u64 > allocation_size || dst_off_u64 + row_bytes > allocation_size) {
          copy_hr = E_FAIL;
          break;
        }
        if (src_off_u64 > static_cast<uint64_t>(SIZE_MAX) || dst_off_u64 > static_cast<uint64_t>(SIZE_MAX)) {
          copy_hr = E_OUTOFMEMORY;
          break;
        }
        const size_t src_off = static_cast<size_t>(src_off_u64);
        const size_t dst_off = static_cast<size_t>(dst_off_u64);
        std::memcpy(dst_base + dst_off, src_base + src_off, row_bytes);
        if (can_clear_padding) {
          std::memset(dst_base + dst_off + row_bytes, 0, dst_pitch - row_bytes);
        }
      }
      if (FAILED(copy_hr)) {
        break;
      }

      uint64_t sub_end_u64 = 0;
      if (can_clear_padding && full_row_end) {
        sub_end_u64 = full_row_end;
      } else {
        const uint64_t rows_minus_one = static_cast<uint64_t>(sub.rows_in_layout - 1u);
        const uint64_t pitch_u64 = static_cast<uint64_t>(dst_pitch);
        const uint64_t row_bytes_u64 = static_cast<uint64_t>(row_bytes);
        const uint64_t step = rows_minus_one * pitch_u64;
        if (rows_minus_one != 0 && step / pitch_u64 != rows_minus_one) {
          copy_hr = E_FAIL;
          break;
        }
        const uint64_t sub_size = step + row_bytes_u64;
        if (sub_size < step) {
          copy_hr = E_FAIL;
          break;
        }
        sub_end_u64 = sub.offset_bytes + sub_size;
      }
      if (sub_end_u64 < sub.offset_bytes || sub_end_u64 > allocation_size) {
        copy_hr = E_FAIL;
        break;
      }

      span_start = std::min(span_start, sub.offset_bytes);
      span_end = std::max(span_end, sub_end_u64);
      copied_any = true;
    }

    if (FAILED(copy_hr)) {
      goto Unlock;
    }
    if (!copied_any) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }
    if (span_start != UINT64_MAX) {
      dirty_offset = span_start;
      dirty_end = span_end;
      if (dirty_end < dirty_offset) {
        copy_hr = E_FAIL;
        goto Unlock;
      }
    }
  } else {
    // For all other cases (including multi-mip/array textures and partial uploads), the
    // allocation is treated as a packed linear blob whose layout matches `res->storage`.
    // This is only safe when the runtime Pitch matches the driver's expected mip0 pitch
    // (validated above).
    std::memcpy(static_cast<uint8_t*>(lock_args.pData) + off, res->storage.data() + off, sz);
  }

Unlock:
  if (FAILED(copy_hr)) {
    // Best-effort rollback for failed uploads: keep the shadow copy in sync with
    // the guest allocation bytes we actually have.
    restore_storage_from_allocation();
  }
  D3DDDICB_UNLOCK unlock_args = {};
  unlock_args.hAllocation = lock_args.hAllocation;
  InitUnlockForWrite(&unlock_args);
  hr = CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
  if (FAILED(hr)) {
    SetError(hDevice, hr);
    return;
  }
  if (FAILED(copy_hr)) {
    SetError(hDevice, copy_hr);
    return;
  }

  if (!dirty_cmd) {
    // Should not happen: we always emit RESOURCE_DIRTY_RANGE before writing into
    // the guest allocation.
    SetError(hDevice, E_FAIL);
    return;
  }
  // Host-side executors validate RESOURCE_DIRTY_RANGE against the protocol-visible
  // resource size (CREATE_TEXTURE2D layouts), not the raw WDDM allocation size.
  // When the Win7 runtime reports a larger Pitch than our protocol layout, the
  // "last row span" in the guest allocation can exceed `res->storage.size()`;
  // clamp so the dirty range is always within protocol bounds.
  const uint64_t protocol_size = static_cast<uint64_t>(res->storage.size());
  uint64_t clamped_offset = dirty_offset;
  uint64_t clamped_end = dirty_end;
  if (clamped_offset > protocol_size) {
    SetError(hDevice, E_INVALIDARG);
    // Still emit a well-formed no-op packet to keep the command stream valid.
    clamped_offset = 0;
    clamped_end = 0;
  } else {
    clamped_end = std::min(clamped_end, protocol_size);
    if (clamped_end < clamped_offset) {
      SetError(hDevice, E_FAIL);
      // Still emit a well-formed no-op packet to keep the command stream valid.
      clamped_offset = 0;
      clamped_end = 0;
    }
  }
  dirty_cmd->offset_bytes = clamped_offset;
  dirty_cmd->size_bytes = clamped_end - clamped_offset;
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
template <typename TFnPtr>
struct DdiStub;

static void ReportNotImpl(D3D10DDI_HDEVICE hDevice) {
  SetError(hDevice, E_NOTIMPL);
}

inline void ReportNotImpl() {}

template <typename Handle0, typename... Rest>
inline void ReportNotImpl(Handle0 handle0, Rest...) {
  using H0 = std::decay_t<Handle0>;
  if constexpr (std::is_same_v<H0, D3D10DDI_HDEVICE>) {
    ReportNotImpl(handle0);
  }
}

template <typename Ret, typename... Args>
struct DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    ((void)args, ...);
    if constexpr (std::is_same_v<Ret, void>) {
      ReportNotImpl(args...);
      return;
    } else if constexpr (std::is_same_v<Ret, HRESULT>) {
      return E_NOTIMPL;
    } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
      // Returning 0 from a CalcPrivate*Size hook often causes the runtime to pass
      // a NULL pDrvPrivate (which then tends to crash on later Create/Destroy
      // probes). Return a small non-zero placeholder so stubs are always safe.
      return sizeof(uint64_t);
    } else {
      return Ret{};
    }
  }
};

template <typename TFnPtr>
struct DdiNoopStub;

template <typename Ret, typename... Args>
struct DdiNoopStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    ((void)args, ...);
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

template <typename FnPtr>
struct SoSetTargetsImpl;

template <typename... Args>
struct SoSetTargetsImpl<void(AEROGPU_APIENTRY*)(Args...)> {
  static void AEROGPU_APIENTRY Call(Args... args) {
    ((void)args, ...);
  }
};

// Stream-output is unsupported for bring-up. Treat unbind (all-null handles) as a no-op but report
// E_NOTIMPL if an app attempts to bind real targets.
template <typename TargetsPtr, typename... Tail>
struct SoSetTargetsImpl<void(AEROGPU_APIENTRY*)(D3D10DDI_HDEVICE, UINT, TargetsPtr, Tail...)> {
  static void AEROGPU_APIENTRY Call(D3D10DDI_HDEVICE hDevice, UINT num_targets, TargetsPtr phTargets, Tail... tail) {
    ((void)tail, ...);
    if (!hDevice.pDrvPrivate || !AnyNonNullHandles(phTargets, num_targets)) {
      return;
    }
    SetError(hDevice, E_NOTIMPL);
  }
};

// Full `D3D10DDI_DEVICEFUNCS` surface (104 function pointers in Win7-era WDKs).
#define AEROGPU_D3D10_DEVICEFUNCS_FIELDS(X)      \
  X(pfnBegin)                                    \
  X(pfnCalcPrivateBlendStateSize)                \
  X(pfnCalcPrivateCounterSize)                   \
  X(pfnCalcPrivateDepthStencilStateSize)         \
  X(pfnCalcPrivateDepthStencilViewSize)          \
  X(pfnCalcPrivateElementLayoutSize)             \
  X(pfnCalcPrivateGeometryShaderSize)            \
  X(pfnCalcPrivateGeometryShaderWithStreamOutputSize) \
  X(pfnCalcPrivatePixelShaderSize)               \
  X(pfnCalcPrivatePredicateSize)                 \
  X(pfnCalcPrivateQuerySize)                     \
  X(pfnCalcPrivateRasterizerStateSize)           \
  X(pfnCalcPrivateRenderTargetViewSize)          \
  X(pfnCalcPrivateResourceSize)                  \
  X(pfnCalcPrivateSamplerSize)                   \
  X(pfnCalcPrivateShaderResourceViewSize)        \
  X(pfnCalcPrivateVertexShaderSize)              \
  X(pfnClearDepthStencilView)                    \
  X(pfnClearRenderTargetView)                    \
  X(pfnClearState)                               \
  X(pfnCopyResource)                             \
  X(pfnCopySubresourceRegion)                    \
  X(pfnCreateBlendState)                         \
  X(pfnCreateCounter)                            \
  X(pfnCreateDepthStencilState)                  \
  X(pfnCreateDepthStencilView)                   \
  X(pfnCreateElementLayout)                      \
  X(pfnCreateGeometryShader)                     \
  X(pfnCreateGeometryShaderWithStreamOutput)     \
  X(pfnCreatePixelShader)                        \
  X(pfnCreatePredicate)                          \
  X(pfnCreateQuery)                              \
  X(pfnCreateRasterizerState)                    \
  X(pfnCreateRenderTargetView)                   \
  X(pfnCreateResource)                           \
  X(pfnCreateSampler)                            \
  X(pfnCreateShaderResourceView)                 \
  X(pfnCreateVertexShader)                       \
  X(pfnDestroyBlendState)                        \
  X(pfnDestroyCounter)                           \
  X(pfnDestroyDepthStencilState)                 \
  X(pfnDestroyDepthStencilView)                  \
  X(pfnDestroyDevice)                            \
  X(pfnDestroyElementLayout)                     \
  X(pfnDestroyGeometryShader)                    \
  X(pfnDestroyPixelShader)                       \
  X(pfnDestroyPredicate)                         \
  X(pfnDestroyQuery)                             \
  X(pfnDestroyRasterizerState)                   \
  X(pfnDestroyRenderTargetView)                  \
  X(pfnDestroyResource)                          \
  X(pfnDestroySampler)                           \
  X(pfnDestroyShaderResourceView)                \
  X(pfnDestroyVertexShader)                      \
  X(pfnDraw)                                     \
  X(pfnDrawAuto)                                 \
  X(pfnDrawIndexed)                              \
  X(pfnDrawIndexedInstanced)                     \
  X(pfnDrawInstanced)                            \
  X(pfnDynamicConstantBufferMapDiscard)          \
  X(pfnDynamicConstantBufferUnmap)               \
  X(pfnDynamicIABufferMapDiscard)                \
  X(pfnDynamicIABufferMapNoOverwrite)            \
  X(pfnDynamicIABufferUnmap)                     \
  X(pfnEnd)                                      \
  X(pfnFlush)                                    \
  X(pfnGenMips)                                  \
  X(pfnGenerateMips)                             \
  X(pfnGsSetConstantBuffers)                     \
  X(pfnGsSetSamplers)                            \
  X(pfnGsSetShader)                              \
  X(pfnGsSetShaderResources)                     \
  X(pfnIaSetIndexBuffer)                         \
  X(pfnIaSetInputLayout)                         \
  X(pfnIaSetTopology)                            \
  X(pfnIaSetVertexBuffers)                       \
  X(pfnMap)                                      \
  X(pfnOpenResource)                             \
  X(pfnPresent)                                  \
  X(pfnPsSetConstantBuffers)                     \
  X(pfnPsSetSamplers)                            \
  X(pfnPsSetShader)                              \
  X(pfnPsSetShaderResources)                     \
  X(pfnReadFromSubresource)                      \
  X(pfnResolveSubresource)                       \
  X(pfnRotateResourceIdentities)                 \
  X(pfnSetBlendState)                            \
  X(pfnSetDepthStencilState)                     \
  X(pfnSetPredication)                           \
  X(pfnSetRasterizerState)                       \
  X(pfnSetRenderTargets)                         \
  X(pfnSetScissorRects)                          \
  X(pfnSetTextFilterSize)                        \
  X(pfnSetViewports)                             \
  X(pfnSoSetTargets)                             \
  X(pfnStagingResourceMap)                       \
  X(pfnStagingResourceUnmap)                     \
  X(pfnUnmap)                                    \
  X(pfnUpdateSubresourceUP)                      \
  X(pfnVsSetConstantBuffers)                     \
  X(pfnVsSetSamplers)                            \
  X(pfnVsSetShader)                              \
  X(pfnVsSetShaderResources)                     \
  X(pfnWriteToSubresource)

#define AEROGPU_D3D10_DEVICEFUNCS_NOOP_FIELDS(X) \
  X(pfnDestroyDevice)                            \
  X(pfnDestroyResource)                          \
  X(pfnDestroyShaderResourceView)                \
  X(pfnDestroyRenderTargetView)                  \
  X(pfnDestroyDepthStencilView)                  \
  X(pfnDestroyVertexShader)                      \
  X(pfnDestroyPixelShader)                       \
  X(pfnDestroyGeometryShader)                    \
  X(pfnDestroyElementLayout)                     \
  X(pfnDestroySampler)                           \
  X(pfnDestroyBlendState)                        \
  X(pfnDestroyRasterizerState)                   \
  X(pfnDestroyDepthStencilState)                 \
  X(pfnDestroyQuery)                             \
  X(pfnDestroyPredicate)                         \
  X(pfnDestroyCounter)                           \
  X(pfnSoSetTargets)                             \
  X(pfnSetPredication)                           \
  X(pfnSetTextFilterSize)                        \
  X(pfnGenMips)                                  \
  X(pfnGenerateMips)                             \
  X(pfnClearState)                               \
  X(pfnFlush)

#define AEROGPU_D3D10_ADAPTERFUNCS_FIELDS(X) \
  X(pfnGetCaps)                              \
  X(pfnCalcPrivateDeviceSize)                \
  X(pfnCreateDevice)                         \
  X(pfnCloseAdapter)

static void InitDeviceFuncsWithStubs(D3D10DDI_DEVICEFUNCS* out) {
  if (!out) {
    return;
  }

  std::memset(out, 0, sizeof(*out));

#define AEROGPU_D3D10_ASSIGN_DEVICE_STUB(field) \
  __if_exists(D3D10DDI_DEVICEFUNCS::field) { out->field = &DdiStub<decltype(out->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_FIELDS(AEROGPU_D3D10_ASSIGN_DEVICE_STUB)
#undef AEROGPU_D3D10_ASSIGN_DEVICE_STUB

#define AEROGPU_D3D10_ASSIGN_DEVICE_NOOP(field) \
  __if_exists(D3D10DDI_DEVICEFUNCS::field) { out->field = &DdiNoopStub<decltype(out->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_NOOP_FIELDS(AEROGPU_D3D10_ASSIGN_DEVICE_NOOP)
#undef AEROGPU_D3D10_ASSIGN_DEVICE_NOOP
}

static void InitAdapterFuncsWithStubs(D3D10DDI_ADAPTERFUNCS* out) {
  if (!out) {
    return;
  }
  std::memset(out, 0, sizeof(*out));
#define AEROGPU_D3D10_ASSIGN_ADAPTER_STUB(field) \
  __if_exists(D3D10DDI_ADAPTERFUNCS::field) { out->field = &DdiStub<decltype(out->field)>::Call; }
  AEROGPU_D3D10_ADAPTERFUNCS_FIELDS(AEROGPU_D3D10_ASSIGN_ADAPTER_STUB)
#undef AEROGPU_D3D10_ASSIGN_ADAPTER_STUB
}
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
  if (dev->wddm_submit_allocation_list_oom) {
    // If we failed to grow the allocation list, submitting the command stream is unsafe
    // because the KMD may not be able to resolve `backing_alloc_id` references.
    if (out_hr) {
      *out_hr = E_OUTOFMEMORY;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }
  if (!dev->adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }

  dev->cmd.finalize();
  const size_t submit_bytes = dev->cmd.size();

  uint64_t fence = 0;
  const auto* allocs =
      dev->wddm_submit_allocation_handles.empty() ? nullptr : dev->wddm_submit_allocation_handles.data();
  const uint32_t alloc_count = static_cast<uint32_t>(dev->wddm_submit_allocation_handles.size());
  const HRESULT hr =
      dev->wddm_submit.SubmitAeroCmdStream(dev->cmd.data(), dev->cmd.size(), want_present, allocs, alloc_count, &fence);
  dev->cmd.reset();
  dev->wddm_submit_allocation_handles.clear();
  dev->wddm_submit_allocation_list_oom = false;
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

template <typename SetErrorFn>
static void TrackStagingWriteLocked(AeroGpuDevice* dev, AeroGpuResource* dst, SetErrorFn&& set_error) {
  if (!dev || !dst) {
    return;
  }

  // Track writes into staging readback resources so Map(READ)/DO_NOT_WAIT can
  // wait on the fence that actually produces the bytes.
  if (dst->usage != 0) {
    if (dst->usage != kD3D10UsageStaging) {
      return;
    }
  } else {
    // Older paths may not capture Usage; fall back to the bind-flags heuristic.
    if (dst->bind_flags != 0) {
      return;
    }
  }

  // Prefer to only track CPU-readable staging resources, but fall back to
  // tracking all bindless resources if CPU access flags were not captured.
  if (dst->cpu_access_flags != 0 && (dst->cpu_access_flags & kD3D10CpuAccessRead) == 0) {
    return;
  }

  auto& tracked = dev->pending_staging_writes;
  if (std::find(tracked.begin(), tracked.end(), dst) != tracked.end()) {
    return;
  }

  try {
    tracked.push_back(dst);
  } catch (...) {
    // If we cannot record the staging write due to OOM, fall back to an
    // immediate submission so we can still stamp the staging fence without
    // needing to grow `pending_staging_writes`.
    HRESULT submit_hr = S_OK;
    const uint64_t fence = submit_locked(dev, /*want_present=*/false, &submit_hr);
    if (FAILED(submit_hr)) {
      set_error(submit_hr);
      return;
    }
    if (fence != 0) {
      dst->last_gpu_write_fence = fence;
    }
  }
}

static void TrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res, bool write) {
  D3D10DDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = dev;
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(
      dev, res, write, [hDevice](HRESULT hr) { SetError(hDevice, hr); });
}

// Best-effort allocation-list tracking used by optional "fast path" packets.
//
// Unlike `TrackWddmAllocForSubmitLocked`, this does not set the global
// `wddm_submit_allocation_list_oom` poison flag or call SetError on OOM: callers
// must skip emitting any packet that would reference `res` if this returns false.
static bool TryTrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res, bool write) {
  if (!dev || !res) {
    return false;
  }
  if (dev->wddm_submit_allocation_list_oom) {
    return false;
  }
  if (res->backing_alloc_id == 0 || res->wddm_allocation_handle == 0) {
    return true;
  }

  const uint32_t handle = res->wddm_allocation_handle;
  for (auto& entry : dev->wddm_submit_allocation_handles) {
    if (entry.allocation_handle == handle) {
      if (write) {
        entry.write = 1;
      }
      return true;
    }
  }

  aerogpu::d3d10_11::WddmSubmitAllocation entry{};
  entry.allocation_handle = handle;
  entry.write = write ? 1 : 0;
  try {
    dev->wddm_submit_allocation_handles.push_back(entry);
  } catch (...) {
    return false;
  }
  return true;
}

static void TrackBoundTargetsForSubmitLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  // Render targets / depth-stencil are written by Draw/Clear.
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    TrackWddmAllocForSubmitLocked(dev, dev->current_rtv_resources[i], /*write=*/true);
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_dsv_res, /*write=*/true);
}

static void TrackDrawStateLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }
  TrackBoundTargetsForSubmitLocked(dev);
  // IA buffers are read by Draw/DrawIndexed.
  for (AeroGpuResource* res : dev->current_vb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_ib_res, /*write=*/false);
  for (AeroGpuResource* res : dev->current_vs_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_ps_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_gs_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_vs_srv_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_ps_srv_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_gs_srv_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
}

static bool SetTextureLocked(AeroGpuDevice* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  if (!dev) {
    return false;
  }
  return aerogpu::d3d10_11::EmitSetTextureCmdLocked(dev, shader_stage, slot, texture, [&](HRESULT hr) {
    D3D10DDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = dev;
    SetError(hDevice, hr);
  });
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
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_srvs;
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
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_samplers;
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
    case AEROGPU_SHADER_STAGE_GEOMETRY:
      return dev->gs_constant_buffers;
    default:
      return nullptr;
  }
}

static bool SetShaderResourceSlotLocked(AeroGpuDevice* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  if (!dev || slot >= kMaxShaderResourceSlots) {
    return true;
  }
  aerogpu_handle_t* table = ShaderResourceTableForStage(dev, shader_stage);
  if (!table) {
    return true;
  }
  if (table[slot] == texture) {
    return true;
  }
  if (!SetTextureLocked(dev, shader_stage, slot, texture)) {
    return false;
  }
  table[slot] = texture;
  return true;
}

static void UnbindResourceFromSrvsLocked(AeroGpuDevice* dev, aerogpu_handle_t handle, const AeroGpuResource* res) {
  if (!dev || (handle == 0 && !res)) {
    return;
  }
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if ((handle != 0 && dev->vs_srvs[slot] == handle) ||
        (res && ResourcesAlias(dev->current_vs_srv_resources[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0);
      if (dev->vs_srvs[slot] == 0) {
        dev->current_vs_srv_resources[slot] = nullptr;
      }
    }
    if ((handle != 0 && dev->ps_srvs[slot] == handle) ||
        (res && ResourcesAlias(dev->current_ps_srv_resources[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0);
      if (dev->ps_srvs[slot] == 0) {
        dev->current_ps_srv_resources[slot] = nullptr;
      }
    }
    if ((handle != 0 && dev->gs_srvs[slot] == handle) ||
        (res && ResourcesAlias(dev->current_gs_srv_resources[slot], res))) {
      SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 0);
      if (dev->gs_srvs[slot] == 0) {
        dev->current_gs_srv_resources[slot] = nullptr;
      }
    }
  }
}

static void UnbindResourceFromSrvsLocked(AeroGpuDevice* dev, const AeroGpuResource* resource) {
  UnbindResourceFromSrvsLocked(dev, /*handle=*/0, resource);
}

static void UnbindResourceFromSrvsLocked(AeroGpuDevice* dev, aerogpu_handle_t resource) {
  if (!dev || !resource) {
    return;
  }
  UnbindResourceFromSrvsLocked(dev, resource, nullptr);
}

static bool UnbindResourceFromOutputsLocked(AeroGpuDevice* dev, aerogpu_handle_t handle, const AeroGpuResource* res) {
  D3D10DDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = dev;
  return aerogpu::d3d10_11::UnbindResourceFromOutputsLocked(
      dev, handle, res, [hDevice](HRESULT hr) { SetError(hDevice, hr); });
}

static bool UnbindResourceFromOutputsLocked(AeroGpuDevice* dev, const AeroGpuResource* resource) {
  return UnbindResourceFromOutputsLocked(dev, /*handle=*/0, resource);
}

static bool UnbindResourceFromOutputsLocked(AeroGpuDevice* dev, aerogpu_handle_t resource) {
  return UnbindResourceFromOutputsLocked(dev, resource, nullptr);
}


// -----------------------------------------------------------------------------
// Device DDI (core bring-up set)
// -----------------------------------------------------------------------------

void APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  void* device_mem = hDevice.pDrvPrivate;
  if (!HasLiveCookie(device_mem, kD3D10DeviceLiveCookie)) {
    return;
  }
  uint32_t cookie = 0;
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
  if (!hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the resource object so DestroyResource is safe even if the
  // runtime calls it after a failed CreateResource.
  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();

  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(res);
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

  res->handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
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

    constexpr size_t kInlineKmtAllocs = 16;
    std::array<D3DKMT_HANDLE, kInlineKmtAllocs> km_allocs_stack{};
    std::vector<D3DKMT_HANDLE> km_allocs_heap;
    D3DKMT_HANDLE* km_allocs = nullptr;
    UINT km_alloc_count = 0;

    const size_t handle_count = res->wddm.km_allocation_handles.size();
    if (handle_count != 0) {
      if (handle_count <= km_allocs_stack.size()) {
        for (size_t i = 0; i < handle_count; ++i) {
          km_allocs_stack[i] = static_cast<D3DKMT_HANDLE>(res->wddm.km_allocation_handles[i]);
        }
        km_allocs = km_allocs_stack.data();
        km_alloc_count = static_cast<UINT>(handle_count);
      } else {
        try {
          km_allocs_heap.reserve(handle_count);
          for (uint64_t h : res->wddm.km_allocation_handles) {
            km_allocs_heap.push_back(static_cast<D3DKMT_HANDLE>(h));
          }
          km_allocs = km_allocs_heap.data();
          km_alloc_count = static_cast<UINT>(km_allocs_heap.size());
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
          km_allocs = nullptr;
          km_alloc_count = 0;
        }
      }
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = km_alloc_count;
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_alloc_count ? km_allocs : nullptr;
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_alloc_count ? km_allocs : nullptr;
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
                                uint32_t pitch_bytes,
                                aerogpu_wddm_alloc_priv_v2* out_priv) -> HRESULT {
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
      alloc_id = static_cast<uint32_t>(aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter)) & AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
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
      if (static_cast<uint32_t>(pDesc->Usage) == kD3D10UsageStaging) {
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
    if (out_priv) {
      *out_priv = priv_out;
    }
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
    const uint64_t size_from_alloc = static_cast<uint64_t>(alloc_info[0].Size);
    const uint64_t size_from_priv = have_priv_out ? static_cast<uint64_t>(priv_out.size_bytes) : 0ull;
    res->wddm_allocation_size_bytes = std::max(size_from_alloc, size_from_priv);
    res->wddm.km_allocation_handles.clear();
    try {
      res->wddm.km_allocation_handles.push_back(km_alloc);
    } catch (...) {
      // Ensure we don't leak the just-allocated KM resource/allocation if the UMD
      // cannot record its handles due to OOM.
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
      res->wddm.km_allocation_handles.clear();
      res->wddm.km_resource_handle = 0;
      res->wddm_allocation_handle = 0;
      return E_OUTOFMEMORY;
    }
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
      is_staging = (static_cast<uint32_t>(pDesc->Usage) == kD3D10UsageStaging);
      cpu_visible = cpu_visible || is_staging;
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
    is_shared = (res->misc_flags & kD3D10ResourceMiscShared) != 0;
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      want_host_owned = (usage == kD3D10UsageDynamic);
    }
    want_host_owned = want_host_owned && !is_shared;

    HRESULT hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0, nullptr);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      ResetObject(res);
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
      ResetObject(res);
      return init_hr;
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created buffer handle=%u alloc_id=%u size=%llu",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->backing_alloc_id),
                         static_cast<unsigned long long>(res->size_bytes));
#endif
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      SetError(hDevice, E_OUTOFMEMORY);
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_buffer_usage_flags(res->bind_flags);
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
        ResetObject(res);
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
        ResetObject(res);
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
        ResetObject(res);
        return submit_hr;
      }
    }
    return S_OK;
  }

  if (dim == 3u /* texture2d */) {
    const uint32_t aer_fmt =
        aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(res);
      return E_NOTIMPL;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      ResetObject(res);
      return E_NOTIMPL;
    }

    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : aerogpu::d3d10_11::CalcFullMipLevels(res->width, res->height);
    res->array_size = pDesc->ArraySize;
    __if_exists(D3D10DDIARG_CREATERESOURCE::SampleDesc) {
      res->sample_count = static_cast<uint32_t>(pDesc->SampleDesc.Count);
      res->sample_quality = static_cast<uint32_t>(pDesc->SampleDesc.Quality);
    }
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);

    if (res->width == 0 || res->height == 0) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting Texture2D with invalid dimensions %ux%u (handle=%u)",
                           static_cast<unsigned>(res->width),
                           static_cast<unsigned>(res->height),
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }

    if (res->array_size == 0) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting Texture2D with invalid ArraySize=0 (handle=%u)",
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }

    const uint32_t max_mips = aerogpu::d3d10_11::CalcFullMipLevels(res->width, res->height);
    if (res->mip_levels == 0 || res->mip_levels > max_mips) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting Texture2D with invalid mip_levels=%u (max=%u handle=%u)",
                           static_cast<unsigned>(res->mip_levels),
                           static_cast<unsigned>(max_mips),
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }

    // Validate bind flags against format class.
    if (AerogpuFormatIsDepth(aer_fmt) && (res->bind_flags & kD3D10BindRenderTarget) != 0) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateResource: rejecting depth Texture2D with BIND_RENDER_TARGET (bind=0x%08X handle=%u)",
          static_cast<unsigned>(res->bind_flags),
          static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }
    if (!AerogpuFormatIsDepth(aer_fmt) && (res->bind_flags & kD3D10BindDepthStencil) != 0) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateResource: rejecting color Texture2D with BIND_DEPTH_STENCIL (bind=0x%08X handle=%u)",
          static_cast<unsigned>(res->bind_flags),
          static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }

    if (res->sample_count == 0) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting Texture2D with invalid SampleDesc.Count=0 (handle=%u)",
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_INVALIDARG;
    }
    if (res->sample_count != 1 || res->sample_quality != 0) {
      // Multisample resources require MSAA view types and resolve operations
      // that are not yet supported by the AeroGPU D3D10 UMD.
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting MSAA Texture2D SampleDesc=(%u,%u handle=%u)",
                           static_cast<unsigned>(res->sample_count),
                           static_cast<unsigned>(res->sample_quality),
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_NOTIMPL;
    }

    // The current host executor only supports mip_levels==1 for render targets /
    // depth-stencil textures. Fail at CreateResource time so apps get a clean
    // HRESULT instead of later host-side validation errors.
    if ((res->bind_flags & (kD3D10BindRenderTarget | kD3D10BindDepthStencil)) != 0 &&
        res->mip_levels != 1) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateResource: rejecting RT/DS Texture2D with mip_levels=%u (bind=0x%08X handle=%u)",
                           static_cast<unsigned>(res->mip_levels),
                           static_cast<unsigned>(res->bind_flags),
                           static_cast<unsigned>(res->handle));
      ResetObject(res);
      return E_NOTIMPL;
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (row_bytes == 0) {
      ResetObject(res);
      return E_OUTOFMEMORY;
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
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
    bool cpu_visible = false;
    if constexpr (has_CPUAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CPUAccessFlags) != 0);
    }
    if constexpr (has_CpuAccessFlags<D3D10DDIARG_CREATERESOURCE>::value) {
      cpu_visible = cpu_visible || (static_cast<uint32_t>(pDesc->CpuAccessFlags) != 0);
    }
    bool is_staging = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      is_staging = (static_cast<uint32_t>(pDesc->Usage) == kD3D10UsageStaging);
      cpu_visible = cpu_visible || is_staging;
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
    is_shared = (res->misc_flags & kD3D10ResourceMiscShared) != 0;
    res->is_shared = is_shared;
    if (is_shared && (res->mip_levels != 1 || res->array_size != 1)) {
      // Keep shared surface interop conservative: only support the legacy single-subresource layout.
      ResetObject(res);
      return E_NOTIMPL;
    }
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    if constexpr (has_Usage<D3D10DDIARG_CREATERESOURCE>::value) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      want_host_owned = (usage == kD3D10UsageDynamic);
    }
    want_host_owned = want_host_owned && !is_shared;
    // Host-owned Texture2D updates go through `AEROGPU_CMD_UPLOAD_RESOURCE`. The protocol supports
    // arbitrary byte ranges, so host-owned is compatible with mip/array textures as long as uploads
    // are expressed in terms of subresource byte offsets (the Map/Unmap and UpdateSubresourceUP
    // paths upload whole subresources).
    aerogpu_wddm_alloc_priv_v2 alloc_priv = {};
    HRESULT hr =
        allocate_one(total_bytes, cpu_visible, is_rt, is_ds, is_shared, is_primary, res->row_pitch_bytes, &alloc_priv);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
      ResetObject(res);
      return hr;
    }

    if (!want_host_owned) {
      uint32_t alloc_pitch = static_cast<uint32_t>(alloc_priv.row_pitch_bytes);
      if (alloc_pitch == 0 && !AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(alloc_priv.reserved0)) {
        alloc_pitch = static_cast<uint32_t>(alloc_priv.reserved0 & 0xFFFFFFFFu);
      }
      if (alloc_pitch != 0 && alloc_pitch != res->row_pitch_bytes) {
        // If the KMD returns a different pitch (via the private driver data blob),
        // update our internal + protocol-visible layout before uploading any data.
        //
        // This keeps the host's `CREATE_TEXTURE2D.row_pitch_bytes` interpretation in
        // sync with the actual guest backing memory layout and avoids silent row
        // corruption when the Win7 runtime/KMD chooses a different pitch.
        static std::atomic<uint32_t> g_create_tex_pitch_logs{0};
        const uint32_t n = g_create_tex_pitch_logs.fetch_add(1, std::memory_order_relaxed);
        if (n < 32) {
          AEROGPU_D3D10_11_LOG("D3D10 CreateResource: KMD overrode Texture2D pitch %u -> %u",
                               static_cast<unsigned>(res->row_pitch_bytes),
                               static_cast<unsigned>(alloc_pitch));
        } else if (n == 32) {
          AEROGPU_D3D10_11_LOG("D3D10 CreateResource: pitch override log limit reached; suppressing further messages");
        }

        if (alloc_pitch < row_bytes) {
          deallocate_if_needed();
          ResetObject(res);
          return E_INVALIDARG;
        }

        std::vector<Texture2DSubresourceLayout> updated_layouts;
        uint64_t updated_total_bytes = 0;
        if (!build_texture2d_subresource_layouts(aer_fmt,
                                                 res->width,
                                                 res->height,
                                                 res->mip_levels,
                                                 res->array_size,
                                                 alloc_pitch,
                                                 &updated_layouts,
                                                 &updated_total_bytes)) {
          deallocate_if_needed();
          ResetObject(res);
          return E_FAIL;
        }

        const uint64_t backing_size = res->wddm_allocation_size_bytes ? res->wddm_allocation_size_bytes : total_bytes;
        if (updated_total_bytes == 0 ||
            updated_total_bytes > backing_size ||
            updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
          deallocate_if_needed();
          ResetObject(res);
          return E_INVALIDARG;
        }

        res->row_pitch_bytes = alloc_pitch;
        res->tex2d_subresources = std::move(updated_layouts);
        total_bytes = updated_total_bytes;
      }
    }

    if (want_host_owned) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    } else {
      uint64_t backing_size = res->wddm_allocation_size_bytes ? res->wddm_allocation_size_bytes : total_bytes;

      uint32_t alloc_pitch = alloc_priv.row_pitch_bytes;
      if (alloc_pitch == 0 && !AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(alloc_priv.reserved0)) {
        alloc_pitch = static_cast<uint32_t>(alloc_priv.reserved0 & 0xFFFFFFFFu);
      }
      if (alloc_pitch != 0 && alloc_pitch != res->row_pitch_bytes) {
        if (alloc_pitch < row_bytes) {
          SetError(hDevice, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          return E_INVALIDARG;
        }

        std::vector<Texture2DSubresourceLayout> updated_layouts;
        uint64_t updated_total_bytes = 0;
        if (!build_texture2d_subresource_layouts(aer_fmt,
                                                 res->width,
                                                 res->height,
                                                 res->mip_levels,
                                                 res->array_size,
                                                 alloc_pitch,
                                                 &updated_layouts,
                                                 &updated_total_bytes)) {
          SetError(hDevice, E_FAIL);
          deallocate_if_needed();
          ResetObject(res);
          return E_FAIL;
        }
        if (updated_total_bytes == 0 || updated_total_bytes > backing_size ||
            updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
          SetError(hDevice, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          return E_INVALIDARG;
        }
        res->row_pitch_bytes = alloc_pitch;
        res->tex2d_subresources = std::move(updated_layouts);
        total_bytes = updated_total_bytes;
      }

      // Query the runtime/KMD-selected pitch via a LockCb round-trip so our
      // protocol-visible layout matches the actual mapped allocation.
      const D3DDDI_DEVICECALLBACKS* ddi = dev->um_callbacks;
      if (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb && res->wddm_allocation_handle != 0) {
        D3DDDICB_LOCK lock_args = {};
        lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
        __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock_args.SubresourceIndex = 0; }
        __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock_args.SubResourceIndex = 0; }
        InitLockForWrite(&lock_args);

        HRESULT lock_hr = CallCbMaybeHandle(ddi->pfnLockCb, dev->hrt_device, &lock_args);
        if (SUCCEEDED(lock_hr)) {
          if (lock_args.pData) {
            uint32_t lock_pitch = aerogpu_lock_pitch_bytes(lock_args);
            if (lock_pitch != 0 && lock_pitch != res->row_pitch_bytes) {
              if (!res->tex2d_subresources.empty()) {
                LogLockPitchMismatchMaybe(res->dxgi_format,
                                          /*subresource_index=*/0,
                                          res->tex2d_subresources[0],
                                          res->row_pitch_bytes,
                                          lock_pitch);
              }

              if (lock_pitch < row_bytes) {
                D3DDDICB_UNLOCK unlock_args = {};
                unlock_args.hAllocation = lock_args.hAllocation;
                InitUnlockForWrite(&unlock_args);
                (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);

                SetError(hDevice, E_INVALIDARG);
                deallocate_if_needed();
                ResetObject(res);
                return E_INVALIDARG;
              }

              std::vector<Texture2DSubresourceLayout> updated_layouts;
              uint64_t updated_total_bytes = 0;
              if (!build_texture2d_subresource_layouts(aer_fmt,
                                                       res->width,
                                                       res->height,
                                                       res->mip_levels,
                                                       res->array_size,
                                                       lock_pitch,
                                                       &updated_layouts,
                                                       &updated_total_bytes)) {
                D3DDDICB_UNLOCK unlock_args = {};
                unlock_args.hAllocation = lock_args.hAllocation;
                InitUnlockForWrite(&unlock_args);
                (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);

                SetError(hDevice, E_FAIL);
                deallocate_if_needed();
                ResetObject(res);
                return E_FAIL;
              }
              if (updated_total_bytes == 0 || updated_total_bytes > backing_size ||
                  updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
                D3DDDICB_UNLOCK unlock_args = {};
                unlock_args.hAllocation = lock_args.hAllocation;
                InitUnlockForWrite(&unlock_args);
                (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);

                SetError(hDevice, E_INVALIDARG);
                deallocate_if_needed();
                ResetObject(res);
                return E_INVALIDARG;
              }
              res->row_pitch_bytes = lock_pitch;
              res->tex2d_subresources = std::move(updated_layouts);
              total_bytes = updated_total_bytes;
            }
          }

          D3DDDICB_UNLOCK unlock_args = {};
          unlock_args.hAllocation = lock_args.hAllocation;
          InitUnlockForWrite(&unlock_args);
          (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
        }
      }
    }

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
      ResetObject(res);
      return init_hr;
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
    AEROGPU_D3D10_11_LOG("trace_resources:  => created tex2d handle=%u alloc_id=%u size=%ux%u row_pitch=%u",
                         static_cast<unsigned>(res->handle),
                         static_cast<unsigned>(res->backing_alloc_id),
                         static_cast<unsigned>(res->width),
                         static_cast<unsigned>(res->height),
                         static_cast<unsigned>(res->row_pitch_bytes));
#endif

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      SetError(hDevice, E_OUTOFMEMORY);
      deallocate_if_needed();
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
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
      EmitUploadLocked(hDevice, dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        SetError(hDevice, E_FAIL);
        deallocate_if_needed();
        ResetObject(res);
        return E_FAIL;
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        ResetObject(res);
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
        ResetObject(res);
        return submit_hr;
      }
    }
    return S_OK;
  }

  deallocate_if_needed();
  ResetObject(res);
  return E_NOTIMPL;
}

HRESULT APIENTRY OpenResource(D3D10DDI_HDEVICE hDevice,
                              const D3D10DDIARG_OPENRESOURCE* pOpenResource,
                              D3D10DDI_HRESOURCE hResource,
                              D3D10DDI_HRTRESOURCE) {
  if (!hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the resource so DestroyResource is safe even if OpenResource
  // fails early.
  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();

  if (!hDevice.pDrvPrivate || !pOpenResource) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(res);
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

  res->handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
  res->backing_alloc_id = static_cast<uint32_t>(priv.alloc_id);
  res->backing_offset_bytes = 0;
  res->wddm_allocation_handle = 0;
  res->wddm_allocation_size_bytes = static_cast<uint64_t>(priv.size_bytes);
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
  // If the WDK OpenResource struct does not expose a Usage field, fall back to
  // the KMD-provided private flag to preserve staging Map behavior.
  if (priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING) {
    res->usage = kD3D10UsageStaging;
  }

  // Recover the runtime allocation handle (`hAllocation`) for LockCb/UnlockCb
  // and the KM handles needed for pfnDeallocateCb. Field availability varies
  // across WDK vintages, so treat all as optional.
  __if_exists(D3D10DDIARG_OPENRESOURCE::hKMResource) {
    res->wddm.km_resource_handle = static_cast<uint64_t>(pOpenResource->hKMResource);
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::hKMAllocation) {
    try {
      res->wddm.km_allocation_handles.push_back(static_cast<uint64_t>(pOpenResource->hKMAllocation));
    } catch (...) {
      ResetObject(res);
      return E_OUTOFMEMORY;
    }
  }
  __if_exists(D3D10DDIARG_OPENRESOURCE::hAllocation) {
    const uint64_t h = static_cast<uint64_t>(pOpenResource->hAllocation);
    if (h != 0) {
      res->wddm_allocation_handle = static_cast<uint32_t>(h);
      if (res->wddm.km_allocation_handles.empty()) {
        try {
          res->wddm.km_allocation_handles.push_back(h);
        } catch (...) {
          ResetObject(res);
          return E_OUTOFMEMORY;
        }
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
            try {
              res->wddm.km_allocation_handles.push_back(h);
            } catch (...) {
              ResetObject(res);
              return E_OUTOFMEMORY;
            }
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
        try {
          res->wddm.km_allocation_handles.push_back(km_alloc);
        } catch (...) {
          ResetObject(res);
          return E_OUTOFMEMORY;
        }
      }
    }
  }

  // Set the resource description from the preserved private data blob (v2).
  if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_BUFFER) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = static_cast<uint64_t>(priv.size_bytes);
  } else if (priv.kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D) {
    const uint32_t aer_fmt =
        aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(priv.format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      ResetObject(res);
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
        ResetObject(res);
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
      ResetObject(res);
      return E_INVALIDARG;
    }
  } else {
    ResetObject(res);
    return E_INVALIDARG;
  }

  auto* import_cmd =
      dev->cmd.append_fixed<aerogpu_cmd_import_shared_surface>(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!import_cmd) {
    ResetObject(res);
    return E_OUTOFMEMORY;
  }
  import_cmd->out_resource_handle = res->handle;
  import_cmd->reserved0 = 0;
  import_cmd->share_token = res->share_token;
  return S_OK;
}

void APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!res) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(res);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    ResetObject(res);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!dev->pending_staging_writes.empty()) {
    dev->pending_staging_writes.erase(
        std::remove(dev->pending_staging_writes.begin(), dev->pending_staging_writes.end(), res),
        dev->pending_staging_writes.end());
  }

  bool oom = false;
  if (res->mapped) {
    if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
      const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
      if (cb && cb->pfnUnlockCb) {
        D3DDDICB_UNLOCK unlock_cb = {};
        unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(res->mapped_wddm_allocation);
        const uint32_t unlock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : res->mapped_subresource;
        InitUnlockArgsForMap(&unlock_cb, unlock_subresource);
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
    // Drop cached bindings that point at the resource being destroyed. Even if we
    // fail to append the corresponding unbind packets (OOM), we must not keep
    // dangling pointers to a freed AeroGpuResource.
    bool rt_state_changed = false;
    for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      if (dev->current_rtv_resources[i] == res ||
          dev->current_rtvs[i] == res->handle ||
          ResourcesAlias(dev->current_rtv_resources[i], res)) {
        dev->current_rtv_resources[i] = nullptr;
        dev->current_rtvs[i] = 0;
        rt_state_changed = true;
      }
    }
    if (dev->current_dsv_res == res ||
        dev->current_dsv == res->handle ||
        ResourcesAlias(dev->current_dsv_res, res)) {
      dev->current_dsv_res = nullptr;
      dev->current_dsv = 0;
      rt_state_changed = true;
    }
    if (rt_state_changed && !oom) {
      if (!EmitSetRenderTargetsCmdLocked(dev,
                                         dev->current_rtv_count,
                                         dev->current_rtvs,
                                         dev->current_dsv,
                                         [&](HRESULT hr) { SetError(hDevice, hr); })) {
        oom = true;
      }
    }

    for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
      if (dev->vs_srvs[slot] == res->handle ||
          ResourcesAlias(dev->current_vs_srv_resources[slot], res)) {
        dev->vs_srvs[slot] = 0;
        dev->current_vs_srv_resources[slot] = nullptr;
        if (!oom && !SetTextureLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0)) {
          oom = true;
        }
      }
      if (dev->ps_srvs[slot] == res->handle ||
          ResourcesAlias(dev->current_ps_srv_resources[slot], res)) {
        dev->ps_srvs[slot] = 0;
        dev->current_ps_srv_resources[slot] = nullptr;
        if (!oom && !SetTextureLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0)) {
          oom = true;
        }
      }
      if (dev->gs_srvs[slot] == res->handle ||
          ResourcesAlias(dev->current_gs_srv_resources[slot], res)) {
        dev->gs_srvs[slot] = 0;
        dev->current_gs_srv_resources[slot] = nullptr;
        if (!oom && !SetTextureLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 0)) {
          oom = true;
        }
      }
    }
  }

  // Unbind any IA vertex buffer slots that reference this resource.
  for (uint32_t slot = 0; slot < kMaxVertexBufferSlots; ++slot) {
    if (dev->current_vb_resources[slot] != res) {
      continue;
    }
    dev->current_vb_resources[slot] = nullptr;
    if (slot == 0) {
      dev->current_vb_res = nullptr;
      dev->current_vb_stride = 0;
      dev->current_vb_offset = 0;
    }

    aerogpu_vertex_buffer_binding binding{};
    binding.buffer = 0;
    binding.stride_bytes = 0;
    binding.offset_bytes = 0;
    binding.reserved0 = 0;

    if (!oom) {
      if (!aerogpu::d3d10_11::EmitSetVertexBuffersCmdLocked(dev,
                                                            slot,
                                                            /*buffer_count=*/1,
                                                            &binding,
                                                            [&](HRESULT hr) { SetError(hDevice, hr); })) {
        oom = true;
      }
    }
  }
  if (dev->current_ib_res == res) {
    dev->current_ib_res = nullptr;
    (void)aerogpu::d3d10_11::EmitSetIndexBufferCmdLocked(dev,
                                                         /*buffer=*/0,
                                                         AEROGPU_INDEX_FORMAT_UINT16,
                                                         /*offset_bytes=*/0,
                                                         [&](HRESULT hr) { SetError(hDevice, hr); });
  }

  // Unbind constant buffers that reference the resource being destroyed.
  //
  // Note: `DestroyResource` may flush the current command stream (for guest-backed
  // allocations) before deallocating the WDDM handles. Ensure we also emit the
  // corresponding SET_CONSTANT_BUFFERS updates so the host does not observe stale
  // constant buffer bindings pointing at a destroyed resource.
  const aerogpu_constant_buffer_binding null_cb{};
  for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
    if (dev->current_vs_cb_resources[slot] == res) {
      dev->current_vs_cb_resources[slot] = nullptr;
      dev->vs_constant_buffers[slot] = {};
      if (!oom) {
        if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                                AEROGPU_SHADER_STAGE_VERTEX,
                                                                slot,
                                                                /*buffer_count=*/1,
                                                                &null_cb,
                                                                [&](HRESULT hr) { SetError(hDevice, hr); })) {
          oom = true;
        }
      }
    }
    if (dev->current_ps_cb_resources[slot] == res) {
      dev->current_ps_cb_resources[slot] = nullptr;
      dev->ps_constant_buffers[slot] = {};
      if (!oom) {
        if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                                AEROGPU_SHADER_STAGE_PIXEL,
                                                                slot,
                                                                /*buffer_count=*/1,
                                                                &null_cb,
                                                                [&](HRESULT hr) { SetError(hDevice, hr); })) {
          oom = true;
        }
      }
    }
    if (dev->current_gs_cb_resources[slot] == res) {
      dev->current_gs_cb_resources[slot] = nullptr;
      dev->gs_constant_buffers[slot] = {};
      if (!oom) {
        if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                                AEROGPU_SHADER_STAGE_GEOMETRY,
                                                                slot,
                                                                /*buffer_count=*/1,
                                                                &null_cb,
                                                                [&](HRESULT hr) { SetError(hDevice, hr); })) {
          oom = true;
        }
      }
    }
  }

  if (res->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    if (!cmd) {
      SetError(hDevice, E_OUTOFMEMORY);
    } else {
      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
    }
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
    constexpr size_t kInlineKmtAllocs = 16;
    std::array<D3DKMT_HANDLE, kInlineKmtAllocs> km_allocs_stack{};
    std::vector<D3DKMT_HANDLE> km_allocs_heap;
    D3DKMT_HANDLE* km_allocs = nullptr;
    UINT km_alloc_count = 0;

    const size_t handle_count = res->wddm.km_allocation_handles.size();
    if (handle_count != 0) {
      if (handle_count <= km_allocs_stack.size()) {
        for (size_t i = 0; i < handle_count; ++i) {
          km_allocs_stack[i] = static_cast<D3DKMT_HANDLE>(res->wddm.km_allocation_handles[i]);
        }
        km_allocs = km_allocs_stack.data();
        km_alloc_count = static_cast<UINT>(handle_count);
      } else {
        try {
          km_allocs_heap.reserve(handle_count);
          for (uint64_t h : res->wddm.km_allocation_handles) {
            km_allocs_heap.push_back(static_cast<D3DKMT_HANDLE>(h));
          }
          km_allocs = km_allocs_heap.data();
          km_alloc_count = static_cast<UINT>(km_allocs_heap.size());
        } catch (...) {
          SetError(hDevice, E_OUTOFMEMORY);
          km_allocs = nullptr;
          km_alloc_count = 0;
        }
      }
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->hContext));
    }
    __if_exists(D3DDDICB_DEALLOCATE::hKMResource) {
      dealloc.hKMResource = static_cast<D3DKMT_HANDLE>(res->wddm.km_resource_handle);
    }
    __if_exists(D3DDDICB_DEALLOCATE::NumAllocations) {
      dealloc.NumAllocations = km_alloc_count;
    }
    __if_exists(D3DDDICB_DEALLOCATE::HandleList) {
      dealloc.HandleList = km_alloc_count ? km_allocs : nullptr;
    }
    __if_exists(D3DDDICB_DEALLOCATE::phAllocations) {
      dealloc.phAllocations = km_alloc_count ? km_allocs : nullptr;
    }
    const HRESULT hr = CallCbMaybeHandle(dev->callbacks.pfnDeallocateCb, dev->hrt_device, &dealloc);
    if (FAILED(hr)) {
      SetError(hDevice, hr);
    }
    res->wddm.km_allocation_handles.clear();
    res->wddm.km_resource_handle = 0;
  }

  ResetObject(res);
}

// D3D10_DDI_MAP subset (numeric values from d3d10umddi.h / d3d10.h).
using aerogpu::d3d10_11::kD3DMapRead;
using aerogpu::d3d10_11::kD3DMapWrite;
using aerogpu::d3d10_11::kD3DMapReadWrite;
using aerogpu::d3d10_11::kD3DMapWriteDiscard;
using aerogpu::d3d10_11::kD3DMapWriteNoOverwrite;

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

  // The Win7 D3D10 runtime validates MapFlags and will reject unknown bits
  // before calling into the driver. Mirror this behavior for robustness (and to
  // match the portable tests).
  if ((map_flags_u & ~kD3DMapFlagDoNotWait) != 0) {
    return E_INVALIDARG;
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

  // Enforce D3D10 usage/CPU-access rules (matches Win7 runtime expectations).
  const uint32_t cpu_read = kD3D10CpuAccessRead;
  const uint32_t cpu_write = kD3D10CpuAccessWrite;
  switch (res->usage) {
    case kD3D10UsageDynamic:
      if (map_type_u != kD3DMapWriteDiscard && map_type_u != kD3DMapWriteNoOverwrite) {
        return E_INVALIDARG;
      }
      break;
    case kD3D10UsageStaging: {
      const uint32_t access_mask = cpu_read | cpu_write;
      const uint32_t access = res->cpu_access_flags & access_mask;
      if (access == cpu_read) {
        if (map_type_u != kD3DMapRead) {
          return E_INVALIDARG;
        }
      } else if (access == cpu_write) {
        if (map_type_u != kD3DMapWrite) {
          return E_INVALIDARG;
        }
      } else if (access == access_mask) {
        if (map_type_u != kD3DMapRead && map_type_u != kD3DMapWrite && map_type_u != kD3DMapReadWrite) {
          return E_INVALIDARG;
        }
      } else {
        return E_INVALIDARG;
      }
      break;
    }
    default:
      // DEFAULT / IMMUTABLE resources are not mappable via D3D10 Map.
      return E_INVALIDARG;
  }

  if (want_read && !(res->cpu_access_flags & cpu_read)) {
    return E_INVALIDARG;
  }
  if (want_write && !(res->cpu_access_flags & cpu_write)) {
    return E_INVALIDARG;
  }

  // Only apply implicit synchronization for staging-style resources. For D3D10
  // this maps to resources with no bind flags (typical staging readback).
  if (want_read && res->usage == kD3D10UsageStaging) {
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

  const uint64_t total = resource_total_bytes(dev, res);
  if (!total) {
    return E_INVALIDARG;
  }

  uint64_t map_offset = 0;
  uint64_t map_size = total;
  uint32_t map_row_pitch = 0;
  const Texture2DSubresourceLayout* tex_layout = nullptr;
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
    tex_layout = &sub_layout;
    map_offset = sub_layout.offset_bytes;
    map_size = sub_layout.size_bytes;
    map_row_pitch = sub_layout.row_pitch_bytes;
    const uint64_t end = map_offset + map_size;
    if (end < map_offset || end > total || map_size == 0) {
      return E_INVALIDARG;
    }
  } else {
    return E_INVALIDARG;
  }

  uint64_t storage_size = total;
  if (res->kind == ResourceKind::Buffer) {
    storage_size = AlignUpU64(total ? total : 1, 4);
  }
  if (storage_size > static_cast<uint64_t>(SIZE_MAX)) {
    return E_OUTOFMEMORY;
  }

  try {
    if (map_type_u == kD3DMapWriteDiscard) {
      if (res->kind == ResourceKind::Buffer) {
        // Approximate DISCARD renaming by allocating a fresh CPU backing store.
        res->storage.assign(static_cast<size_t>(storage_size), 0);
      } else if (res->kind == ResourceKind::Texture2D) {
        if (res->storage.size() < static_cast<size_t>(storage_size)) {
          res->storage.resize(static_cast<size_t>(storage_size), 0);
        }
        if (map_offset < res->storage.size()) {
          const size_t remaining = res->storage.size() - static_cast<size_t>(map_offset);
          const size_t clear_bytes = static_cast<size_t>(std::min<uint64_t>(map_size, remaining));
          std::fill(res->storage.begin() + static_cast<size_t>(map_offset),
                    res->storage.begin() + static_cast<size_t>(map_offset) + clear_bytes,
                    0);
        }
      }
    } else if (res->storage.size() < static_cast<size_t>(storage_size)) {
      res->storage.resize(static_cast<size_t>(storage_size), 0);
    }
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  const bool allow_storage_map =
      (res->backing_alloc_id == 0) && !(want_read && res->usage == kD3D10UsageStaging);
  const auto map_storage = [&]() -> HRESULT {
    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_offset = map_offset;
    res->mapped_size = map_size;
    res->mapped_wddm_ptr = nullptr;
    res->mapped_wddm_allocation = 0;
    res->mapped_wddm_pitch = 0;
    res->mapped_wddm_slice_pitch = 0;

    if (res->storage.empty()) {
      pMap->pData = nullptr;
    } else {
      pMap->pData = res->storage.data() + static_cast<size_t>(map_offset);
    }
    if (res->kind == ResourceKind::Texture2D) {
      pMap->RowPitch = map_row_pitch;
      pMap->DepthPitch = static_cast<UINT>(map_size);
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
  const uint32_t lock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : subresource;
  InitLockArgsForMap(&lock_cb, lock_subresource, map_type_u, map_flags_u);

  const bool do_not_wait = (map_flags_u & kD3DMapFlagDoNotWait) != 0;
  HRESULT hr = CallCbMaybeHandle(cb->pfnLockCb, dev->hrt_device, &lock_cb);
  if (hr == kDxgiErrorWasStillDrawing || hr == kHrNtStatusGraphicsGpuBusy ||
      (do_not_wait && (hr == kHrPending || hr == kHrWaitTimeout || hr == kHrErrorTimeout || hr == kHrNtStatusTimeout)))) {
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
  res->mapped_wddm_allocation = alloc_handle;
  const bool is_guest_backed = (res->backing_alloc_id != 0);

  uint32_t tex_row_bytes = 0;
  uint32_t tex_rows = 0;
  uint32_t tex_pitch = 0;
  uint32_t tex_slice_pitch = 0;
  if (res->kind == ResourceKind::Texture2D) {
    if (!tex_layout) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
      InitUnlockArgsForMap(&unlock_cb, lock_subresource);
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (allow_storage_map) {
        return map_storage();
      }
      return E_FAIL;
    }

    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
      InitUnlockArgsForMap(&unlock_cb, lock_subresource);
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (allow_storage_map) {
        return map_storage();
      }
      return E_INVALIDARG;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
      InitUnlockArgsForMap(&unlock_cb, lock_subresource);
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (allow_storage_map) {
        return map_storage();
      }
      return E_INVALIDARG;
    }

    // We lock SubresourceIndex=0 for packed Texture2D allocations. Treat the
    // runtime-provided Pitch/SlicePitch as applying to mip0 subresources (same
    // width across array layers); other mips use our packed layout pitch.
    const bool pitch_applies = (tex_layout->mip_level == 0);
    const uint32_t lock_pitch = pitch_applies ? aerogpu_lock_pitch_bytes(lock_cb) : 0;
    if (pitch_applies) {
      LogLockPitchMismatchMaybe(res->dxgi_format, subresource, *tex_layout, map_row_pitch, lock_pitch);
    }
    // Guest-backed resources are interpreted by the host using the protocol pitch
    // (CREATE_TEXTURE2D.row_pitch_bytes). Ignore the runtime's LockCb pitch so
    // Map returns the same row stride that the host will use.
    tex_pitch = (!is_guest_backed && lock_pitch) ? lock_pitch : map_row_pitch;
    const uint32_t lock_slice = pitch_applies ? aerogpu_lock_slice_pitch_bytes(lock_cb) : 0;
    const uint64_t allocation_size =
        res->wddm_allocation_size_bytes ? res->wddm_allocation_size_bytes : total;
    if (!ValidateTexture2DRowSpan(aer_fmt, *tex_layout, tex_pitch, allocation_size, &tex_row_bytes)) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
      InitUnlockArgsForMap(&unlock_cb, lock_subresource);
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (allow_storage_map) {
        return map_storage();
      }
      return E_INVALIDARG;
    }
    tex_rows = tex_layout->rows_in_layout;

    if (!is_guest_backed && lock_slice) {
      tex_slice_pitch = lock_slice;
    } else {
      const uint64_t slice_pitch_u64 =
          static_cast<uint64_t>(tex_pitch) * static_cast<uint64_t>(tex_rows);
      if (slice_pitch_u64 == 0 || slice_pitch_u64 > static_cast<uint64_t>(UINT32_MAX)) {
        D3DDDICB_UNLOCK unlock_cb = {};
        unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
        InitUnlockArgsForMap(&unlock_cb, lock_subresource);
        (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
        if (allow_storage_map) {
          return map_storage();
        }
        return E_OUTOFMEMORY;
      }
      tex_slice_pitch = static_cast<uint32_t>(slice_pitch_u64);
    }

    // We keep the runtime pitch around for host-owned resources so Unmap can
    // translate the runtime allocation layout into our protocol layout. For
    // guest-backed resources we always interpret memory using protocol pitches.
    res->mapped_wddm_pitch = is_guest_backed ? 0u : tex_pitch;
    res->mapped_wddm_slice_pitch = is_guest_backed ? 0u : tex_slice_pitch;
  } else {
    res->mapped_wddm_pitch = aerogpu_lock_pitch_bytes(lock_cb);
    res->mapped_wddm_slice_pitch = aerogpu_lock_slice_pitch_bytes(lock_cb);
  }

  if (!res->storage.empty()) {
    if (res->kind == ResourceKind::Texture2D && tex_pitch && tex_row_bytes && tex_rows && tex_layout) {
      const uint64_t allocation_size =
          res->wddm_allocation_size_bytes ? res->wddm_allocation_size_bytes : total;
      const uint64_t end_row_u64 = tex_layout->offset_bytes +
                                  static_cast<uint64_t>(tex_rows - 1u) * static_cast<uint64_t>(tex_pitch) +
                                  static_cast<uint64_t>(tex_row_bytes);
      if (end_row_u64 > static_cast<uint64_t>(SIZE_MAX) || end_row_u64 > allocation_size) {
        // Should already be handled by ValidateTexture2DRowSpan, but keep the pointer arithmetic guarded.
        D3DDDICB_UNLOCK unlock_cb = {};
        unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
        InitUnlockArgsForMap(&unlock_cb, lock_subresource);
        (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
        if (allow_storage_map) {
          return map_storage();
        }
        return E_INVALIDARG;
      }

      uint8_t* dst_wddm = static_cast<uint8_t*>(lock_cb.pData);
      uint8_t* dst_storage = res->storage.data();

      bool can_clear_padding = false;
      if (tex_pitch > tex_row_bytes) {
        const uint64_t end_full_row_u64 = tex_layout->offset_bytes +
                                          static_cast<uint64_t>(tex_rows - 1u) * static_cast<uint64_t>(tex_pitch) +
                                          static_cast<uint64_t>(tex_pitch);
        can_clear_padding = (end_full_row_u64 >= tex_layout->offset_bytes) &&
                            (end_full_row_u64 <= allocation_size) &&
                            (end_full_row_u64 <= static_cast<uint64_t>(SIZE_MAX));
      }

      for (uint32_t y = 0; y < tex_rows; ++y) {
        const size_t off_storage =
            static_cast<size_t>(tex_layout->offset_bytes) + static_cast<size_t>(y) * static_cast<size_t>(map_row_pitch);
        const size_t off_wddm =
            static_cast<size_t>(tex_layout->offset_bytes) + static_cast<size_t>(y) * static_cast<size_t>(tex_pitch);

        if (map_type_u == kD3DMapWriteDiscard) {
          std::memset(dst_wddm + off_wddm, 0, tex_row_bytes);
          if (can_clear_padding) {
            std::memset(dst_wddm + off_wddm + tex_row_bytes, 0, tex_pitch - tex_row_bytes);
          }
          continue;
        }

        if (!is_guest_backed) {
          std::memcpy(dst_wddm + off_wddm, dst_storage + off_storage, tex_row_bytes);
          if (can_clear_padding) {
            std::memset(dst_wddm + off_wddm + tex_row_bytes, 0, tex_pitch - tex_row_bytes);
          }
          continue;
        }

        if (want_read || (want_write && res->usage == kD3D10UsageStaging)) {
          std::memcpy(dst_storage + off_storage, dst_wddm + off_wddm, tex_row_bytes);
        }
      }
    } else {
      // Buffer/unvalidated paths: treat as a linear byte range.
      if (map_type_u == kD3DMapWriteDiscard) {
        // Discard contents are undefined; clear for deterministic tests.
        if (map_offset < static_cast<uint64_t>(SIZE_MAX) && map_size <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                      0,
                      static_cast<size_t>(map_size));
        }
      } else if (!is_guest_backed) {
        if (map_offset <= res->storage.size()) {
          const size_t remaining = res->storage.size() - static_cast<size_t>(map_offset);
          const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(map_size, remaining));
          if (copy_bytes) {
            std::memcpy(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                        res->storage.data() + static_cast<size_t>(map_offset),
                        copy_bytes);
          }
        }
      } else if (want_read || (want_write && res->usage == kD3D10UsageStaging)) {
        if (map_offset <= res->storage.size()) {
          const size_t remaining = res->storage.size() - static_cast<size_t>(map_offset);
          const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(map_size, remaining));
          if (copy_bytes) {
            std::memcpy(res->storage.data() + static_cast<size_t>(map_offset),
                        static_cast<const uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                        copy_bytes);
          }
        }
      }
    }
  }

  if (res->kind == ResourceKind::Texture2D) {
    pMap->pData = static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset);
    pMap->RowPitch = tex_pitch ? tex_pitch : map_row_pitch;
    pMap->DepthPitch = tex_slice_pitch ? static_cast<UINT>(tex_slice_pitch) : static_cast<UINT>(map_size);
  } else {
    pMap->pData = lock_cb.pData;
    pMap->RowPitch = 0;
    pMap->DepthPitch = 0;
  }

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_offset = map_offset;
  res->mapped_size = map_size;
  return S_OK;
}

void unmap_resource_locked(D3D10DDI_HDEVICE hDevice, AeroGpuDevice* dev, AeroGpuResource* res, uint32_t subresource) {
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  bool dirty_emitted_on_unmap = false;
  bool dirty_failed_on_unmap = false;
  uint64_t upload_offset = res->mapped_offset;
  uint64_t upload_size_storage = res->mapped_size;
  bool emit_ok = (res->mapped_write && res->mapped_size != 0);
  if (emit_ok && res->kind == ResourceKind::Buffer) {
    const uint64_t end = res->mapped_offset + res->mapped_size;
    if (end < res->mapped_offset) {
      SetError(hDevice, E_INVALIDARG);
      emit_ok = false;
    }
    if (emit_ok) {
      upload_offset = res->mapped_offset & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      upload_size_storage = upload_end - upload_offset;
    }
  }
  if (emit_ok && !res->storage.empty()) {
    if (upload_offset > static_cast<uint64_t>(res->storage.size())) {
      SetError(hDevice, E_INVALIDARG);
      emit_ok = false;
    }
    if (emit_ok) {
      const size_t remaining = res->storage.size() - static_cast<size_t>(upload_offset);
      if (upload_size_storage > static_cast<uint64_t>(remaining)) {
        SetError(hDevice, E_INVALIDARG);
        emit_ok = false;
      } else if (upload_size_storage > static_cast<uint64_t>(SIZE_MAX)) {
        SetError(hDevice, E_OUTOFMEMORY);
        emit_ok = false;
      }
    }
  }

  HRESULT copy_back_hr = S_OK;
  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (emit_ok && res->mapped_write && res->mapped_size != 0 && res->backing_alloc_id != 0) {
      // Guest-backed resources: record the dirty range before committing the
      // CPU-written bytes into the software shadow copy. If we cannot record the
      // dirty range due to OOM, restore the guest allocation bytes from the
      // shadow copy so the host and guest do not diverge.
      const auto cmd_checkpoint = dev->cmd.checkpoint();
      const WddmAllocListCheckpoint alloc_checkpoint(dev);
      TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
      if (!dev->wddm_submit_allocation_list_oom) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (cmd) {
          cmd->resource_handle = res->handle;
          cmd->reserved0 = 0;
          cmd->offset_bytes = upload_offset;
          cmd->size_bytes = upload_size_storage;
          dirty_emitted_on_unmap = true;
        }
      }
      if (!dirty_emitted_on_unmap) {
        dirty_failed_on_unmap = true;
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();

        if (!res->storage.empty() &&
            upload_offset <= static_cast<uint64_t>(SIZE_MAX) &&
            upload_size_storage <= static_cast<uint64_t>(SIZE_MAX) &&
            upload_offset + upload_size_storage <= static_cast<uint64_t>(res->storage.size())) {
          const size_t off = static_cast<size_t>(upload_offset);
          const size_t sz = static_cast<size_t>(upload_size_storage);
          uint8_t* dst_bytes = static_cast<uint8_t*>(res->mapped_wddm_ptr);
          std::memcpy(dst_bytes + off, res->storage.data() + off, sz);
        }
        SetError(hDevice, E_OUTOFMEMORY);
      }
    }

    if (!dirty_failed_on_unmap && res->mapped_write && !res->storage.empty() && res->mapped_size) {
      if (res->kind == ResourceKind::Texture2D) {
        do {
           if (subresource >= res->tex2d_subresources.size()) {
             copy_back_hr = E_INVALIDARG;
             break;
           }
           const Texture2DSubresourceLayout& sub_layout = res->tex2d_subresources[subresource];
           const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
           if (aer_fmt == AEROGPU_FORMAT_INVALID) {
             copy_back_hr = E_INVALIDARG;
             break;
           }

          const uint32_t src_pitch = res->mapped_wddm_pitch ? res->mapped_wddm_pitch : sub_layout.row_pitch_bytes;
          const uint64_t alloc_size =
              res->wddm_allocation_size_bytes ? res->wddm_allocation_size_bytes : resource_total_bytes(dev, res);
          uint32_t row_bytes = 0;
          if (!ValidateTexture2DRowSpan(aer_fmt, sub_layout, src_pitch, alloc_size, &row_bytes)) {
            copy_back_hr = E_INVALIDARG;
            break;
          }
          if (sub_layout.row_pitch_bytes < row_bytes) {
            copy_back_hr = E_INVALIDARG;
            break;
          }

          const uint32_t rows = sub_layout.rows_in_layout;
          const uint64_t storage_total = static_cast<uint64_t>(res->storage.size());
          const uint64_t dst_end_u64 =
              sub_layout.offset_bytes +
              static_cast<uint64_t>(rows - 1u) * static_cast<uint64_t>(sub_layout.row_pitch_bytes) +
              static_cast<uint64_t>(row_bytes);
          if (dst_end_u64 > storage_total) {
            copy_back_hr = E_INVALIDARG;
            break;
          }

          const uint8_t* src_base = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
          uint8_t* dst_base = res->storage.data();
          for (uint32_t y = 0; y < rows; ++y) {
            const uint64_t src_off_u64 =
                sub_layout.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(src_pitch);
            const uint64_t dst_off_u64 =
                sub_layout.offset_bytes + static_cast<uint64_t>(y) * static_cast<uint64_t>(sub_layout.row_pitch_bytes);
            if (src_off_u64 + row_bytes > alloc_size || dst_off_u64 + row_bytes > storage_total) {
              copy_back_hr = E_INVALIDARG;
              break;
            }
            if (src_off_u64 > static_cast<uint64_t>(SIZE_MAX) || dst_off_u64 > static_cast<uint64_t>(SIZE_MAX)) {
              copy_back_hr = E_OUTOFMEMORY;
              break;
            }
            const size_t src_off = static_cast<size_t>(src_off_u64);
            const size_t dst_off = static_cast<size_t>(dst_off_u64);
            std::memcpy(dst_base + dst_off, src_base + src_off, row_bytes);
          }
        } while (false);
      } else {
        const uint8_t* src = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
        const uint64_t off_u64 = res->mapped_offset;
        const uint64_t size_u64 = res->mapped_size;
        if (off_u64 <= res->storage.size()) {
          const size_t remaining = res->storage.size() - static_cast<size_t>(off_u64);
          const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size_u64, remaining));
          if (copy_bytes) {
            std::memcpy(res->storage.data() + static_cast<size_t>(off_u64),
                        src + static_cast<size_t>(off_u64),
                        copy_bytes);
          }
        }
      }
    }

    const D3DDDI_DEVICECALLBACKS* cb = dev->um_callbacks;
    if (cb && cb->pfnUnlockCb) {
      D3DDDICB_UNLOCK unlock_cb = {};
      unlock_cb.hAllocation =
          UintPtrToD3dHandle<decltype(unlock_cb.hAllocation)>(static_cast<std::uintptr_t>(res->mapped_wddm_allocation));
      const uint32_t unlock_subresource = (res->kind == ResourceKind::Texture2D) ? 0u : subresource;
      InitUnlockArgsForMap(&unlock_cb, unlock_subresource);
      const HRESULT unlock_hr = CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
      if (FAILED(unlock_hr)) {
        SetError(hDevice, unlock_hr);
      }
    }
  }

  if (FAILED(copy_back_hr)) {
    SetError(hDevice, copy_back_hr);
  }

  if (emit_ok) {
    if (res->backing_alloc_id != 0) {
      // RESOURCE_DIRTY_RANGE causes the host to read the guest allocation to update the host copy.
      //
      // If we already emitted (or failed to emit) a dirty range while the
      // allocation was still mapped, do not emit another one here.
      if (!dirty_emitted_on_unmap && !dirty_failed_on_unmap) {
        TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!cmd) {
          SetError(hDevice, E_FAIL);
        } else {
          cmd->resource_handle = res->handle;
          cmd->reserved0 = 0;
          cmd->offset_bytes = upload_offset;
          // Host-side executors validate dirty ranges against the protocol-visible
          // texture size (CREATE_TEXTURE2D layouts). Avoid using the runtime
          // Pitch-derived span (`upload_size_dirty`), which can exceed the protocol
          // layout when the WDDM allocation pitch differs.
          cmd->size_bytes = upload_size_storage;
        }
      }
    } else {
      EmitUploadLocked(hDevice, dev, res, upload_offset, upload_size_storage);
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
    SetError(hDevice, E_INVALIDARG);
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
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || static_cast<uint32_t>(subresource) != res->mapped_subresource) {
    SetError(hDevice, E_INVALIDARG);
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
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    SetError(hDevice, E_INVALIDARG);
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
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    SetError(hDevice, E_INVALIDARG);
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

  uint64_t tex_upload_offset = 0;
  uint64_t tex_upload_size = 0;
  bool do_tex_upload = false;

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
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
    if (!fmt_layout.valid || fmt_layout.block_width == 0 || fmt_layout.block_height == 0 || fmt_layout.bytes_per_block == 0) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    if (res->row_pitch_bytes == 0) {
      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
      if (row_bytes == 0) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      res->row_pitch_bytes = AlignUpU32(row_bytes, 256);
      if (res->row_pitch_bytes == 0) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
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
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }

    const uint64_t subresource_count =
        static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
    const uint64_t dst_subresource_u64 = static_cast<uint64_t>(pUpdate->DstSubresource);
    if (subresource_count == 0 ||
        dst_subresource_u64 >= subresource_count ||
        dst_subresource_u64 >= static_cast<uint64_t>(res->tex2d_subresources.size())) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const Texture2DSubresourceLayout& dst_layout =
        res->tex2d_subresources[static_cast<size_t>(dst_subresource_u64)];

    if (res->storage.size() < static_cast<size_t>(total_bytes)) {
      try {
        res->storage.resize(static_cast<size_t>(total_bytes), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }

    const uint32_t mip_w = dst_layout.width;
    const uint32_t mip_h = dst_layout.height;
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, mip_w);
    if (min_row_bytes == 0 || dst_layout.row_pitch_bytes < min_row_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    uint32_t left = 0;
    uint32_t top = 0;
    uint32_t right = mip_w;
    uint32_t bottom = mip_h;
    if (pUpdate->pDstBox) {
      const auto* box = pUpdate->pDstBox;
      if (box->right < box->left || box->bottom < box->top || box->front != 0 || box->back != 1) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      left = box->left;
      top = box->top;
      right = box->right;
      bottom = box->bottom;
      AEROGPU_D3D10_11_LOG("D3D10 UpdateSubresourceUP: tex2d sub=%u box=(%u,%u)-(%u,%u)",
                           static_cast<unsigned>(pUpdate->DstSubresource),
                           static_cast<unsigned>(left),
                           static_cast<unsigned>(top),
                           static_cast<unsigned>(right),
                           static_cast<unsigned>(bottom));
    }
    if (right > mip_w || bottom > mip_h) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    if (fmt_layout.block_width > 1 || fmt_layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((left % fmt_layout.block_width) != 0 ||
          (top % fmt_layout.block_height) != 0 ||
          !aligned_or_edge(right, fmt_layout.block_width, mip_w) ||
          !aligned_or_edge(bottom, fmt_layout.block_height, mip_h)) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
    }

    const uint32_t block_left = left / fmt_layout.block_width;
    const uint32_t block_top = top / fmt_layout.block_height;
    const uint32_t block_right = aerogpu_div_round_up_u32(right, fmt_layout.block_width);
    const uint32_t block_bottom = aerogpu_div_round_up_u32(bottom, fmt_layout.block_height);
    if (block_right < block_left || block_bottom < block_top) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width_blocks = block_right - block_left;
    const uint32_t copy_height_blocks = block_bottom - block_top;
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > UINT32_MAX || copy_height_blocks == 0) {
      // Treat empty boxes as a no-op.
      return;
    }
    const uint32_t row_bytes = static_cast<uint32_t>(row_bytes_u64);

    const uint32_t pitch = pUpdate->RowPitch ? static_cast<uint32_t>(pUpdate->RowPitch) : row_bytes;
    if (pitch < row_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const bool full_row_update = (left == 0) && (right == mip_w);
    const uint64_t row_needed =
        static_cast<uint64_t>(block_left) * static_cast<uint64_t>(fmt_layout.bytes_per_block) + static_cast<uint64_t>(row_bytes);
    if (row_needed > dst_layout.row_pitch_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    if (block_top + copy_height_blocks > dst_layout.rows_in_layout) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    if (dst_layout.offset_bytes > res->storage.size()) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    const size_t dst_base = static_cast<size_t>(dst_layout.offset_bytes);
    if (dst_layout.size_bytes > res->storage.size() - dst_base) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint8_t* src_bytes = static_cast<const uint8_t*>(pUpdate->pSysMemUP);
    for (uint32_t y = 0; y < copy_height_blocks; ++y) {
      const size_t dst_off =
          dst_base +
          static_cast<size_t>(block_top + y) * dst_layout.row_pitch_bytes +
          static_cast<size_t>(block_left) * fmt_layout.bytes_per_block;
      const size_t src_off = static_cast<size_t>(y) * static_cast<size_t>(pitch);
      if (dst_off + row_bytes > res->storage.size()) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      std::memcpy(res->storage.data() + dst_off, src_bytes + src_off, row_bytes);
      // For boxed updates, preserve any per-row padding outside the updated
      // rectangle. Only clear padding for full-subresource uploads.
      if (!pUpdate->pDstBox && full_row_update && dst_layout.row_pitch_bytes > row_bytes) {
        const size_t dst_row_start = dst_base + static_cast<size_t>(block_top + y) * dst_layout.row_pitch_bytes;
        std::memset(res->storage.data() + dst_row_start + row_bytes, 0, dst_layout.row_pitch_bytes - row_bytes);
      }
    }

    if (res->backing_alloc_id == 0 && pUpdate->pDstBox) {
      // Host-owned boxed texture uploads must be row-aligned for the host-side
      // executor. Upload the affected row range (full rows) rather than
      // attempting to upload per-row subranges.
      const uint64_t row_pitch_u64 = static_cast<uint64_t>(dst_layout.row_pitch_bytes);
      const uint64_t upload_offset =
          dst_layout.offset_bytes + static_cast<uint64_t>(block_top) * row_pitch_u64;
      const uint64_t upload_size =
          static_cast<uint64_t>(copy_height_blocks) * row_pitch_u64;
      EmitUploadLocked(hDevice, dev, res, upload_offset, upload_size);
      return;
    }

    if (res->backing_alloc_id != 0 && pUpdate->pDstBox) {
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

      uint32_t wddm_pitch = 0;
      __if_exists(D3DDDICB_LOCK::Pitch) {
        wddm_pitch = lock_args.Pitch;
      }
      if (dst_layout.mip_level == 0) {
        LogLockPitchMismatchMaybe(res->dxgi_format,
                                  static_cast<uint32_t>(pUpdate->DstSubresource),
                                  dst_layout,
                                  dst_layout.row_pitch_bytes,
                                  wddm_pitch);
      }

      // Guest-backed textures are interpreted by the host using the protocol
      // pitch (CREATE_TEXTURE2D.row_pitch_bytes). Ignore the runtime's LockCb
      // pitch so CPU writes into the guest allocation match what the host
      // expects.
      const uint32_t dst_pitch = dst_layout.row_pitch_bytes;

      const auto restore_storage_from_allocation = [&]() {
        if (res->storage.empty() || dst_layout.size_bytes == 0) {
          return;
        }
        uint64_t allocation_size = res->wddm_allocation_size_bytes;
        if (allocation_size == 0) {
          allocation_size = static_cast<uint64_t>(res->storage.size());
        }
        const uint64_t off_u64 = dst_layout.offset_bytes;
        const uint64_t size_u64 = dst_layout.size_bytes;
        const uint64_t end_u64 = off_u64 + size_u64;
        if (end_u64 < off_u64) {
          return;
        }
        if (end_u64 > allocation_size) {
          return;
        }
        if (off_u64 > static_cast<uint64_t>(SIZE_MAX) || size_u64 > static_cast<uint64_t>(SIZE_MAX)) {
          return;
        }
        if (off_u64 > static_cast<uint64_t>(res->storage.size())) {
          return;
        }
        const size_t remaining = res->storage.size() - static_cast<size_t>(off_u64);
        if (size_u64 > static_cast<uint64_t>(remaining)) {
          return;
        }
        const size_t off_sz = static_cast<size_t>(off_u64);
        const size_t sz = static_cast<size_t>(size_u64);
        std::memcpy(res->storage.data() + off_sz, static_cast<const uint8_t*>(lock_args.pData) + off_sz, sz);
      };

      if (dst_pitch < row_bytes) {
        restore_storage_from_allocation();
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        InitUnlockForWrite(&unlock_args);
        (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
        SetError(hDevice, E_INVALIDARG);
        return;
      }

      // Record RESOURCE_DIRTY_RANGE before writing into the guest allocation so
      // an OOM while growing the command stream / allocation list cannot leave
      // the host unaware of the CPU write.
      const auto cmd_checkpoint = dev->cmd.checkpoint();
      const WddmAllocListCheckpoint alloc_checkpoint(dev);
      if (dev->wddm_submit_allocation_list_oom) {
        restore_storage_from_allocation();
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        InitUnlockForWrite(&unlock_args);
        (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
      TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
      if (dev->wddm_submit_allocation_list_oom) {
        restore_storage_from_allocation();
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        InitUnlockForWrite(&unlock_args);
        (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
        return;
      }

      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!dirty) {
        restore_storage_from_allocation();
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();
        D3DDDICB_UNLOCK unlock_args = {};
        unlock_args.hAllocation = lock_args.hAllocation;
        InitUnlockForWrite(&unlock_args);
        (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = dst_layout.offset_bytes;
      // Host-side executors validate dirty ranges against the protocol-visible
      // texture size, not the raw WDDM allocation size.
      dirty->size_bytes = dst_layout.size_bytes;

      // Commit the updated bytes into the guest allocation now that the dirty
      // range is recorded.
      uint8_t* dst_alloc_base = static_cast<uint8_t*>(lock_args.pData) + dst_base;
      for (uint32_t y = 0; y < copy_height_blocks; ++y) {
        const size_t dst_off =
            static_cast<size_t>(block_top + y) * dst_pitch +
            static_cast<size_t>(block_left) * fmt_layout.bytes_per_block;
        const size_t src_off = static_cast<size_t>(y) * static_cast<size_t>(pitch);
        std::memcpy(dst_alloc_base + dst_off, src_bytes + src_off, row_bytes);
      }

      D3DDDICB_UNLOCK unlock_args = {};
      unlock_args.hAllocation = lock_args.hAllocation;
      InitUnlockForWrite(&unlock_args);
      hr = CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);
      if (FAILED(hr)) {
        SetError(hDevice, hr);
        return;
      }
      return;
    }

    do_tex_upload = true;
    tex_upload_offset = dst_layout.offset_bytes;
    tex_upload_size = dst_layout.size_bytes;
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
    if (do_tex_upload && !res->storage.empty()) {
      EmitUploadLocked(hDevice, dev, res, tex_upload_offset, tex_upload_size);
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
  if (!hDevice.pDrvPrivate || !hDst.pDrvPrivate || !hSrc.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hDst);
  auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hSrc);
  if (dst && src && dst->kind == ResourceKind::Texture2D && src->kind == ResourceKind::Texture2D) {
    const uint64_t dst_sub_count =
        static_cast<uint64_t>(dst->mip_levels) * static_cast<uint64_t>(dst->array_size);
    const uint64_t src_sub_count =
        static_cast<uint64_t>(src->mip_levels) * static_cast<uint64_t>(src->array_size);
    const uint64_t sub_count = std::min(dst_sub_count, src_sub_count);
    const uint32_t sub_count_u32 =
        static_cast<uint32_t>(std::min<uint64_t>(sub_count, static_cast<uint64_t>(UINT32_MAX)));
    for (uint32_t sub = 0; sub < sub_count_u32; ++sub) {
      CopySubresourceRegion(hDevice, hDst, sub, 0, 0, 0, hSrc, sub, nullptr);
    }
    return;
  }

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
    if (dst_subresource != 0 || src_subresource != 0) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
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
      // Ensure guest-backed buffers are updated via the backing allocation +
      // RESOURCE_DIRTY_RANGE (UPLOAD_RESOURCE does not write guest memory).
      EmitUploadLocked(hDevice, dev, dst, dst_off, bytes);
    }

    const bool transfer_aligned = (((dst_off | src_left | bytes) & 3ull) == 0);
    const bool same_buffer = (dst->handle == src->handle);
    if (!aerogpu::d3d10_11::SupportsTransfer(dev) || !transfer_aligned || same_buffer) {
      return;
    }

    if (!TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false) ||
        !TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true)) {
      return;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      // COPY_BUFFER is an optimization; the CPU copy + upload has already run.
      return;
    }
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = dst_off;
    cmd->src_offset_bytes = src_left;
    cmd->size_bytes = bytes;
    uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
    if (dst->backing_alloc_id != 0 &&
        dst->usage == kD3D10UsageStaging &&
        (dst->cpu_access_flags & kD3D10CpuAccessRead) != 0) {
      copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
    }
    cmd->flags = copy_flags;
    cmd->reserved0 = 0;
    TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { SetError(hDevice, hr); });
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

    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      SetError(hDevice, E_NOTIMPL);
      return;
    }

    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
    if (!fmt_layout.valid ||
        fmt_layout.block_width == 0 ||
        fmt_layout.block_height == 0 ||
        fmt_layout.bytes_per_block == 0) {
      SetError(hDevice, E_INVALIDARG);
      return;
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
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint64_t dst_sub_count =
        static_cast<uint64_t>(dst->mip_levels) * static_cast<uint64_t>(dst->array_size);
    const uint64_t src_sub_count =
        static_cast<uint64_t>(src->mip_levels) * static_cast<uint64_t>(src->array_size);
    if (dst_sub_count == 0 ||
        src_sub_count == 0 ||
        static_cast<uint64_t>(dst_subresource) >= dst_sub_count ||
        static_cast<uint64_t>(src_subresource) >= src_sub_count ||
        static_cast<uint64_t>(dst_subresource) >= static_cast<uint64_t>(dst->tex2d_subresources.size()) ||
        static_cast<uint64_t>(src_subresource) >= static_cast<uint64_t>(src->tex2d_subresources.size())) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const Texture2DSubresourceLayout dst_sub = dst->tex2d_subresources[dst_subresource];
    const Texture2DSubresourceLayout src_sub = src->tex2d_subresources[src_subresource];

    uint32_t src_left = 0;
    uint32_t src_top = 0;
    uint32_t src_right = src_sub.width;
    uint32_t src_bottom = src_sub.height;
    if (pSrcBox) {
      // Only support 2D boxes.
      if (pSrcBox->front != 0 || pSrcBox->back != 1) {
        SetError(hDevice, E_NOTIMPL);
        return;
      }
      if (pSrcBox->right < pSrcBox->left || pSrcBox->bottom < pSrcBox->top) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      src_left = static_cast<uint32_t>(pSrcBox->left);
      src_top = static_cast<uint32_t>(pSrcBox->top);
      src_right = static_cast<uint32_t>(pSrcBox->right);
      src_bottom = static_cast<uint32_t>(pSrcBox->bottom);
    }

    if (src_right > src_sub.width || src_bottom > src_sub.height) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    if (dstX > dst_sub.width || dstY > dst_sub.height) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint32_t src_extent_w = src_right - src_left;
    const uint32_t src_extent_h = src_bottom - src_top;
    const uint32_t max_dst_w = dst_sub.width - dstX;
    const uint32_t max_dst_h = dst_sub.height - dstY;
    const uint32_t copy_width = std::min(src_extent_w, max_dst_w);
    const uint32_t copy_height = std::min(src_extent_h, max_dst_h);
    if (copy_width == 0 || copy_height == 0) {
      return;
    }

    const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
      return (v % align) == 0 || v == extent;
    };
    if (fmt_layout.block_width > 1 || fmt_layout.block_height > 1) {
      if (!aligned_or_edge(src_left, fmt_layout.block_width, src_sub.width) ||
          !aligned_or_edge(src_right, fmt_layout.block_width, src_sub.width) ||
          !aligned_or_edge(dstX, fmt_layout.block_width, dst_sub.width) ||
          !aligned_or_edge(dstX + copy_width, fmt_layout.block_width, dst_sub.width) ||
          !aligned_or_edge(src_top, fmt_layout.block_height, src_sub.height) ||
          !aligned_or_edge(src_bottom, fmt_layout.block_height, src_sub.height) ||
          !aligned_or_edge(dstY, fmt_layout.block_height, dst_sub.height) ||
          !aligned_or_edge(dstY + copy_height, fmt_layout.block_height, dst_sub.height)) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
    }

    const uint32_t src_x_blocks = src_left / fmt_layout.block_width;
    const uint32_t src_y_blocks = src_top / fmt_layout.block_height;
    const uint32_t dst_x_blocks = dstX / fmt_layout.block_width;
    const uint32_t dst_y_blocks = dstY / fmt_layout.block_height;

    const uint32_t copy_width_blocks = aerogpu_div_round_up_u32(copy_width, fmt_layout.block_width);
    const uint32_t copy_height_blocks = aerogpu_div_round_up_u32(copy_height, fmt_layout.block_height);
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > static_cast<uint64_t>(SIZE_MAX)) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    const size_t row_bytes = static_cast<size_t>(row_bytes_u64);

    const uint64_t dst_total = resource_total_bytes(dev, dst);
    const uint64_t src_total = resource_total_bytes(dev, src);
    if (dst_total > static_cast<uint64_t>(SIZE_MAX) || src_total > static_cast<uint64_t>(SIZE_MAX)) {
      SetError(hDevice, E_OUTOFMEMORY);
      return;
    }
    if (dst->storage.size() < static_cast<size_t>(dst_total)) {
      try {
        dst->storage.resize(static_cast<size_t>(dst_total), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }
    if (src->storage.size() < static_cast<size_t>(src_total)) {
      try {
        src->storage.resize(static_cast<size_t>(src_total), 0);
      } catch (...) {
        SetError(hDevice, E_OUTOFMEMORY);
        return;
      }
    }

    if (copy_height_blocks > dst_sub.rows_in_layout || copy_height_blocks > src_sub.rows_in_layout) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }

    const uint64_t dst_row_needed =
        static_cast<uint64_t>(dst_x_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block) + row_bytes_u64;
    const uint64_t src_row_needed =
        static_cast<uint64_t>(src_x_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block) + row_bytes_u64;
    if (dst_row_needed > dst_sub.row_pitch_bytes || src_row_needed > src_sub.row_pitch_bytes) {
      SetError(hDevice, E_INVALIDARG);
      return;
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
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      const size_t src_off = static_cast<size_t>(src_off_u64);
      const size_t dst_off = static_cast<size_t>(dst_off_u64);
      if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
        SetError(hDevice, E_INVALIDARG);
        return;
      }
      std::memcpy(dst->storage.data() + dst_off, src->storage.data() + src_off, row_bytes);
    }

    // Keep guest-backed staging allocations coherent for CPU readback when the
    // transfer backend is unavailable or stubbed out.
    bool did_staging_upload = false;
    if (copy_width && copy_height &&
        dst->backing_alloc_id != 0 &&
        dst->usage == kD3D10UsageStaging &&
        (dst->cpu_access_flags == 0 ||
         (dst->cpu_access_flags & kD3D10CpuAccessRead) != 0)) {
      EmitUploadLocked(hDevice, dev, dst, dst_sub.offset_bytes, dst_sub.size_bytes);
      did_staging_upload = true;
    }

    if (!aerogpu::d3d10_11::SupportsTransfer(dev)) {
      // No transfer backend: commit the updated bytes to the destination backing
      // store via UPLOAD_RESOURCE (host-owned) or RESOURCE_DIRTY_RANGE (guest-backed).
      if (!did_staging_upload) {
        const uint64_t row_pitch_u64 = static_cast<uint64_t>(dst_sub.row_pitch_bytes);
        const uint64_t upload_offset =
            dst_sub.offset_bytes + static_cast<uint64_t>(dst_y_blocks) * row_pitch_u64;
        const uint64_t upload_size = static_cast<uint64_t>(copy_height_blocks) * row_pitch_u64;
        EmitUploadLocked(hDevice, dev, dst, upload_offset, upload_size);
      }
      return;
    }

    // Transfer backend available: prefer COPY_TEXTURE2D. If we cannot record the
    // packet (OOM while growing the cmd stream or allocation list), fall back to
    // a row-range upload so the host sees the updated bytes.
    const auto cmd_checkpoint = dev->cmd.checkpoint();
    const WddmAllocListCheckpoint alloc_checkpoint(dev);

    bool emitted_copy = false;
    if (did_staging_upload) {
      // COPY_TEXTURE2D is optional when the staging upload fallback already ran.
      if (TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false) &&
          TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true)) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
        if (cmd) {
          cmd->dst_texture = dst->handle;
          cmd->src_texture = src->handle;
          cmd->dst_mip_level = dst_sub.mip_level;
          cmd->dst_array_layer = dst_sub.array_layer;
          cmd->src_mip_level = src_sub.mip_level;
          cmd->src_array_layer = src_sub.array_layer;
          cmd->dst_x = dstX;
          cmd->dst_y = dstY;
          cmd->src_x = src_left;
          cmd->src_y = src_top;
          cmd->width = copy_width;
          cmd->height = copy_height;
          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->backing_alloc_id != 0 &&
              dst->usage == kD3D10UsageStaging &&
              (dst->cpu_access_flags & kD3D10CpuAccessRead) != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          cmd->flags = copy_flags;
          cmd->reserved0 = 0;
          TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { SetError(hDevice, hr); });
          emitted_copy = true;
        }
      }
    } else {
      // Required COPY_TEXTURE2D path: use poisoning tracking, but rollback if it
      // fails so earlier packets can still be submitted safely.
      TrackWddmAllocForSubmitLocked(dev, src, /*write=*/false);
      TrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true);
      if (!dev->wddm_submit_allocation_list_oom) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
        if (cmd) {
          cmd->dst_texture = dst->handle;
          cmd->src_texture = src->handle;
          cmd->dst_mip_level = dst_sub.mip_level;
          cmd->dst_array_layer = dst_sub.array_layer;
          cmd->src_mip_level = src_sub.mip_level;
          cmd->src_array_layer = src_sub.array_layer;
          cmd->dst_x = dstX;
          cmd->dst_y = dstY;
          cmd->src_x = src_left;
          cmd->src_y = src_top;
          cmd->width = copy_width;
          cmd->height = copy_height;
          uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
          if (dst->backing_alloc_id != 0 &&
              dst->usage == kD3D10UsageStaging &&
              (dst->cpu_access_flags & kD3D10CpuAccessRead) != 0) {
            copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
          }
          cmd->flags = copy_flags;
          cmd->reserved0 = 0;
          TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { SetError(hDevice, hr); });
          emitted_copy = true;
        }
      }
    }

    if (emitted_copy) {
      return;
    }

    dev->cmd.rollback(cmd_checkpoint);
    alloc_checkpoint.rollback();

    if (did_staging_upload) {
      // Upload fallback already ran; COPY_TEXTURE2D is purely an optimization.
      return;
    }

    const uint64_t row_pitch_u64 = static_cast<uint64_t>(dst_sub.row_pitch_bytes);
    const uint64_t upload_offset =
        dst_sub.offset_bytes + static_cast<uint64_t>(dst_y_blocks) * row_pitch_u64;
    const uint64_t upload_size = static_cast<uint64_t>(copy_height_blocks) * row_pitch_u64;
    EmitUploadLocked(hDevice, dev, dst, upload_offset, upload_size);
    return;
  }

  SetError(hDevice, E_NOTIMPL);
}

SIZE_T APIENTRY CalcPrivateRenderTargetViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

static bool DxgiViewFormatTriviallyCompatible(const AeroGpuDevice* dev,
                                              uint32_t resource_dxgi_format,
                                              uint32_t view_dxgi_format) {
  // DXGI_FORMAT_UNKNOWN / 0 means "use the resource format".
  if (view_dxgi_format == 0) {
    return true;
  }
  if (resource_dxgi_format == view_dxgi_format) {
    return true;
  }

  // Allow only trivial bit-compatible cases (typeless->typed, srgb->unorm when
  // the device ABI does not expose explicit sRGB formats, etc).
  const uint32_t res_aer = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, resource_dxgi_format);
  const uint32_t view_aer = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
  return res_aer != AEROGPU_FORMAT_INVALID && res_aer == view_aer;
}

static bool AerogpuFormatIsDepth(uint32_t aerogpu_format) {
  return aerogpu_format == AEROGPU_FORMAT_D24_UNORM_S8_UINT ||
         aerogpu_format == AEROGPU_FORMAT_D32_FLOAT;
}

HRESULT APIENTRY CreateRenderTargetView(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                        D3D10DDI_HRENDERTARGETVIEW hView,
                                        D3D10DDI_HRTRENDERTARGETVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyRenderTargetView is safe even if
  // we reject the descriptor (some runtimes may still call Destroy on failure).
  auto* rtv = new (hView.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = 0;
  rtv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
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
  if (!res) {
    return E_INVALIDARG;
  }

  if (res->kind != ResourceKind::Texture2D) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateRenderTargetView: rejecting non-texture2d resource kind=%u (handle=%u)",
                         static_cast<unsigned>(res->kind),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if ((res->bind_flags & kD3D10BindRenderTarget) == 0) {
    // D3D requires the resource to be created with the appropriate bind flag
    // for the view type. Failing here avoids later host-side validation errors.
    AEROGPU_D3D10_11_LOG(
        "D3D10 CreateRenderTargetView: rejecting RTV for resource missing BIND_RENDER_TARGET (bind=0x%08X handle=%u)",
        static_cast<unsigned>(res->bind_flags),
        static_cast<unsigned>(res->handle));
    return E_INVALIDARG;
  }

  // Validate view format (allow only trivial compatible cases).
  uint32_t view_format = 0;
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Format) {
    view_format = static_cast<uint32_t>(pDesc->Format);
  }
  if (!DxgiViewFormatTriviallyCompatible(dev, res->dxgi_format, view_format)) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateRenderTargetView: rejecting format reinterpretation res_fmt=%u view_fmt=%u (handle=%u)",
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(view_format),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }
  {
    const uint32_t resolved_fmt = view_format ? view_format : res->dxgi_format;
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, resolved_fmt);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateRenderTargetView: rejecting RTV with unsupported format res_fmt=%u view_fmt=%u (handle=%u)",
          static_cast<unsigned>(res->dxgi_format),
          static_cast<unsigned>(view_format),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
    if (AerogpuFormatIsDepth(aer_fmt)) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateRenderTargetView: rejecting RTV for depth format res_fmt=%u view_fmt=%u (handle=%u)",
          static_cast<unsigned>(res->dxgi_format),
          static_cast<unsigned>(view_format),
          static_cast<unsigned>(res->handle));
      return E_INVALIDARG;
    }
  }

  // Enforce "subresource 0" RTVs only (MipSlice==0, no arrays).
  uint32_t view_dim = 0;
  bool have_view_dim = false;
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_view_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_view_dim = true;
    }
  }

  if (have_view_dim) {
    if (!D3dViewDimensionIsTexture2D(view_dim) && !D3dViewDimensionIsTexture2DArray(view_dim)) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateRenderTargetView: rejecting RTV dimension=%u (only Texture2D/Texture2DArray supported handle=%u)",
          static_cast<unsigned>(view_dim),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  } else {
    // If the header exposes array/MSAA RTV union variants but does not expose a
    // view dimension discriminator, we cannot safely determine which union
    // member is active. Reject to avoid accidentally accepting a subresource or
    // MSAA view and silently binding the whole resource.
    bool has_ambiguous_union = false;
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DMSArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DMSArray) { has_ambiguous_union = true; }
    if (has_ambiguous_union) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateRenderTargetView: rejecting RTV (missing view dimension discriminator handle=%u)",
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  }

  const bool view_is_array = have_view_dim && D3dViewDimensionIsTexture2DArray(view_dim);

  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
  // Field names/union layouts vary across WDK vintages.
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::MipSlice) {
    mip_slice = static_cast<uint32_t>(pDesc->MipSlice);
    have_mip_slice = true;
  }
  __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::MipSlice) {
    if (view_is_array) {
      __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2DArray.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2DArray.MipSlice);
          have_mip_slice = true;
        }
      }
    } else {
      __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2D) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2D.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2D) {
        __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2D) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2D.MipSlice);
          have_mip_slice = true;
        }
      }
    }
  }

  if (!have_mip_slice) {
    return E_NOTIMPL;
  }

  if (mip_slice >= res->mip_levels) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateRenderTargetView: rejecting invalid mip_slice=%u (res mips=%u)",
                         static_cast<unsigned>(mip_slice),
                         static_cast<unsigned>(res->mip_levels));
    return E_INVALIDARG;
  }

  uint32_t first_slice = 0;
  uint32_t slice_count = res->array_size;
  bool have_slice_range = !view_is_array;
  if (view_is_array) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      slice_count = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
  }

  if (!have_slice_range) {
    return E_NOTIMPL;
  }

  slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

  if (first_slice >= res->array_size || slice_count == 0 || first_slice + slice_count > res->array_size) {
    return E_INVALIDARG;
  }

  const uint32_t view_dxgi_format = view_format ? view_format : res->dxgi_format;
  const bool format_reinterpret = (view_format != 0) && (view_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || mip_slice != 0 || first_slice != 0 || slice_count != res->array_size;
  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }

  rtv->resource = res;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(rtv);
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(rtv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = first_slice;
    cmd->array_layer_count = slice_count;
    cmd->reserved0 = 0;

    rtv->texture = view_handle;
  }

  return S_OK;
}

void APIENTRY DestroyRenderTargetView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(hDevice, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  view->~AeroGpuRenderTargetView();
  new (view) AeroGpuRenderTargetView();
}

SIZE_T APIENTRY CalcPrivateDepthStencilViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT APIENTRY CreateDepthStencilView(D3D10DDI_HDEVICE hDevice,
                                        const D3D10DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                        D3D10DDI_HDEPTHSTENCILVIEW hView,
                                        D3D10DDI_HRTDEPTHSTENCILVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyDepthStencilView is safe even if
  // we reject the descriptor.
  auto* dsv = new (hView.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = 0;
  dsv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
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
  if (!res) {
    return E_INVALIDARG;
  }

  if (res->kind != ResourceKind::Texture2D) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateDepthStencilView: rejecting non-texture2d resource kind=%u (handle=%u)",
                         static_cast<unsigned>(res->kind),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if ((res->bind_flags & kD3D10BindDepthStencil) == 0) {
    AEROGPU_D3D10_11_LOG(
        "D3D10 CreateDepthStencilView: rejecting DSV for resource missing BIND_DEPTH_STENCIL (bind=0x%08X handle=%u)",
        static_cast<unsigned>(res->bind_flags),
        static_cast<unsigned>(res->handle));
    return E_INVALIDARG;
  }

  // Validate view format (allow only trivial compatible cases).
  uint32_t view_format = 0;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Format) {
    view_format = static_cast<uint32_t>(pDesc->Format);
  }
  if (!DxgiViewFormatTriviallyCompatible(dev, res->dxgi_format, view_format)) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateDepthStencilView: rejecting format reinterpretation res_fmt=%u view_fmt=%u (handle=%u)",
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(view_format),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }
  {
    const uint32_t resolved_fmt = view_format ? view_format : res->dxgi_format;
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, resolved_fmt);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateDepthStencilView: rejecting DSV with unsupported format res_fmt=%u view_fmt=%u (handle=%u)",
          static_cast<unsigned>(res->dxgi_format),
          static_cast<unsigned>(view_format),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
    if (!AerogpuFormatIsDepth(aer_fmt)) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateDepthStencilView: rejecting DSV for non-depth format res_fmt=%u view_fmt=%u (handle=%u)",
          static_cast<unsigned>(res->dxgi_format),
          static_cast<unsigned>(view_format),
          static_cast<unsigned>(res->handle));
      return E_INVALIDARG;
    }
  }

  // Enforce "subresource 0" DSVs only (MipSlice==0, no arrays).
  uint32_t view_dim = 0;
  bool have_view_dim = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_view_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_view_dim = true;
    }
  }
  if (have_view_dim) {
    if (!D3dViewDimensionIsTexture2D(view_dim) && !D3dViewDimensionIsTexture2DArray(view_dim)) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateDepthStencilView: rejecting DSV dimension=%u (only Texture2D/Texture2DArray supported handle=%u)",
          static_cast<unsigned>(view_dim),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  } else {
    // If the header exposes array/MSAA DSV union variants but does not expose a
    // view dimension discriminator, we cannot safely determine which union
    // member is active. Reject to avoid accidentally accepting a subresource or
    // MSAA view and silently binding the whole resource.
    bool has_ambiguous_union = false;
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DMSArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DMSArray) { has_ambiguous_union = true; }
    if (has_ambiguous_union) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateDepthStencilView: rejecting DSV (missing view dimension discriminator handle=%u)",
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  }

  const bool view_is_array = have_view_dim && D3dViewDimensionIsTexture2DArray(view_dim);

  // Some newer headers expose depth-stencil view flags (read-only depth/stencil).
  // The current command stream has no way to encode this; reject if requested.
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Flags) {
    uint32_t flags = 0;
    bool have_flags = false;
    using FlagsT = decltype(pDesc->Flags);
    __if_exists(FlagsT::Value) {
      flags = static_cast<uint32_t>(pDesc->Flags.Value);
      have_flags = true;
    }
    __if_not_exists(FlagsT::Value) {
      flags = static_cast<uint32_t>(pDesc->Flags);
      have_flags = true;
    }
    if (have_flags && flags != 0) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateDepthStencilView: rejecting DSV flags=0x%08X (unsupported handle=%u)",
                           static_cast<unsigned>(flags),
                           static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  }

  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::MipSlice) {
    mip_slice = static_cast<uint32_t>(pDesc->MipSlice);
    have_mip_slice = true;
  }
  __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::MipSlice) {
    if (view_is_array) {
      __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2DArray.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2DArray.MipSlice);
          have_mip_slice = true;
        }
      }
    } else {
      __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2D) {
        mip_slice = static_cast<uint32_t>(pDesc->Tex2D.MipSlice);
        have_mip_slice = true;
      }
      __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2D) {
        __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2D) {
          mip_slice = static_cast<uint32_t>(pDesc->Texture2D.MipSlice);
          have_mip_slice = true;
        }
      }
    }
  }
  if (!have_mip_slice) {
    return E_NOTIMPL;
  }

  if (mip_slice >= res->mip_levels) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateDepthStencilView: rejecting invalid mip_slice=%u (res mips=%u)",
                         static_cast<unsigned>(mip_slice),
                         static_cast<unsigned>(res->mip_levels));
    return E_INVALIDARG;
  }

  uint32_t first_slice = 0;
  uint32_t slice_count = res->array_size;
  bool have_slice_range = !view_is_array;
  if (view_is_array) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      slice_count = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
  }

  if (!have_slice_range) {
    return E_NOTIMPL;
  }

  slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

  if (first_slice >= res->array_size || slice_count == 0 || first_slice + slice_count > res->array_size) {
    return E_INVALIDARG;
  }

  const uint32_t view_dxgi_format = view_format ? view_format : res->dxgi_format;
  const bool format_reinterpret = (view_format != 0) && (view_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || mip_slice != 0 || first_slice != 0 || slice_count != res->array_size;
  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }

  dsv->resource = res;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(dsv);
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(dsv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = first_slice;
    cmd->array_layer_count = slice_count;
    cmd->reserved0 = 0;

    dsv->texture = view_handle;
  }

  return S_OK;
}

void APIENTRY DestroyDepthStencilView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HDEPTHSTENCILVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(hDevice, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  view->~AeroGpuDepthStencilView();
  new (view) AeroGpuDepthStencilView();
}

SIZE_T APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
                                          const D3D10DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                          D3D10DDI_HSHADERRESOURCEVIEW hView,
                                          D3D10DDI_HRTSHADERRESOURCEVIEW) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyShaderResourceView is safe even
  // if we reject the descriptor.
  auto* srv = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  srv->texture = 0;
  srv->resource = nullptr;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
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
  if (!res) {
    return E_INVALIDARG;
  }

  if (res->kind != ResourceKind::Texture2D) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateShaderResourceView: rejecting non-texture2d SRV resource kind=%u (handle=%u)",
                         static_cast<unsigned>(res->kind),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if ((res->bind_flags & kD3D10BindShaderResource) == 0) {
    AEROGPU_D3D10_11_LOG(
        "D3D10 CreateShaderResourceView: rejecting SRV for resource missing BIND_SHADER_RESOURCE (bind=0x%08X handle=%u)",
        static_cast<unsigned>(res->bind_flags),
        static_cast<unsigned>(res->handle));
    return E_INVALIDARG;
  }

  uint32_t view_format = 0;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Format) {
    view_format = static_cast<uint32_t>(pDesc->Format);
  }
  if (!DxgiViewFormatTriviallyCompatible(dev, res->dxgi_format, view_format)) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateShaderResourceView: rejecting format reinterpretation res_fmt=%u view_fmt=%u (handle=%u)",
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(view_format),
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  uint32_t view_dim = 0;
  bool have_view_dim = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_view_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_view_dim = true;
    }
  }

  if (have_view_dim) {
    if (!D3dViewDimensionIsTexture2D(view_dim) && !D3dViewDimensionIsTexture2DArray(view_dim)) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateShaderResourceView: rejecting SRV dimension=%u (only Texture2D/Texture2DArray supported handle=%u)",
          static_cast<unsigned>(view_dim),
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  } else {
    // If the header exposes array/MSAA SRV union variants but does not expose a
    // view dimension discriminator, we cannot safely determine which union
    // member is active.
    bool has_ambiguous_union = false;
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DMSArray) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DMS) { has_ambiguous_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DMSArray) { has_ambiguous_union = true; }
    if (has_ambiguous_union) {
      AEROGPU_D3D10_11_LOG(
          "D3D10 CreateShaderResourceView: rejecting SRV (missing view dimension discriminator handle=%u)",
          static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  }

  const bool view_is_array = have_view_dim && D3dViewDimensionIsTexture2DArray(view_dim);

  // Some WDK versions expose SRV flags (e.g. RAW buffer views). The current
  // AeroGPU D3D10 path has no way to encode SRV flags in the command stream;
  // reject if requested.
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Flags) {
    uint32_t flags = 0;
    bool have_flags = false;
    using FlagsT = decltype(pDesc->Flags);
    __if_exists(FlagsT::Value) {
      flags = static_cast<uint32_t>(pDesc->Flags.Value);
      have_flags = true;
    }
    __if_not_exists(FlagsT::Value) {
      flags = static_cast<uint32_t>(pDesc->Flags);
      have_flags = true;
    }
    if (have_flags && flags != 0) {
      AEROGPU_D3D10_11_LOG("D3D10 CreateShaderResourceView: rejecting SRV flags=0x%08X (unsupported handle=%u)",
                           static_cast<unsigned>(flags),
                           static_cast<unsigned>(res->handle));
      return E_NOTIMPL;
    }
  }

  uint32_t most_detailed_mip = 0;
  uint32_t mip_levels = 0;
  bool have_most_detailed_mip = false;
  bool have_mip_levels = false;

  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
    most_detailed_mip = static_cast<uint32_t>(pDesc->MostDetailedMip);
    have_most_detailed_mip = true;
  }
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MipLevels) {
    mip_levels = static_cast<uint32_t>(pDesc->MipLevels);
    have_mip_levels = true;
  }

  if (!have_most_detailed_mip || !have_mip_levels) {
    if (view_is_array) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2DArray.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2DArray.MipLevels);
        have_most_detailed_mip = true;
        have_mip_levels = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2DArray.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2DArray.MipLevels);
          have_most_detailed_mip = true;
          have_mip_levels = true;
        }
      }
    } else {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2D.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2D.MipLevels);
        have_most_detailed_mip = true;
        have_mip_levels = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2D) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2D.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2D.MipLevels);
          have_most_detailed_mip = true;
          have_mip_levels = true;
        }
      }
    }
  }

  if (!have_most_detailed_mip || !have_mip_levels) {
    AEROGPU_D3D10_11_LOG("D3D10 CreateShaderResourceView: rejecting SRV (missing mip range fields handle=%u)",
                         static_cast<unsigned>(res->handle));
    return E_NOTIMPL;
  }

  uint32_t mip_count = D3dViewCountToRemaining(most_detailed_mip, mip_levels, res->mip_levels);

  if (res->mip_levels == 0 ||
      most_detailed_mip >= res->mip_levels ||
      mip_count == 0 ||
      most_detailed_mip + mip_count > res->mip_levels) {
    return E_INVALIDARG;
  }

  uint32_t first_slice = 0;
  uint32_t slice_count = res->array_size;
  bool have_slice_range = !view_is_array;
  if (view_is_array) {
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      slice_count = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        slice_count = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          slice_count = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
  }

  if (!have_slice_range) {
    return E_NOTIMPL;
  }

  slice_count = D3dViewCountToRemaining(first_slice, slice_count, res->array_size);

  if (res->array_size == 0 ||
      first_slice >= res->array_size ||
      slice_count == 0 ||
      first_slice + slice_count > res->array_size) {
    return E_INVALIDARG;
  }

  const uint32_t view_dxgi_format = view_format ? view_format : res->dxgi_format;
  const bool format_reinterpret = (view_format != 0) && (view_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret ||
      most_detailed_mip != 0 ||
      mip_count != res->mip_levels ||
      first_slice != 0 ||
      slice_count != res->array_size;
  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }
  srv->resource = res;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(srv);
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(srv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = most_detailed_mip;
    cmd->mip_level_count = mip_count;
    cmd->base_array_layer = first_slice;
    cmd->array_layer_count = slice_count;
    cmd->reserved0 = 0;

    srv->texture = view_handle;
  }

  return S_OK;
}

void APIENTRY DestroyShaderResourceView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        SetError(hDevice, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  view->~AeroGpuShaderResourceView();
  new (view) AeroGpuShaderResourceView();
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

static size_t dxbc_size_from_header_noexcept(const void* pCode) {
  __try {
    return dxbc_size_from_header(pCode);
  } __except (EXCEPTION_EXECUTE_HANDLER) {
    return 0;
  }
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
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the shader so Destroy*Shader is safe even if CreateShader
  // fails early.
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();

  if (!hDevice.pDrvPrivate || !pCode || !code_size) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(sh);
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  sh->handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
  if (sh->handle == kInvalidHandle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still call
    // Destroy* after a failed Create* probe, and double-destruction would be
    // unsafe.
    return E_FAIL;
  }
  sh->stage = stage;
  try {
    sh->dxbc.resize(code_size);
  } catch (...) {
    ResetObject(sh);
    return E_OUTOFMEMORY;
  }
  std::memcpy(sh->dxbc.data(), pCode, code_size);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  if (!cmd) {
    ResetObject(sh);
    SetError(hDevice, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

template <typename FnPtr>
struct CalcPrivateGeometryShaderWithStreamOutputSizeImpl;

template <typename Ret, typename... Args>
struct CalcPrivateGeometryShaderWithStreamOutputSizeImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args...) {
    return static_cast<Ret>(sizeof(AeroGpuShader));
  }
};

template <typename FnPtr>
struct CreateGeometryShaderWithStreamOutputImpl;

template <typename Ret, typename... Args>
struct CreateGeometryShaderWithStreamOutputImpl<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) {
    D3D10DDI_HDEVICE hDevice{};
    D3D10DDI_HSHADER hShader{};
    const void* shader_code = nullptr;
    size_t shader_code_size = 0;

    auto capture = [&](auto v) {
      using T = std::decay_t<decltype(v)>;
      if constexpr (std::is_same_v<T, D3D10DDI_HDEVICE>) {
        hDevice = v;
      } else if constexpr (std::is_same_v<T, D3D10DDI_HSHADER>) {
        hShader = v;
      } else if constexpr (std::is_pointer_v<T>) {
        if (!v || shader_code) {
          return;
        }

        using Pointee = std::remove_pointer_t<T>;
        if constexpr (std::is_void_v<Pointee> || std::is_arithmetic_v<Pointee> || std::is_enum_v<Pointee>) {
          const void* maybe_code = static_cast<const void*>(v);
          const size_t maybe_size = dxbc_size_from_header_noexcept(maybe_code);
          if (maybe_size) {
            shader_code = maybe_code;
            shader_code_size = maybe_size;
          }
          return;
        }

        // D3D10 WDK shader create args structs are not stable across SDK
        // revisions, but they consistently begin with a DXBC pointer. Read the
        // first field to recover the bytecode.
        const void* code = nullptr;
        std::memcpy(&code, v, sizeof(code));
        const size_t size = dxbc_size_from_header_noexcept(code);
        if (size) {
          shader_code = code;
          shader_code_size = size;
        }
      }
    };
    (capture(args), ...);

    return static_cast<Ret>(CreateShaderCommon(hDevice, shader_code, shader_code_size, hShader, AEROGPU_SHADER_STAGE_GEOMETRY));
  }
};

HRESULT APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                    const D3D10DDIARG_CREATEVERTEXSHADER* pDesc,
                                    D3D10DDI_HSHADER hShader,
                                    D3D10DDI_HRTSHADER) {
  const void* code = nullptr;
  size_t size = 0;
  if (pDesc) {
    std::memcpy(&code, pDesc, sizeof(code));
    size = dxbc_size_from_header(code);
  }
  return CreateShaderCommon(hDevice, code, size, hShader, AEROGPU_SHADER_STAGE_VERTEX);
}

HRESULT APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                   const D3D10DDIARG_CREATEPIXELSHADER* pDesc,
                                   D3D10DDI_HSHADER hShader,
                                   D3D10DDI_HRTSHADER) {
  const void* code = nullptr;
  size_t size = 0;
  if (pDesc) {
    std::memcpy(&code, pDesc, sizeof(code));
    size = dxbc_size_from_header(code);
  }
  return CreateShaderCommon(hDevice, code, size, hShader, AEROGPU_SHADER_STAGE_PIXEL);
}

HRESULT APIENTRY CreateGeometryShader(D3D10DDI_HDEVICE hDevice,
                                      const D3D10DDIARG_CREATEGEOMETRYSHADER* pDesc,
                                      D3D10DDI_HSHADER hShader,
                                      D3D10DDI_HRTSHADER) {
  const void* code = nullptr;
  size_t size = 0;
  if (pDesc) {
    std::memcpy(&code, pDesc, sizeof(code));
    size = dxbc_size_from_header(code);
  }
  return CreateShaderCommon(hDevice, code, size, hShader, AEROGPU_SHADER_STAGE_GEOMETRY);
}

void DestroyShaderCommon(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  auto* sh = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader);
  if (!sh) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(sh);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    ResetObject(sh);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (sh->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    if (cmd) {
      cmd->shader_handle = sh->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(hDevice, E_OUTOFMEMORY);
    }
  }
  ResetObject(sh);
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
  if (!hLayout.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();

  if (!hDevice.pDrvPrivate || !pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(layout);
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  layout->handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
  if (!layout->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still call
    // Destroy* after a failed Create* probe.
    ResetObject(layout);
    return E_FAIL;
  }

  if (pDesc->NumElements && !pDesc->pVertexElements) {
    ResetObject(layout);
    return E_INVALIDARG;
  }

  const size_t header_size = sizeof(aerogpu_input_layout_blob_header);
  const size_t elem_size = sizeof(aerogpu_input_layout_element_dxgi);
  if (pDesc->NumElements > (SIZE_MAX - header_size) / elem_size) {
    ResetObject(layout);
    return E_OUTOFMEMORY;
  }

  const size_t blob_size = header_size + static_cast<size_t>(pDesc->NumElements) * elem_size;
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    ResetObject(layout);
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
  if (!cmd) {
    ResetObject(layout);
    SetError(hDevice, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void APIENTRY DestroyElementLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  auto* layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  if (!layout) {
    return;
  }

  if (!IsDeviceLive(hDevice)) {
    ResetObject(layout);
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    ResetObject(layout);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (layout->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    if (cmd) {
      cmd->input_layout_handle = layout->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(hDevice, E_OUTOFMEMORY);
    }
  }
  ResetObject(layout);
}

SIZE_T APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEBLENDSTATE*) {
  return sizeof(AeroGpuBlendState);
}
HRESULT APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                  const D3D10DDIARG_CREATEBLENDSTATE* pDesc,
                                  D3D10DDI_HBLENDSTATE hState,
                                  D3D10DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }

  aerogpu::d3d10_11::AerogpuBlendStateBase base{};
  // Always construct the state object so DestroyBlendState is safe even if we
  // reject the descriptor (some runtimes may still call Destroy on failure).
  auto* s = new (hState.pDrvPrivate) AeroGpuBlendState();
  s->state = base;
  const auto fail = [&](HRESULT hr) -> HRESULT {
    s->~AeroGpuBlendState();
    new (s) AeroGpuBlendState();
    return hr;
  };

  if (pDesc) {
    aerogpu::d3d10_11::D3dRtBlendDesc rts[AEROGPU_MAX_RENDER_TARGETS]{};
    bool alpha_to_coverage = false;

    bool filled = FillBlendRtDescsFromDesc(*pDesc, rts, AEROGPU_MAX_RENDER_TARGETS, &alpha_to_coverage);
    if (!filled) {
      __if_exists(D3D10DDIARG_CREATEBLENDSTATE::BlendDesc) {
        filled = FillBlendRtDescsFromDesc(pDesc->BlendDesc, rts, AEROGPU_MAX_RENDER_TARGETS, &alpha_to_coverage);
      }
    }
    if (!filled) {
      __if_exists(D3D10DDIARG_CREATEBLENDSTATE::Desc) {
        filled = FillBlendRtDescsFromDesc(pDesc->Desc, rts, AEROGPU_MAX_RENDER_TARGETS, &alpha_to_coverage);
      }
    }
    if (!filled) {
      __if_exists(D3D10DDIARG_CREATEBLENDSTATE::pBlendDesc) {
        if (pDesc->pBlendDesc) {
          filled = FillBlendRtDescsFromDesc(*pDesc->pBlendDesc, rts, AEROGPU_MAX_RENDER_TARGETS, &alpha_to_coverage);
        }
      }
    }

    // Some WDK header vintages wrap the blend descriptor differently. If we
    // cannot extract a recognized descriptor variant, fall back to D3D10
    // defaults instead of failing CreateBlendState.
    if (filled) {
      const HRESULT hr =
          aerogpu::d3d10_11::ValidateAndConvertBlendDesc(rts, AEROGPU_MAX_RENDER_TARGETS, alpha_to_coverage, &base);
      if (FAILED(hr)) {
        return fail(hr);
      }
    }
  }

  s->state = base;
  return S_OK;
}
void APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
  new (s) AeroGpuBlendState();
}

SIZE_T APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERASTERIZERSTATE*) {
  return sizeof(AeroGpuRasterizerState);
}
HRESULT APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                       const D3D10DDIARG_CREATERASTERIZERSTATE* pDesc,
                                       D3D10DDI_HRASTERIZERSTATE hState,
                                       D3D10DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* state = new (hState.pDrvPrivate) AeroGpuRasterizerState();
  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D10DDIARG_CREATERASTERIZERSTATE::CullMode) {
    state->fill_mode = static_cast<uint32_t>(pDesc->FillMode);
    state->cull_mode = static_cast<uint32_t>(pDesc->CullMode);
    state->front_ccw = pDesc->FrontCounterClockwise ? 1u : 0u;
    state->scissor_enable = pDesc->ScissorEnable ? 1u : 0u;
    state->depth_bias = static_cast<int32_t>(pDesc->DepthBias);
    state->depth_clip_enable = pDesc->DepthClipEnable ? 1u : 0u;
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATERASTERIZERSTATE::RasterizerDesc) {
      const auto& desc = pDesc->RasterizerDesc;
      state->fill_mode = static_cast<uint32_t>(desc.FillMode);
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_bias = static_cast<int32_t>(desc.DepthBias);
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATERASTERIZERSTATE::Desc) {
      const auto& desc = pDesc->Desc;
      state->fill_mode = static_cast<uint32_t>(desc.FillMode);
      state->cull_mode = static_cast<uint32_t>(desc.CullMode);
      state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
      state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
      state->depth_bias = static_cast<int32_t>(desc.DepthBias);
      state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATERASTERIZERSTATE::pRasterizerDesc) {
      if (pDesc->pRasterizerDesc) {
        const auto& desc = *pDesc->pRasterizerDesc;
        state->fill_mode = static_cast<uint32_t>(desc.FillMode);
        state->cull_mode = static_cast<uint32_t>(desc.CullMode);
        state->front_ccw = desc.FrontCounterClockwise ? 1u : 0u;
        state->scissor_enable = desc.ScissorEnable ? 1u : 0u;
        state->depth_bias = static_cast<int32_t>(desc.DepthBias);
        state->depth_clip_enable = desc.DepthClipEnable ? 1u : 0u;
        filled = true;
      }
    }
  }
  return S_OK;
}
void APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
  new (s) AeroGpuRasterizerState();
}

SIZE_T APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILSTATE*) {
  return sizeof(AeroGpuDepthStencilState);
}
HRESULT APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                         const D3D10DDIARG_CREATEDEPTHSTENCILSTATE* pDesc,
                                         D3D10DDI_HDEPTHSTENCILSTATE hState,
                                         D3D10DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* state = new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  if (!pDesc) {
    return S_OK;
  }

  bool filled = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILSTATE::DepthEnable) {
    state->depth_enable = pDesc->DepthEnable ? 1u : 0u;
    state->depth_write_mask = static_cast<uint32_t>(pDesc->DepthWriteMask);
    state->depth_func = static_cast<uint32_t>(pDesc->DepthFunc);
    state->stencil_enable = pDesc->StencilEnable ? 1u : 0u;
    state->stencil_read_mask = static_cast<uint8_t>(pDesc->StencilReadMask);
    state->stencil_write_mask = static_cast<uint8_t>(pDesc->StencilWriteMask);
    filled = true;
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILSTATE::DepthStencilDesc) {
      const auto& desc = pDesc->DepthStencilDesc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
      state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILSTATE::Desc) {
      const auto& desc = pDesc->Desc;
      state->depth_enable = desc.DepthEnable ? 1u : 0u;
      state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
      state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
      state->stencil_enable = desc.StencilEnable ? 1u : 0u;
      state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
      state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
      filled = true;
    }
  }
  if (!filled) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILSTATE::pDepthStencilDesc) {
      if (pDesc->pDepthStencilDesc) {
        const auto& desc = *pDesc->pDepthStencilDesc;
        state->depth_enable = desc.DepthEnable ? 1u : 0u;
        state->depth_write_mask = static_cast<uint32_t>(desc.DepthWriteMask);
        state->depth_func = static_cast<uint32_t>(desc.DepthFunc);
        state->stencil_enable = desc.StencilEnable ? 1u : 0u;
        state->stencil_read_mask = static_cast<uint8_t>(desc.StencilReadMask);
        state->stencil_write_mask = static_cast<uint8_t>(desc.StencilWriteMask);
        filled = true;
      }
    }
  }
  return S_OK;
}
void APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
  new (s) AeroGpuDepthStencilState();
}

SIZE_T APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}
HRESULT APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                               const D3D10DDIARG_CREATESAMPLER* pDesc,
                               D3D10DDI_HSAMPLER hSampler,
                               D3D10DDI_HRTSAMPLER) {
  if (!hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the sampler so DestroySampler is safe even if CreateSampler
  // fails early.
  auto* sampler = new (hSampler.pDrvPrivate) AeroGpuSampler();

  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(sampler);
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  sampler->handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
  if (!sampler->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still call
    // Destroy* after a failed Create* probe.
    ResetObject(sampler);
    return E_FAIL;
  }

  InitSamplerFromCreateSamplerArg(sampler, pDesc);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    ResetObject(sampler);
    SetError(hDevice, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->sampler_handle = sampler->handle;
  cmd->filter = sampler->filter;
  cmd->address_u = sampler->address_u;
  cmd->address_v = sampler->address_v;
  cmd->address_w = sampler->address_w;
  return S_OK;
}

void APIENTRY DestroySampler(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSAMPLER hSampler) {
  auto* sampler = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  if (!sampler) {
    return;
  }
  if (!IsDeviceLive(hDevice)) {
    ResetObject(sampler);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    ResetObject(sampler);
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  if (sampler->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_sampler>(AEROGPU_CMD_DESTROY_SAMPLER);
    if (cmd) {
      cmd->sampler_handle = sampler->handle;
      cmd->reserved0 = 0;
    } else {
      SetError(hDevice, E_OUTOFMEMORY);
    }
  }
  ResetObject(sampler);
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

  if (!aerogpu::d3d10_11::EmitSetInputLayoutCmdLocked(dev,
                                                      handle,
                                                      [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }
  dev->current_input_layout = handle;
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

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (startSlot > kMaxVertexBufferSlots) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  // D3D10 allows updating any subrange of IA vertex buffer slots.
  UINT bind_count = numBuffers;
  if (bind_count != 0) {
    if (!phBuffers || !pStrides || !pOffsets) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    if (startSlot >= kMaxVertexBufferSlots) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
    if (bind_count > (kMaxVertexBufferSlots - startSlot)) {
      SetError(hDevice, E_INVALIDARG);
      return;
    }
  } else {
    // Treat NumBuffers==0 as an unbind request from StartSlot to the end of the
    // slot range (used by some D3D10 runtimes for state clearing).
    if (startSlot == kMaxVertexBufferSlots) {
      return;
    }
    bind_count = kMaxVertexBufferSlots - startSlot;
  }

  std::array<aerogpu_vertex_buffer_binding, kMaxVertexBufferSlots> bindings{};
  std::array<AeroGpuResource*, kMaxVertexBufferSlots> new_resources{};
  AeroGpuResource* new_vb_res0 = dev->current_vb_res;
  uint32_t new_vb_stride0 = dev->current_vb_stride;
  uint32_t new_vb_offset0 = dev->current_vb_offset;
  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(startSlot + i);

    aerogpu_vertex_buffer_binding b{};
    AeroGpuResource* vb_res = nullptr;
    if (numBuffers != 0) {
      vb_res = phBuffers[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phBuffers[i]) : nullptr;
      b.buffer = vb_res ? vb_res->handle : 0;
      b.stride_bytes = pStrides[i];
      b.offset_bytes = pOffsets[i];
    } else {
      b.buffer = 0;
      b.stride_bytes = 0;
      b.offset_bytes = 0;
    }
    b.reserved0 = 0;
    bindings[i] = b;

    if (slot == 0) {
      new_vb_res0 = vb_res;
      new_vb_stride0 = b.stride_bytes;
      new_vb_offset0 = b.offset_bytes;
    }
    new_resources[i] = vb_res;
  }

  if (!aerogpu::d3d10_11::EmitSetVertexBuffersCmdLocked(dev,
                                                        static_cast<uint32_t>(startSlot),
                                                        static_cast<uint32_t>(bind_count),
                                                        bindings.data(),
                                                        [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }

  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(startSlot + i);
    dev->current_vb_resources[slot] = new_resources[i];
    TrackWddmAllocForSubmitLocked(dev, new_resources[i], /*write=*/false);
  }
  dev->current_vb_res = new_vb_res0;
  dev->current_vb_stride = new_vb_stride0;
  dev->current_vb_offset = new_vb_offset0;
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

  if (!aerogpu::d3d10_11::EmitSetIndexBufferCmdLocked(
          dev,
          ib_res ? ib_res->handle : 0,
          dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format)),
          offset,
          [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }
  dev->current_ib_res = ib_res;
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
  (void)aerogpu::d3d10_11::SetPrimitiveTopologyLocked(dev,
                                                      topo_u32,
                                                      [&](HRESULT hr) { SetError(hDevice, hr); });
}

static bool EmitBindShadersCmdLocked(AeroGpuDevice* dev,
                                     aerogpu_handle_t vs,
                                     aerogpu_handle_t ps,
                                     aerogpu_handle_t gs) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.bind_shaders_with_gs(vs, ps, /*cs=*/0, gs);
  if (!cmd) {
    D3D10DDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = dev;
    SetError(hDevice, E_OUTOFMEMORY);
    return false;
  }
  return true;
}

[[maybe_unused]] static bool EmitBindShadersLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return false;
  }
  return EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, dev->current_gs);
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
  const aerogpu_handle_t new_vs = hShader.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, new_vs, dev->current_ps, dev->current_gs)) {
    return;
  }
  dev->current_vs = new_vs;
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
  const aerogpu_handle_t new_ps = hShader.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, new_ps, dev->current_gs)) {
    return;
  }
  dev->current_ps = new_ps;
}

void APIENTRY GsSetShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
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
  const aerogpu_handle_t new_gs = hShader.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, new_gs)) {
    return;
  }
  dev->current_gs = new_gs;
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
  } else if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    bound_resources = dev->current_gs_cb_resources;
  }

  // D3D10 constant buffer bindings are limited to 14 slots; avoid heap
  // allocations in this hot path so OOM cannot surface as an unexpected C++
  // exception.
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> bindings{};
  std::array<AeroGpuResource*, kMaxConstantBufferSlots> new_resources{};
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
      b.size_bytes = ClampU64ToU32(res->size_bytes);
    }

    bindings[i] = b;
    new_resources[i] = buf_res;
  }

  if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                          shader_stage,
                                                          static_cast<uint32_t>(start_slot),
                                                          static_cast<uint32_t>(buffer_count),
                                                          bindings.data(),
                                                          [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }

  for (UINT i = 0; i < buffer_count; i++) {
    table[start_slot + i] = bindings[i];
    if (bound_resources) {
      bound_resources[start_slot + i] = new_resources[i];
    }
  }
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

void APIENTRY GsSetConstantBuffers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numBuffers, const D3D10DDI_HRESOURCE* phBuffers) {
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
  SetConstantBuffersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, startSlot, numBuffers, phBuffers);
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
      tex = view ? (view->texture ? view->texture : (srv_res ? srv_res->handle : 0)) : 0;
    }
    if (tex != 0 || srv_res) {
      if (!UnbindResourceFromOutputsLocked(dev, tex, srv_res)) {
        return;
      }
    }
    if (!SetShaderResourceSlotLocked(dev, shader_stage, slot, tex)) {
      return;
    }
    if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
      if (dev->vs_srvs[slot] == tex) {
        dev->current_vs_srv_resources[slot] = srv_res;
      }
    } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
      if (dev->ps_srvs[slot] == tex) {
        dev->current_ps_srv_resources[slot] = srv_res;
      }
    } else if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
      if (dev->gs_srvs[slot] == tex) {
        dev->current_gs_srv_resources[slot] = srv_res;
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
    if (!SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, 0)) {
      return;
    }
    if (dev->vs_srvs[slot] == 0) {
      dev->current_vs_srv_resources[slot] = nullptr;
    }
    if (!SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, 0)) {
      return;
    }
    if (dev->ps_srvs[slot] == 0) {
      dev->current_ps_srv_resources[slot] = nullptr;
    }
    if (!SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_GEOMETRY, slot, 0)) {
      return;
    }
    if (dev->gs_srvs[slot] == 0) {
      dev->current_gs_srv_resources[slot] = nullptr;
    }
  }

  auto clear_constant_buffers = [&](uint32_t shader_stage,
                                    aerogpu_constant_buffer_binding* table,
                                    AeroGpuResource** resources) -> bool {
    if (!table || !resources) {
      return true;
    }
    bool any = false;
    for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
      if (table[slot].buffer != 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      std::memset(resources, 0, sizeof(*resources) * kMaxConstantBufferSlots);
      return true;
    }

    aerogpu_constant_buffer_binding zeros[kMaxConstantBufferSlots] = {};
    if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                            shader_stage,
                                                            /*start_slot=*/0,
                                                            /*buffer_count=*/kMaxConstantBufferSlots,
                                                            zeros,
                                                            [&](HRESULT hr) { SetError(hDevice, hr); })) {
      return false;
    }

    std::memset(table, 0, sizeof(*table) * kMaxConstantBufferSlots);
    std::memset(resources, 0, sizeof(*resources) * kMaxConstantBufferSlots);
    return true;
  };

  if (!clear_constant_buffers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_constant_buffers, dev->current_vs_cb_resources)) {
    return;
  }
  if (!clear_constant_buffers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_constant_buffers, dev->current_ps_cb_resources)) {
    return;
  }
  if (!clear_constant_buffers(AEROGPU_SHADER_STAGE_GEOMETRY, dev->gs_constant_buffers, dev->current_gs_cb_resources)) {
    return;
  }

  auto clear_samplers = [&](uint32_t shader_stage, aerogpu_handle_t* table) -> bool {
    if (!table) {
      return true;
    }
    bool any = false;
    for (uint32_t slot = 0; slot < kMaxSamplerSlots; ++slot) {
      if (table[slot] != 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      return true;
    }

    aerogpu_handle_t zeros[kMaxSamplerSlots] = {};
    if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                     shader_stage,
                                                     /*start_slot=*/0,
                                                     /*sampler_count=*/kMaxSamplerSlots,
                                                     zeros,
                                                     [&](HRESULT hr) { SetError(hDevice, hr); })) {
      return false;
    }

    for (uint32_t slot = 0; slot < kMaxSamplerSlots; ++slot) {
      table[slot] = 0;
    }
    return true;
  };

  if (!clear_samplers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_samplers)) {
    return;
  }
  if (!clear_samplers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_samplers)) {
    return;
  }
  if (!clear_samplers(AEROGPU_SHADER_STAGE_GEOMETRY, dev->gs_samplers)) {
    return;
  }

  if (!EmitSetRenderTargetsCmdLocked(dev,
                                     /*rtv_count=*/0,
                                     /*rtvs=*/nullptr,
                                     /*dsv=*/0,
                                     [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }
  dev->current_rtv_count = 0;
  std::memset(dev->current_rtvs, 0, sizeof(dev->current_rtvs));
  std::memset(dev->current_rtv_resources, 0, sizeof(dev->current_rtv_resources));
  dev->current_dsv = 0;
  dev->current_dsv_res = nullptr;

  auto* bind_cmd = dev->cmd.bind_shaders(/*vs=*/0, /*ps=*/0, /*cs=*/0);
  if (!bind_cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_gs = 0;

  if (!aerogpu::d3d10_11::EmitSetInputLayoutCmdLocked(dev,
                                                      /*input_layout_handle=*/0,
                                                      [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }
  dev->current_input_layout = 0;

  auto* topo_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!topo_cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  topo_cmd->topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  topo_cmd->reserved0 = 0;
  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  std::array<aerogpu_vertex_buffer_binding, kMaxVertexBufferSlots> vb_zeros{};
  if (!aerogpu::d3d10_11::EmitSetVertexBuffersCmdLocked(dev,
                                                        /*start_slot=*/0,
                                                        /*buffer_count=*/kMaxVertexBufferSlots,
                                                        vb_zeros.data(),
                                                        [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }

  dev->current_vb_res = nullptr;
  dev->current_vb_stride = 0;
  dev->current_vb_offset = 0;
  for (uint32_t slot = 0; slot < kMaxVertexBufferSlots; ++slot) {
    dev->current_vb_resources[slot] = nullptr;
  }

  if (!aerogpu::d3d10_11::EmitSetIndexBufferCmdLocked(dev,
                                                      /*buffer=*/0,
                                                      AEROGPU_INDEX_FORMAT_UINT16,
                                                      /*offset_bytes=*/0,
                                                      [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }
  dev->current_ib_res = nullptr;

  // Reset fixed-function state to D3D10 defaults. Without this, a previous
  // Set*State call would persist across ClearState.
  // Blend state.
  auto* bs_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!bs_cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  bs_cmd->state.enable = 0;
  bs_cmd->state.src_factor = AEROGPU_BLEND_ONE;
  bs_cmd->state.dst_factor = AEROGPU_BLEND_ZERO;
  bs_cmd->state.blend_op = AEROGPU_BLEND_OP_ADD;
  bs_cmd->state.color_write_mask = kD3DColorWriteMaskAll;
  bs_cmd->state.reserved0[0] = 0;
  bs_cmd->state.reserved0[1] = 0;
  bs_cmd->state.reserved0[2] = 0;
  bs_cmd->state.src_factor_alpha = AEROGPU_BLEND_ONE;
  bs_cmd->state.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  bs_cmd->state.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  bs_cmd->state.blend_constant_rgba_f32[0] = f32_bits(1.0f);
  bs_cmd->state.blend_constant_rgba_f32[1] = f32_bits(1.0f);
  bs_cmd->state.blend_constant_rgba_f32[2] = f32_bits(1.0f);
  bs_cmd->state.blend_constant_rgba_f32[3] = f32_bits(1.0f);
  bs_cmd->state.sample_mask = kD3DSampleMaskAll;
  dev->current_bs = nullptr;
  dev->current_blend_factor[0] = 1.0f;
  dev->current_blend_factor[1] = 1.0f;
  dev->current_blend_factor[2] = 1.0f;
  dev->current_blend_factor[3] = 1.0f;
  dev->current_sample_mask = kD3DSampleMaskAll;

  // Depth-stencil state.
  auto* dss_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!dss_cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  dss_cmd->state.depth_enable = 1u;
  dss_cmd->state.depth_write_enable = 1u;
  dss_cmd->state.depth_func = AEROGPU_COMPARE_LESS;
  dss_cmd->state.stencil_enable = 0u;
  dss_cmd->state.stencil_read_mask = kD3DStencilMaskAll;
  dss_cmd->state.stencil_write_mask = kD3DStencilMaskAll;
  dss_cmd->state.reserved0[0] = 0;
  dss_cmd->state.reserved0[1] = 0;
  dev->current_dss = nullptr;
  dev->current_stencil_ref = 0;

  // Rasterizer state.
  auto* rs_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!rs_cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  rs_cmd->state.fill_mode = AEROGPU_FILL_SOLID;
  rs_cmd->state.cull_mode = AEROGPU_CULL_BACK;
  rs_cmd->state.front_ccw = 0;
  rs_cmd->state.scissor_enable = 0;
  rs_cmd->state.depth_bias = 0;
  rs_cmd->state.flags = AEROGPU_RASTERIZER_FLAG_NONE;
  dev->current_rs = nullptr;

  // ClearState must also reset dynamic viewport/scissor state. Without emitting
  // these commands, the host-side command executor would continue using the
  // previous values until the app calls SetViewports/SetScissorRects again.
  bool ok = true;
  aerogpu::d3d10_11::validate_and_emit_viewports_locked(dev,
                                                       /*num_viewports=*/0,
                                                       static_cast<const D3D10_DDI_VIEWPORT*>(nullptr),
                                                       [&](HRESULT hr) {
                                                         SetError(hDevice, hr);
                                                         ok = false;
                                                       });
  if (!ok) {
    return;
  }
  aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked(dev,
                                                           /*num_rects=*/0,
                                                           static_cast<const D3D10_DDI_RECT*>(nullptr),
                                                           [&](HRESULT hr) {
                                                             SetError(hDevice, hr);
                                                             ok = false;
                                                           });
  if (!ok) {
    return;
  }
}

void APIENTRY VsSetShaderResources(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numViews, const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_VERTEX, startSlot, numViews, phViews);
}
void APIENTRY PsSetShaderResources(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numViews, const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_PIXEL, startSlot, numViews, phViews);
}
void APIENTRY GsSetShaderResources(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numViews, const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, startSlot, numViews, phViews);
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

  // Sampler binding ranges are small (D3D10 max is 16). Avoid heap allocations
  // so OOM cannot surface as an unexpected C++ exception.
  std::array<aerogpu_handle_t, kMaxSamplerSlots> handles{};
  for (UINT i = 0; i < sampler_count; i++) {
    aerogpu_handle_t handle = 0;
    if (phSamplers[i].pDrvPrivate) {
      handle = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(phSamplers[i])->handle;
    }
    handles[i] = handle;
  }

  if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                   shader_stage,
                                                   static_cast<uint32_t>(start_slot),
                                                   static_cast<uint32_t>(sampler_count),
                                                   handles.data(),
                                                   [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }

  for (UINT i = 0; i < sampler_count; i++) {
    table[start_slot + i] = handles[i];
  }
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

void APIENTRY GsSetSamplers(D3D10DDI_HDEVICE hDevice, UINT startSlot, UINT numSamplers, const D3D10DDI_HSAMPLER* phSamplers) {
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
  SetSamplersLocked(dev, hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, startSlot, numSamplers, phSamplers);
}

void APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice, UINT numViewports, const D3D10_DDI_VIEWPORT* pViewports) {
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
  aerogpu::d3d10_11::validate_and_emit_viewports_locked(dev,
                                                       static_cast<uint32_t>(numViewports),
                                                       pViewports,
                                                       [&](HRESULT hr) { SetError(hDevice, hr); });
}

void APIENTRY SetScissorRects(D3D10DDI_HDEVICE hDevice, UINT numRects, const D3D10_DDI_RECT* pRects) {
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
  aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked(dev,
                                                           static_cast<uint32_t>(numRects),
                                                           pRects,
                                                           [&](HRESULT hr) { SetError(hDevice, hr); });
}

static bool EmitRasterizerStateLocked(D3D10DDI_HDEVICE hDevice, AeroGpuDevice* dev, const AeroGpuRasterizerState* rs) {
  if (!dev) {
    return false;
  }

  uint32_t fill_mode = static_cast<uint32_t>(D3D10_FILL_SOLID);
  uint32_t cull_mode = static_cast<uint32_t>(D3D10_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  int32_t depth_bias = 0;
  uint32_t depth_clip_enable = 1u;
  if (rs) {
    fill_mode = rs->fill_mode;
    cull_mode = rs->cull_mode;
    front_ccw = rs->front_ccw;
    scissor_enable = rs->scissor_enable;
    depth_bias = rs->depth_bias;
    depth_clip_enable = rs->depth_clip_enable;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return false;
  }

  cmd->state.fill_mode = aerogpu::d3d10_11::D3DFillModeToAerogpu(fill_mode);
  cmd->state.cull_mode = aerogpu::d3d10_11::D3DCullModeToAerogpu(cull_mode);
  cmd->state.front_ccw = front_ccw ? 1u : 0u;
  cmd->state.scissor_enable = scissor_enable ? 1u : 0u;
  cmd->state.depth_bias = depth_bias;
  cmd->state.flags = depth_clip_enable ? AEROGPU_RASTERIZER_FLAG_NONE
                                       : AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;
  return true;
}

static bool EmitBlendStateLocked(D3D10DDI_HDEVICE hDevice,
                                 AeroGpuDevice* dev,
                                 const AeroGpuBlendState* bs,
                                 const float blend_factor[4],
                                 uint32_t sample_mask) {
  if (!dev) {
    return false;
  }

  aerogpu::d3d10_11::AerogpuBlendStateBase base{};
  if (bs) {
    base = bs->state;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return false;
  }

  cmd->state.enable = base.enable ? 1u : 0u;
  cmd->state.src_factor = base.src_factor;
  cmd->state.dst_factor = base.dst_factor;
  cmd->state.blend_op = base.blend_op;
  cmd->state.color_write_mask = base.color_write_mask;
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
  cmd->state.reserved0[2] = 0;

  cmd->state.src_factor_alpha = base.src_factor_alpha;
  cmd->state.dst_factor_alpha = base.dst_factor_alpha;
  cmd->state.blend_op_alpha = base.blend_op_alpha;

  cmd->state.blend_constant_rgba_f32[0] = f32_bits(blend_factor ? blend_factor[0] : 1.0f);
  cmd->state.blend_constant_rgba_f32[1] = f32_bits(blend_factor ? blend_factor[1] : 1.0f);
  cmd->state.blend_constant_rgba_f32[2] = f32_bits(blend_factor ? blend_factor[2] : 1.0f);
  cmd->state.blend_constant_rgba_f32[3] = f32_bits(blend_factor ? blend_factor[3] : 1.0f);
  cmd->state.sample_mask = sample_mask;
  return true;
}

static bool EmitDepthStencilStateLocked(D3D10DDI_HDEVICE hDevice,
                                        AeroGpuDevice* dev,
                                        const AeroGpuDepthStencilState* dss) {
  if (!dev) {
    return false;
  }

  uint32_t depth_enable = 1u;
  uint32_t depth_write_mask = static_cast<uint32_t>(D3D10_DEPTH_WRITE_MASK_ALL);
  uint32_t depth_func = static_cast<uint32_t>(D3D10_COMPARISON_LESS);
  uint32_t stencil_enable = 0u;
  uint8_t stencil_read_mask = kD3DStencilMaskAll;
  uint8_t stencil_write_mask = kD3DStencilMaskAll;
  if (dss) {
    depth_enable = dss->depth_enable;
    depth_write_mask = dss->depth_write_mask;
    depth_func = dss->depth_func;
    stencil_enable = dss->stencil_enable;
    stencil_read_mask = dss->stencil_read_mask;
    stencil_write_mask = dss->stencil_write_mask;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return false;
  }

  cmd->state.depth_enable = depth_enable ? 1u : 0u;
  // D3D10/11 semantics: DepthWriteMask is ignored when depth testing is disabled.
  cmd->state.depth_write_enable = (depth_enable && depth_write_mask) ? 1u : 0u;
  cmd->state.depth_func = aerogpu::d3d10_11::D3DCompareFuncToAerogpu(depth_func);
  cmd->state.stencil_enable = stencil_enable ? 1u : 0u;
  cmd->state.stencil_read_mask = stencil_read_mask;
  cmd->state.stencil_write_mask = stencil_write_mask;
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
  return true;
}

void APIENTRY SetRasterizerState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRASTERIZERSTATE hState) {
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
  AeroGpuRasterizerState* new_rs =
      hState.pDrvPrivate ? FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState) : nullptr;
  if (!EmitRasterizerStateLocked(hDevice, dev, new_rs)) {
    return;
  }
  dev->current_rs = new_rs;
}

void APIENTRY SetBlendState(D3D10DDI_HDEVICE hDevice,
                            D3D10DDI_HBLENDSTATE hState,
                            const FLOAT blend_factor[4],
                            UINT sample_mask) {
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
  AeroGpuBlendState* new_bs = hState.pDrvPrivate ? FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState) : nullptr;
  float new_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  if (blend_factor) {
    std::memcpy(new_blend_factor, blend_factor, sizeof(new_blend_factor));
  }
  const uint32_t new_sample_mask = static_cast<uint32_t>(sample_mask);
  if (!EmitBlendStateLocked(hDevice, dev, new_bs, new_blend_factor, new_sample_mask)) {
    return;
  }
  dev->current_bs = new_bs;
  std::memcpy(dev->current_blend_factor, new_blend_factor, sizeof(dev->current_blend_factor));
  dev->current_sample_mask = new_sample_mask;
}

void APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HDEPTHSTENCILSTATE hState, UINT stencil_ref) {
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
  AeroGpuDepthStencilState* new_dss =
      hState.pDrvPrivate ? FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState) : nullptr;
  if (!EmitDepthStencilStateLocked(hDevice, dev, new_dss)) {
    return;
  }
  dev->current_dss = new_dss;
  dev->current_stencil_ref = stencil_ref;
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

  if (numViews != 0 && !phViews) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  const uint32_t count = std::min<uint32_t>(static_cast<uint32_t>(numViews), AEROGPU_MAX_RENDER_TARGETS);
  aerogpu_handle_t rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < count; ++i) {
    aerogpu_handle_t rtv_handle = 0;
    AeroGpuResource* rtv_res = nullptr;
    if (phViews && phViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(phViews[i]);
      rtv_res = view ? view->resource : nullptr;
      rtv_handle = view ? (view->texture ? view->texture : (rtv_res ? rtv_res->handle : 0)) : 0;
    }
    rtvs[i] = rtv_handle;
    rtv_resources[i] = rtv_res;
  }

  aerogpu_handle_t dsv_handle = 0;
  AeroGpuResource* dsv_res = nullptr;
  if (hDsv.pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
    dsv_res = view ? view->resource : nullptr;
    dsv_handle = view ? (view->texture ? view->texture : (dsv_res ? dsv_res->handle : 0)) : 0;
  }

  // D3D10/11 hazard rule: outputs cannot be simultaneously bound as SRVs.
  for (uint32_t i = 0; i < count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    UnbindResourceFromSrvsLocked(dev, rtvs[i], rtv_resources[i]);
  }
  UnbindResourceFromSrvsLocked(dev, dsv_handle, dsv_res);

  if (!EmitSetRenderTargetsCmdLocked(dev, count, rtvs, dsv_handle, [&](HRESULT hr) { SetError(hDevice, hr); })) {
    return;
  }

  dev->current_rtv_count = count;
  for (uint32_t i = 0; i < count; ++i) {
    dev->current_rtvs[i] = rtvs[i];
    dev->current_rtv_resources[i] = rtv_resources[i];
  }
  for (uint32_t i = count; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    dev->current_rtvs[i] = 0;
    dev->current_rtv_resources[i] = nullptr;
  }
  dev->current_dsv = dsv_handle;
  dev->current_dsv_res = dsv_res;
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
    for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      if (dev->current_rtv_resources[i]) {
        res = dev->current_rtv_resources[i];
        break;
      }
    }
  }

  if (res && res->kind == ResourceKind::Texture2D && res->width && res->height) {
    uint32_t bytes_per_pixel = 4;
    bool is_16bpp = false;
    uint16_t packed16 = 0;

    if (res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm ||
        res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm) {
      bytes_per_pixel = 2;
      is_16bpp = true;
    }
    if (res->row_pitch_bytes == 0) {
      res->row_pitch_bytes = res->width * bytes_per_pixel;
    }
    if (res->row_pitch_bytes >= res->width * bytes_per_pixel) {
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

      if (is_16bpp) {
        auto float_to_unorm = [](float v, uint32_t max) -> uint32_t {
          // Use ordered comparisons so NaNs resolve to zero.
          if (!(v > 0.0f)) {
            return 0;
          }
          if (v >= 1.0f) {
            return max;
          }
          const float scaled = v * static_cast<float>(max) + 0.5f;
          if (!(scaled > 0.0f)) {
            return 0;
          }
          if (scaled >= static_cast<float>(max)) {
            return max;
          }
          return static_cast<uint32_t>(scaled);
        };

        bool have_packed = false;
        if (res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm) {
          const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(color[0], 31));
          const uint16_t g6 = static_cast<uint16_t>(float_to_unorm(color[1], 63));
          const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(color[2], 31));
          packed16 = static_cast<uint16_t>((r5 << 11) | (g6 << 5) | b5);
          have_packed = true;
        } else if (res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm) {
          const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(color[0], 31));
          const uint16_t g5 = static_cast<uint16_t>(float_to_unorm(color[1], 31));
          const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(color[2], 31));
          const uint16_t a1 = static_cast<uint16_t>(float_to_unorm(color[3], 1));
          packed16 = static_cast<uint16_t>((a1 << 15) | (r5 << 10) | (g5 << 5) | b5);
          have_packed = true;
        }

        if (have_packed) {
          for (uint32_t y = 0; y < res->height; ++y) {
            uint8_t* row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
            for (uint32_t x = 0; x < res->width; ++x) {
              std::memcpy(row + static_cast<size_t>(x) * 2, &packed16, sizeof(packed16));
            }
          }
        }
      } else {
      for (uint32_t y = 0; y < res->height; ++y) {
        uint8_t* row = res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes;
        for (uint32_t x = 0; x < res->width; ++x) {
          uint8_t* dst = row + static_cast<size_t>(x) * 4;
          switch (res->dxgi_format) {
            case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Unorm:
            case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb:
            case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Typeless:
              dst[0] = out_r;
              dst[1] = out_g;
              dst[2] = out_b;
              dst[3] = out_a;
              break;
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Unorm:
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb:
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Typeless:
              dst[0] = out_b;
              dst[1] = out_g;
              dst[2] = out_r;
              dst[3] = 255;
              break;
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm:
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb:
            case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Typeless:
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
  }

  TrackBoundTargetsForSubmitLocked(dev);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
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
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
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

static bool SoftwareDrawTriangleListLocked(D3D10DDI_HDEVICE hDevice, AeroGpuDevice* dev, UINT vertexCount, UINT startVertex) {
  if (!dev) {
    return true;
  }

  AeroGpuResource* primary_rtv = nullptr;
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (dev->current_rtv_resources[i]) {
      primary_rtv = dev->current_rtv_resources[i];
      break;
    }
  }

  if (vertexCount == 3 && dev->current_topology == static_cast<uint32_t>(D3D10_DDI_PRIMITIVE_TOPOLOGY_TRIANGLELIST) &&
      primary_rtv && dev->current_vb_res) {
    auto* rt = primary_rtv;
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
          return false;
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

      // If VS/PS CB0 are bound, use them to drive the output color. This keeps the
      // bring-up software rasterizer useful for constant-buffer binding tests
      // (see d3d10_triangle / d3d10_1_triangle).
      AeroGpuResource* vs_cb0 = dev->current_vs_cb_resources[0];
      AeroGpuResource* ps_cb0 = dev->current_ps_cb_resources[0];
      if (vs_cb0 && ps_cb0 &&
          vs_cb0->kind == ResourceKind::Buffer &&
          ps_cb0->kind == ResourceKind::Buffer &&
          vs_cb0->storage.size() >= 16 &&
          ps_cb0->storage.size() >= 32) {
        float vs_color[4]{};
        float ps_mod[4]{};
        std::memcpy(&vs_color[0], vs_cb0->storage.data(), 16);
        std::memcpy(&ps_mod[0], ps_cb0->storage.data() + 16, 16);
        for (int i = 0; i < 4; ++i) {
          col[i] = vs_color[i] * ps_mod[i];
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
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Typeless:
                dst[0] = out_r;
                dst[1] = out_g;
                dst[2] = out_b;
                dst[3] = out_a;
                break;
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Typeless:
                dst[0] = out_b;
                dst[1] = out_g;
                dst[2] = out_r;
                dst[3] = 255;
                break;
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Typeless:
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

  return true;
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
  if (!SoftwareDrawTriangleListLocked(hDevice, dev, vertexCount, startVertex)) {
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = vertexCount;
  cmd->instance_count = 1;
  cmd->first_vertex = startVertex;
  cmd->first_instance = 0;
}

void APIENTRY DrawInstanced(D3D10DDI_HDEVICE hDevice,
                            UINT vertexCountPerInstance,
                            UINT instanceCount,
                            UINT startVertexLocation,
                            UINT startInstanceLocation) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (vertexCountPerInstance == 0 || instanceCount == 0) {
    return;
  }

#if defined(AEROGPU_UMD_TRACE_DRAWS)
  AEROGPU_D3D10_11_LOG("trace_draws: D3D10 DrawInstanced vc_per_inst=%u inst=%u first_vtx=%u first_inst=%u",
                       static_cast<unsigned>(vertexCountPerInstance),
                       static_cast<unsigned>(instanceCount),
                       static_cast<unsigned>(startVertexLocation),
                       static_cast<unsigned>(startInstanceLocation));
#endif

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  // The bring-up software renderer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  if (!SoftwareDrawTriangleListLocked(hDevice, dev, vertexCountPerInstance, startVertexLocation)) {
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = vertexCountPerInstance;
  cmd->instance_count = instanceCount;
  cmd->first_vertex = startVertexLocation;
  cmd->first_instance = startInstanceLocation;
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
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = indexCount;
  cmd->instance_count = 1;
  cmd->first_index = startIndex;
  cmd->base_vertex = baseVertex;
  cmd->first_instance = 0;
}

void APIENTRY DrawIndexedInstanced(D3D10DDI_HDEVICE hDevice,
                                   UINT indexCountPerInstance,
                                   UINT instanceCount,
                                   UINT startIndexLocation,
                                   INT baseVertexLocation,
                                   UINT startInstanceLocation) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  if (indexCountPerInstance == 0 || instanceCount == 0) {
    return;
  }

#if defined(AEROGPU_UMD_TRACE_DRAWS)
  AEROGPU_D3D10_11_LOG("trace_draws: D3D10 DrawIndexedInstanced ic_per_inst=%u inst=%u first_idx=%u base_vtx=%d first_inst=%u",
                       static_cast<unsigned>(indexCountPerInstance),
                       static_cast<unsigned>(instanceCount),
                       static_cast<unsigned>(startIndexLocation),
                       static_cast<int>(baseVertexLocation),
                       static_cast<unsigned>(startInstanceLocation));
#endif

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = indexCountPerInstance;
  cmd->instance_count = instanceCount;
  cmd->first_index = startIndexLocation;
  cmd->base_vertex = baseVertexLocation;
  cmd->first_instance = startInstanceLocation;
}

void APIENTRY DrawAuto(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    SetError(hDevice, E_INVALIDARG);
    return;
  }

  // `DrawAuto` is defined in terms of the vertex count produced by stream
  // output. AeroGPU does not implement stream output yet, so treat this as a
  // no-op draw (keeps runtimes/apps that probe the entrypoint alive).
  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = 0;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
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

  if (auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH)) {
    cmd->reserved0 = 0;
    cmd->reserved1 = 0;
  }
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
  TrackWddmAllocForSubmitLocked(dev, src_res, /*write=*/false);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  aerogpu_handle_t src_handle = 0;
  src_handle = src_res ? src_res->handle : 0;

  AEROGPU_D3D10_11_LOG("trace_resources: D3D10 Present sync=%u src_handle=%u",
                       static_cast<unsigned>(pPresent->SyncInterval),
                       static_cast<unsigned>(src_handle));
#endif

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!cmd) {
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    dev->pending_staging_writes.clear();
    return E_OUTOFMEMORY;
  }
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
  try {
    resources.reserve(numResources);
  } catch (...) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
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

  // Treat RotateResourceIdentities as a transaction: if rebinding packets cannot
  // be appended (OOM), roll back the command stream and undo the rotation so the
  // runtime-visible state remains unchanged.
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const uint32_t prev_rtv_count = dev->current_rtv_count;
  const std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> prev_rtvs = dev->current_rtvs;
  const aerogpu_handle_t prev_dsv = dev->current_dsv;
  aerogpu_handle_t prev_vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_gs_srvs[kMaxShaderResourceSlots] = {};
  std::memcpy(prev_vs_srvs, dev->vs_srvs, sizeof(prev_vs_srvs));
  std::memcpy(prev_ps_srvs, dev->ps_srvs, sizeof(prev_ps_srvs));
  std::memcpy(prev_gs_srvs, dev->gs_srvs, sizeof(prev_gs_srvs));

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

  auto rollback_rotation = [&](bool report_oom) {
    dev->cmd.rollback(cmd_checkpoint);

    // Undo the rotation (rotate right by one).
    ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
    for (UINT i = numResources - 1; i > 0; --i) {
      put_identity(resources[i], take_identity(resources[i - 1]));
    }
    put_identity(resources[0], std::move(undo_saved));

    dev->current_rtv_count = prev_rtv_count;
    dev->current_rtvs = prev_rtvs;
    dev->current_dsv = prev_dsv;
    std::memcpy(dev->vs_srvs, prev_vs_srvs, sizeof(prev_vs_srvs));
    std::memcpy(dev->ps_srvs, prev_ps_srvs, sizeof(prev_ps_srvs));
    std::memcpy(dev->gs_srvs, prev_gs_srvs, sizeof(prev_gs_srvs));

    if (report_oom) {
      SetError(hDevice, E_OUTOFMEMORY);
    }
  };

  std::vector<aerogpu_handle_t> old_handles;
  try {
    old_handles.reserve(resources.size());
  } catch (...) {
    SetError(hDevice, E_OUTOFMEMORY);
    return;
  }
  for (auto* res : resources) {
    old_handles.push_back(res ? res->handle : 0);
  }

  ResourceIdentity saved = take_identity(resources[0]);
  for (UINT i = 0; i + 1 < numResources; ++i) {
    put_identity(resources[i], take_identity(resources[i + 1]));
  }
  put_identity(resources[numResources - 1], std::move(saved));

  auto remap_handle = [&](aerogpu_handle_t handle) -> aerogpu_handle_t {
    if (!handle) {
      return handle;
    }
    for (size_t i = 0; i < old_handles.size(); ++i) {
      if (old_handles[i] == handle) {
        return resources[i] ? resources[i]->handle : 0;
      }
    }
    return handle;
  };

  bool needs_rebind = false;
  const uint32_t bound_rtv_count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> new_rtvs{};
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    new_rtvs[i] = dev->current_rtvs[i];
  }
  for (uint32_t i = 0; i < bound_rtv_count; ++i) {
    if (dev->current_rtv_resources[i] &&
        std::find(resources.begin(), resources.end(), dev->current_rtv_resources[i]) != resources.end()) {
      needs_rebind = true;
    }
    new_rtvs[i] = remap_handle(new_rtvs[i]);
  }
  const aerogpu_handle_t new_dsv = remap_handle(dev->current_dsv);
  if (dev->current_dsv_res &&
      std::find(resources.begin(), resources.end(), dev->current_dsv_res) != resources.end()) {
    needs_rebind = true;
  }

  if (needs_rebind) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!cmd) {
      rollback_rotation(/*report_oom=*/true);
      return;
    }

    // Update cached handles only after we successfully appended the rebind packet. If we fail to
    // append (OOM), we roll back the rotation and must keep the previous handles intact.
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      dev->current_rtvs[i] = new_rtvs[i];
    }
    dev->current_dsv = new_dsv;

    cmd->color_count = bound_rtv_count;
    cmd->depth_stencil = new_dsv;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      cmd->colors[i] = (i < bound_rtv_count) ? new_rtvs[i] : 0;
    }

    // Bring-up logging: swapchains may rebind RT state via RotateResourceIdentities.
    AEROGPU_D3D10_11_LOG("SET_RENDER_TARGETS (rotate): color_count=%u depth=%u colors=[%u,%u,%u,%u,%u,%u,%u,%u]",
                         static_cast<unsigned>(cmd->color_count),
                         static_cast<unsigned>(cmd->depth_stencil),
                         static_cast<unsigned>(cmd->colors[0]),
                         static_cast<unsigned>(cmd->colors[1]),
                         static_cast<unsigned>(cmd->colors[2]),
                         static_cast<unsigned>(cmd->colors[3]),
                         static_cast<unsigned>(cmd->colors[4]),
                         static_cast<unsigned>(cmd->colors[5]),
                         static_cast<unsigned>(cmd->colors[6]),
                         static_cast<unsigned>(cmd->colors[7]));
  }

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    const aerogpu_handle_t new_vs = remap_handle(dev->vs_srvs[slot]);
    if (new_vs != dev->vs_srvs[slot]) {
      if (!SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_VERTEX, slot, new_vs)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
    }
    const aerogpu_handle_t new_ps = remap_handle(dev->ps_srvs[slot]);
    if (new_ps != dev->ps_srvs[slot]) {
      if (!SetShaderResourceSlotLocked(dev, AEROGPU_SHADER_STAGE_PIXEL, slot, new_ps)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
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
  auto* caps_adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);

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
        auto write_u32 = [&](size_t offset, UINT value) {
          if (pCaps->DataSize < offset + sizeof(UINT)) {
            return;
          }
          *reinterpret_cast<UINT*>(reinterpret_cast<uint8_t*>(pCaps->pData) + offset) = value;
        };

        write_u32(0,
                  aerogpu::d3d10_11::DxbcShaderVersionToken(aerogpu::d3d10_11::kD3DDxbcProgramTypePixel, 4, 0));
        write_u32(sizeof(UINT),
                  aerogpu::d3d10_11::DxbcShaderVersionToken(aerogpu::d3d10_11::kD3DDxbcProgramTypeVertex, 4, 0));
        write_u32(sizeof(UINT) * 2,
                  aerogpu::d3d10_11::DxbcShaderVersionToken(aerogpu::d3d10_11::kD3DDxbcProgramTypeGeometry, 4, 0));
        break;
      }
    }

    case D3D10DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
        fmt->Format = in_format;
        const uint32_t format = static_cast<uint32_t>(in_format);

        const uint32_t caps = aerogpu::d3d10_11::AerogpuDxgiFormatCapsMask(caps_adapter, format);
        UINT support = 0;
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapTexture2D) {
          support |= D3D10_FORMAT_SUPPORT_TEXTURE2D;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapRenderTarget) {
          support |= D3D10_FORMAT_SUPPORT_RENDER_TARGET;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapDepthStencil) {
          support |= D3D10_FORMAT_SUPPORT_DEPTH_STENCIL;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapShaderSample) {
          support |= D3D10_FORMAT_SUPPORT_SHADER_SAMPLE;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapDisplay) {
          support |= D3D10_FORMAT_SUPPORT_DISPLAY;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapBlendable) {
          support |= D3D10_FORMAT_SUPPORT_BLENDABLE;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapCpuLockable) {
          support |= D3D10_FORMAT_SUPPORT_CPU_LOCKABLE;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapBuffer) {
          support |= D3D10_FORMAT_SUPPORT_BUFFER;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapIaVertexBuffer) {
          support |= D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER;
        }
        if (caps & aerogpu::d3d10_11::kAerogpuDxgiFormatCapIaIndexBuffer) {
          support |= D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER;
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
        const bool supported_format =
            aerogpu::d3d10_11::AerogpuSupportsMultisampleQualityLevels(caps_adapter, static_cast<uint32_t>(msaa_format));
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

  // Stub-fill the entire function table first so we never expose NULL pointers
  // to the runtime. Then override the subset of entrypoints we actually
  // implement below.
  D3D10DDI_DEVICEFUNCS funcs;
  InitDeviceFuncsWithStubs(&funcs);

  // Optional entrypoints that may be present depending on the WDK headers.
  // Bind them opportunistically when the function signature matches.
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDrawInstanced) {
    using Fn = decltype(funcs.pfnDrawInstanced);
    if constexpr (std::is_convertible_v<decltype(&DrawInstanced), Fn>) {
      funcs.pfnDrawInstanced = AEROGPU_D3D10_WDK_DDI(DrawInstanced);
    }
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDrawIndexedInstanced) {
    using Fn = decltype(funcs.pfnDrawIndexedInstanced);
    if constexpr (std::is_convertible_v<decltype(&DrawIndexedInstanced), Fn>) {
      funcs.pfnDrawIndexedInstanced = AEROGPU_D3D10_WDK_DDI(DrawIndexedInstanced);
    }
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDrawAuto) {
    using Fn = decltype(funcs.pfnDrawAuto);
    if constexpr (std::is_convertible_v<decltype(&DrawAuto), Fn>) {
      funcs.pfnDrawAuto = AEROGPU_D3D10_WDK_DDI(DrawAuto);
    }
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnOpenResource) {
    using Fn = decltype(funcs.pfnOpenResource);
    if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
      funcs.pfnOpenResource = AEROGPU_D3D10_WDK_DDI(OpenResource);
    }
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnClearState) {
    funcs.pfnClearState = AEROGPU_D3D10_WDK_DDI(ClearState);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnStagingResourceMap) {
    funcs.pfnStagingResourceMap = AEROGPU_D3D10_WDK_DDI(StagingResourceMap<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnStagingResourceUnmap) {
    funcs.pfnStagingResourceUnmap = AEROGPU_D3D10_WDK_DDI(StagingResourceUnmap<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDynamicIABufferMapDiscard) {
    funcs.pfnDynamicIABufferMapDiscard = AEROGPU_D3D10_WDK_DDI(DynamicIABufferMapDiscard<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDynamicIABufferMapNoOverwrite) {
    funcs.pfnDynamicIABufferMapNoOverwrite = AEROGPU_D3D10_WDK_DDI(DynamicIABufferMapNoOverwrite<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDynamicIABufferUnmap) {
    funcs.pfnDynamicIABufferUnmap = AEROGPU_D3D10_WDK_DDI(DynamicIABufferUnmap<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDynamicConstantBufferMapDiscard) {
    funcs.pfnDynamicConstantBufferMapDiscard = AEROGPU_D3D10_WDK_DDI(DynamicConstantBufferMapDiscard<>);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnDynamicConstantBufferUnmap) {
    funcs.pfnDynamicConstantBufferUnmap = AEROGPU_D3D10_WDK_DDI(DynamicConstantBufferUnmap<>);
  }

  // Lifecycle.
  funcs.pfnDestroyDevice = AEROGPU_D3D10_WDK_DDI(DestroyDevice);

  // Resources.
  funcs.pfnCalcPrivateResourceSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateResourceSize);
  funcs.pfnCreateResource = AEROGPU_D3D10_WDK_DDI(CreateResource);
  funcs.pfnDestroyResource = AEROGPU_D3D10_WDK_DDI(DestroyResource);
  funcs.pfnMap = AEROGPU_D3D10_WDK_DDI(Map);
  funcs.pfnUnmap = AEROGPU_D3D10_WDK_DDI(Unmap);
  funcs.pfnUpdateSubresourceUP = AEROGPU_D3D10_WDK_DDI(UpdateSubresourceUP);
  funcs.pfnCopyResource = AEROGPU_D3D10_WDK_DDI(CopyResource);
  funcs.pfnCopySubresourceRegion = AEROGPU_D3D10_WDK_DDI(CopySubresourceRegion);

  // Views.
  funcs.pfnCalcPrivateRenderTargetViewSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateRenderTargetViewSize);
  funcs.pfnCreateRenderTargetView = AEROGPU_D3D10_WDK_DDI(CreateRenderTargetView);
  funcs.pfnDestroyRenderTargetView = AEROGPU_D3D10_WDK_DDI(DestroyRenderTargetView);

  funcs.pfnCalcPrivateDepthStencilViewSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateDepthStencilViewSize);
  funcs.pfnCreateDepthStencilView = AEROGPU_D3D10_WDK_DDI(CreateDepthStencilView);
  funcs.pfnDestroyDepthStencilView = AEROGPU_D3D10_WDK_DDI(DestroyDepthStencilView);

  funcs.pfnCalcPrivateShaderResourceViewSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateShaderResourceViewSize);
  funcs.pfnCreateShaderResourceView = AEROGPU_D3D10_WDK_DDI(CreateShaderResourceView);
  funcs.pfnDestroyShaderResourceView = AEROGPU_D3D10_WDK_DDI(DestroyShaderResourceView);

  // Shaders.
  funcs.pfnCalcPrivateVertexShaderSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateVertexShaderSize);
  funcs.pfnCreateVertexShader = AEROGPU_D3D10_WDK_DDI(CreateVertexShader);
  funcs.pfnDestroyVertexShader = AEROGPU_D3D10_WDK_DDI(DestroyVertexShader);

  funcs.pfnCalcPrivatePixelShaderSize = AEROGPU_D3D10_WDK_DDI(CalcPrivatePixelShaderSize);
  funcs.pfnCreatePixelShader = AEROGPU_D3D10_WDK_DDI(CreatePixelShader);
  funcs.pfnDestroyPixelShader = AEROGPU_D3D10_WDK_DDI(DestroyPixelShader);

  funcs.pfnCalcPrivateGeometryShaderSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateGeometryShaderSize);
  funcs.pfnCreateGeometryShader = AEROGPU_D3D10_WDK_DDI(CreateGeometryShader);
  funcs.pfnDestroyGeometryShader = AEROGPU_D3D10_WDK_DDI(DestroyGeometryShader);
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderWithStreamOutputSize) {
    funcs.pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        AEROGPU_D3D10_WDK_DDI(CalcPrivateGeometryShaderWithStreamOutputSizeImpl<
                              decltype(funcs.pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnCreateGeometryShaderWithStreamOutput) {
    funcs.pfnCreateGeometryShaderWithStreamOutput =
        AEROGPU_D3D10_WDK_DDI(CreateGeometryShaderWithStreamOutputImpl<
                              decltype(funcs.pfnCreateGeometryShaderWithStreamOutput)>::Call);
  }

  // Input layout.
  funcs.pfnCalcPrivateElementLayoutSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateElementLayoutSize);
  funcs.pfnCreateElementLayout = AEROGPU_D3D10_WDK_DDI(CreateElementLayout);
  funcs.pfnDestroyElementLayout = AEROGPU_D3D10_WDK_DDI(DestroyElementLayout);

  // State objects.
  funcs.pfnCalcPrivateBlendStateSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateBlendStateSize);
  funcs.pfnCreateBlendState = AEROGPU_D3D10_WDK_DDI(CreateBlendState);
  funcs.pfnDestroyBlendState = AEROGPU_D3D10_WDK_DDI(DestroyBlendState);

  funcs.pfnCalcPrivateRasterizerStateSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateRasterizerStateSize);
  funcs.pfnCreateRasterizerState = AEROGPU_D3D10_WDK_DDI(CreateRasterizerState);
  funcs.pfnDestroyRasterizerState = AEROGPU_D3D10_WDK_DDI(DestroyRasterizerState);

  funcs.pfnCalcPrivateDepthStencilStateSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateDepthStencilStateSize);
  funcs.pfnCreateDepthStencilState = AEROGPU_D3D10_WDK_DDI(CreateDepthStencilState);
  funcs.pfnDestroyDepthStencilState = AEROGPU_D3D10_WDK_DDI(DestroyDepthStencilState);

  funcs.pfnCalcPrivateSamplerSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateSamplerSize);
  funcs.pfnCreateSampler = AEROGPU_D3D10_WDK_DDI(CreateSampler);
  funcs.pfnDestroySampler = AEROGPU_D3D10_WDK_DDI(DestroySampler);

  // Binding/state setting.
  funcs.pfnIaSetInputLayout = AEROGPU_D3D10_WDK_DDI(IaSetInputLayout);
  funcs.pfnIaSetVertexBuffers = AEROGPU_D3D10_WDK_DDI(IaSetVertexBuffers);
  funcs.pfnIaSetIndexBuffer = AEROGPU_D3D10_WDK_DDI(IaSetIndexBuffer);
  funcs.pfnIaSetTopology = AEROGPU_D3D10_WDK_DDI(IaSetTopology);

  funcs.pfnVsSetShader = AEROGPU_D3D10_WDK_DDI(VsSetShader);
  funcs.pfnVsSetConstantBuffers = AEROGPU_D3D10_WDK_DDI(VsSetConstantBuffers);
  funcs.pfnVsSetShaderResources = AEROGPU_D3D10_WDK_DDI(VsSetShaderResources);
  funcs.pfnVsSetSamplers = AEROGPU_D3D10_WDK_DDI(VsSetSamplers);

  funcs.pfnGsSetShader = AEROGPU_D3D10_WDK_DDI(GsSetShader);
  funcs.pfnGsSetConstantBuffers = AEROGPU_D3D10_WDK_DDI(GsSetConstantBuffers);
  funcs.pfnGsSetShaderResources = AEROGPU_D3D10_WDK_DDI(GsSetShaderResources);
  funcs.pfnGsSetSamplers = AEROGPU_D3D10_WDK_DDI(GsSetSamplers);

  funcs.pfnPsSetShader = AEROGPU_D3D10_WDK_DDI(PsSetShader);
  funcs.pfnPsSetConstantBuffers = AEROGPU_D3D10_WDK_DDI(PsSetConstantBuffers);
  funcs.pfnPsSetShaderResources = AEROGPU_D3D10_WDK_DDI(PsSetShaderResources);
  funcs.pfnPsSetSamplers = AEROGPU_D3D10_WDK_DDI(PsSetSamplers);

  funcs.pfnSetViewports = AEROGPU_D3D10_WDK_DDI(SetViewports);
  funcs.pfnSetScissorRects = AEROGPU_D3D10_WDK_DDI(SetScissorRects);
  funcs.pfnSetRasterizerState = AEROGPU_D3D10_WDK_DDI(SetRasterizerState);
  funcs.pfnSetBlendState = AEROGPU_D3D10_WDK_DDI(SetBlendState);
  funcs.pfnSetDepthStencilState = AEROGPU_D3D10_WDK_DDI(SetDepthStencilState);
  funcs.pfnSetRenderTargets = AEROGPU_D3D10_WDK_DDI(SetRenderTargets);
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnSoSetTargets) {
    funcs.pfnSoSetTargets = AEROGPU_D3D10_WDK_DDI(SoSetTargetsImpl<decltype(funcs.pfnSoSetTargets)>::Call);
  }

  // Clears/draw.
  funcs.pfnClearRenderTargetView = AEROGPU_D3D10_WDK_DDI(ClearRenderTargetView);
  funcs.pfnClearDepthStencilView = AEROGPU_D3D10_WDK_DDI(ClearDepthStencilView);
  funcs.pfnDraw = AEROGPU_D3D10_WDK_DDI(Draw);
  funcs.pfnDrawIndexed = AEROGPU_D3D10_WDK_DDI(DrawIndexed);

  // Present.
  funcs.pfnFlush = AEROGPU_D3D10_WDK_DDI(Flush);
  funcs.pfnPresent = AEROGPU_D3D10_WDK_DDI(Present);
  funcs.pfnRotateResourceIdentities = AEROGPU_D3D10_WDK_DDI(RotateResourceIdentities);

  if (!ValidateNoNullDdiTable("D3D10DDI_DEVICEFUNCS", &funcs, sizeof(funcs))) {
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return E_NOINTERFACE;
  }

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
  InitAdapterFuncsWithStubs(&funcs);
  funcs.pfnGetCaps = AEROGPU_D3D10_WDK_DDI(GetCaps);
  funcs.pfnCalcPrivateDeviceSize = AEROGPU_D3D10_WDK_DDI(CalcPrivateDeviceSize);
  funcs.pfnCreateDevice = AEROGPU_D3D10_WDK_DDI(CreateDevice);
  funcs.pfnCloseAdapter = AEROGPU_D3D10_WDK_DDI(CloseAdapter);
  if (!ValidateNoNullDdiTable("D3D10DDI_ADAPTERFUNCS", &funcs, sizeof(funcs))) {
    pOpenData->hAdapter.pDrvPrivate = nullptr;
    DestroyKmtAdapterHandle(adapter);
    delete adapter;
    return E_NOINTERFACE;
  }

  auto* out_funcs = reinterpret_cast<D3D10DDI_ADAPTERFUNCS*>(pOpenData->pAdapterFuncs);
  if (!out_funcs) {
    return E_INVALIDARG;
  }
  *out_funcs = funcs;
  if (!ValidateNoNullDdiTable("D3D10DDI_ADAPTERFUNCS", out_funcs, sizeof(*out_funcs))) {
    pOpenData->hAdapter.pDrvPrivate = nullptr;
    DestroyKmtAdapterHandle(adapter);
    delete adapter;
    return E_NOINTERFACE;
  }
  return S_OK;
}

} // namespace

HRESULT AEROGPU_APIENTRY AeroGpuOpenAdapter10Wdk(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapterCommon(pOpenData);
}

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
