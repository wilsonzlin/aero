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
#include <tuple>
#include <type_traits>
#include <unordered_map>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_blend_state_validate.h"
#include "aerogpu_legacy_d3d9_format_fixup.h"
#include "aerogpu_d3d10_11_wddm_submit.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../common/aerogpu_win32_security.h"
#include "../../../protocol/aerogpu_dbgctl_escape.h"
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"
#include "../../../protocol/aerogpu_win7_abi.h"

// Implemented in `aerogpu_d3d10_umd_wdk.cpp` (D3D10.0 DDI).
HRESULT AEROGPU_APIENTRY AeroGpuOpenAdapter10Wdk(D3D10DDIARG_OPENADAPTER* pOpenData);

namespace {

using aerogpu::d3d10_11::kInvalidHandle;
using aerogpu::d3d10_11::AllocateGlobalHandle;
using aerogpu::d3d10_11::kD3DMapFlagDoNotWait;
using aerogpu::d3d10_11::kD3D10_1DeviceLiveCookie;
using aerogpu::d3d10_11::NtSuccess;
using aerogpu::d3d10_11::kDxgiErrorWasStillDrawing;
using aerogpu::d3d10_11::kHrPending;
using aerogpu::d3d10_11::kHrWaitTimeout;
using aerogpu::d3d10_11::kHrErrorTimeout;
using aerogpu::d3d10_11::kHrNtStatusTimeout;
using aerogpu::d3d10_11::kHrNtStatusGraphicsGpuBusy;
using aerogpu::d3d10_11::kAeroGpuTimeoutMsInfinite;
using aerogpu::d3d10_11::kD3D10UsageDynamic;
using aerogpu::d3d10_11::kD3D10UsageStaging;
using aerogpu::d3d10_11::kD3D10CpuAccessRead;
using aerogpu::d3d10_11::kD3D10CpuAccessWrite;
using aerogpu::d3d10_11::kD3D10ResourceMiscShared;
using aerogpu::d3d10_11::kD3D10ResourceMiscSharedKeyedMutex;
using aerogpu::d3d10_11::kD3DSampleMaskAll;
using aerogpu::d3d10_11::kD3DColorWriteMaskAll;
using aerogpu::d3d10_11::kD3DStencilMaskAll;
using aerogpu::d3d10_11::LogModulePathOnce;
using aerogpu::d3d10_11::ResetObject;
using aerogpu::d3d10_11::HasLiveCookie;

static bool IsDeviceLive(D3D10DDI_HDEVICE hDevice) {
  return HasLiveCookie(hDevice.pDrvPrivate, kD3D10_1DeviceLiveCookie);
}

struct AeroGpuAdapter;

using aerogpu::d3d10_11::AlignUpU64;
using aerogpu::d3d10_11::AlignUpU32;

// D3D10_BIND_* subset (numeric values from d3d10.h).
using aerogpu::d3d10_11::kD3D10BindVertexBuffer;
using aerogpu::d3d10_11::kD3D10BindIndexBuffer;
using aerogpu::d3d10_11::kD3D10BindConstantBuffer;
using aerogpu::d3d10_11::kD3D10BindShaderResource;
using aerogpu::d3d10_11::kD3D10BindRenderTarget;
using aerogpu::d3d10_11::kD3D10BindDepthStencil;

using aerogpu::d3d10_11::kMaxConstantBufferSlots;
constexpr uint32_t kAeroGpuD3D10MaxSrvSlots = aerogpu::d3d10_11::kMaxShaderResourceSlots;
constexpr uint32_t kAeroGpuD3D10MaxSamplerSlots = aerogpu::d3d10_11::kMaxSamplerSlots;

// D3D10-class IA supports 16 vertex buffer slots (D3D10_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT).
constexpr uint32_t kMaxVertexBufferSlots = aerogpu::d3d10_11::kD3D10IaVertexInputResourceSlotCount;

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
using aerogpu::d3d10_11::atomic_max_u64;
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
using aerogpu::d3d10_11::ConsumeWddmAllocPrivV2;
using aerogpu::d3d10_11::ValidateNoNullDdiTable;
using aerogpu::d3d10_11::AnyNonNullHandles;
using aerogpu::d3d10_11::D3dSrvMipLevelsIsAll;
using aerogpu::d3d10_11::D3dViewDimensionIsTexture2D;
using aerogpu::d3d10_11::D3dViewDimensionIsTexture2DArray;
using aerogpu::d3d10_11::D3dViewCountToRemaining;
using aerogpu::d3d10_11::ClampU64ToU32;
using aerogpu::d3d10_11::kD3DUintAll;
using aerogpu::d3d10_11::InitSamplerFromCreateSamplerArg;
using aerogpu::d3d10_11::InitLockForWrite;
using aerogpu::d3d10_11::InitLockArgsForMap;
using aerogpu::d3d10_11::InitUnlockArgsForMap;
using aerogpu::d3d10_11::InitUnlockForWrite;
using aerogpu::d3d10_11::UintPtrToD3dHandle;
using aerogpu::d3d10_11::TrackStagingWriteLocked;
using aerogpu::d3d10_11::ResourcesAlias;
using aerogpu::d3d10_11::ValidateWddmTexturePitch;
using aerogpu::d3d10_11::resource_total_bytes;
using aerogpu::d3d10_11::NormalizeRenderTargetsLocked;
using aerogpu::d3d10_11::EmitSetRenderTargetsCmdLocked;
using aerogpu::d3d10_11::EmitSetRenderTargetsLocked;

using AerogpuTextureFormatLayout = aerogpu::d3d10_11::AerogpuTextureFormatLayout;
using aerogpu::d3d10_11::aerogpu_texture_format_layout;

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

using Texture2DSubresourceLayout = aerogpu::d3d10_11::Texture2DSubresourceLayout;

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

struct AeroGpuSampler {
  aerogpu_handle_t handle = 0;
  uint32_t filter = AEROGPU_SAMPLER_FILTER_LINEAR;
  uint32_t address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t address_w = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
};

// Win7-era WDK headers disagree on whether pfnSetErrorCb takes HRTDEVICE or
// HDEVICE. Keep the callback typed exactly as declared by the active headers,
// then use `std::is_invocable_v` at call sites to choose the right handle type.
using SetErrorFn = decltype(std::declval<std::remove_pointer_t<decltype(std::declval<D3D10_1DDIARG_CREATEDEVICE>().pCallbacks)>>().pfnSetErrorCb);

struct AeroGpuDevice {
  uint32_t live_cookie = kD3D10_1DeviceLiveCookie;
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
  std::vector<aerogpu::d3d10_11::WddmSubmitAllocation> wddm_submit_allocation_handles;
  bool wddm_submit_allocation_list_oom = false;

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

  uint32_t current_rtv_count = 0;
  aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  aerogpu_handle_t current_dsv = 0;
  std::array<AeroGpuResource*, kAeroGpuD3D10MaxSrvSlots> current_vs_srvs{};
  std::array<AeroGpuResource*, kAeroGpuD3D10MaxSrvSlots> current_ps_srvs{};
  std::array<AeroGpuResource*, kAeroGpuD3D10MaxSrvSlots> current_gs_srvs{};
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> vs_constant_buffers{};
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> ps_constant_buffers{};
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> gs_constant_buffers{};
  std::array<AeroGpuResource*, kMaxConstantBufferSlots> current_vs_cb_resources{};
  std::array<AeroGpuResource*, kMaxConstantBufferSlots> current_ps_cb_resources{};
  std::array<AeroGpuResource*, kMaxConstantBufferSlots> current_gs_cb_resources{};
  std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots> current_vs_samplers{};
  std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots> current_ps_samplers{};
  std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots> current_gs_samplers{};
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  // Minimal state required for CPU-side readback tests (`d3d10_triangle`, `d3d10_1_triangle`).
  AeroGpuResource* current_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* current_dsv_res = nullptr;
  AeroGpuResource* current_vb_res = nullptr;
  std::array<AeroGpuResource*, kMaxVertexBufferSlots> current_vb_resources{};
  std::array<uint32_t, kMaxVertexBufferSlots> current_vb_strides{};
  std::array<uint32_t, kMaxVertexBufferSlots> current_vb_offsets{};
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

  if (NtSuccess(st) && open.hAdapter) {
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
    if (!NtSuccess(st)) {
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
  if (dev->wddm_submit_allocation_list_oom) {
    // Submitting with an incomplete allocation list is unsafe when the command
    // stream references guest-backed allocations (`backing_alloc_id`).
    if (out_hr) {
      *out_hr = E_OUTOFMEMORY;
    }
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }
  if (!dev->adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->pending_staging_writes.clear();
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

void set_error(AeroGpuDevice* dev, HRESULT hr) noexcept;
void unmap_resource_locked(AeroGpuDevice* dev, AeroGpuResource* res, uint32_t subresource);

void flush_locked(AeroGpuDevice* dev) {
  if (dev) {
    if (auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH)) {
      cmd->reserved0 = 0;
      cmd->reserved1 = 0;
    }
  }
  HRESULT hr = S_OK;
  submit_locked(dev, false, &hr);
  if (FAILED(hr)) {
    set_error(dev, hr);
  }
}

static void TrackWddmAllocForSubmitLocked(AeroGpuDevice* dev, const AeroGpuResource* res, bool write) {
  aerogpu::d3d10_11::TrackWddmAllocForSubmitLocked(
      dev, res, write, [&](HRESULT hr) { set_error(dev, hr); });
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

static bool UnbindResourceFromOutputsLocked(AeroGpuDevice* dev, aerogpu_handle_t handle, const AeroGpuResource* res) {
  return aerogpu::d3d10_11::UnbindResourceFromOutputsLocked(
      dev, handle, res, [&](HRESULT hr) { set_error(dev, hr); });
}

static void TrackDrawStateLocked(AeroGpuDevice* dev) {
  if (!dev) {
    return;
  }

  TrackBoundTargetsForSubmitLocked(dev);
  for (AeroGpuResource* res : dev->current_vb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  TrackWddmAllocForSubmitLocked(dev, dev->current_ib_res, /*write=*/false);

  for (AeroGpuResource* res : dev->current_vs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_ps_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_gs_srvs) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_vs_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_ps_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
  for (AeroGpuResource* res : dev->current_gs_cb_resources) {
    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);
  }
}

void set_error(AeroGpuDevice* dev, HRESULT hr) noexcept {
  // Many D3D10/DDI entrypoints are `void` and must signal failures via the
  // runtime callback instead of returning HRESULT.
  //
  // This helper is used in stub entrypoints and in teardown/error paths. Be
  // defensive and swallow any unexpected C++ exceptions (e.g. from tracing or a
  // runtime callback).
  if (!HasLiveCookie(dev, kD3D10_1DeviceLiveCookie)) {
    return;
  }

  try {
    // Best-effort logging so bring-up can correlate failures to the last DDI call.
    AEROGPU_D3D10_11_LOG("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
    AEROGPU_D3D10_TRACEF("SetErrorCb hr=0x%08X", static_cast<unsigned>(hr));
  } catch (...) {
  }

  if (!dev || !dev->pfn_set_error) {
    return;
  }
  try {
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
  } catch (...) {
  }
}

// -----------------------------------------------------------------------------
// D3D10.1 WDK DDI exception barrier
// -----------------------------------------------------------------------------
//
// D3D10/DDI entrypoints are invoked through runtime-provided function tables. The
// runtime expects these callbacks to not throw C++ exceptions. Wrap entrypoints
// at the table boundary so unexpected exceptions (e.g. std::bad_alloc) cannot
// unwind into the runtime.
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
        if (hDevice.pDrvPrivate) {
          auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
          set_error(dev, hr);
        }
      }
    }
  } catch (...) {
  }
}

template <auto Impl>
struct aerogpu_d3d10_1_wdk_ddi_thunk;

template <typename Ret, typename... Args, Ret(AEROGPU_APIENTRY* Impl)(Args...)>
struct aerogpu_d3d10_1_wdk_ddi_thunk<Impl> {
  static Ret AEROGPU_APIENTRY thunk(Args... args) noexcept {
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

#define AEROGPU_D3D10_1_WDK_DDI(fn) aerogpu_d3d10_1_wdk_ddi_thunk<&fn>::thunk

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
      set_error(dev, E_OUTOFMEMORY);
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

  uint32_t wddm_pitch = 0;
  __if_exists(D3DDDICB_LOCK::Pitch) {
    wddm_pitch = lock_args.Pitch;
  }

  // Guest-backed resources are updated by writing directly into the backing
  // allocation and emitting RESOURCE_DIRTY_RANGE. Ensure we can record the dirty
  // range before committing any bytes into the guest allocation (avoid
  // host/guest drift on OOM).
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const WddmAllocListCheckpoint alloc_checkpoint(dev);
  const auto restore_storage_from_allocation = [&]() {
    if (!res || res->storage.empty() || upload_size == 0) {
      return;
    }
    uint64_t allocation_size = res->wddm_allocation_size_bytes;
    if (allocation_size == 0) {
      allocation_size = static_cast<uint64_t>(res->storage.size());
    }
    const uint64_t end_u64 = upload_offset + upload_size;
    if (end_u64 < upload_offset) {
      return;
    }
    if (end_u64 > allocation_size) {
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
  aerogpu_cmd_resource_dirty_range* dirty_cmd = nullptr;

  HRESULT copy_hr = S_OK;
  if (res->kind == ResourceKind::Texture2D && !ValidateWddmTexturePitch(dev, res, wddm_pitch)) {
    copy_hr = E_INVALIDARG;
    goto Unlock;
  }

  if (dev->wddm_submit_allocation_list_oom) {
    restore_storage_from_allocation();
    dev->cmd.rollback(cmd_checkpoint);
    alloc_checkpoint.rollback();
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
    copy_hr = E_OUTOFMEMORY;
    goto Unlock;
  }
  dirty_cmd->resource_handle = res->handle;
  dirty_cmd->reserved0 = 0;
  dirty_cmd->offset_bytes = upload_offset;
  dirty_cmd->size_bytes = upload_size;

  if (res->kind == ResourceKind::Texture2D && upload_offset == 0 && upload_size == res->storage.size() &&
      res->mip_levels == 1 && res->array_size == 1) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      copy_hr = E_NOTIMPL;
      goto Unlock;
    }
    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    const uint32_t rows = aerogpu_texture_num_rows(aer_fmt, res->height);
    if (row_bytes == 0 || rows == 0) {
      copy_hr = E_INVALIDARG;
      goto Unlock;
    }

    // Guest-backed textures are interpreted by the host using the protocol pitch
    // (`CREATE_TEXTURE2D.row_pitch_bytes`). Ignore the runtime-reported pitch to
    // avoid writing rows with a stride the host does not expect.
    const uint32_t dst_pitch = res->row_pitch_bytes;
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
  if (FAILED(copy_hr)) {
    // Best-effort rollback for failed uploads: keep the shadow copy in sync with
    // the guest allocation bytes we actually have.
    restore_storage_from_allocation();
  }
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

  (void)dirty_cmd;
}

void emit_dirty_range_locked(AeroGpuDevice* dev,
                             const AeroGpuResource* res,
                             uint64_t offset_bytes,
                             uint64_t size_bytes) {
  if (!dev || !res || res->handle == kInvalidHandle || !size_bytes) {
    return;
  }

  // RESOURCE_DIRTY_RANGE causes the host to read the guest allocation to update the host copy.
  TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->offset_bytes = offset_bytes;
  cmd->size_bytes = size_bytes;
}

template <typename TFnPtr>
struct DdiStub;

static AeroGpuDevice* DeviceFromHandle(D3D10DDI_HDEVICE hDevice) {
  return hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
}

template <typename T>
static AeroGpuDevice* DeviceFromHandle(T) {
  return nullptr;
}

inline void ReportNotImpl() {}

template <typename Handle0, typename... Rest>
inline void ReportNotImpl(Handle0 handle0, Rest...) {
  if (auto* dev = DeviceFromHandle(handle0)) {
    set_error(dev, E_NOTIMPL);
  }
}

template <typename Ret, typename... Args>
struct DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args... args) noexcept {
    try {
      ((void)args, ...);
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_NOTIMPL;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        // Returning zero from a CalcPrivate*Size stub often causes the runtime to
        // pass a null pDrvPrivate, which in turn tends to crash when the runtime
        // tries to create/destroy the object. Return a small non-zero size so the
        // handle always has valid storage, even when Create* returns E_NOTIMPL.
        return sizeof(uint64_t);
      } else if constexpr (std::is_same_v<Ret, void>) {
        ReportNotImpl(args...);
        return;
      } else {
        return Ret{};
      }
    } catch (...) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_NOTIMPL;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        return sizeof(uint64_t);
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
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
    set_error(DeviceFromHandle(hDevice), E_NOTIMPL);
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
  static Ret AEROGPU_APIENTRY Call(Args... args) noexcept {
    try {
      constexpr const char* kName = kDdiTraceStubNames[static_cast<size_t>(Id)];
      AEROGPU_D3D10_TRACEF("%s (stub)", kName);

      if constexpr (std::is_same_v<Ret, HRESULT>) {
        const HRESULT hr = DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
        return ::aerogpu::d3d10trace::ret_hr(kName, hr);
      } else {
        return DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
      }
    } catch (...) {
      return DdiStub<Ret(AEROGPU_APIENTRY*)(Args...)>::Call(args...);
    }
  }
};
#endif // AEROGPU_D3D10_TRACE
template <typename TFnPtr>
struct DdiErrorStub;

template <typename... Args>
struct DdiErrorStub<void(AEROGPU_APIENTRY*)(D3D10DDI_HDEVICE, Args...)> {
  static void AEROGPU_APIENTRY Call(D3D10DDI_HDEVICE hDevice, Args...) noexcept {
    try {
      auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
      set_error(dev, E_NOTIMPL);
    } catch (...) {
    }
  }
};

template <typename TFnPtr>
struct DdiNoopStub;

template <typename Ret, typename... Args>
struct DdiNoopStub<Ret(AEROGPU_APIENTRY*)(Args...)> {
  static Ret AEROGPU_APIENTRY Call(Args...) noexcept {
    try {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return S_OK;
      } else if constexpr (std::is_same_v<Ret, SIZE_T>) {
        return sizeof(uint64_t);
      } else if constexpr (std::is_same_v<Ret, void>) {
        return;
      } else {
        return Ret{};
      }
    } catch (...) {
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
  }
};

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
  X(pfnIaSetInputLayout)                         \
  X(pfnIaSetVertexBuffers)                       \
  X(pfnIaSetIndexBuffer)                         \
  X(pfnIaSetTopology)                            \
  X(pfnVsSetShader)                              \
  X(pfnVsSetConstantBuffers)                     \
  X(pfnVsSetShaderResources)                     \
  X(pfnVsSetSamplers)                            \
  X(pfnGsSetShader)                              \
  X(pfnGsSetConstantBuffers)                     \
  X(pfnGsSetShaderResources)                     \
  X(pfnGsSetSamplers)                            \
  X(pfnSoSetTargets)                             \
  X(pfnPsSetShader)                              \
  X(pfnPsSetConstantBuffers)                     \
  X(pfnPsSetShaderResources)                     \
  X(pfnPsSetSamplers)                            \
  X(pfnSetViewports)                             \
  X(pfnSetScissorRects)                          \
  X(pfnSetRasterizerState)                       \
  X(pfnSetBlendState)                            \
  X(pfnSetDepthStencilState)                     \
  X(pfnSetRenderTargets)                         \
  X(pfnSetPredication)                           \
  X(pfnClearState)                               \
  X(pfnSetTextFilterSize)                        \
  X(pfnGenMips)                                  \
  X(pfnGenerateMips)                             \
  X(pfnFlush)                                    \
  X(pfnUnmap)

#define AEROGPU_D3D10_ADAPTERFUNCS_FIELDS(X) \
  X(pfnGetCaps)                              \
  X(pfnCalcPrivateDeviceSize)                \
  X(pfnCreateDevice)                         \
  X(pfnCloseAdapter)

static void InitDeviceFuncsWithStubs(D3D10DDI_DEVICEFUNCS* funcs) {
  if (!funcs) {
    return;
  }
  std::memset(funcs, 0, sizeof(*funcs));
#define AEROGPU_D3D10_ASSIGN_STUB(field) \
  __if_exists(D3D10DDI_DEVICEFUNCS::field) { funcs->field = &DdiStub<decltype(funcs->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_FIELDS(AEROGPU_D3D10_ASSIGN_STUB)
#undef AEROGPU_D3D10_ASSIGN_STUB
#define AEROGPU_D3D10_ASSIGN_NOOP(field) \
  __if_exists(D3D10DDI_DEVICEFUNCS::field) { funcs->field = &DdiNoopStub<decltype(funcs->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_NOOP_FIELDS(AEROGPU_D3D10_ASSIGN_NOOP)
#undef AEROGPU_D3D10_ASSIGN_NOOP
}

static void InitDeviceFuncsWithStubs(D3D10_1DDI_DEVICEFUNCS* funcs) {
  if (!funcs) {
    return;
  }
  std::memset(funcs, 0, sizeof(*funcs));
#define AEROGPU_D3D10_ASSIGN_STUB(field) \
  __if_exists(D3D10_1DDI_DEVICEFUNCS::field) { funcs->field = &DdiStub<decltype(funcs->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_FIELDS(AEROGPU_D3D10_ASSIGN_STUB)
#undef AEROGPU_D3D10_ASSIGN_STUB
#define AEROGPU_D3D10_ASSIGN_NOOP(field) \
  __if_exists(D3D10_1DDI_DEVICEFUNCS::field) { funcs->field = &DdiNoopStub<decltype(funcs->field)>::Call; }
  AEROGPU_D3D10_DEVICEFUNCS_NOOP_FIELDS(AEROGPU_D3D10_ASSIGN_NOOP)
#undef AEROGPU_D3D10_ASSIGN_NOOP
}

static void InitAdapterFuncsWithStubs(D3D10_1DDI_ADAPTERFUNCS* funcs) {
  if (!funcs) {
    return;
  }
  std::memset(funcs, 0, sizeof(*funcs));
#define AEROGPU_D3D10_ASSIGN_ADAPTER_STUB(field) \
  __if_exists(D3D10_1DDI_ADAPTERFUNCS::field) { funcs->field = &DdiStub<decltype(funcs->field)>::Call; }
  AEROGPU_D3D10_ADAPTERFUNCS_FIELDS(AEROGPU_D3D10_ASSIGN_ADAPTER_STUB)
#undef AEROGPU_D3D10_ASSIGN_ADAPTER_STUB
}

// CopyResource is used by the Win7 staging readback path (copy backbuffer ->
// staging, then Map). Prefer emitting COPY_* commands so the host executor can
// perform the copy; for staging destinations request WRITEBACK_DST so Map(READ)
// observes the updated bytes.

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
        bool emitted_copy = false;
        if (aerogpu::d3d10_11::SupportsTransfer(dev) && transfer_aligned && !same_buffer) {
          const auto cmd_checkpoint = dev->cmd.checkpoint();
          const WddmAllocListCheckpoint alloc_checkpoint(dev);

          if (TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true) &&
              TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false)) {
            auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
            if (cmd) {
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
              TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { set_error(dev, hr); });
              emitted_copy = true;
            }
          }

          if (!emitted_copy) {
            dev->cmd.rollback(cmd_checkpoint);
            alloc_checkpoint.rollback();
          }
        }

        if (!emitted_copy && copy_bytes) {
          emit_upload_resource_locked(dev, dst, 0, copy_bytes);
        }
      } else if (dst->kind == ResourceKind::Texture2D) {
        if (dst->dxgi_format != src->dxgi_format) {
          return finish(E_INVALIDARG);
        }

        const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
        if (aer_fmt == AEROGPU_FORMAT_INVALID) {
          return finish(E_NOTIMPL);
        }
        if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
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
        bool emitted_copy = false;
        if (aerogpu::d3d10_11::SupportsTransfer(dev) && !same_texture) {
          const auto cmd_checkpoint = dev->cmd.checkpoint();
          const WddmAllocListCheckpoint alloc_checkpoint(dev);

           if (TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true) &&
               TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false)) {
             uint32_t copy_flags = AEROGPU_COPY_FLAG_NONE;
             if (dst->backing_alloc_id != 0 && dst->wddm_allocation_handle != 0) {
               // Keep guest-backed Texture2D resources coherent: host executors may
               // upload whole subresources on any intersecting RESOURCE_DIRTY_RANGE.
               // If a prior GPU-side copy updates only the host texture, later CPU
               // updates could overwrite it with stale guest backing bytes.
               copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
             }

             bool ok = true;
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
                ok = false;
                break;
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
            if (ok) {
              TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { set_error(dev, hr); });
              emitted_copy = true;
            }
          }

          if (!emitted_copy) {
            dev->cmd.rollback(cmd_checkpoint);
            alloc_checkpoint.rollback();
          }
        }

        if (!emitted_copy && dst_total != 0) {
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
        bool emitted_copy = false;
        if (aerogpu::d3d10_11::SupportsTransfer(dev) && transfer_aligned && bytes && !same_buffer) {
          const auto cmd_checkpoint = dev->cmd.checkpoint();
          const WddmAllocListCheckpoint alloc_checkpoint(dev);

          if (TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true) &&
              TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false)) {
            auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
            if (cmd) {
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
              TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { set_error(dev, hr); });
              emitted_copy = true;
            }
          }

          if (!emitted_copy) {
            dev->cmd.rollback(cmd_checkpoint);
            alloc_checkpoint.rollback();
          }
        }

        if (!emitted_copy && bytes) {
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

        const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, dst->dxgi_format);
        if (aer_fmt == AEROGPU_FORMAT_INVALID) {
          return finish(E_NOTIMPL);
        }
        if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
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
        bool emitted_copy = false;
        if (aerogpu::d3d10_11::SupportsTransfer(dev) && !same_texture) {
          const auto cmd_checkpoint = dev->cmd.checkpoint();
          const WddmAllocListCheckpoint alloc_checkpoint(dev);

          if (TryTrackWddmAllocForSubmitLocked(dev, dst, /*write=*/true) &&
              TryTrackWddmAllocForSubmitLocked(dev, src, /*write=*/false)) {
            auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
            if (cmd) {
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
               if (dst->backing_alloc_id != 0 && dst->wddm_allocation_handle != 0) {
                 // Keep guest-backed Texture2D resources coherent: host executors may
                 // upload whole subresources on any intersecting RESOURCE_DIRTY_RANGE.
                 // If a prior GPU-side copy updates only the host texture, later CPU
                 // updates could overwrite it with stale guest backing bytes.
                 copy_flags |= AEROGPU_COPY_FLAG_WRITEBACK_DST;
               }
               cmd->flags = copy_flags;
               cmd->reserved0 = 0;
               TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { set_error(dev, hr); });
               emitted_copy = true;
             }
          }

          if (!emitted_copy) {
            dev->cmd.rollback(cmd_checkpoint);
            alloc_checkpoint.rollback();
          }
        }

        if (!emitted_copy) {
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
  if (!HasLiveCookie(device_mem, kD3D10_1DeviceLiveCookie)) {
    return;
  }
  uint32_t cookie = 0;
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
  if (!hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the resource so DestroyResource is safe even if CreateResource
  // fails early.
  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();

  if (!hDevice.pDrvPrivate || !pDesc) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(res);
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

  res->handle = AllocateGlobalHandle(dev->adapter);
  res->bind_flags = pDesc->BindFlags;
  res->misc_flags = pDesc->MiscFlags;
  __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
    res->usage = static_cast<uint32_t>(pDesc->Usage);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::CPUAccessFlags) {
    res->cpu_access_flags |= static_cast<uint32_t>(pDesc->CPUAccessFlags);
  }
  __if_exists(D3D10DDIARG_CREATERESOURCE::CpuAccessFlags) {
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
          set_error(dev, E_OUTOFMEMORY);
          km_allocs = nullptr;
          km_alloc_count = 0;
        }
      }
    }

    D3DDDICB_DEALLOCATE dealloc = {};
    __if_exists(D3DDDICB_DEALLOCATE::hContext) {
      dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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
      if (static_cast<uint32_t>(pDesc->Usage) == kD3D10UsageStaging) {
        priv.flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING;
      }
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
    try {
      res->wddm.km_allocation_handles.push_back(km_alloc);
    } catch (...) {
      // Ensure we don't leak the just-allocated KM resource/allocation if the UMD
      // cannot record its handles due to OOM.
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
      res->wddm.km_allocation_handles.clear();
      res->wddm.km_resource_handle = 0;
      res->wddm_allocation_handle = 0;
      return E_OUTOFMEMORY;
    }
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
      is_staging = (usage == kD3D10UsageStaging);
      cpu_visible = cpu_visible || is_staging;
    }

    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
    is_shared = (res->misc_flags & kD3D10ResourceMiscShared) != 0;
    if (res->misc_flags & kD3D10ResourceMiscSharedKeyedMutex) {
      is_shared = true;
    }
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      want_host_owned = (usage == kD3D10UsageDynamic);
    }
    want_host_owned = want_host_owned && !is_shared;

    HRESULT alloc_hr = allocate_one(alloc_size, cpu_visible, is_rt, is_ds, is_shared, is_primary, 0, nullptr);
    if (FAILED(alloc_hr)) {
      set_error(dev, alloc_hr);
      deallocate_if_needed();
      ResetObject(res);
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
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(init_hr);
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_buffer_usage_flags(res->bind_flags);
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
        ResetObject(res);
        AEROGPU_D3D10_RET_HR(E_FAIL);
      }

      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        ResetObject(res);
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
        ResetObject(res);
        AEROGPU_D3D10_RET_HR(submit_hr);
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  if (pDesc->ResourceDimension == D3D10DDIRESOURCE_TEXTURE2D) {
    const uint32_t aer_fmt =
        aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, static_cast<uint32_t>(pDesc->Format));
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    if (!pDesc->pMipInfoList) {
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    res->kind = ResourceKind::Texture2D;
    res->width = pDesc->pMipInfoList[0].TexelWidth;
    res->height = pDesc->pMipInfoList[0].TexelHeight;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : aerogpu::d3d10_11::CalcFullMipLevels(res->width, res->height);
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = static_cast<uint32_t>(pDesc->Format);
    if (res->mip_levels == 0 || res->array_size == 0) {
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (row_bytes == 0) {
      deallocate_if_needed();
      ResetObject(res);
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
      ResetObject(res);
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
      is_staging = (usage == kD3D10UsageStaging);
      cpu_visible = cpu_visible || is_staging;
    }
    const bool is_rt = (res->bind_flags & kD3D10BindRenderTarget) != 0;
    const bool is_ds = (res->bind_flags & kD3D10BindDepthStencil) != 0;
    bool is_shared = false;
    is_shared = (res->misc_flags & kD3D10ResourceMiscShared) != 0;
    if (res->misc_flags & kD3D10ResourceMiscSharedKeyedMutex) {
      is_shared = true;
    }
    if (is_shared && (res->mip_levels != 1 || res->array_size != 1)) {
      // Keep shared surface interop conservative: only support the legacy single-subresource layout.
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
    res->is_shared = is_shared;
    const bool want_guest_backed = !is_shared && !is_primary && !is_staging && !is_rt && !is_ds;
    cpu_visible = cpu_visible || want_guest_backed;

    bool want_host_owned = false;
    __if_exists(D3D10DDIARG_CREATERESOURCE::Usage) {
      const uint32_t usage = static_cast<uint32_t>(pDesc->Usage);
      want_host_owned = (usage == kD3D10UsageDynamic);
    }
    want_host_owned = want_host_owned && !is_shared;
    // Host-owned Texture2D updates go through `AEROGPU_CMD_UPLOAD_RESOURCE`. The protocol supports
    // arbitrary byte ranges, so host-owned is compatible with mip/array textures as long as uploads
    // are expressed in terms of subresource byte offsets (the Map/Unmap and UpdateSubresourceUP
    // paths upload whole subresources).

    aerogpu_wddm_alloc_priv_v2 alloc_priv = {};
    HRESULT alloc_hr = allocate_one(total_bytes,
                                    cpu_visible,
                                    is_rt,
                                    is_ds,
                                    is_shared,
                                    is_primary,
                                    res->row_pitch_bytes,
                                    &alloc_priv);
    if (FAILED(alloc_hr)) {
      set_error(dev, alloc_hr);
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(alloc_hr);
    }

    if (want_host_owned) {
      res->backing_alloc_id = 0;
      res->backing_offset_bytes = 0;
    } else {
      uint64_t backing_size = total_bytes;
      if (alloc_priv.size_bytes) {
        backing_size = static_cast<uint64_t>(alloc_priv.size_bytes);
      } else if (pDesc->pAllocationInfo) {
        backing_size = static_cast<uint64_t>(pDesc->pAllocationInfo[0].Size);
      }

      uint32_t alloc_pitch = alloc_priv.row_pitch_bytes;
      if (alloc_pitch == 0 && !AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(alloc_priv.reserved0)) {
        alloc_pitch = static_cast<uint32_t>(alloc_priv.reserved0 & 0xFFFFFFFFu);
      }
      if (alloc_pitch != 0 && alloc_pitch != res->row_pitch_bytes) {
        if (alloc_pitch < row_bytes) {
          set_error(dev, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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
          set_error(dev, E_FAIL);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_FAIL);
        }
        if (updated_total_bytes == 0 || updated_total_bytes > backing_size ||
            updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
          set_error(dev, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_INVALIDARG);
        }
        res->row_pitch_bytes = alloc_pitch;
        res->tex2d_subresources = std::move(updated_layouts);
        total_bytes = updated_total_bytes;
      }

      // Query the runtime/KMD-selected pitch via a LockCb round-trip so our
      // protocol-visible layout matches the actual mapped allocation.
      const D3DDDI_DEVICECALLBACKS* ddi = dev->callbacks;
      if (ddi && ddi->pfnLockCb && ddi->pfnUnlockCb && res->wddm_allocation_handle != 0) {
        D3DDDICB_LOCK lock_args = {};
        lock_args.hAllocation = static_cast<D3DKMT_HANDLE>(res->wddm_allocation_handle);
        __if_exists(D3DDDICB_LOCK::SubresourceIndex) { lock_args.SubresourceIndex = 0; }
        __if_exists(D3DDDICB_LOCK::SubResourceIndex) { lock_args.SubResourceIndex = 0; }
        InitLockForWrite(&lock_args);

        HRESULT lock_hr = CallCbMaybeHandle(ddi->pfnLockCb, dev->hrt_device, &lock_args);
        if (SUCCEEDED(lock_hr)) {
          if (lock_args.pData) {
            uint32_t lock_pitch = 0;
            __if_exists(D3DDDICB_LOCK::Pitch) {
              lock_pitch = lock_args.Pitch;
            }
            if (lock_pitch != 0 && lock_pitch != res->row_pitch_bytes) {
              if (lock_pitch < row_bytes) {
                D3DDDICB_UNLOCK unlock_args = {};
                unlock_args.hAllocation = lock_args.hAllocation;
                InitUnlockForWrite(&unlock_args);
                (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);

                set_error(dev, E_INVALIDARG);
                deallocate_if_needed();
                ResetObject(res);
                AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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

                set_error(dev, E_FAIL);
                deallocate_if_needed();
                ResetObject(res);
                AEROGPU_D3D10_RET_HR(E_FAIL);
              }
              if (updated_total_bytes == 0 || updated_total_bytes > backing_size ||
                  updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
                D3DDDICB_UNLOCK unlock_args = {};
                unlock_args.hAllocation = lock_args.hAllocation;
                InitUnlockForWrite(&unlock_args);
                (void)CallCbMaybeHandle(ddi->pfnUnlockCb, dev->hrt_device, &unlock_args);

                set_error(dev, E_INVALIDARG);
                deallocate_if_needed();
                ResetObject(res);
                AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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
          AEROGPU_D3D10_11_LOG("D3D10.1 CreateResource: KMD overrode Texture2D pitch %u -> %u",
                               static_cast<unsigned>(res->row_pitch_bytes),
                               static_cast<unsigned>(alloc_pitch));
        } else if (n == 32) {
          AEROGPU_D3D10_11_LOG("D3D10.1 CreateResource: pitch override log limit reached; suppressing further messages");
        }

        if (alloc_pitch < row_bytes) {
          set_error(dev, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_INVALIDARG);
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
          set_error(dev, E_FAIL);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_FAIL);
        }

        const uint64_t backing_size =
            alloc_priv.size_bytes ? static_cast<uint64_t>(alloc_priv.size_bytes) : total_bytes;
        if (updated_total_bytes == 0 ||
            updated_total_bytes > backing_size ||
            updated_total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
          set_error(dev, E_INVALIDARG);
          deallocate_if_needed();
          ResetObject(res);
          AEROGPU_D3D10_RET_HR(E_INVALIDARG);
        }

        res->row_pitch_bytes = alloc_pitch;
        res->tex2d_subresources = std::move(updated_layouts);
        total_bytes = updated_total_bytes;
      }
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
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(init_hr);
    }

    TrackWddmAllocForSubmitLocked(dev, res, /*write=*/false);

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      deallocate_if_needed();
      ResetObject(res);
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
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
      emit_upload_resource_locked(dev, res, 0, res->storage.size());
    }

    if (is_shared) {
      if (res->share_token == 0) {
        set_error(dev, E_FAIL);
        deallocate_if_needed();
        ResetObject(res);
        AEROGPU_D3D10_RET_HR(E_FAIL);
      }
      auto* export_cmd =
          dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
      if (!export_cmd) {
        deallocate_if_needed();
        ResetObject(res);
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
        ResetObject(res);
        AEROGPU_D3D10_RET_HR(submit_hr);
      }
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  deallocate_if_needed();
  ResetObject(res);
  AEROGPU_D3D10_RET_HR(E_NOTIMPL);
}

HRESULT AEROGPU_APIENTRY OpenResource(D3D10DDI_HDEVICE hDevice,
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
    if (total_bytes == 0 || total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      ResetObject(res);
      return E_INVALIDARG;
    }
    try {
      res->storage.resize(static_cast<size_t>(total_bytes), 0);
    } catch (...) {
      ResetObject(res);
      return E_OUTOFMEMORY;
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

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_TRACEF("DestroyResource hDevice=%p hResource=%p", hDevice.pDrvPrivate, hResource.pDrvPrivate);
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
  if (res->mapped) {
    unmap_resource_locked(dev, res, res->mapped_subresource);
  }
  bool rt_state_changed = false;
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (dev->current_rtv_resources[i] == res) {
      dev->current_rtv_resources[i] = nullptr;
      dev->current_rtvs[i] = 0;
      rt_state_changed = true;
    }
  }
  if (dev->current_dsv_res == res) {
    dev->current_dsv_res = nullptr;
    dev->current_dsv = 0;
    rt_state_changed = true;
  }
  if (rt_state_changed) {
    (void)EmitSetRenderTargetsLocked(dev, [&](HRESULT hr) { set_error(dev, hr); });
  }
  bool oom = false;
  // Unbind any IA vertex buffer slots that reference this resource.
  for (uint32_t slot = 0; slot < dev->current_vb_resources.size(); ++slot) {
    if (dev->current_vb_resources[slot] != res) {
      continue;
    }
    dev->current_vb_resources[slot] = nullptr;
    dev->current_vb_strides[slot] = 0;
    dev->current_vb_offsets[slot] = 0;
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
                                                            [&](HRESULT hr) { set_error(dev, hr); })) {
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
                                                         [&](HRESULT hr) { set_error(dev, hr); });
  }

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (dev->current_vs_srvs[slot] == res) {
      dev->current_vs_srvs[slot] = nullptr;
      if (!oom) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
        } else {
          cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
          cmd->slot = slot;
          cmd->texture = 0;
          cmd->reserved0 = 0;
        }
      }
    }
  }
  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (dev->current_ps_srvs[slot] == res) {
      dev->current_ps_srvs[slot] = nullptr;
      if (!oom) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
        } else {
          cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
          cmd->slot = slot;
          cmd->texture = 0;
          cmd->reserved0 = 0;
        }
      }
    }
  }
  for (uint32_t slot = 0; slot < dev->current_gs_srvs.size(); ++slot) {
    if (dev->current_gs_srvs[slot] == res) {
      dev->current_gs_srvs[slot] = nullptr;
      if (!oom) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
        } else {
          cmd->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
          cmd->slot = slot;
          cmd->texture = 0;
          cmd->reserved0 = 0;
        }
      }
    }
  }

  auto unbind_constant_buffers = [&](uint32_t shader_stage,
                                     std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots>& table,
                                     std::array<AeroGpuResource*, kMaxConstantBufferSlots>& resources) {
    bool any = false;
    for (uint32_t slot = 0; slot < kMaxConstantBufferSlots; ++slot) {
      if (resources[slot] == res) {
        resources[slot] = nullptr;
        table[slot] = {};
        any = true;
      }
    }
    if (!any) {
      return;
    }

    for (AeroGpuResource* bound : resources) {
      TrackWddmAllocForSubmitLocked(dev, bound, /*write=*/false);
    }

    if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                            shader_stage,
                                                            /*start_slot=*/0,
                                                            static_cast<uint32_t>(table.size()),
                                                            table.data(),
                                                            [&](HRESULT hr) { set_error(dev, hr); })) {
      return;
    }
  };

  unbind_constant_buffers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_constant_buffers, dev->current_vs_cb_resources);
  unbind_constant_buffers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_constant_buffers, dev->current_ps_cb_resources);
  unbind_constant_buffers(AEROGPU_SHADER_STAGE_GEOMETRY, dev->gs_constant_buffers, dev->current_gs_cb_resources);

  if (res->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
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
      set_error(dev, submit_hr);
    }
  }

  if (res->wddm.km_resource_handle != 0 || !res->wddm.km_allocation_handles.empty()) {
    const D3DDDI_DEVICECALLBACKS* cb = dev->callbacks;
    if (!cb || !cb->pfnDeallocateCb) {
      set_error(dev, E_FAIL);
    } else {
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
            set_error(dev, E_OUTOFMEMORY);
            km_allocs = nullptr;
            km_alloc_count = 0;
          }
        }
      }

      D3DDDICB_DEALLOCATE dealloc = {};
      __if_exists(D3DDDICB_DEALLOCATE::hContext) {
        dealloc.hContext = UintPtrToD3dHandle<decltype(dealloc.hContext)>(static_cast<std::uintptr_t>(dev->kmt_context));
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
  ResetObject(res);
}

// -------------------------------------------------------------------------------------------------
// Map/unmap (Win7 D3D11 runtimes may use specialized entrypoints).
// -------------------------------------------------------------------------------------------------

using aerogpu::d3d10_11::kD3DMapRead;
using aerogpu::d3d10_11::kD3DMapWrite;
using aerogpu::d3d10_11::kD3DMapReadWrite;
using aerogpu::d3d10_11::kD3DMapWriteDiscard;
using aerogpu::d3d10_11::kD3DMapWriteNoOverwrite;
// D3D10_MAP_FLAG_DO_NOT_WAIT (numeric value from d3d10.h / d3d10_1.h).

HRESULT sync_read_map_locked(AeroGpuDevice* dev, const AeroGpuResource* res, uint32_t map_type, uint32_t map_flags) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }
  const bool want_read = (map_type == kD3DMapRead || map_type == kD3DMapReadWrite);
  if (!want_read) {
    return S_OK;
  }

  // Only apply implicit readback synchronization for staging resources.
  if (res->usage != kD3D10UsageStaging) {
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
  Texture2DSubresourceLayout tex_layout{};
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
    tex_layout = res->tex2d_subresources[subresource];
    map_offset = tex_layout.offset_bytes;
    map_size = tex_layout.size_bytes;
    map_row_pitch = tex_layout.row_pitch_bytes;
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

  const bool allow_storage_map =
      (res->backing_alloc_id == 0) && !(want_read && res->usage == kD3D10UsageStaging);
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

  const bool is_guest_backed = (res->backing_alloc_id != 0);
  const auto unlock_locked_allocation = [&]() {
    D3DDDICB_UNLOCK unlock_cb = {};
    unlock_cb.hAllocation = static_cast<D3DKMT_HANDLE>(alloc_handle);
    InitUnlockArgsForMap(&unlock_cb, lock_subresource);
    (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_cb);
  };

  // For Texture2D allocations, LockCb may return a pitch that differs from our
  // `Texture2DSubresourceLayout::row_pitch_bytes`. On Win7, we lock
  // SubresourceIndex=0 and use `offset_bytes` to reach other subresources, so the
  // LockCb pitch is only meaningful for mip0.
  uint32_t mapped_row_pitch = 0;
  uint32_t mapped_slice_pitch = 0;
  uint32_t tex_row_bytes = 0;
  uint32_t tex_rows = 0;
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t expected_pitch = map_row_pitch;
    const bool use_lock_pitch = (tex_layout.mip_level == 0);
    if (use_lock_pitch) {
      uint32_t lock_row_pitch = 0;
      uint32_t lock_slice_pitch = 0;
      __if_exists(D3DDDICB_LOCK::Pitch) {
        lock_row_pitch = lock_cb.Pitch;
      }
      __if_exists(D3DDDICB_LOCK::SlicePitch) {
        lock_slice_pitch = lock_cb.SlicePitch;
      }

      // Guest-backed resources are interpreted by the host using the protocol
      // pitch (CREATE_TEXTURE2D.row_pitch_bytes). Ignore runtime-reported pitch
      // so Map returns the same row stride the host will use.
      if (!is_guest_backed) {
        mapped_row_pitch = lock_row_pitch;
        mapped_slice_pitch = lock_slice_pitch;
      }
    }
    const uint32_t effective_row_pitch = mapped_row_pitch ? mapped_row_pitch : expected_pitch;

    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    tex_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, tex_layout.width);
    tex_rows = tex_layout.rows_in_layout;
    if (tex_row_bytes == 0 || tex_rows == 0 || expected_pitch < tex_row_bytes) {
      unlock_locked_allocation();
      if (allow_storage_map && !want_read) {
        return map_storage();
      }
      return E_INVALIDARG;
    }
    // Fail cleanly if the runtime reports a pitch that cannot fit the texel row.
    if (mapped_row_pitch != 0 && mapped_row_pitch < tex_row_bytes) {
      unlock_locked_allocation();
      if (allow_storage_map && !want_read) {
        return map_storage();
      }
      return E_INVALIDARG;
    }
    if (mapped_slice_pitch == 0) {
      const uint64_t slice_pitch_u64 =
          static_cast<uint64_t>(effective_row_pitch) * static_cast<uint64_t>(tex_rows);
      if (slice_pitch_u64 == 0 || slice_pitch_u64 > UINT32_MAX) {
        unlock_locked_allocation();
        if (allow_storage_map && !want_read) {
          return map_storage();
        }
        return E_INVALIDARG;
      }
      mapped_slice_pitch = static_cast<uint32_t>(slice_pitch_u64);
    }
  }

  uint8_t* mapped_ptr = static_cast<uint8_t*>(lock_cb.pData);
  if (res->kind == ResourceKind::Texture2D) {
    if (map_offset > static_cast<uint64_t>(SIZE_MAX)) {
      unlock_locked_allocation();
      if (allow_storage_map && !want_read) {
        return map_storage();
      }
      return E_FAIL;
    }
    if (map_offset != 0) {
      mapped_ptr = mapped_ptr + static_cast<size_t>(map_offset);
    }
  }

  if (!res->storage.empty()) {
    if (map_type == kD3DMapWriteDiscard) {
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t dst_pitch = mapped_row_pitch ? mapped_row_pitch : map_row_pitch;
        if (tex_row_bytes != 0 && tex_rows != 0 && dst_pitch >= tex_row_bytes) {
          for (uint32_t y = 0; y < tex_rows; ++y) {
            const size_t dst_off_row = static_cast<size_t>(y) * dst_pitch;
            std::memset(mapped_ptr + dst_off_row, 0, dst_pitch);
          }
        } else if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
          std::memset(mapped_ptr, 0, static_cast<size_t>(map_size));
        }
      } else if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memset(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    0,
                    static_cast<size_t>(map_size));
      }
    } else if (!is_guest_backed && res->kind == ResourceKind::Texture2D) {
      const uint32_t src_pitch = map_row_pitch;
      const uint32_t dst_pitch = mapped_row_pitch ? mapped_row_pitch : map_row_pitch;
      const uint8_t* src_bytes = res->storage.data();
      uint8_t* dst_bytes = static_cast<uint8_t*>(lock_cb.pData);
      if (tex_row_bytes != 0 && tex_rows != 0 &&
          src_pitch >= tex_row_bytes && dst_pitch >= tex_row_bytes &&
          map_offset <= res->storage.size()) {
        for (uint32_t y = 0; y < tex_rows; ++y) {
          const uint64_t src_off_u64 =
              map_offset + static_cast<uint64_t>(y) * static_cast<uint64_t>(src_pitch);
          if (src_off_u64 > res->storage.size() ||
              tex_row_bytes > res->storage.size() - static_cast<size_t>(src_off_u64)) {
            break;
          }
          const size_t src_off = static_cast<size_t>(src_off_u64);
          const size_t dst_off = static_cast<size_t>(map_offset) + static_cast<size_t>(y) * dst_pitch;
          std::memcpy(dst_bytes + dst_off, src_bytes + src_off, tex_row_bytes);
          if (dst_pitch > tex_row_bytes) {
            std::memset(dst_bytes + dst_off + tex_row_bytes, 0, dst_pitch - tex_row_bytes);
          }
        }
      }
    } else if (!is_guest_backed) {
      if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memcpy(static_cast<uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    res->storage.data() + static_cast<size_t>(map_offset),
                    static_cast<size_t>(map_size));
      }
    } else if (want_read || (want_write && res->usage == kD3D10UsageStaging)) {
      if (res->kind == ResourceKind::Texture2D) {
        const uint32_t src_pitch = mapped_row_pitch ? mapped_row_pitch : map_row_pitch;
        const uint32_t dst_pitch = map_row_pitch;
        const uint8_t* src_bytes = static_cast<const uint8_t*>(lock_cb.pData);
        uint8_t* dst_bytes = res->storage.data();
        if (tex_row_bytes != 0 && tex_rows != 0 &&
            src_pitch >= tex_row_bytes && dst_pitch >= tex_row_bytes &&
            map_offset <= res->storage.size()) {
          for (uint32_t y = 0; y < tex_rows; ++y) {
            const uint64_t dst_off_u64 =
                map_offset + static_cast<uint64_t>(y) * static_cast<uint64_t>(dst_pitch);
            if (dst_off_u64 > res->storage.size() ||
                tex_row_bytes > res->storage.size() - static_cast<size_t>(dst_off_u64)) {
              break;
            }
            const size_t dst_off = static_cast<size_t>(dst_off_u64);
            const size_t src_off = static_cast<size_t>(map_offset) + static_cast<size_t>(y) * src_pitch;
            std::memcpy(dst_bytes + dst_off, src_bytes + src_off, tex_row_bytes);
            if (dst_pitch > tex_row_bytes) {
              std::memset(dst_bytes + dst_off + tex_row_bytes, 0, dst_pitch - tex_row_bytes);
            }
          }
        }
      } else if (map_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memcpy(res->storage.data() + static_cast<size_t>(map_offset),
                    static_cast<const uint8_t*>(lock_cb.pData) + static_cast<size_t>(map_offset),
                    static_cast<size_t>(map_size));
      }
    }
  }

  if (res->kind == ResourceKind::Texture2D) {
    pMapped->pData = mapped_ptr;
    const uint32_t row_pitch = mapped_row_pitch ? mapped_row_pitch : map_row_pitch;
    pMapped->RowPitch = row_pitch;
    pMapped->DepthPitch =
        mapped_slice_pitch ? mapped_slice_pitch
                           : static_cast<UINT>(row_pitch) * static_cast<UINT>(tex_rows);
  } else {
    pMapped->pData = lock_cb.pData;
    pMapped->RowPitch = 0;
    pMapped->DepthPitch = 0;
  }

  res->mapped_wddm_ptr = lock_cb.pData;
  res->mapped_wddm_allocation = static_cast<uint64_t>(alloc_handle);
  res->mapped_wddm_pitch = mapped_row_pitch;
  res->mapped_wddm_slice_pitch = mapped_slice_pitch;

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

  const bool is_write = res->mapped_write;
  bool dirty_emitted_on_unmap = false;
  bool dirty_failed_on_unmap = false;

  uint64_t upload_offset = res->mapped_offset;
  uint64_t upload_size = res->mapped_size;
  bool emit_ok = (is_write && res->mapped_size != 0);
  if (emit_ok && res->kind == ResourceKind::Buffer) {
    const uint64_t end = res->mapped_offset + res->mapped_size;
    if (end < res->mapped_offset) {
      emit_ok = false;
    } else {
      upload_offset = res->mapped_offset & ~3ull;
      const uint64_t upload_end = AlignUpU64(end, 4);
      upload_size = upload_end - upload_offset;
    }
  }

  if (res->mapped_wddm_ptr && res->mapped_wddm_allocation) {
    if (emit_ok && res->backing_alloc_id != 0) {
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
          cmd->size_bytes = upload_size;
          dirty_emitted_on_unmap = true;
        }
      }
      if (!dirty_emitted_on_unmap) {
        dirty_failed_on_unmap = true;
        dev->cmd.rollback(cmd_checkpoint);
        alloc_checkpoint.rollback();

        // Best-effort rollback: restore the allocation bytes from the existing
        // shadow copy. This keeps guest memory consistent with the host-visible
        // contents even if we cannot notify the host of the CPU write.
        if (!res->storage.empty()) {
          const uint64_t off = res->mapped_offset;
          const uint64_t size = res->mapped_size;
          if (off <= static_cast<uint64_t>(SIZE_MAX) && off <= res->storage.size()) {
            const size_t off_sz = static_cast<size_t>(off);
            const size_t remaining = res->storage.size() - off_sz;
            const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
            if (copy_bytes) {
              uint8_t* dst = static_cast<uint8_t*>(res->mapped_wddm_ptr) + off_sz;
              const uint8_t* src = res->storage.data() + off_sz;
              std::memcpy(dst, src, copy_bytes);
            }
          }
        }

        set_error(dev, E_OUTOFMEMORY);
      }
    }

    if (is_write && !res->storage.empty() && res->mapped_size) {
      if (dirty_failed_on_unmap && res->backing_alloc_id != 0) {
        // We restored the allocation from the pre-map shadow copy above; keep
        // the shadow copy unchanged.
        goto UnlockMappedAllocation;
      }
      const uint8_t* src_base = static_cast<const uint8_t*>(res->mapped_wddm_ptr);
      const uint64_t off = res->mapped_offset;
      const uint64_t size = res->mapped_size;
      if (off <= res->storage.size()) {
        const size_t remaining = res->storage.size() - static_cast<size_t>(off);
        const size_t copy_bytes = static_cast<size_t>(std::min<uint64_t>(size, remaining));
        if (copy_bytes) {
          if (res->kind == ResourceKind::Texture2D) {
            if (res->mapped_subresource >= res->tex2d_subresources.size()) {
              // Fallback: best-effort linear copy.
              std::memcpy(res->storage.data() + static_cast<size_t>(off),
                          src_base + static_cast<size_t>(off),
                          copy_bytes);
            } else {
              const Texture2DSubresourceLayout& sub_layout = res->tex2d_subresources[res->mapped_subresource];
              const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
              const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, sub_layout.width);
              const uint32_t rows = sub_layout.rows_in_layout;
              // Only mip0 may report a pitch via LockCb; for other subresources we
              // rely on our packed layout pitches.
              const uint32_t src_pitch =
                  (sub_layout.mip_level == 0 && res->mapped_wddm_pitch) ? res->mapped_wddm_pitch : sub_layout.row_pitch_bytes;
              const uint32_t dst_pitch = sub_layout.row_pitch_bytes;

              const uint64_t src_needed =
                  (rows == 0) ? 0 : (static_cast<uint64_t>(rows - 1) * static_cast<uint64_t>(src_pitch) + row_bytes);
              const uint64_t dst_needed =
                  (rows == 0) ? 0 : (static_cast<uint64_t>(rows - 1) * static_cast<uint64_t>(dst_pitch) + row_bytes);

              if (row_bytes != 0 && rows != 0 && src_pitch != 0 && dst_pitch != 0 &&
                  src_pitch >= row_bytes && dst_pitch >= row_bytes &&
                  dst_needed <= static_cast<uint64_t>(remaining) &&
                  (res->mapped_wddm_slice_pitch == 0 || src_needed <= res->mapped_wddm_slice_pitch)) {
                const uint8_t* src = src_base + static_cast<size_t>(off);
                uint8_t* dst = res->storage.data() + static_cast<size_t>(off);
                for (uint32_t y = 0; y < rows; ++y) {
                  uint8_t* dst_row = dst + static_cast<size_t>(y) * dst_pitch;
                  const uint8_t* src_row = src + static_cast<size_t>(y) * src_pitch;
                  std::memcpy(dst_row, src_row, row_bytes);
                  if (dst_pitch > row_bytes) {
                    std::memset(dst_row + row_bytes, 0, dst_pitch - row_bytes);
                  }
                }
              } else {
                // Fallback: best-effort linear copy.
                std::memcpy(res->storage.data() + static_cast<size_t>(off),
                            src_base + static_cast<size_t>(off),
                            copy_bytes);
              }
            }
          } else {
            std::memcpy(res->storage.data() + static_cast<size_t>(off),
                        src_base + static_cast<size_t>(off),
                        copy_bytes);
          }
        }
      }
    }

  UnlockMappedAllocation:
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

  if (emit_ok) {
    if (res->backing_alloc_id != 0) {
      // If we already emitted (or failed to emit) a dirty range while the
      // allocation was still mapped, do not emit another one here.
      if (!dirty_emitted_on_unmap && !dirty_failed_on_unmap) {
        emit_dirty_range_locked(dev, res, upload_offset, upload_size);
      }
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
  if (res->usage != kD3D10UsageDynamic) {
    return E_INVALIDARG;
  }
  if ((res->cpu_access_flags & kD3D10CpuAccessWrite) == 0) {
    return E_INVALIDARG;
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
  if ((map_flags & ~kD3DMapFlagDoNotWait) != 0) {
    return E_INVALIDARG;
  }

  const uint32_t map_type_u = static_cast<uint32_t>(map_type);
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

  if (res->usage != kD3D10UsageStaging) {
    return E_INVALIDARG;
  }
  const uint32_t cpu_read = kD3D10CpuAccessRead;
  const uint32_t cpu_write = kD3D10CpuAccessWrite;
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
  if (want_read && !(res->cpu_access_flags & cpu_read)) {
    return E_INVALIDARG;
  }
  if (want_write && !(res->cpu_access_flags & cpu_write)) {
    return E_INVALIDARG;
  }

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
    if (dev) {
      set_error(dev, E_INVALIDARG);
    }
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || subresource != res->mapped_subresource) {
    set_error(dev, E_INVALIDARG);
    return;
  }
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
    if (dev) {
      set_error(dev, E_INVALIDARG);
    }
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    set_error(dev, E_INVALIDARG);
    return;
  }
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
    if (dev) {
      set_error(dev, E_INVALIDARG);
    }
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  if (!res->mapped || res->mapped_subresource != 0) {
    set_error(dev, E_INVALIDARG);
    return;
  }
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
  if ((static_cast<uint32_t>(map_flags) & ~kD3DMapFlagDoNotWait) != 0) {
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
      return E_INVALIDARG;
  }

  if (want_read && !(res->cpu_access_flags & cpu_read)) {
    return E_INVALIDARG;
  }
  if (want_write && !(res->cpu_access_flags & cpu_write)) {
    return E_INVALIDARG;
  }

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

SIZE_T AEROGPU_APIENTRY CalcPrivateGeometryShaderSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEGEOMETRYSHADER*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateGeometryShaderSize");
  return sizeof(AeroGpuShader);
}

template <typename TShaderHandle>
static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const void* pCode,
                                  SIZE_T code_size,
                                  TShaderHandle hShader,
                                  uint32_t stage) {
  if (!hShader.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the shader so Destroy*Shader is safe even if CreateShaderCommon
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

  sh->handle = AllocateGlobalHandle(dev->adapter);
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
    set_error(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

template <typename T, typename = void>
struct has_member_pShaderCode : std::false_type {};
template <typename T>
struct has_member_pShaderCode<T, std::void_t<decltype(std::declval<T>().pShaderCode)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CodeSize : std::false_type {};
template <typename T>
struct has_member_CodeSize<T, std::void_t<decltype(std::declval<T>().CodeSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ShaderCodeSize : std::false_type {};
template <typename T>
struct has_member_ShaderCodeSize<T, std::void_t<decltype(std::declval<T>().ShaderCodeSize)>> : std::true_type {};

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
    D3D10DDI_HGEOMETRYSHADER hShader{};
    const void* shader_code = nullptr;
    SIZE_T shader_code_size = 0;

    auto capture = [&](auto v) {
      using T = std::decay_t<decltype(v)>;
      if constexpr (std::is_same_v<T, D3D10DDI_HDEVICE>) {
        hDevice = v;
      } else if constexpr (std::is_same_v<T, D3D10DDI_HGEOMETRYSHADER>) {
        hShader = v;
      } else if constexpr (std::is_pointer_v<T>) {
        using Pointee = std::remove_pointer_t<T>;
        if constexpr (has_member_pShaderCode<Pointee>::value &&
                      (has_member_CodeSize<Pointee>::value || has_member_ShaderCodeSize<Pointee>::value)) {
          if (v) {
            shader_code = v->pShaderCode;
            if constexpr (has_member_CodeSize<Pointee>::value) {
              shader_code_size = static_cast<SIZE_T>(v->CodeSize);
            } else {
              shader_code_size = static_cast<SIZE_T>(v->ShaderCodeSize);
            }
          }
        }
      }
    };
    (capture(args), ...);

    const HRESULT hr = CreateShaderCommon(hDevice, shader_code, shader_code_size, hShader, AEROGPU_SHADER_STAGE_GEOMETRY);
    return static_cast<Ret>(hr);
  }
};

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const D3D10DDIARG_CREATEVERTEXSHADER* pDesc,
                                            D3D10DDI_HVERTEXSHADER hShader,
                                            D3D10DDI_HRTVERTEXSHADER) {
  AEROGPU_D3D10_TRACEF("CreateVertexShader codeSize=%u", pDesc ? static_cast<unsigned>(pDesc->CodeSize) : 0u);
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc ? pDesc->pShaderCode : nullptr, pDesc ? pDesc->CodeSize : 0, hShader, AEROGPU_SHADER_STAGE_VERTEX);
  AEROGPU_D3D10_RET_HR(hr);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const D3D10DDIARG_CREATEPIXELSHADER* pDesc,
                                           D3D10DDI_HPIXELSHADER hShader,
                                           D3D10DDI_HRTPIXELSHADER) {
  AEROGPU_D3D10_TRACEF("CreatePixelShader codeSize=%u", pDesc ? static_cast<unsigned>(pDesc->CodeSize) : 0u);
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc ? pDesc->pShaderCode : nullptr, pDesc ? pDesc->CodeSize : 0, hShader, AEROGPU_SHADER_STAGE_PIXEL);
  AEROGPU_D3D10_RET_HR(hr);
}

HRESULT AEROGPU_APIENTRY CreateGeometryShader(D3D10DDI_HDEVICE hDevice,
                                              const D3D10DDIARG_CREATEGEOMETRYSHADER* pDesc,
                                              D3D10DDI_HGEOMETRYSHADER hShader,
                                              D3D10DDI_HRTGEOMETRYSHADER) {
  AEROGPU_D3D10_TRACEF("CreateGeometryShader codeSize=%u", pDesc ? static_cast<unsigned>(pDesc->CodeSize) : 0u);
  const HRESULT hr =
      CreateShaderCommon(hDevice, pDesc ? pDesc->pShaderCode : nullptr, pDesc ? pDesc->CodeSize : 0, hShader, AEROGPU_SHADER_STAGE_GEOMETRY);
  AEROGPU_D3D10_RET_HR(hr);
}

template <typename TShaderHandle>
void DestroyShaderCommon(D3D10DDI_HDEVICE hDevice, TShaderHandle hShader) {
  AEROGPU_D3D10_TRACEF("DestroyShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
  auto* sh = reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate);
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
      set_error(dev, E_OUTOFMEMORY);
    }
  }
  ResetObject(sh);
}

void AEROGPU_APIENTRY DestroyVertexShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HVERTEXSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}

void AEROGPU_APIENTRY DestroyPixelShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HPIXELSHADER hShader) {
  DestroyShaderCommon(hDevice, hShader);
}

void AEROGPU_APIENTRY DestroyGeometryShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HGEOMETRYSHADER hShader) {
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
  if (!hLayout.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the layout object so DestroyElementLayout is safe even if
  // CreateElementLayout fails early.
  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();

  if (!hDevice.pDrvPrivate || !pDesc) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  if (pDesc->NumElements && !pDesc->pVertexElements) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    ResetObject(layout);
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  layout->handle = AllocateGlobalHandle(dev->adapter);
  if (!layout->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still call
    // Destroy* after a failed Create* probe.
    ResetObject(layout);
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  const size_t header_size = sizeof(aerogpu_input_layout_blob_header);
  const size_t elem_size = sizeof(aerogpu_input_layout_element_dxgi);
  if (pDesc->NumElements > (SIZE_MAX - header_size) / elem_size) {
    ResetObject(layout);
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  const size_t blob_size = header_size + static_cast<size_t>(pDesc->NumElements) * elem_size;
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    ResetObject(layout);
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
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
    set_error(dev, E_OUTOFMEMORY);
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyElementLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  AEROGPU_D3D10_TRACEF("DestroyElementLayout hDevice=%p hLayout=%p", hDevice.pDrvPrivate, hLayout.pDrvPrivate);
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
      set_error(dev, E_OUTOFMEMORY);
    }
  }
  ResetObject(layout);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATERENDERTARGETVIEW*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateRenderTargetViewSize");
  return sizeof(AeroGpuRenderTargetView);
}

static HRESULT ValidateFullResourceRtvDesc(const AeroGpuResource* res,
                                           const D3D10DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                           const char** reason_out) {
  if (reason_out) {
    *reason_out = nullptr;
  }
  if (!res || !pDesc) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    if (reason_out) {
      *reason_out = "resource kind is not Texture2D";
    }
    return E_NOTIMPL;
  }

  // Reject format reinterpretation; allow UNKNOWN/0 to mean "use resource format".
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Format) {
    const uint32_t view_format = static_cast<uint32_t>(pDesc->Format);
    if (view_format != 0 && view_format != res->dxgi_format) {
      if (reason_out) {
        *reason_out = "format reinterpretation";
      }
      return E_NOTIMPL;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      if (reason_out) {
        *reason_out = "unsupported view dimension";
      }
      return E_NOTIMPL;
    }
  } else if (allow_array_view) {
    if (reason_out) {
      *reason_out = "missing view dimension for array resource";
    }
    return E_NOTIMPL;
  }

  // If the header exposes MSAA RTV union variants but does not expose a view
  // dimension discriminator, we cannot safely determine which union member is
  // active. Reject to avoid accidentally accepting a subresource/MSAA view and
  // silently binding the whole resource.
  if (!have_dim) {
    bool has_msaa_union = false;
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DMSArray) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DMSArray) { has_msaa_union = true; }
    if (has_msaa_union) {
      if (reason_out) {
        *reason_out = "missing view dimension discriminator";
      }
      return E_NOTIMPL;
    }
  }

  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
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

  if (have_mip_slice) {
    if (mip_slice >= res->mip_levels) {
      return E_INVALIDARG;
    }
    if (mip_slice != 0) {
      if (reason_out) {
        *reason_out = "MipSlice != 0";
      }
      return E_NOTIMPL;
    }
  } else {
    if (reason_out) {
      *reason_out = "missing mip slice in RTV desc";
    }
    return E_NOTIMPL;
  }

  // If the resource is an array, require the view to span the entire array. If
  // the runtime explicitly requests an array view for a single-slice resource,
  // validate the array slice range as well so out-of-bounds descriptors fail
  // cleanly instead of silently aliasing slice 0.
  if (res->array_size > 1 && !view_is_array) {
    if (reason_out) {
      *reason_out = "view is not Texture2DArray";
    }
    return E_NOTIMPL;
  }

  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }

    if (!have_slice_range) {
      if (reason_out) {
        *reason_out = "missing array slice range in RTV desc";
      }
      return E_NOTIMPL;
    }

    if (first_slice >= res->array_size) {
      return E_INVALIDARG;
    }
    if (first_slice != 0) {
      if (reason_out) {
        *reason_out = "FirstArraySlice != 0";
      }
      return E_NOTIMPL;
    }

    if (array_size != 0 && array_size != kD3DUintAll && array_size > (res->array_size - first_slice)) {
      return E_INVALIDARG;
    }

    const uint32_t requested_slices =
        (array_size == 0 || array_size == kD3DUintAll) ? res->array_size : array_size;
    if (requested_slices != res->array_size) {
      if (reason_out) {
        *reason_out = "ArraySize does not span full resource";
      }
      return E_NOTIMPL;
    }
  }

  return S_OK;
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
  if (!hDevice.pDrvPrivate || !hRtv.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the view object so DestroyRenderTargetView is safe even if
  // we reject the descriptor.
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = 0;
  rtv->resource = nullptr;

  if (!pDesc || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !dev->adapter || !res) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  if (res->kind != ResourceKind::Texture2D) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev);

  uint32_t view_dxgi_format = res->dxgi_format;
  bool format_specified = false;
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Format) {
    const uint32_t fmt = static_cast<uint32_t>(pDesc->Format);
    if (fmt != 0) {
      view_dxgi_format = fmt;
      format_specified = true;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
  } else if (allow_array_view) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  // RTVs always select a single mip slice.
  uint32_t mip_slice = 0;
  bool have_mip_slice = false;
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
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }
  if (mip_slice >= res->mip_levels) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // If the resource is an array, require an array view. If the runtime requests an array view for
  // a single-slice resource, validate the slice range as well.
  if (res->array_size > 1 && !view_is_array) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = view_is_array ? res->array_size : 1u;
  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATERENDERTARGETVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }

    if (!have_slice_range) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    if (first_slice >= res->array_size) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    base_array_layer = first_slice;
    array_layer_count = D3dViewCountToRemaining(base_array_layer, array_size, res->array_size);
    if (array_layer_count == 0 || base_array_layer + array_layer_count > res->array_size) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
  }

  const bool format_reinterpret = format_specified && (view_dxgi_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret ||
      mip_slice != 0 ||
      base_array_layer != 0 ||
      array_layer_count != res->array_size;

  if (non_trivial && !supports_views) {
    AEROGPU_D3D10_11_LOG("D3D10.1 CreateRenderTargetView: rejecting unsupported view (res=%p fmt=%u mips=%u array=%u)",
                         res_private,
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(res->mip_levels),
                         static_cast<unsigned>(res->array_size));
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }
  rtv->resource = res;
  // Trivial views bind the underlying resource handle at bind-time (texture==0) so
  // RotateResourceIdentities can update the handle.
  rtv->texture = 0;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(rtv);
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
    if (!view_handle) {
      ResetObject(rtv);
      AEROGPU_D3D10_RET_HR(E_FAIL);
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(rtv);
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;

    rtv->texture = view_handle;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyRenderTargetView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_TRACEF("DestroyRenderTargetView hRtv=%p", hRtv.pDrvPrivate);
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && rtv) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev) && rtv->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = rtv->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (rtv) {
    rtv->~AeroGpuRenderTargetView();
    new (rtv) AeroGpuRenderTargetView();
  }
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATEDEPTHSTENCILVIEW*) {
  AEROGPU_D3D10_TRACEF("CalcPrivateDepthStencilViewSize");
  return sizeof(AeroGpuDepthStencilView);
}

static HRESULT ValidateFullResourceDsvDesc(const AeroGpuResource* res,
                                           const D3D10DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                           const char** reason_out) {
  if (reason_out) {
    *reason_out = nullptr;
  }
  if (!res || !pDesc) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    if (reason_out) {
      *reason_out = "resource kind is not Texture2D";
    }
    return E_NOTIMPL;
  }

  // Reject format reinterpretation; allow UNKNOWN/0 to mean "use resource format".
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Format) {
    const uint32_t view_format = static_cast<uint32_t>(pDesc->Format);
    if (view_format != 0 && view_format != res->dxgi_format) {
      if (reason_out) {
        *reason_out = "format reinterpretation";
      }
      return E_NOTIMPL;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      if (reason_out) {
        *reason_out = "unsupported view dimension";
      }
      return E_NOTIMPL;
    }
  } else if (allow_array_view) {
    if (reason_out) {
      *reason_out = "missing view dimension for array resource";
    }
    return E_NOTIMPL;
  }

  // If the header exposes MSAA DSV union variants but does not expose a view
  // dimension discriminator, we cannot safely determine which union member is
  // active. Reject to avoid accidentally accepting a subresource/MSAA view and
  // silently binding the whole resource.
  if (!have_dim) {
    bool has_msaa_union = false;
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DMSArray) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DMSArray) { has_msaa_union = true; }
    if (has_msaa_union) {
      if (reason_out) {
        *reason_out = "missing view dimension discriminator";
      }
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

  if (have_mip_slice) {
    if (mip_slice >= res->mip_levels) {
      return E_INVALIDARG;
    }
    if (mip_slice != 0) {
      if (reason_out) {
        *reason_out = "MipSlice != 0";
      }
      return E_NOTIMPL;
    }
  } else {
    if (reason_out) {
      *reason_out = "missing mip slice in DSV desc";
    }
    return E_NOTIMPL;
  }

  // If the resource is an array, require the view to span the entire array. If
  // the runtime explicitly requests an array view for a single-slice resource,
  // validate the array slice range as well so out-of-bounds descriptors fail
  // cleanly instead of silently aliasing slice 0.
  if (res->array_size > 1 && !view_is_array) {
    if (reason_out) {
      *reason_out = "view is not Texture2DArray";
    }
    return E_NOTIMPL;
  }

  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }

    if (!have_slice_range) {
      if (reason_out) {
        *reason_out = "missing array slice range in DSV desc";
      }
      return E_NOTIMPL;
    }

    if (first_slice >= res->array_size) {
      return E_INVALIDARG;
    }
    if (first_slice != 0) {
      if (reason_out) {
        *reason_out = "FirstArraySlice != 0";
      }
      return E_NOTIMPL;
    }

    if (array_size != 0 && array_size != kD3DUintAll && array_size > (res->array_size - first_slice)) {
      return E_INVALIDARG;
    }

    const uint32_t requested_slices =
        (array_size == 0 || array_size == kD3DUintAll) ? res->array_size : array_size;
    if (requested_slices != res->array_size) {
      if (reason_out) {
        *reason_out = "ArraySize does not span full resource";
      }
      return E_NOTIMPL;
    }
  }

  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Flags) {
    const uint32_t flags = static_cast<uint32_t>(pDesc->Flags);
    if (flags != 0) {
      if (reason_out) {
        *reason_out = "unsupported DSV flags";
      }
      return E_NOTIMPL;
    }
  }

  return S_OK;
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
  if (!hDevice.pDrvPrivate || !hDsv.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the view object so DestroyDepthStencilView is safe even if
  // we reject the descriptor.
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = 0;
  dsv->resource = nullptr;

  if (!pDesc || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !dev->adapter || !res) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  if (res->kind != ResourceKind::Texture2D) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev);

  uint32_t view_dxgi_format = res->dxgi_format;
  bool format_specified = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Format) {
    const uint32_t fmt = static_cast<uint32_t>(pDesc->Format);
    if (fmt != 0) {
      view_dxgi_format = fmt;
      format_specified = true;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
  } else if (allow_array_view) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  // DSVs always select a single mip slice.
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
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }
  if (mip_slice >= res->mip_levels) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // If the resource is an array, require an array view. If the runtime requests an array view for
  // a single-slice resource, validate the slice range as well.
  if (res->array_size > 1 && !view_is_array) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = view_is_array ? res->array_size : 1u;
  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }

    if (!have_slice_range) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    if (first_slice >= res->array_size) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    base_array_layer = first_slice;
    array_layer_count = D3dViewCountToRemaining(base_array_layer, array_size, res->array_size);
    if (array_layer_count == 0 || base_array_layer + array_layer_count > res->array_size) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
  }

  __if_exists(D3D10DDIARG_CREATEDEPTHSTENCILVIEW::Flags) {
    const uint32_t flags = static_cast<uint32_t>(pDesc->Flags);
    if (flags != 0) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
  }

  const bool format_reinterpret = format_specified && (view_dxgi_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret ||
      mip_slice != 0 ||
      base_array_layer != 0 ||
      array_layer_count != res->array_size;

  if (non_trivial && !supports_views) {
    AEROGPU_D3D10_11_LOG("D3D10.1 CreateDepthStencilView: rejecting unsupported view (res=%p fmt=%u mips=%u array=%u)",
                         res_private,
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(res->mip_levels),
                         static_cast<unsigned>(res->array_size));
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }
  dsv->resource = res;
  dsv->texture = 0;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(dsv);
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }
    const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
    if (!view_handle) {
      ResetObject(dsv);
      AEROGPU_D3D10_RET_HR(E_FAIL);
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(dsv);
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = mip_slice;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;
    dsv->texture = view_handle;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyDepthStencilView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_TRACEF("DestroyDepthStencilView hDsv=%p", hDsv.pDrvPrivate);
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && dsv) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev) && dsv->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = dsv->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (dsv) {
    dsv->~AeroGpuDepthStencilView();
    new (dsv) AeroGpuDepthStencilView();
  }
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
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
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

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

static HRESULT ValidateFullResourceSrvDesc(const AeroGpuResource* res,
                                           const D3D10DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                           const char** reason_out) {
  if (reason_out) {
    *reason_out = nullptr;
  }
  if (!res || !pDesc) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    if (reason_out) {
      *reason_out = "resource kind is not Texture2D";
    }
    return E_NOTIMPL;
  }

  // Reject format reinterpretation; allow UNKNOWN/0 to mean "use resource format".
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Format) {
    const uint32_t view_format = static_cast<uint32_t>(pDesc->Format);
    if (view_format != 0 && view_format != res->dxgi_format) {
      if (reason_out) {
        *reason_out = "format reinterpretation";
      }
      return E_NOTIMPL;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      if (reason_out) {
        *reason_out = "unsupported view dimension";
      }
      return E_NOTIMPL;
    }
  } else if (allow_array_view) {
    if (reason_out) {
      *reason_out = "missing view dimension for array resource";
    }
    return E_NOTIMPL;
  }

  // If the header exposes MSAA SRV union variants but does not expose a view
  // dimension discriminator, we cannot safely determine which union member is
  // active. Reject to avoid accidentally accepting a subresource/MSAA view and
  // silently binding the whole resource.
  if (!have_dim) {
    bool has_msaa_union = false;
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DMSArray) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DMS) { has_msaa_union = true; }
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DMSArray) { has_msaa_union = true; }
    if (has_msaa_union) {
      if (reason_out) {
        *reason_out = "missing view dimension discriminator";
      }
      return E_NOTIMPL;
    }
  }

  uint32_t most_detailed_mip = 0;
  uint32_t mip_levels = 0;
  bool have_mip_range = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
    most_detailed_mip = static_cast<uint32_t>(pDesc->MostDetailedMip);
    mip_levels = static_cast<uint32_t>(pDesc->MipLevels);
    have_mip_range = true;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
    if (view_is_array) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2DArray.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2DArray.MipLevels);
        have_mip_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2DArray.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2DArray.MipLevels);
          have_mip_range = true;
        }
      }
    } else {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2D.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2D.MipLevels);
        have_mip_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2D) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2D.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2D.MipLevels);
          have_mip_range = true;
        }
      }
    }
  }

  if (!have_mip_range) {
    if (reason_out) {
      *reason_out = "missing mip range in SRV desc";
    }
    return E_NOTIMPL;
  }

  if (most_detailed_mip >= res->mip_levels) {
    return E_INVALIDARG;
  }

  if (most_detailed_mip != 0 || !D3dSrvMipLevelsIsAll(mip_levels, res->mip_levels)) {
    if (reason_out) {
      *reason_out = "mip range does not span full resource";
    }
    return E_NOTIMPL;
  }

  if (mip_levels != 0 && mip_levels != kD3DUintAll && mip_levels > res->mip_levels) {
    return E_INVALIDARG;
  }

  // If the resource is an array, require the view to span the entire array. If
  // the runtime explicitly requests an array view for a single-slice resource,
  // validate the array slice range as well so out-of-bounds descriptors fail
  // cleanly instead of silently aliasing slice 0.
  if (res->array_size > 1 && !view_is_array) {
    if (reason_out) {
      *reason_out = "view is not Texture2DArray";
    }
    return E_NOTIMPL;
  }

  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }

    if (!have_slice_range) {
      if (reason_out) {
        *reason_out = "missing array slice range in SRV desc";
      }
      return E_NOTIMPL;
    }

    if (first_slice >= res->array_size) {
      return E_INVALIDARG;
    }
    if (first_slice != 0) {
      if (reason_out) {
        *reason_out = "FirstArraySlice != 0";
      }
      return E_NOTIMPL;
    }

    if (array_size != 0 && array_size != kD3DUintAll && array_size > (res->array_size - first_slice)) {
      return E_INVALIDARG;
    }

    const uint32_t requested_slices =
        (array_size == 0 || array_size == kD3DUintAll) ? res->array_size : array_size;
    if (requested_slices != res->array_size) {
      if (reason_out) {
        *reason_out = "ArraySize does not span full resource";
      }
      return E_NOTIMPL;
    }

  }

  return S_OK;
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
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

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !dev->adapter || !res) {
    return E_INVALIDARG;
  }
  if (res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev);

  uint32_t view_dxgi_format = res->dxgi_format;
  bool format_specified = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Format) {
    const uint32_t fmt = static_cast<uint32_t>(pDesc->Format);
    if (fmt != 0) {
      view_dxgi_format = fmt;
      format_specified = true;
    }
  }

  uint32_t view_dim = 0;
  bool have_dim = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    view_dim = static_cast<uint32_t>(pDesc->ResourceDimension);
    have_dim = true;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ResourceDimension) {
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::ViewDimension) {
      view_dim = static_cast<uint32_t>(pDesc->ViewDimension);
      have_dim = true;
    }
  }

  const bool allow_array_view = (res->array_size > 1);
  bool view_is_array = false;
  if (have_dim) {
    if (D3dViewDimensionIsTexture2D(view_dim)) {
      view_is_array = false;
    } else if (D3dViewDimensionIsTexture2DArray(view_dim)) {
      view_is_array = true;
    } else {
      return E_NOTIMPL;
    }
  } else if (allow_array_view) {
    return E_NOTIMPL;
  }

  uint32_t most_detailed_mip = 0;
  uint32_t mip_levels = 0;
  bool have_mip_range = false;
  __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
    most_detailed_mip = static_cast<uint32_t>(pDesc->MostDetailedMip);
    mip_levels = static_cast<uint32_t>(pDesc->MipLevels);
    have_mip_range = true;
  }
  __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::MostDetailedMip) {
    if (view_is_array) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2DArray.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2DArray.MipLevels);
        have_mip_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2DArray.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2DArray.MipLevels);
          have_mip_range = true;
        }
      }
    } else {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        most_detailed_mip = static_cast<uint32_t>(pDesc->Tex2D.MostDetailedMip);
        mip_levels = static_cast<uint32_t>(pDesc->Tex2D.MipLevels);
        have_mip_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2D) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2D) {
          most_detailed_mip = static_cast<uint32_t>(pDesc->Texture2D.MostDetailedMip);
          mip_levels = static_cast<uint32_t>(pDesc->Texture2D.MipLevels);
          have_mip_range = true;
        }
      }
    }
  }
  if (!have_mip_range) {
    return E_NOTIMPL;
  }
  if (most_detailed_mip >= res->mip_levels) {
    return E_INVALIDARG;
  }
  uint32_t mip_level_count = D3dViewCountToRemaining(most_detailed_mip, mip_levels, res->mip_levels);
  if (mip_level_count == 0 || most_detailed_mip + mip_level_count > res->mip_levels) {
    return E_INVALIDARG;
  }

  if (res->array_size > 1 && !view_is_array) {
    return E_NOTIMPL;
  }

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = view_is_array ? res->array_size : 1u;
  if (view_is_array) {
    uint32_t first_slice = 0;
    uint32_t array_size = 0;
    bool have_slice_range = false;
    __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      first_slice = static_cast<uint32_t>(pDesc->FirstArraySlice);
      array_size = static_cast<uint32_t>(pDesc->ArraySize);
      have_slice_range = true;
    }
    __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::FirstArraySlice) {
      __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        first_slice = static_cast<uint32_t>(pDesc->Tex2DArray.FirstArraySlice);
        array_size = static_cast<uint32_t>(pDesc->Tex2DArray.ArraySize);
        have_slice_range = true;
      }
      __if_not_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Tex2DArray) {
        __if_exists(D3D10DDIARG_CREATESHADERRESOURCEVIEW::Texture2DArray) {
          first_slice = static_cast<uint32_t>(pDesc->Texture2DArray.FirstArraySlice);
          array_size = static_cast<uint32_t>(pDesc->Texture2DArray.ArraySize);
          have_slice_range = true;
        }
      }
    }
    if (!have_slice_range) {
      return E_NOTIMPL;
    }
    if (first_slice >= res->array_size) {
      return E_INVALIDARG;
    }
    base_array_layer = first_slice;
    array_layer_count = D3dViewCountToRemaining(base_array_layer, array_size, res->array_size);
    if (array_layer_count == 0 || base_array_layer + array_layer_count > res->array_size) {
      return E_INVALIDARG;
    }
  }

  const bool format_reinterpret = format_specified && (view_dxgi_format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret ||
      most_detailed_mip != 0 ||
      mip_level_count != res->mip_levels ||
      base_array_layer != 0 ||
      array_layer_count != res->array_size;

  if (non_trivial && !supports_views) {
    AEROGPU_D3D10_11_LOG("D3D10.1 CreateShaderResourceView: rejecting unsupported view (res=%p fmt=%u mips=%u array=%u)",
                         hResource.pDrvPrivate,
                         static_cast<unsigned>(res->dxgi_format),
                         static_cast<unsigned>(res->mip_levels),
                         static_cast<unsigned>(res->array_size));
    return E_NOTIMPL;
  }
  srv->resource = res;
  srv->texture = 0;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      ResetObject(srv);
      return E_NOTIMPL;
    }
    const aerogpu_handle_t view_handle = AllocateGlobalHandle(dev->adapter);
    if (!view_handle) {
      ResetObject(srv);
      return E_FAIL;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      ResetObject(srv);
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = most_detailed_mip;
    cmd->mip_level_count = mip_level_count;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;
    srv->texture = view_handle;
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyShaderResourceView(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hView.pDrvPrivate) {
    return;
  }
  auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(hView);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && view) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
      } else {
        cmd->view_handle = view->texture;
        cmd->reserved0 = 0;
      }
    }
  }
  if (view) {
    view->~AeroGpuShaderResourceView();
    new (view) AeroGpuShaderResourceView();
  }
}

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const D3D10DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                                       const D3D10DDIARG_CREATESAMPLER* pDesc,
                                       D3D10DDI_HSAMPLER hSampler,
                                       D3D10DDI_HRTSAMPLER) {
  if (!hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the sampler so DestroySampler is safe even when CreateSampler
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

  sampler->handle = AllocateGlobalHandle(dev->adapter);
  if (!sampler->handle) {
    // Leave the object alive in pDrvPrivate memory. Some runtimes may still call
    // Destroy* after a failed Create* probe.
    ResetObject(sampler);
    return E_FAIL;
  }

  InitSamplerFromCreateSamplerArg(sampler, pDesc);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    // Avoid leaving a stale non-zero handle in pDrvPrivate memory if the runtime
    // probes Destroy after a failed Create.
    ResetObject(sampler);
    set_error(dev, E_OUTOFMEMORY);
    return E_OUTOFMEMORY;
  }
  cmd->sampler_handle = sampler->handle;
  cmd->filter = sampler->filter;
  cmd->address_u = sampler->address_u;
  cmd->address_v = sampler->address_v;
  cmd->address_w = sampler->address_w;
  return S_OK;
}

void AEROGPU_APIENTRY DestroySampler(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSAMPLER hSampler) {
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
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
    } else {
      cmd->sampler_handle = sampler->handle;
      cmd->reserved0 = 0;
    }
  }
  ResetObject(sampler);
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const D3D10_1_DDI_BLEND_DESC*) {
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const D3D10_1_DDI_BLEND_DESC* pDesc,
                                          D3D10DDI_HBLENDSTATE hState,
                                          D3D10DDI_HRTBLENDSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Default to the D3D10 default blend state (blending disabled, write RGBA).
  aerogpu::d3d10_11::AerogpuBlendStateBase base{};
  base.enable = 0;
  base.src_factor = AEROGPU_BLEND_ONE;
  base.dst_factor = AEROGPU_BLEND_ZERO;
  base.blend_op = AEROGPU_BLEND_OP_ADD;
  base.src_factor_alpha = AEROGPU_BLEND_ONE;
  base.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  base.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  base.color_write_mask = kD3DColorWriteMaskAll;

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
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      const auto& rt = pDesc->RenderTarget[i];
      rts[i].blend_enable = rt.BlendEnable ? true : false;
      rts[i].write_mask = static_cast<uint8_t>(rt.RenderTargetWriteMask);
      rts[i].src_blend = static_cast<uint32_t>(rt.SrcBlend);
      rts[i].dest_blend = static_cast<uint32_t>(rt.DestBlend);
      rts[i].blend_op = static_cast<uint32_t>(rt.BlendOp);
      rts[i].src_blend_alpha = static_cast<uint32_t>(rt.SrcBlendAlpha);
      rts[i].dest_blend_alpha = static_cast<uint32_t>(rt.DestBlendAlpha);
      rts[i].blend_op_alpha = static_cast<uint32_t>(rt.BlendOpAlpha);
    }

    // D3D10.1 supports independent blend state per render target when
    // IndependentBlendEnable is TRUE. When it's FALSE, only RenderTarget[0]
    // is used and the remaining entries are ignored by the runtime.
    bool independent_blend = true;
    __if_exists(D3D10_1_DDI_BLEND_DESC::IndependentBlendEnable) {
      independent_blend = pDesc->IndependentBlendEnable ? true : false;
    }
    const uint32_t rt_count = independent_blend ? AEROGPU_MAX_RENDER_TARGETS : 1u;

    const HRESULT hr = aerogpu::d3d10_11::ValidateAndConvertBlendDesc(
        rts, rt_count, pDesc->AlphaToCoverageEnable ? true : false, &base);
    if (FAILED(hr)) {
      return fail(hr);
    }
  }

  s->state = base;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
  new (s) AeroGpuBlendState();
}

void AEROGPU_APIENTRY SetBlendState(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HBLENDSTATE hState,
                                    const FLOAT blend_factor[4],
                                    UINT sample_mask) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("SetBlendState hDevice=%p hState=%p",
                               hDevice.pDrvPrivate,
                               hState.pDrvPrivate);

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Default to the D3D10 default blend state (disabled, write RGBA).
  aerogpu::d3d10_11::AerogpuBlendStateBase base{};
  base.enable = 0;
  base.src_factor = AEROGPU_BLEND_ONE;
  base.dst_factor = AEROGPU_BLEND_ZERO;
  base.blend_op = AEROGPU_BLEND_OP_ADD;
  base.src_factor_alpha = AEROGPU_BLEND_ONE;
  base.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  base.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  base.color_write_mask = kD3DColorWriteMaskAll;

  if (hState.pDrvPrivate) {
    auto* bs = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
    if (bs) {
      base = bs->state;
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }

  cmd->state.enable = base.enable ? 1u : 0u;
  cmd->state.src_factor = base.src_factor;
  cmd->state.dst_factor = base.dst_factor;
  cmd->state.blend_op = base.blend_op;
  cmd->state.color_write_mask = static_cast<uint8_t>(base.color_write_mask & kD3DColorWriteMaskAll);
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
}

void AEROGPU_APIENTRY SetRasterizerState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("SetRasterizerState hDevice=%p hState=%p", hDevice.pDrvPrivate, hState.pDrvPrivate);

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t fill_mode = static_cast<uint32_t>(D3D10_FILL_SOLID);
  uint32_t cull_mode = static_cast<uint32_t>(D3D10_CULL_BACK);
  uint32_t front_ccw = 0u;
  uint32_t scissor_enable = 0u;
  int32_t depth_bias = 0;
  uint32_t depth_clip_enable = 1u;
  if (hState.pDrvPrivate) {
    const auto* rs = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
    if (rs) {
      fill_mode = rs->fill_mode;
      cull_mode = rs->cull_mode;
      front_ccw = rs->front_ccw;
      scissor_enable = rs->scissor_enable;
      depth_bias = rs->depth_bias;
      depth_clip_enable = rs->depth_clip_enable;
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }

  cmd->state.fill_mode = aerogpu::d3d10_11::D3DFillModeToAerogpu(fill_mode);
  cmd->state.cull_mode = aerogpu::d3d10_11::D3DCullModeToAerogpu(cull_mode);
  cmd->state.front_ccw = front_ccw ? 1u : 0u;
  cmd->state.scissor_enable = scissor_enable ? 1u : 0u;
  cmd->state.depth_bias = depth_bias;
  cmd->state.flags = depth_clip_enable ? AEROGPU_RASTERIZER_FLAG_NONE : AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;
}

void AEROGPU_APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                          D3D10DDI_HDEPTHSTENCILSTATE hState,
                                          UINT stencil_ref) {
  (void)stencil_ref;
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("SetDepthStencilState hDevice=%p hState=%p",
                               hDevice.pDrvPrivate,
                               hState.pDrvPrivate);

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t depth_enable = 1u;
  uint32_t depth_write_mask = static_cast<uint32_t>(D3D10_DEPTH_WRITE_MASK_ALL);
  uint32_t depth_func = static_cast<uint32_t>(D3D10_COMPARISON_LESS);
  uint32_t stencil_enable = 0u;
  uint8_t stencil_read_mask = kD3DStencilMaskAll;
  uint8_t stencil_write_mask = kD3DStencilMaskAll;
  if (hState.pDrvPrivate) {
    const auto* dss = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
    if (dss) {
      depth_enable = dss->depth_enable;
      depth_write_mask = dss->depth_write_mask;
      depth_func = dss->depth_func;
      stencil_enable = dss->stencil_enable;
      stencil_read_mask = dss->stencil_read_mask;
      stencil_write_mask = dss->stencil_write_mask;
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
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
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const D3D10_DDI_RASTERIZER_DESC*) {
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const D3D10_DDI_RASTERIZER_DESC* pDesc,
                                               D3D10DDI_HRASTERIZERSTATE hState,
                                               D3D10DDI_HRTRASTERIZERSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* state = new (hState.pDrvPrivate) AeroGpuRasterizerState();
  if (!pDesc) {
    return S_OK;
  }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::FillMode) { state->fill_mode = static_cast<uint32_t>(pDesc->FillMode); }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::CullMode) { state->cull_mode = static_cast<uint32_t>(pDesc->CullMode); }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::FrontCounterClockwise) {
    state->front_ccw = pDesc->FrontCounterClockwise ? 1u : 0u;
  }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::ScissorEnable) { state->scissor_enable = pDesc->ScissorEnable ? 1u : 0u; }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::DepthBias) { state->depth_bias = static_cast<int32_t>(pDesc->DepthBias); }
  __if_exists(D3D10_DDI_RASTERIZER_DESC::DepthClipEnable) {
    state->depth_clip_enable = pDesc->DepthClipEnable ? 1u : 0u;
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
  new (s) AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE, const D3D10_DDI_DEPTH_STENCIL_DESC*) {
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const D3D10_DDI_DEPTH_STENCIL_DESC* pDesc,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState,
                                                 D3D10DDI_HRTDEPTHSTENCILSTATE) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* state = new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  if (!pDesc) {
    return S_OK;
  }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::DepthEnable) { state->depth_enable = pDesc->DepthEnable ? 1u : 0u; }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::DepthWriteMask) {
    state->depth_write_mask = static_cast<uint32_t>(pDesc->DepthWriteMask);
  }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::DepthFunc) { state->depth_func = static_cast<uint32_t>(pDesc->DepthFunc); }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::StencilEnable) { state->stencil_enable = pDesc->StencilEnable ? 1u : 0u; }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::StencilReadMask) {
    state->stencil_read_mask = static_cast<uint8_t>(pDesc->StencilReadMask);
  }
  __if_exists(D3D10_DDI_DEPTH_STENCIL_DESC::StencilWriteMask) {
    state->stencil_write_mask = static_cast<uint8_t>(pDesc->StencilWriteMask);
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
  new (s) AeroGpuDepthStencilState();
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

    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
    const bool is_b5 = (res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm ||
                        res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm);
    if (aer_fmt == AEROGPU_FORMAT_INVALID || (is_b5 ? (bpp != 2) : (bpp != 4))) {
      // Only maintain CPU-side shadow clears for the uncompressed 32-bit RGBA/BGRA formats and
      // the 16-bit B5 formats used by the bring-up render-target path.
      goto EmitClearCmd;
    }

    uint16_t packed16 = 0;
    if (is_b5) {
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

      if (res->dxgi_format == aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm) {
        const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
        const uint16_t g6 = static_cast<uint16_t>(float_to_unorm(rgba[1], 63));
        const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
        packed16 = static_cast<uint16_t>((r5 << 11) | (g6 << 5) | b5);
      } else {
        const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
        const uint16_t g5 = static_cast<uint16_t>(float_to_unorm(rgba[1], 31));
        const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
        const uint16_t a1 = static_cast<uint16_t>(float_to_unorm(rgba[3], 1));
        packed16 = static_cast<uint16_t>((a1 << 15) | (r5 << 10) | (g5 << 5) | b5);
      }
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
        if (is_b5) {
          for (uint32_t x = 0; x < res->width; ++x) {
            std::memcpy(row + static_cast<size_t>(x) * 2, &packed16, sizeof(packed16));
          }
        } else {
          for (uint32_t x = 0; x < res->width; ++x) {
            uint8_t* px = row + static_cast<size_t>(x) * 4;
            switch (res->dxgi_format) {
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Typeless:
                px[0] = r;
                px[1] = g;
                px[2] = b;
                px[3] = a;
                break;
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Typeless:
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = 255;
                break;
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb:
              case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Typeless:
              default:
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = a;
                break;
            }
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
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
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

  if (!aerogpu::d3d10_11::EmitSetInputLayoutCmdLocked(dev,
                                                      handle,
                                                      [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_input_layout = handle;
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

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (start_slot > kMaxVertexBufferSlots) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  // D3D10.1 allows updating any subrange of IA vertex buffer slots.
  UINT bind_count = buffer_count;
  if (bind_count != 0) {
    if (!pBuffers || !pStrides || !pOffsets) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (start_slot >= kMaxVertexBufferSlots) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (bind_count > (kMaxVertexBufferSlots - start_slot)) {
      set_error(dev, E_INVALIDARG);
      return;
    }
  } else {
    // Treat NumBuffers==0 as an unbind request from StartSlot to the end of the
    // slot range (used by some D3D10 runtimes for state clearing).
    if (start_slot == kMaxVertexBufferSlots) {
      return;
    }
    bind_count = kMaxVertexBufferSlots - start_slot;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("IaSetVertexBuffers hDevice=%p start_slot=%u count=%u",
                               hDevice.pDrvPrivate,
                               start_slot,
                               bind_count);

  std::array<aerogpu_vertex_buffer_binding, kMaxVertexBufferSlots> bindings{};
  std::array<AeroGpuResource*, kMaxVertexBufferSlots> new_resources{};
  std::array<uint32_t, kMaxVertexBufferSlots> new_strides{};
  std::array<uint32_t, kMaxVertexBufferSlots> new_offsets{};
  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);

    aerogpu_vertex_buffer_binding b{};
    AeroGpuResource* vb_res = nullptr;
    if (buffer_count != 0) {
      vb_res = pBuffers[i].pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pBuffers[i]) : nullptr;
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

    new_resources[i] = vb_res;
    new_strides[i] = b.stride_bytes;
    new_offsets[i] = b.offset_bytes;
  }

  if (!aerogpu::d3d10_11::EmitSetVertexBuffersCmdLocked(dev,
                                                        static_cast<uint32_t>(start_slot),
                                                        static_cast<uint32_t>(bind_count),
                                                        bindings.data(),
                                                        [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }

  for (UINT i = 0; i < bind_count; ++i) {
    const uint32_t slot = static_cast<uint32_t>(start_slot + i);
    dev->current_vb_resources[slot] = new_resources[i];
    dev->current_vb_strides[slot] = new_strides[i];
    dev->current_vb_offsets[slot] = new_offsets[i];
    if (slot == 0) {
      dev->current_vb_res = new_resources[i];
      dev->current_vb_stride = new_strides[i];
      dev->current_vb_offset = new_offsets[i];
    }
    TrackWddmAllocForSubmitLocked(dev, new_resources[i], /*write=*/false);
  }
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

  if (!aerogpu::d3d10_11::EmitSetIndexBufferCmdLocked(
          dev,
          ib_res ? ib_res->handle : 0,
          dxgi_index_format_to_aerogpu(static_cast<uint32_t>(format)),
          offset,
          [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_ib_res = ib_res;
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
  (void)aerogpu::d3d10_11::SetPrimitiveTopologyLocked(dev,
                                                      topo,
                                                      [&](HRESULT hr) { set_error(dev, hr); });
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
    set_error(dev, E_OUTOFMEMORY);
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
  const aerogpu_handle_t new_vs = hShader.pDrvPrivate ? reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, new_vs, dev->current_ps, dev->current_gs)) {
    return;
  }
  dev->current_vs = new_vs;
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
  const aerogpu_handle_t new_ps = hShader.pDrvPrivate ? reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate)->handle : 0;
  if (!EmitBindShadersCmdLocked(dev, dev->current_vs, new_ps, dev->current_gs)) {
    return;
  }
  dev->current_ps = new_ps;
}

template <typename FnPtr>
struct GsSetShaderImpl;

template <typename... Tail>
struct GsSetShaderImpl<void(AEROGPU_APIENTRY*)(D3D10DDI_HDEVICE, Tail...)> {
  static void AEROGPU_APIENTRY Call(D3D10DDI_HDEVICE hDevice, Tail... tail) {
    if (!hDevice.pDrvPrivate) {
      return;
    }

    auto args = std::tie(tail...);
    static_assert(sizeof...(Tail) >= 1, "GsSetShader must take a shader handle");
    auto hShader = std::get<0>(args);

    AEROGPU_D3D10_TRACEF_VERBOSE("GsSetShader hDevice=%p hShader=%p", hDevice.pDrvPrivate, hShader.pDrvPrivate);
    auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
    if (!dev) {
      return;
    }

    std::lock_guard<std::mutex> lock(dev->mutex);
    const aerogpu_handle_t new_gs = hShader.pDrvPrivate ? reinterpret_cast<AeroGpuShader*>(hShader.pDrvPrivate)->handle : 0;
    if (!EmitBindShadersCmdLocked(dev, dev->current_vs, dev->current_ps, new_gs)) {
      return;
    }
    dev->current_gs = new_gs;
  }
};

static void SetConstantBuffersCommon(D3D10DDI_HDEVICE hDevice,
                                     uint32_t shader_stage,
                                     UINT start_slot,
                                     UINT buffer_count,
                                     const D3D10DDI_HRESOURCE* phBuffers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  if (buffer_count == 0) {
    return;
  }
  const uint64_t end_slot = static_cast<uint64_t>(start_slot) + static_cast<uint64_t>(buffer_count);
  if (start_slot >= kMaxConstantBufferSlots || end_slot > kMaxConstantBufferSlots) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots>* table = nullptr;
  std::array<AeroGpuResource*, kMaxConstantBufferSlots>* resources = nullptr;
  if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
    table = &dev->vs_constant_buffers;
    resources = &dev->current_vs_cb_resources;
  } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
    table = &dev->ps_constant_buffers;
    resources = &dev->current_ps_cb_resources;
  } else if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    table = &dev->gs_constant_buffers;
    resources = &dev->current_gs_cb_resources;
  } else {
    set_error(dev, E_INVALIDARG);
    return;
  }

  // Constant buffer bindings are limited to 14 slots in D3D10/10.1, so avoid heap
  // allocations in this hot path.
  std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> bindings{};
  std::array<AeroGpuResource*, kMaxConstantBufferSlots> new_resources{};
  bool bindings_changed = false;
  bool resources_changed = false;
  for (UINT i = 0; i < buffer_count; i++) {
    aerogpu_constant_buffer_binding b{};
    b.buffer = 0;
    b.offset_bytes = 0;
    b.size_bytes = 0;
    b.reserved0 = 0;

    auto* res = (phBuffers && phBuffers[i].pDrvPrivate) ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(phBuffers[i])
                                                        : nullptr;
    auto* buf_res = (res && res->kind == ResourceKind::Buffer) ? res : nullptr;
    if (buf_res) {
      b.buffer = buf_res->handle;
      b.offset_bytes = 0;
      b.size_bytes = ClampU64ToU32(buf_res->size_bytes);
    }

    bindings[i] = b;
    new_resources[i] = buf_res;

    if (!bindings_changed) {
      const aerogpu_constant_buffer_binding& cur = (*table)[start_slot + i];
      bindings_changed = (cur.buffer != b.buffer) || (cur.offset_bytes != b.offset_bytes) || (cur.size_bytes != b.size_bytes);
    }
    if (!resources_changed) {
      resources_changed = ((*resources)[start_slot + i] != buf_res);
    }
  }

  if (!bindings_changed) {
    if (resources_changed) {
      for (UINT i = 0; i < buffer_count; i++) {
        (*resources)[start_slot + i] = new_resources[i];
      }
    }
    return;
  }

  for (UINT i = 0; i < buffer_count; i++) {
    TrackWddmAllocForSubmitLocked(dev, new_resources[i], /*write=*/false);
  }

  if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                          shader_stage,
                                                          static_cast<uint32_t>(start_slot),
                                                          static_cast<uint32_t>(buffer_count),
                                                          bindings.data(),
                                                          [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }

  for (UINT i = 0; i < buffer_count; i++) {
    (*table)[start_slot + i] = bindings[i];
    (*resources)[start_slot + i] = new_resources[i];
  }
}

void AEROGPU_APIENTRY VsSetConstantBuffers(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_buffers,
                                          const D3D10DDI_HRESOURCE* phBuffers) {
  AEROGPU_D3D10_TRACEF_VERBOSE("VsSetConstantBuffers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(num_buffers));
  SetConstantBuffersCommon(hDevice, AEROGPU_SHADER_STAGE_VERTEX, start_slot, num_buffers, phBuffers);
}

void AEROGPU_APIENTRY PsSetConstantBuffers(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_buffers,
                                          const D3D10DDI_HRESOURCE* phBuffers) {
  AEROGPU_D3D10_TRACEF_VERBOSE("PsSetConstantBuffers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(num_buffers));
  SetConstantBuffersCommon(hDevice, AEROGPU_SHADER_STAGE_PIXEL, start_slot, num_buffers, phBuffers);
}

void AEROGPU_APIENTRY GsSetConstantBuffers(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_buffers,
                                          const D3D10DDI_HRESOURCE* phBuffers) {
  AEROGPU_D3D10_TRACEF_VERBOSE("GsSetConstantBuffers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(num_buffers));
  SetConstantBuffersCommon(hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, start_slot, num_buffers, phBuffers);
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

  if (num_views == 0) {
    return;
  }
  const uint64_t end_slot = static_cast<uint64_t>(start_slot) + static_cast<uint64_t>(num_views);
  if (start_slot >= kAeroGpuD3D10MaxSrvSlots || end_slot > kAeroGpuD3D10MaxSrvSlots) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (shader_stage != AEROGPU_SHADER_STAGE_VERTEX &&
      shader_stage != AEROGPU_SHADER_STAGE_PIXEL &&
      shader_stage != AEROGPU_SHADER_STAGE_GEOMETRY) {
    set_error(dev, E_INVALIDARG);
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
      tex = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
    }
    if (tex) {
      // Hazard rule: a resource cannot be bound simultaneously as an SRV and as
      // an RTV/DSV. Match D3D10/11 behavior by unbinding it from outputs before
      // binding as an SRV.
      if (!UnbindResourceFromOutputsLocked(dev, tex, res)) {
        return;
      }
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      return;
    }
    cmd->shader_stage = shader_stage;
    cmd->slot = slot;
    cmd->texture = tex;
    cmd->reserved0 = 0;

    if (slot < dev->current_vs_srvs.size() && shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
      dev->current_vs_srvs[slot] = res;
    } else if (slot < dev->current_ps_srvs.size() && shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
      dev->current_ps_srvs[slot] = res;
    } else if (slot < dev->current_gs_srvs.size() && shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
      dev->current_gs_srvs[slot] = res;
    }
  }
}

static void SetSamplersCommon(D3D10DDI_HDEVICE hDevice,
                              uint32_t shader_stage,
                              UINT start_slot,
                              UINT sampler_count,
                              const D3D10DDI_HSAMPLER* phSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  if (sampler_count == 0) {
    return;
  }
  if (start_slot >= kAeroGpuD3D10MaxSamplerSlots) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (!phSamplers) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  UINT count = sampler_count;
  if (start_slot + count > kAeroGpuD3D10MaxSamplerSlots) {
    count = kAeroGpuD3D10MaxSamplerSlots - start_slot;
  }

  std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots> handles{};
  bool changed = false;
  std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots>* table = nullptr;
  if (shader_stage == AEROGPU_SHADER_STAGE_VERTEX) {
    table = &dev->current_vs_samplers;
  } else if (shader_stage == AEROGPU_SHADER_STAGE_PIXEL) {
    table = &dev->current_ps_samplers;
  } else if (shader_stage == AEROGPU_SHADER_STAGE_GEOMETRY) {
    table = &dev->current_gs_samplers;
  } else {
    set_error(dev, E_INVALIDARG);
    return;
  }

  for (UINT i = 0; i < count; ++i) {
    aerogpu_handle_t handle = 0;
    if (phSamplers[i].pDrvPrivate) {
      auto* sampler = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(phSamplers[i]);
      handle = sampler ? sampler->handle : 0;
    }
    handles[i] = handle;
    if (!changed && (*table)[start_slot + i] != handle) {
      changed = true;
    }
  }

  if (!changed) {
    return;
  }

  if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                   shader_stage,
                                                   static_cast<uint32_t>(start_slot),
                                                   static_cast<uint32_t>(count),
                                                   handles.data(),
                                                   [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }

  for (UINT i = 0; i < count; ++i) {
    (*table)[start_slot + i] = handles[i];
  }
}

void AEROGPU_APIENTRY VsSetSamplers(D3D10DDI_HDEVICE hDevice,
                                   UINT start_slot,
                                   UINT sampler_count,
                                   const D3D10DDI_HSAMPLER* phSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("VsSetSamplers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(sampler_count));
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplersCommon(hDevice, AEROGPU_SHADER_STAGE_VERTEX, start_slot, sampler_count, phSamplers);
}

void AEROGPU_APIENTRY PsSetSamplers(D3D10DDI_HDEVICE hDevice,
                                   UINT start_slot,
                                   UINT sampler_count,
                                   const D3D10DDI_HSAMPLER* phSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("PsSetSamplers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(sampler_count));
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplersCommon(hDevice, AEROGPU_SHADER_STAGE_PIXEL, start_slot, sampler_count, phSamplers);
}

void AEROGPU_APIENTRY GsSetSamplers(D3D10DDI_HDEVICE hDevice,
                                   UINT start_slot,
                                   UINT sampler_count,
                                   const D3D10DDI_HSAMPLER* phSamplers) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("GsSetSamplers hDevice=%p start=%u count=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(start_slot),
                               static_cast<unsigned>(sampler_count));
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  SetSamplersCommon(hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, start_slot, sampler_count, phSamplers);
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

  auto clear_constant_buffers = [&](uint32_t shader_stage,
                                    std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots>& table,
                                    std::array<AeroGpuResource*, kMaxConstantBufferSlots>& resources) {
    bool any = false;
    for (const aerogpu_constant_buffer_binding& b : table) {
      if (b.buffer != 0) {
        any = true;
        break;
      }
    }

    if (!any) {
      table.fill({});
      resources.fill(nullptr);
      return;
    }

    std::array<aerogpu_constant_buffer_binding, kMaxConstantBufferSlots> zeros{};
    if (!aerogpu::d3d10_11::EmitSetConstantBuffersCmdLocked(dev,
                                                            shader_stage,
                                                            /*start_slot=*/0,
                                                            static_cast<uint32_t>(zeros.size()),
                                                            zeros.data(),
                                                            [&](HRESULT hr) { set_error(dev, hr); })) {
      return;
    }

    table.fill({});
    resources.fill(nullptr);
  };

  clear_constant_buffers(AEROGPU_SHADER_STAGE_VERTEX, dev->vs_constant_buffers, dev->current_vs_cb_resources);
  clear_constant_buffers(AEROGPU_SHADER_STAGE_PIXEL, dev->ps_constant_buffers, dev->current_ps_cb_resources);
  clear_constant_buffers(AEROGPU_SHADER_STAGE_GEOMETRY, dev->gs_constant_buffers, dev->current_gs_cb_resources);

  for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
    if (dev->current_vs_srvs[slot]) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
      cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
      dev->current_vs_srvs[slot] = nullptr;
    }
  }
  for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
    if (dev->current_ps_srvs[slot]) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
      cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
      dev->current_ps_srvs[slot] = nullptr;
    }
  }

  for (uint32_t slot = 0; slot < dev->current_gs_srvs.size(); ++slot) {
    if (dev->current_gs_srvs[slot]) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
      if (!cmd) {
        set_error(dev, E_OUTOFMEMORY);
        return;
      }
      cmd->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
      cmd->slot = slot;
      cmd->texture = 0;
      cmd->reserved0 = 0;
      dev->current_gs_srvs[slot] = nullptr;
    }
  }

  auto clear_samplers = [&](uint32_t shader_stage, std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots>& table) {
    bool any = false;
    for (aerogpu_handle_t h : table) {
      if (h != 0) {
        any = true;
        break;
      }
    }
    if (!any) {
      return;
    }

    std::array<aerogpu_handle_t, kAeroGpuD3D10MaxSamplerSlots> zeros{};
    if (!aerogpu::d3d10_11::EmitSetSamplersCmdLocked(dev,
                                                     shader_stage,
                                                     /*start_slot=*/0,
                                                     static_cast<uint32_t>(zeros.size()),
                                                     zeros.data(),
                                                     [&](HRESULT hr) { set_error(dev, hr); })) {
      return;
    }

    table.fill(0);
  };

  clear_samplers(AEROGPU_SHADER_STAGE_VERTEX, dev->current_vs_samplers);
  clear_samplers(AEROGPU_SHADER_STAGE_PIXEL, dev->current_ps_samplers);
  clear_samplers(AEROGPU_SHADER_STAGE_GEOMETRY, dev->current_gs_samplers);

  if (!EmitSetRenderTargetsCmdLocked(dev,
                                     /*rtv_count=*/0,
                                     /*rtvs=*/nullptr,
                                     /*dsv=*/0,
                                     [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_rtv_count = 0;
  std::memset(dev->current_rtvs, 0, sizeof(dev->current_rtvs));
  std::memset(dev->current_rtv_resources, 0, sizeof(dev->current_rtv_resources));
  dev->current_dsv = 0;
  dev->current_dsv_res = nullptr;

  auto* bind_cmd = dev->cmd.bind_shaders(/*vs=*/0, /*ps=*/0, /*cs=*/0);
  if (!bind_cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  dev->current_vs = 0;
  dev->current_ps = 0;
  dev->current_gs = 0;

  if (!aerogpu::d3d10_11::EmitSetInputLayoutCmdLocked(dev,
                                                      /*input_layout_handle=*/0,
                                                      [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_input_layout = 0;

  auto* topo_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!topo_cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  dev->current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  topo_cmd->topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  topo_cmd->reserved0 = 0;

  std::array<aerogpu_vertex_buffer_binding, kMaxVertexBufferSlots> vb_zeros{};
  if (!aerogpu::d3d10_11::EmitSetVertexBuffersCmdLocked(dev,
                                                        /*start_slot=*/0,
                                                        static_cast<uint32_t>(vb_zeros.size()),
                                                        vb_zeros.data(),
                                                        [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_vb_res = nullptr;
  dev->current_vb_resources.fill(nullptr);
  dev->current_vb_strides.fill(0);
  dev->current_vb_offsets.fill(0);
  dev->current_vb_stride = 0;
  dev->current_vb_offset = 0;

  if (!aerogpu::d3d10_11::EmitSetIndexBufferCmdLocked(dev,
                                                      /*buffer=*/0,
                                                      AEROGPU_INDEX_FORMAT_UINT16,
                                                      /*offset_bytes=*/0,
                                                      [&](HRESULT hr) { set_error(dev, hr); })) {
    return;
  }
  dev->current_ib_res = nullptr;

  // Reset blend state to D3D10 defaults (disabled, write RGBA).
  auto* bs_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!bs_cmd) {
    set_error(dev, E_OUTOFMEMORY);
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

  // Reset depth/stencil state to D3D10 defaults (depth enabled, write enabled, LESS, stencil disabled).
  auto* dss_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!dss_cmd) {
    set_error(dev, E_OUTOFMEMORY);
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

  // Reset rasterizer state to D3D10 defaults (solid, cull back, depth clip enabled).
  auto* rs_cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!rs_cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  rs_cmd->state.fill_mode = AEROGPU_FILL_SOLID;
  rs_cmd->state.cull_mode = AEROGPU_CULL_BACK;
  rs_cmd->state.front_ccw = 0;
  rs_cmd->state.scissor_enable = 0;
  rs_cmd->state.depth_bias = 0;
  rs_cmd->state.flags = AEROGPU_RASTERIZER_FLAG_NONE;

  // ClearState must also reset dynamic viewport/scissor state. Without emitting
  // these commands, the host-side command executor would continue using the
  // previous values until the app calls SetViewports/SetScissorRects again.
  bool ok = true;
  aerogpu::d3d10_11::validate_and_emit_viewports_locked(dev,
                                                       /*num_viewports=*/0,
                                                       static_cast<const D3D10_DDI_VIEWPORT*>(nullptr),
                                                       [&](HRESULT hr) {
                                                         set_error(dev, hr);
                                                         ok = false;
                                                       });
  if (!ok) {
    return;
  }
  aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked(dev,
                                                           /*num_rects=*/0,
                                                           static_cast<const D3D10_DDI_RECT*>(nullptr),
                                                           [&](HRESULT hr) {
                                                             set_error(dev, hr);
                                                             ok = false;
                                                           });
  if (!ok) {
    return;
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

void AEROGPU_APIENTRY GsSetShaderResources(D3D10DDI_HDEVICE hDevice,
                                          UINT start_slot,
                                          UINT num_views,
                                          const D3D10DDI_HSHADERRESOURCEVIEW* phViews) {
  SetShaderResourcesCommon(hDevice, AEROGPU_SHADER_STAGE_GEOMETRY, start_slot, num_views, phViews);
}

void AEROGPU_APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice, UINT num_viewports, const D3D10_DDI_VIEWPORT* pViewports) {
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("SetViewports hDevice=%p num=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(num_viewports));

  std::lock_guard<std::mutex> lock(dev->mutex);
  aerogpu::d3d10_11::validate_and_emit_viewports_locked(dev,
                                                       static_cast<uint32_t>(num_viewports),
                                                       pViewports,
                                                       [&](HRESULT hr) { set_error(dev, hr); });
}

void AEROGPU_APIENTRY SetScissorRects(D3D10DDI_HDEVICE hDevice, UINT num_rects, const D3D10_DDI_RECT* pRects) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  AEROGPU_D3D10_TRACEF_VERBOSE("SetScissorRects hDevice=%p num=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(num_rects));

  std::lock_guard<std::mutex> lock(dev->mutex);
  aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked(dev,
                                                           static_cast<uint32_t>(num_rects),
                                                           pRects,
                                                           [&](HRESULT hr) { set_error(dev, hr); });
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

  if (num_rtvs != 0 && !pRTVs) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  const uint32_t count = std::min<uint32_t>(static_cast<uint32_t>(num_rtvs), AEROGPU_MAX_RENDER_TARGETS);
  aerogpu_handle_t rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < count; ++i) {
    aerogpu_handle_t rtv_handle = 0;
    AeroGpuResource* rtv_res = nullptr;
    if (pRTVs && pRTVs[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(pRTVs[i]);
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

  // Auto-unbind SRVs that alias any newly bound RTV/DSV.
  bool oom = false;
  auto unbind_srvs_for_resource = [&](AeroGpuResource* res) {
    if (!res || oom) {
      return;
    }
    for (uint32_t slot = 0; slot < dev->current_vs_srvs.size(); ++slot) {
      if (ResourcesAlias(dev->current_vs_srvs[slot], res)) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
          return;
        }
        dev->current_vs_srvs[slot] = nullptr;
        cmd->shader_stage = AEROGPU_SHADER_STAGE_VERTEX;
        cmd->slot = slot;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }
    for (uint32_t slot = 0; slot < dev->current_ps_srvs.size(); ++slot) {
      if (ResourcesAlias(dev->current_ps_srvs[slot], res)) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
          return;
        }
        dev->current_ps_srvs[slot] = nullptr;
        cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
        cmd->slot = slot;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }
    for (uint32_t slot = 0; slot < dev->current_gs_srvs.size(); ++slot) {
      if (ResourcesAlias(dev->current_gs_srvs[slot], res)) {
        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
        if (!cmd) {
          set_error(dev, E_OUTOFMEMORY);
          oom = true;
          return;
        }
        dev->current_gs_srvs[slot] = nullptr;
        cmd->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
        cmd->slot = slot;
        cmd->texture = 0;
        cmd->reserved0 = 0;
      }
    }
  };
  for (uint32_t i = 0; i < count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    unbind_srvs_for_resource(rtv_resources[i]);
  }
  unbind_srvs_for_resource(dsv_res);
  if (oom) {
    return;
  }

  if (!EmitSetRenderTargetsCmdLocked(dev, count, rtvs, dsv_handle, [&](HRESULT hr) { set_error(dev, hr); })) {
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

static bool SoftwareDrawTriangleListBringupLocked(AeroGpuDevice* dev, UINT vertex_count, UINT start_vertex) {
  if (!dev) {
    return true;
  }

  // The bring-up software rasterizer only understands a single triangle list
  // with a fixed vertex format. This is used by staging readback tests (render
  // a triangle, Present, CopyResource staging, Map).
  AeroGpuResource* primary_rtv = nullptr;
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (dev->current_rtv_resources[i]) {
      primary_rtv = dev->current_rtv_resources[i];
      break;
    }
  }
  if (vertex_count == 3 && dev->current_topology == static_cast<uint32_t>(D3D10_DDI_PRIMITIVE_TOPOLOGY_TRIANGLELIST) &&
      primary_rtv && dev->current_vb_res) {
    auto* rt = primary_rtv;
    auto* vb = dev->current_vb_res;

    if (rt->kind == ResourceKind::Texture2D && vb->kind == ResourceKind::Buffer && rt->width && rt->height &&
        vb->storage.size() >= static_cast<size_t>(dev->current_vb_offset) +
                                static_cast<size_t>(start_vertex + 3) * static_cast<size_t>(dev->current_vb_stride)) {
      const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, rt->dxgi_format);
      const uint32_t bpp = bytes_per_pixel_aerogpu(aer_fmt);
      if (aer_fmt == AEROGPU_FORMAT_INVALID || bpp != 4) {
        return true;
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

  if (!SoftwareDrawTriangleListBringupLocked(dev, vertex_count, start_vertex)) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawInstanced(D3D10DDI_HDEVICE hDevice,
                                   UINT vertex_count_per_instance,
                                   UINT instance_count,
                                   UINT start_vertex_location,
                                   UINT start_instance_location) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawInstanced hDevice=%p vcpi=%u ic=%u startV=%u startI=%u",
                               hDevice.pDrvPrivate,
                               vertex_count_per_instance,
                               instance_count,
                               start_vertex_location,
                               start_instance_location);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  if (vertex_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);

  // The bring-up software rasterizer does not understand instance data. Draw a
  // single instance so staging readback tests still have sensible contents.
  if (!SoftwareDrawTriangleListBringupLocked(dev, vertex_count_per_instance, start_vertex_location)) {
    return;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = vertex_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_vertex = start_vertex_location;
  cmd->first_instance = start_instance_location;
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
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexedInstanced(D3D10DDI_HDEVICE hDevice,
                                          UINT index_count_per_instance,
                                          UINT instance_count,
                                          UINT start_index_location,
                                          INT base_vertex_location,
                                          UINT start_instance_location) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawIndexedInstanced hDevice=%p icpi=%u ic=%u startIndex=%u baseVertex=%d startI=%u",
                               hDevice.pDrvPrivate,
                               index_count_per_instance,
                               instance_count,
                               start_index_location,
                               base_vertex_location,
                               start_instance_location);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  if (index_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->index_count = index_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_index = start_index_location;
  cmd->base_vertex = base_vertex_location;
  cmd->first_instance = start_instance_location;
}

void AEROGPU_APIENTRY DrawAuto(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawAuto hDevice=%p", hDevice.pDrvPrivate);
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  // `DrawAuto` draws based on the vertex count written by stream output. We do
  // not implement stream output yet, so treat it as a no-op draw that keeps the
  // runtime/app alive without returning E_NOTIMPL.
  std::lock_guard<std::mutex> lock(dev->mutex);
  TrackDrawStateLocked(dev);
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = 0;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
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
  TrackWddmAllocForSubmitLocked(dev, src_res, /*write=*/false);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  aerogpu_handle_t src_handle = src_res ? src_res->handle : 0;
  AEROGPU_D3D10_11_LOG("trace_resources: D3D10.1 Present sync=%u src_handle=%u",
                       static_cast<unsigned>(pPresent->SyncInterval),
                       static_cast<unsigned>(src_handle));
#endif

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!cmd) {
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    dev->pending_staging_writes.clear();
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
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

  // The Win7 D3D10.1 runtime validates MapFlags and rejects unknown bits.
  // Mirror this behavior to keep invalid usage deterministic and to avoid
  // passing unexpected flags into the WDDM callbacks.
  if ((map_flags_u & ~kD3DMapFlagDoNotWait) != 0) {
    set_error(dev, E_INVALIDARG);
    return;
  }

  if (pMap->Subresource != 0) {
    set_error(dev, E_NOTIMPL);
    return;
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
      set_error(dev, E_INVALIDARG);
      return;
  }
  const bool want_read = (map_type_u == kD3DMapRead || map_type_u == kD3DMapReadWrite);

  // Enforce D3D10 usage/CPU-access rules (matches Win7 runtime expectations).
  const uint32_t cpu_read = kD3D10CpuAccessRead;
  const uint32_t cpu_write = kD3D10CpuAccessWrite;
  switch (res->usage) {
    case kD3D10UsageDynamic:
      if (map_type_u != kD3DMapWriteDiscard && map_type_u != kD3DMapWriteNoOverwrite) {
        set_error(dev, E_INVALIDARG);
        return;
      }
      break;
    case kD3D10UsageStaging: {
      const uint32_t access_mask = cpu_read | cpu_write;
      const uint32_t access = res->cpu_access_flags & access_mask;
      if (access == cpu_read) {
        if (map_type_u != kD3DMapRead) {
          set_error(dev, E_INVALIDARG);
          return;
        }
      } else if (access == cpu_write) {
        if (map_type_u != kD3DMapWrite) {
          set_error(dev, E_INVALIDARG);
          return;
        }
      } else if (access == access_mask) {
        if (map_type_u != kD3DMapRead && map_type_u != kD3DMapWrite && map_type_u != kD3DMapReadWrite) {
          set_error(dev, E_INVALIDARG);
          return;
        }
      } else {
        set_error(dev, E_INVALIDARG);
        return;
      }
      break;
    }
    default:
      // DEFAULT/IMMUTABLE resources are not mappable via D3D10 Map.
      set_error(dev, E_INVALIDARG);
      return;
  }

  if (want_read && !(res->cpu_access_flags & cpu_read)) {
    set_error(dev, E_INVALIDARG);
    return;
  }
  if (want_write && !(res->cpu_access_flags & cpu_write)) {
    set_error(dev, E_INVALIDARG);
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
    set_error(dev, E_INVALIDARG);
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
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (aerogpu_format_is_block_compressed(aer_fmt) && !aerogpu::d3d10_11::SupportsBcFormats(dev)) {
      set_error(dev, E_NOTIMPL);
      return;
    }
    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
    if (!fmt_layout.valid || fmt_layout.block_width == 0 || fmt_layout.block_height == 0 || fmt_layout.bytes_per_block == 0) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    const uint32_t mip_w = dst_layout.width;
    const uint32_t mip_h = dst_layout.height;
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, mip_w);
    if (min_row_bytes == 0 || dst_layout.row_pitch_bytes < min_row_bytes || dst_layout.size_bytes == 0) {
      set_error(dev, E_INVALIDARG);
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

    if (dst_layout.offset_bytes > res->storage.size()) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    const size_t dst_base = static_cast<size_t>(dst_layout.offset_bytes);
    if (dst_layout.size_bytes > res->storage.size() - dst_base) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    uint32_t left = 0;
    uint32_t top = 0;
    uint32_t right = mip_w;
    uint32_t bottom = mip_h;
    if (pArgs->pDstBox) {
      const auto* box = pArgs->pDstBox;
      if (box->right < box->left || box->bottom < box->top || box->front != 0 || box->back != 1) {
        set_error(dev, E_INVALIDARG);
        return;
      }
      left = box->left;
      top = box->top;
      right = box->right;
      bottom = box->bottom;
    }
    if (right > mip_w || bottom > mip_h) {
      set_error(dev, E_INVALIDARG);
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
        set_error(dev, E_INVALIDARG);
        return;
      }
    }

    const uint32_t block_left = left / fmt_layout.block_width;
    const uint32_t block_top = top / fmt_layout.block_height;
    const uint32_t block_right = aerogpu_div_round_up_u32(right, fmt_layout.block_width);
    const uint32_t block_bottom = aerogpu_div_round_up_u32(bottom, fmt_layout.block_height);
    if (block_right < block_left || block_bottom < block_top) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    const uint32_t copy_width_blocks = block_right - block_left;
    const uint32_t copy_height_blocks = block_bottom - block_top;
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
    if (row_bytes_u64 == 0 || row_bytes_u64 > UINT32_MAX || copy_height_blocks == 0) {
      // Empty boxes are a no-op.
      return;
    }
    const uint32_t row_bytes = static_cast<uint32_t>(row_bytes_u64);

    const uint32_t pitch = pArgs->RowPitch ? static_cast<uint32_t>(pArgs->RowPitch) : row_bytes;
    if (pitch < row_bytes) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    const bool full_row_update = (left == 0) && (right == mip_w);
    const uint64_t row_needed =
        static_cast<uint64_t>(block_left) * static_cast<uint64_t>(fmt_layout.bytes_per_block) + static_cast<uint64_t>(row_bytes);
    if (row_needed > dst_layout.row_pitch_bytes) {
      set_error(dev, E_INVALIDARG);
      return;
    }
    if (block_top + copy_height_blocks > dst_layout.rows_in_layout) {
      set_error(dev, E_INVALIDARG);
      return;
    }

    const uint8_t* src_bytes = static_cast<const uint8_t*>(pSysMem);
    for (uint32_t y = 0; y < copy_height_blocks; ++y) {
      const size_t dst_off =
          dst_base +
          static_cast<size_t>(block_top + y) * dst_layout.row_pitch_bytes +
          static_cast<size_t>(block_left) * fmt_layout.bytes_per_block;
      const size_t src_off = static_cast<size_t>(y) * static_cast<size_t>(pitch);
      std::memcpy(res->storage.data() + dst_off, src_bytes + src_off, row_bytes);
      // For boxed updates, preserve any per-row padding outside the updated
      // rectangle. Only clear padding for full-subresource uploads.
      if (!pArgs->pDstBox && full_row_update && dst_layout.row_pitch_bytes > row_bytes) {
        const size_t dst_row_start = dst_base + static_cast<size_t>(block_top + y) * dst_layout.row_pitch_bytes;
        std::memset(res->storage.data() + dst_row_start + row_bytes, 0, dst_layout.row_pitch_bytes - row_bytes);
      }
    }

    if (res->backing_alloc_id == 0 && pArgs->pDstBox) {
      // Host-owned boxed texture uploads must be row-aligned for the host-side
      // executor. Upload the affected row range (full rows) rather than
      // attempting to upload per-row subranges.
      const uint64_t row_pitch_u64 = static_cast<uint64_t>(dst_layout.row_pitch_bytes);
      const uint64_t upload_offset =
          dst_layout.offset_bytes + static_cast<uint64_t>(block_top) * row_pitch_u64;
      const uint64_t upload_size =
          static_cast<uint64_t>(copy_height_blocks) * row_pitch_u64;
      emit_upload_resource_locked(dev, res, upload_offset, upload_size);
      return;
    }

    if (res->backing_alloc_id == 0) {
      emit_upload_resource_locked(dev, res, dst_layout.offset_bytes, dst_layout.size_bytes);
      return;
    }

    if (!pArgs->pDstBox) {
      emit_upload_resource_locked(dev, res, dst_layout.offset_bytes, dst_layout.size_bytes);
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

    // Guest-backed textures are interpreted by the host using the protocol pitch
    // (`CREATE_TEXTURE2D.row_pitch_bytes`). Ignore the runtime's LockCb pitch so
    // CPU writes into the guest allocation match what the host expects.
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
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
      set_error(dev, E_INVALIDARG);
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
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
      set_error(dev, E_OUTOFMEMORY);
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
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
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
      (void)CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
      set_error(dev, E_OUTOFMEMORY);
      return;
    }
    dirty->resource_handle = res->handle;
    dirty->reserved0 = 0;
    dirty->offset_bytes = dst_layout.offset_bytes;
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
    hr = CallCbMaybeHandle(cb->pfnUnlockCb, dev->hrt_device, &unlock_args);
    if (FAILED(hr)) {
      set_error(dev, hr);
      return;
    }
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
  try {
    resources.reserve(numResources);
  } catch (...) {
    set_error(dev, E_OUTOFMEMORY);
    return;
  }
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

  // Treat RotateResourceIdentities as a transaction: if rebinding packets cannot
  // be appended (OOM), roll back the command stream and undo the rotation so the
  // runtime-visible state remains unchanged.
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const uint32_t prev_rtv_count = dev->current_rtv_count;
  const std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> prev_rtvs = dev->current_rtvs;
  const aerogpu_handle_t prev_dsv = dev->current_dsv;

  struct ResourceIdentity {
    aerogpu_handle_t handle = 0;
    uint32_t backing_alloc_id = 0;
    uint32_t backing_offset_bytes = 0;
    uint32_t wddm_allocation_handle = 0;
    uint32_t usage = 0;
    uint32_t cpu_access_flags = 0;
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
    id.usage = res->usage;
    id.cpu_access_flags = res->cpu_access_flags;
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
    res->usage = id.usage;
    res->cpu_access_flags = id.cpu_access_flags;
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

    if (report_oom) {
      set_error(dev, E_OUTOFMEMORY);
    }
  };

  // Capture the pre-rotation AeroGPU handles so we can remap bound handle slots
  // (which store raw protocol handles, not resource pointers).
  std::vector<aerogpu_handle_t> old_handles;
  try {
    old_handles.reserve(resources.size());
  } catch (...) {
    set_error(dev, E_OUTOFMEMORY);
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

    // Update the cached handles only after we've successfully appended the
    // rebind packet. If we fail to append (OOM), we roll back the rotation and
    // must keep the previous handles intact.
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
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      rollback_rotation(/*report_oom=*/false);
      return;
    }
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
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      rollback_rotation(/*report_oom=*/false);
      return;
    }
    cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->slot = slot;
    cmd->texture = dev->current_ps_srvs[slot] ? dev->current_ps_srvs[slot]->handle : 0;
    cmd->reserved0 = 0;
  }

  for (uint32_t slot = 0; slot < dev->current_gs_srvs.size(); ++slot) {
    if (!is_rotated(dev->current_gs_srvs[slot])) {
      continue;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
    if (!cmd) {
      set_error(dev, E_OUTOFMEMORY);
      rollback_rotation(/*report_oom=*/false);
      return;
    }
    cmd->shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY;
    cmd->slot = slot;
    cmd->texture = dev->current_gs_srvs[slot] ? dev->current_gs_srvs[slot]->handle : 0;
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
  if (!ValidateNoNullDdiTable("D3D10_1DDI_DEVICEFUNCS (stubs)", pCreateDevice->pDeviceFuncs, sizeof(*pCreateDevice->pDeviceFuncs))) {
#if defined(_WIN32)
    OutputDebugStringA("aerogpu-d3d10_1: CreateDevice: device function table has NULL entries after stub fill\n");
#endif
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return E_NOINTERFACE;
  }

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = AEROGPU_D3D10_1_WDK_DDI(DestroyDevice);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateResourceSize);
  pCreateDevice->pDeviceFuncs->pfnCreateResource = AEROGPU_D3D10_1_WDK_DDI(CreateResource);
  {
    using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
    if constexpr (HasOpenResource<DeviceFuncs>::value) {
      using Fn = decltype(pCreateDevice->pDeviceFuncs->pfnOpenResource);
      if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
        pCreateDevice->pDeviceFuncs->pfnOpenResource = AEROGPU_D3D10_1_WDK_DDI(OpenResource);
      }
    }
  }
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = AEROGPU_D3D10_1_WDK_DDI(DestroyResource);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateVertexShaderSize);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivatePixelShaderSize);
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = AEROGPU_D3D10_1_WDK_DDI(CreateVertexShader);
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = AEROGPU_D3D10_1_WDK_DDI(CreatePixelShader);
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = AEROGPU_D3D10_1_WDK_DDI(DestroyVertexShader);
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = AEROGPU_D3D10_1_WDK_DDI(DestroyPixelShader);
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize =
        AEROGPU_D3D10_1_WDK_DDI(CalcPrivateGeometryShaderSize);
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader =
        AEROGPU_D3D10_1_WDK_DDI(CreateGeometryShader);
    pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader =
        AEROGPU_D3D10_1_WDK_DDI(DestroyGeometryShader);
  }
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderWithStreamOutputSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        AEROGPU_D3D10_1_WDK_DDI(CalcPrivateGeometryShaderWithStreamOutputSizeImpl<
                                decltype(pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call);
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput =
        AEROGPU_D3D10_1_WDK_DDI(CreateGeometryShaderWithStreamOutputImpl<
                                decltype(pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput)>::Call);
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateElementLayoutSize);
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = AEROGPU_D3D10_1_WDK_DDI(CreateElementLayout);
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = AEROGPU_D3D10_1_WDK_DDI(DestroyElementLayout);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateRTVSize);
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(CreateRenderTargetView);
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(DestroyRenderTargetView);
  pCreateDevice->pDeviceFuncs->pfnClearRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(ClearRenderTargetView);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateDSVSize);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(CreateDepthStencilView);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(DestroyDepthStencilView);
  pCreateDevice->pDeviceFuncs->pfnClearDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(ClearDepthStencilView);
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateShaderResourceViewSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateShaderResourceViewSize);
    pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = AEROGPU_D3D10_1_WDK_DDI(CreateShaderResourceView);
    pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = AEROGPU_D3D10_1_WDK_DDI(DestroyShaderResourceView);
  }
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnCalcPrivateSamplerSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateSamplerSize);
    pCreateDevice->pDeviceFuncs->pfnCreateSampler = AEROGPU_D3D10_1_WDK_DDI(CreateSampler);
    pCreateDevice->pDeviceFuncs->pfnDestroySampler = AEROGPU_D3D10_1_WDK_DDI(DestroySampler);
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateBlendStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = AEROGPU_D3D10_1_WDK_DDI(CreateBlendState);
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = AEROGPU_D3D10_1_WDK_DDI(DestroyBlendState);
#if AEROGPU_D3D10_TRACE
  #define AEROGPU_D3D10_ASSIGN_STUB(field, id)                                     \
    pCreateDevice->pDeviceFuncs->field =                                           \
        &DdiTraceStub<decltype(pCreateDevice->pDeviceFuncs->field), DdiTraceStubId::id>::Call
#else
  #define AEROGPU_D3D10_ASSIGN_STUB(field, id) \
    pCreateDevice->pDeviceFuncs->field = &DdiStub<decltype(pCreateDevice->pDeviceFuncs->field)>::Call
#endif

  pCreateDevice->pDeviceFuncs->pfnSetBlendState = AEROGPU_D3D10_1_WDK_DDI(SetBlendState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateRasterizerStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = AEROGPU_D3D10_1_WDK_DDI(CreateRasterizerState);
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = AEROGPU_D3D10_1_WDK_DDI(DestroyRasterizerState);
  pCreateDevice->pDeviceFuncs->pfnSetRasterizerState = AEROGPU_D3D10_1_WDK_DDI(SetRasterizerState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateDepthStencilStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(CreateDepthStencilState);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(DestroyDepthStencilState);
  pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(SetDepthStencilState);

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = AEROGPU_D3D10_1_WDK_DDI(IaSetInputLayout);
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = AEROGPU_D3D10_1_WDK_DDI(IaSetVertexBuffers);
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = AEROGPU_D3D10_1_WDK_DDI(IaSetIndexBuffer);
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = AEROGPU_D3D10_1_WDK_DDI(IaSetTopology);

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = AEROGPU_D3D10_1_WDK_DDI(VsSetShader);
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = AEROGPU_D3D10_1_WDK_DDI(PsSetShader);

  pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(VsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(PsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(VsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(PsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnVsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(VsSetSamplers);
  pCreateDevice->pDeviceFuncs->pfnPsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(PsSetSamplers);

  pCreateDevice->pDeviceFuncs->pfnGsSetShader =
      AEROGPU_D3D10_1_WDK_DDI(GsSetShaderImpl<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShader)>::Call);
  pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(GsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(GsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnGsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(GsSetSamplers);

  pCreateDevice->pDeviceFuncs->pfnSetViewports = AEROGPU_D3D10_1_WDK_DDI(SetViewports);
  pCreateDevice->pDeviceFuncs->pfnSetScissorRects = AEROGPU_D3D10_1_WDK_DDI(SetScissorRects);
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = AEROGPU_D3D10_1_WDK_DDI(SetRenderTargets);
  __if_exists(D3D10_1DDI_DEVICEFUNCS::pfnSoSetTargets) {
    pCreateDevice->pDeviceFuncs->pfnSoSetTargets =
        AEROGPU_D3D10_1_WDK_DDI(SoSetTargetsImpl<decltype(pCreateDevice->pDeviceFuncs->pfnSoSetTargets)>::Call);
  }

  pCreateDevice->pDeviceFuncs->pfnDraw = AEROGPU_D3D10_1_WDK_DDI(Draw);
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = AEROGPU_D3D10_1_WDK_DDI(DrawIndexed);
  pCreateDevice->pDeviceFuncs->pfnDrawInstanced = AEROGPU_D3D10_1_WDK_DDI(DrawInstanced);
  pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced = AEROGPU_D3D10_1_WDK_DDI(DrawIndexedInstanced);
  pCreateDevice->pDeviceFuncs->pfnDrawAuto = AEROGPU_D3D10_1_WDK_DDI(DrawAuto);
#undef AEROGPU_D3D10_ASSIGN_STUB
  pCreateDevice->pDeviceFuncs->pfnPresent = AEROGPU_D3D10_1_WDK_DDI(Present);
  pCreateDevice->pDeviceFuncs->pfnFlush = AEROGPU_D3D10_1_WDK_DDI(Flush);
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = AEROGPU_D3D10_1_WDK_DDI(RotateResourceIdentities);
  pCreateDevice->pDeviceFuncs->pfnClearState = AEROGPU_D3D10_1_WDK_DDI(ClearState);

  // Map/unmap. Win7 D3D11 runtimes may use specialized entrypoints.
  pCreateDevice->pDeviceFuncs->pfnMap = AEROGPU_D3D10_1_WDK_DDI(Map);
  pCreateDevice->pDeviceFuncs->pfnUnmap = AEROGPU_D3D10_1_WDK_DDI(Unmap);
  using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
  if constexpr (HasStagingResourceMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnStagingResourceMap = AEROGPU_D3D10_1_WDK_DDI(StagingResourceMap<>);
    pCreateDevice->pDeviceFuncs->pfnStagingResourceUnmap = AEROGPU_D3D10_1_WDK_DDI(StagingResourceUnmap<>);
  }
  if constexpr (HasDynamicIABufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapDiscard = AEROGPU_D3D10_1_WDK_DDI(DynamicIABufferMapDiscard<>);
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferMapNoOverwrite = AEROGPU_D3D10_1_WDK_DDI(DynamicIABufferMapNoOverwrite<>);
    pCreateDevice->pDeviceFuncs->pfnDynamicIABufferUnmap = AEROGPU_D3D10_1_WDK_DDI(DynamicIABufferUnmap<>);
  }
  if constexpr (HasDynamicConstantBufferMap<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferMapDiscard = AEROGPU_D3D10_1_WDK_DDI(DynamicConstantBufferMapDiscard<>);
    pCreateDevice->pDeviceFuncs->pfnDynamicConstantBufferUnmap = AEROGPU_D3D10_1_WDK_DDI(DynamicConstantBufferUnmap<>);
  }
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = AEROGPU_D3D10_1_WDK_DDI(UpdateSubresourceUP);
  pCreateDevice->pDeviceFuncs->pfnCopyResource =
      AEROGPU_D3D10_1_WDK_DDI(CopyResourceImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call);
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      AEROGPU_D3D10_1_WDK_DDI(CopySubresourceRegionImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call);

  if (!ValidateNoNullDdiTable("D3D10_1DDI_DEVICEFUNCS", pCreateDevice->pDeviceFuncs, sizeof(*pCreateDevice->pDeviceFuncs))) {
#if defined(_WIN32)
    OutputDebugStringA("aerogpu-d3d10_1: CreateDevice: device function table has NULL entries after overrides\n");
#endif
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    AEROGPU_D3D10_RET_HR(E_NOINTERFACE);
  }

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
  if (!ValidateNoNullDdiTable("D3D10DDI_DEVICEFUNCS (stubs)", pCreateDevice->pDeviceFuncs, sizeof(*pCreateDevice->pDeviceFuncs))) {
#if defined(_WIN32)
    OutputDebugStringA("aerogpu-d3d10_1: CreateDevice10: device function table has NULL entries after stub fill\n");
#endif
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return E_NOINTERFACE;
  }

  pCreateDevice->pDeviceFuncs->pfnDestroyDevice = AEROGPU_D3D10_1_WDK_DDI(DestroyDevice);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateResourceSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateResourceSize);
  pCreateDevice->pDeviceFuncs->pfnCreateResource = AEROGPU_D3D10_1_WDK_DDI(CreateResource);
  {
    using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
    if constexpr (HasOpenResource<DeviceFuncs>::value) {
      using Fn = decltype(pCreateDevice->pDeviceFuncs->pfnOpenResource);
      if constexpr (std::is_convertible_v<decltype(&OpenResource), Fn>) {
        pCreateDevice->pDeviceFuncs->pfnOpenResource = AEROGPU_D3D10_1_WDK_DDI(OpenResource);
      }
    }
  }
  pCreateDevice->pDeviceFuncs->pfnDestroyResource = AEROGPU_D3D10_1_WDK_DDI(DestroyResource);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateVertexShaderSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateVertexShaderSize);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivatePixelShaderSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivatePixelShaderSize);
  pCreateDevice->pDeviceFuncs->pfnCreateVertexShader = AEROGPU_D3D10_1_WDK_DDI(CreateVertexShader);
  pCreateDevice->pDeviceFuncs->pfnCreatePixelShader = AEROGPU_D3D10_1_WDK_DDI(CreatePixelShader);
  pCreateDevice->pDeviceFuncs->pfnDestroyVertexShader = AEROGPU_D3D10_1_WDK_DDI(DestroyVertexShader);
  pCreateDevice->pDeviceFuncs->pfnDestroyPixelShader = AEROGPU_D3D10_1_WDK_DDI(DestroyPixelShader);
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderSize) {
    // Geometry shaders are accepted by the Win7 D3D10 runtime at FL10_0; forward
    // DXBC to the host and bind via `BIND_SHADERS` (legacy compat: GS handle carried via
    // `aerogpu_cmd_bind_shaders.reserved0`).
    //
    // Newer protocol versions also support an append-only extension that appends `{gs, hs, ds}`
    // handles after the stable 24-byte prefix. Producers may mirror `gs` into `reserved0` so older
    // hosts/tools can still observe a bound GS. When present, the appended `{gs,hs,ds}` handles are
    // authoritative; `reserved0` is only a legacy compatibility mirror. If mirrored, it should
    // match the appended `gs` handle.
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateGeometryShaderSize);
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShader = AEROGPU_D3D10_1_WDK_DDI(CreateGeometryShader);
    pCreateDevice->pDeviceFuncs->pfnDestroyGeometryShader = AEROGPU_D3D10_1_WDK_DDI(DestroyGeometryShader);
  }
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnCalcPrivateGeometryShaderWithStreamOutputSize) {
    pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize =
        AEROGPU_D3D10_1_WDK_DDI(CalcPrivateGeometryShaderWithStreamOutputSizeImpl<
                                decltype(pCreateDevice->pDeviceFuncs->pfnCalcPrivateGeometryShaderWithStreamOutputSize)>::Call);
    pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput =
        AEROGPU_D3D10_1_WDK_DDI(CreateGeometryShaderWithStreamOutputImpl<
                                decltype(pCreateDevice->pDeviceFuncs->pfnCreateGeometryShaderWithStreamOutput)>::Call);
  }

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateElementLayoutSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateElementLayoutSize);
  pCreateDevice->pDeviceFuncs->pfnCreateElementLayout = AEROGPU_D3D10_1_WDK_DDI(CreateElementLayout);
  pCreateDevice->pDeviceFuncs->pfnDestroyElementLayout = AEROGPU_D3D10_1_WDK_DDI(DestroyElementLayout);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRenderTargetViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateRTVSize);
  pCreateDevice->pDeviceFuncs->pfnCreateRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(CreateRenderTargetView);
  pCreateDevice->pDeviceFuncs->pfnDestroyRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(DestroyRenderTargetView);
  pCreateDevice->pDeviceFuncs->pfnClearRenderTargetView = AEROGPU_D3D10_1_WDK_DDI(ClearRenderTargetView);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateDSVSize);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(CreateDepthStencilView);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(DestroyDepthStencilView);
  pCreateDevice->pDeviceFuncs->pfnClearDepthStencilView = AEROGPU_D3D10_1_WDK_DDI(ClearDepthStencilView);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateShaderResourceViewSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateShaderResourceViewSize);
  pCreateDevice->pDeviceFuncs->pfnCreateShaderResourceView = AEROGPU_D3D10_1_WDK_DDI(CreateShaderResourceView);
  pCreateDevice->pDeviceFuncs->pfnDestroyShaderResourceView = AEROGPU_D3D10_1_WDK_DDI(DestroyShaderResourceView);
  pCreateDevice->pDeviceFuncs->pfnCalcPrivateSamplerSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateSamplerSize);
  pCreateDevice->pDeviceFuncs->pfnCreateSampler = AEROGPU_D3D10_1_WDK_DDI(CreateSampler);
  pCreateDevice->pDeviceFuncs->pfnDestroySampler = AEROGPU_D3D10_1_WDK_DDI(DestroySampler);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateBlendStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateBlendStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateBlendState = AEROGPU_D3D10_1_WDK_DDI(CreateBlendState);
  pCreateDevice->pDeviceFuncs->pfnDestroyBlendState = AEROGPU_D3D10_1_WDK_DDI(DestroyBlendState);
  pCreateDevice->pDeviceFuncs->pfnSetBlendState = AEROGPU_D3D10_1_WDK_DDI(SetBlendState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateRasterizerStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateRasterizerStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateRasterizerState = AEROGPU_D3D10_1_WDK_DDI(CreateRasterizerState);
  pCreateDevice->pDeviceFuncs->pfnDestroyRasterizerState = AEROGPU_D3D10_1_WDK_DDI(DestroyRasterizerState);
  pCreateDevice->pDeviceFuncs->pfnSetRasterizerState = AEROGPU_D3D10_1_WDK_DDI(SetRasterizerState);

  pCreateDevice->pDeviceFuncs->pfnCalcPrivateDepthStencilStateSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateDepthStencilStateSize);
  pCreateDevice->pDeviceFuncs->pfnCreateDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(CreateDepthStencilState);
  pCreateDevice->pDeviceFuncs->pfnDestroyDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(DestroyDepthStencilState);
  pCreateDevice->pDeviceFuncs->pfnSetDepthStencilState = AEROGPU_D3D10_1_WDK_DDI(SetDepthStencilState);

  pCreateDevice->pDeviceFuncs->pfnIaSetInputLayout = AEROGPU_D3D10_1_WDK_DDI(IaSetInputLayout);
  pCreateDevice->pDeviceFuncs->pfnIaSetVertexBuffers = AEROGPU_D3D10_1_WDK_DDI(IaSetVertexBuffers);
  pCreateDevice->pDeviceFuncs->pfnIaSetIndexBuffer = AEROGPU_D3D10_1_WDK_DDI(IaSetIndexBuffer);
  pCreateDevice->pDeviceFuncs->pfnIaSetTopology = AEROGPU_D3D10_1_WDK_DDI(IaSetTopology);

  pCreateDevice->pDeviceFuncs->pfnVsSetShader = AEROGPU_D3D10_1_WDK_DDI(VsSetShader);
  pCreateDevice->pDeviceFuncs->pfnPsSetShader = AEROGPU_D3D10_1_WDK_DDI(PsSetShader);

  pCreateDevice->pDeviceFuncs->pfnVsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(VsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnPsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(PsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnVsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(VsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnPsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(PsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnVsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(VsSetSamplers);
  pCreateDevice->pDeviceFuncs->pfnPsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(PsSetSamplers);

  pCreateDevice->pDeviceFuncs->pfnGsSetShader =
      AEROGPU_D3D10_1_WDK_DDI(GsSetShaderImpl<decltype(pCreateDevice->pDeviceFuncs->pfnGsSetShader)>::Call);
  pCreateDevice->pDeviceFuncs->pfnGsSetConstantBuffers = AEROGPU_D3D10_1_WDK_DDI(GsSetConstantBuffers);
  pCreateDevice->pDeviceFuncs->pfnGsSetShaderResources = AEROGPU_D3D10_1_WDK_DDI(GsSetShaderResources);
  pCreateDevice->pDeviceFuncs->pfnGsSetSamplers = AEROGPU_D3D10_1_WDK_DDI(GsSetSamplers);

  pCreateDevice->pDeviceFuncs->pfnSetViewports = AEROGPU_D3D10_1_WDK_DDI(SetViewports);
  pCreateDevice->pDeviceFuncs->pfnSetScissorRects = AEROGPU_D3D10_1_WDK_DDI(SetScissorRects);
  pCreateDevice->pDeviceFuncs->pfnSetRenderTargets = AEROGPU_D3D10_1_WDK_DDI(SetRenderTargets);
  __if_exists(D3D10DDI_DEVICEFUNCS::pfnSoSetTargets) {
    pCreateDevice->pDeviceFuncs->pfnSoSetTargets =
        AEROGPU_D3D10_1_WDK_DDI(SoSetTargetsImpl<decltype(pCreateDevice->pDeviceFuncs->pfnSoSetTargets)>::Call);
  }

  pCreateDevice->pDeviceFuncs->pfnDraw = AEROGPU_D3D10_1_WDK_DDI(Draw);
  pCreateDevice->pDeviceFuncs->pfnDrawIndexed = AEROGPU_D3D10_1_WDK_DDI(DrawIndexed);
  pCreateDevice->pDeviceFuncs->pfnDrawInstanced = AEROGPU_D3D10_1_WDK_DDI(DrawInstanced);
  pCreateDevice->pDeviceFuncs->pfnDrawIndexedInstanced = AEROGPU_D3D10_1_WDK_DDI(DrawIndexedInstanced);
  pCreateDevice->pDeviceFuncs->pfnDrawAuto = AEROGPU_D3D10_1_WDK_DDI(DrawAuto);
  pCreateDevice->pDeviceFuncs->pfnPresent = AEROGPU_D3D10_1_WDK_DDI(Present);
  pCreateDevice->pDeviceFuncs->pfnFlush = AEROGPU_D3D10_1_WDK_DDI(Flush);
  pCreateDevice->pDeviceFuncs->pfnRotateResourceIdentities = AEROGPU_D3D10_1_WDK_DDI(RotateResourceIdentities);
  pCreateDevice->pDeviceFuncs->pfnClearState = AEROGPU_D3D10_1_WDK_DDI(ClearState);

  using DeviceFuncs = std::remove_pointer_t<decltype(pCreateDevice->pDeviceFuncs)>;
  if constexpr (HasOpenResource<DeviceFuncs>::value) {
    pCreateDevice->pDeviceFuncs->pfnOpenResource = AEROGPU_D3D10_1_WDK_DDI(OpenResource);
  }

  pCreateDevice->pDeviceFuncs->pfnMap = AEROGPU_D3D10_1_WDK_DDI(Map);
  pCreateDevice->pDeviceFuncs->pfnUnmap = AEROGPU_D3D10_1_WDK_DDI(Unmap);
  pCreateDevice->pDeviceFuncs->pfnUpdateSubresourceUP = AEROGPU_D3D10_1_WDK_DDI(UpdateSubresourceUP);
  pCreateDevice->pDeviceFuncs->pfnCopyResource =
      AEROGPU_D3D10_1_WDK_DDI(CopyResourceImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopyResource)>::Call);
  pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion =
      AEROGPU_D3D10_1_WDK_DDI(CopySubresourceRegionImpl<decltype(pCreateDevice->pDeviceFuncs->pfnCopySubresourceRegion)>::Call);

  if (!ValidateNoNullDdiTable("D3D10DDI_DEVICEFUNCS", pCreateDevice->pDeviceFuncs, sizeof(*pCreateDevice->pDeviceFuncs))) {
#if defined(_WIN32)
    OutputDebugStringA("aerogpu-d3d10_1: CreateDevice10: device function table has NULL entries after overrides\n");
#endif
    DestroyKernelDeviceContext(device);
    device->~AeroGpuDevice();
    return E_NOINTERFACE;
  }

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
        __if_exists(D3D10DDIARG_FORMAT_SUPPORT::FormatSupport2) {
          fmt->FormatSupport2 = 0;
        }
        AEROGPU_D3D10_TRACEF("GetCaps10 FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
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
  auto* caps_adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);

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

    case D3D10_1DDICAPS_TYPE_FORMAT_SUPPORT:
      if (pCaps->DataSize >= sizeof(D3D10_1DDIARG_FORMAT_SUPPORT)) {
        auto* fmt = reinterpret_cast<D3D10_1DDIARG_FORMAT_SUPPORT*>(pCaps->pData);
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
        fmt->FormatSupport2 = 0;
        AEROGPU_D3D10_TRACEF("GetCaps FORMAT_SUPPORT fmt=%u support=0x%x", format, support);
      }
      break;

    case D3D10_1DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS:
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
    InitAdapterFuncsWithStubs(funcs);
    funcs->pfnGetCaps = AEROGPU_D3D10_1_WDK_DDI(GetCaps);
    funcs->pfnCalcPrivateDeviceSize = AEROGPU_D3D10_1_WDK_DDI(CalcPrivateDeviceSize);
    funcs->pfnCreateDevice = AEROGPU_D3D10_1_WDK_DDI(CreateDevice);
    funcs->pfnCloseAdapter = AEROGPU_D3D10_1_WDK_DDI(CloseAdapter);
    if (!ValidateNoNullDdiTable("D3D10_1DDI_ADAPTERFUNCS", funcs, sizeof(*funcs))) {
      pOpenData->hAdapter.pDrvPrivate = nullptr;
      DestroyKmtAdapterHandle(adapter);
      delete adapter;
      AEROGPU_D3D10_RET_HR(E_NOINTERFACE);
    }
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  AEROGPU_D3D10_RET_HR(E_INVALIDARG);
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
  try {
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
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  try {
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
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}
} // extern "C"

#endif // defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
