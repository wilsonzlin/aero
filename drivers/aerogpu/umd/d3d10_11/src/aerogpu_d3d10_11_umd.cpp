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
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_d3d10_blend_state_validate.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_trace.h"
#include "../../common/aerogpu_win32_security.h"

#ifndef FAILED
#define FAILED(hr) (((HRESULT)(hr)) < 0)
#endif

namespace {

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

// Reuse the shared D3D10/11 helpers from `aerogpu_d3d10_11_internal.h` in the
// portable (non-WDK) UMD so format/packing logic stays in sync with the Win7
// WDK path.
using aerogpu::d3d10_11::AerogpuTextureFormatLayout;
using aerogpu::d3d10_11::Texture2DSubresourceLayout;
using aerogpu::d3d10_11::aerogpu_div_round_up_u32;
using aerogpu::d3d10_11::aerogpu_format_is_block_compressed;
using aerogpu::d3d10_11::aerogpu_mip_dim;
using aerogpu::d3d10_11::aerogpu_texture_format_layout;
using aerogpu::d3d10_11::aerogpu_texture_min_row_pitch_bytes;
using aerogpu::d3d10_11::aerogpu_texture_num_rows;
using aerogpu::d3d10_11::aerogpu_texture_required_size_bytes;
using aerogpu::d3d10_11::bind_flags_to_buffer_usage_flags;
using aerogpu::d3d10_11::bind_flags_to_usage_flags;
using aerogpu::d3d10_11::bytes_per_pixel_aerogpu;
using aerogpu::d3d10_11::dxgi_index_format_to_aerogpu;
using aerogpu::d3d10_11::f32_bits;
using aerogpu::d3d10_11::FromHandle;
using aerogpu::d3d10_11::HashSemanticName;
using aerogpu::d3d10_11::InitSamplerFromCreateSamplerArg;
using aerogpu::d3d10_11::AllocateGlobalHandle;
using aerogpu::d3d10_11::kAeroGpuTimeoutMsInfinite;
using aerogpu::d3d10_11::kInvalidHandle;
using aerogpu::d3d10_11::kDeviceDestroyLiveCookie;
using aerogpu::d3d10_11::HasLiveCookie;
using aerogpu::d3d10_11::atomic_max_u64;
using aerogpu::d3d10_11::TrackStagingWriteLocked;
using aerogpu::d3d10_11::ResourcesAlias;
using aerogpu::d3d10_11::kDxgiErrorWasStillDrawing;
using aerogpu::d3d10_11::kHrPending;
using aerogpu::d3d10_11::kHrWaitTimeout;
using aerogpu::d3d10_11::kHrErrorTimeout;
using aerogpu::d3d10_11::kHrNtStatusTimeout;
using aerogpu::d3d10_11::kHrNtStatusGraphicsGpuBusy;
using aerogpu::d3d10_11::validate_and_emit_scissor_rects_locked;
using aerogpu::d3d10_11::validate_and_emit_viewports_locked;

bool IsDeviceLive(D3D10DDI_HDEVICE hDevice) {
  return HasLiveCookie(hDevice.pDrvPrivate, kDeviceDestroyLiveCookie);
}


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

using aerogpu::d3d10_11::kMaxConstantBufferSlots;
using aerogpu::d3d10_11::kMaxShaderResourceSlots;
using aerogpu::d3d10_11::kMaxSamplerSlots;

using aerogpu::d3d10_11::kD3DSampleMaskAll;
using aerogpu::d3d10_11::kD3DColorWriteMaskAll;
using aerogpu::d3d10_11::kD3DStencilMaskAll;

// D3D11_BIND_* subset (numeric values from d3d11.h).
using aerogpu::d3d10_11::kD3D11BindVertexBuffer;
using aerogpu::d3d10_11::kD3D11BindIndexBuffer;
using aerogpu::d3d10_11::kD3D11BindConstantBuffer;
using aerogpu::d3d10_11::kD3D11BindShaderResource;
using aerogpu::d3d10_11::kD3D11BindRenderTarget;
using aerogpu::d3d10_11::kD3D11BindDepthStencil;

// D3D11_USAGE subset (numeric values from d3d11.h).
using aerogpu::d3d10_11::kD3D11UsageDefault;
using aerogpu::d3d10_11::kD3D11UsageImmutable;
using aerogpu::d3d10_11::kD3D11UsageDynamic;
using aerogpu::d3d10_11::kD3D11UsageStaging;

// D3D11_CPU_ACCESS_FLAG subset (numeric values from d3d11.h).
using aerogpu::d3d10_11::kD3D11CpuAccessWrite;
using aerogpu::d3d10_11::kD3D11CpuAccessRead;

// D3D11_MAP subset (numeric values from d3d11.h).
using aerogpu::d3d10_11::kD3D11MapRead;
using aerogpu::d3d10_11::kD3D11MapWrite;
using aerogpu::d3d10_11::kD3D11MapReadWrite;
using aerogpu::d3d10_11::kD3D11MapWriteDiscard;
using aerogpu::d3d10_11::kD3D11MapWriteNoOverwrite;

// D3D11_MAP_FLAG_DO_NOT_WAIT (numeric value from d3d11.h).
using aerogpu::d3d10_11::kD3D11MapFlagDoNotWait;

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

// D3D_FEATURE_LEVEL subset (numeric values from d3dcommon.h).
using aerogpu::d3d10_11::kD3DFeatureLevel10_0;

// D3D11DDICAPS_TYPE subset (numeric values from d3d11umddi.h).
using aerogpu::d3d10_11::kD3D11DdiCapsTypeThreading;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeDoubles;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeFormatSupport;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeFormatSupport2;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeD3D10XHardwareOptions;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeD3D11Options;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeArchitectureInfo;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeD3D9Options;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeFeatureLevels;
using aerogpu::d3d10_11::kD3D11DdiCapsTypeMultisampleQualityLevels;

// D3D11_RESOURCE_MISC_SHARED (numeric value from d3d11.h).
using aerogpu::d3d10_11::kD3D11ResourceMiscShared;

using AeroGpuAdapter = aerogpu::d3d10_11::Adapter;
using aerogpu::d3d10_11::AlignUpU64;
using aerogpu::d3d10_11::AlignDownU64;

uint32_t d3d11_format_support_flags(const AeroGpuAdapter* adapter, uint32_t dxgi_format) {
  return aerogpu::d3d10_11::D3D11FormatSupportFlags(adapter, dxgi_format);
}

bool compute_texture2d_subresource_layout(uint32_t aerogpu_format,
                                          uint32_t width,
                                          uint32_t height,
                                          uint32_t mip_levels,
                                          uint32_t array_layers,
                                          uint32_t mip0_row_pitch_bytes,
                                          uint32_t subresource,
                                          Texture2DSubresourceLayout* out_layout) {
  if (!out_layout) {
    return false;
  }
  *out_layout = Texture2DSubresourceLayout{};

  if (width == 0 || height == 0 || mip_levels == 0 || array_layers == 0 || mip0_row_pitch_bytes == 0) {
    return false;
  }

  const uint64_t subresource_count = static_cast<uint64_t>(mip_levels) * static_cast<uint64_t>(array_layers);
  if (subresource_count == 0 || subresource_count > static_cast<uint64_t>(UINT32_MAX)) {
    return false;
  }
  if (static_cast<uint64_t>(subresource) >= subresource_count) {
    return false;
  }

  const uint32_t mip = subresource % mip_levels;
  const uint32_t layer = subresource / mip_levels;
  if (layer >= array_layers) {
    return false;
  }

  uint64_t layer_stride = 0;
  uint64_t offset_in_layer = 0;
  uint64_t sub_size = 0;
  uint32_t sub_row_pitch = 0;
  uint32_t sub_rows = 0;
  uint32_t sub_w = 0;
  uint32_t sub_h = 0;

  uint32_t level_w = width;
  uint32_t level_h = height;
  for (uint32_t level = 0; level < mip_levels; ++level) {
    const uint32_t tight_pitch = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, level_w);
    const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, level_h);
    if (tight_pitch == 0 || rows == 0) {
      return false;
    }

    const uint32_t pitch = (level == 0) ? mip0_row_pitch_bytes : tight_pitch;
    if (pitch < tight_pitch) {
      return false;
    }

    const uint64_t size_bytes = static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows);
    if (size_bytes == 0) {
      return false;
    }
    if (layer_stride > UINT64_MAX - size_bytes) {
      return false;
    }
    layer_stride += size_bytes;

    if (level < mip) {
      if (offset_in_layer > UINT64_MAX - size_bytes) {
        return false;
      }
      offset_in_layer += size_bytes;
    }
    if (level == mip) {
      sub_size = size_bytes;
      sub_row_pitch = pitch;
      sub_rows = rows;
      sub_w = level_w;
      sub_h = level_h;
    }

    level_w = (level_w > 1) ? (level_w / 2) : 1u;
    level_h = (level_h > 1) ? (level_h / 2) : 1u;
  }

  const uint64_t layer_offset = static_cast<uint64_t>(layer) * layer_stride;
  if (layer != 0 && layer_stride != 0 && layer_offset / layer_stride != layer) {
    return false;
  }
  if (layer_offset > UINT64_MAX - offset_in_layer) {
    return false;
  }

  out_layout->mip_level = mip;
  out_layout->array_layer = layer;
  out_layout->width = sub_w;
  out_layout->height = sub_h;
  out_layout->offset_bytes = layer_offset + offset_in_layer;
  out_layout->row_pitch_bytes = sub_row_pitch;
  out_layout->rows_in_layout = sub_rows;
  out_layout->size_bytes = sub_size;
  return true;
}

bool compute_texture2d_total_bytes(uint32_t aerogpu_format,
                                  uint32_t width,
                                  uint32_t height,
                                  uint32_t mip_levels,
                                  uint32_t array_layers,
                                  uint32_t mip0_row_pitch_bytes,
                                  uint64_t* out_total_bytes) {
  if (!out_total_bytes) {
    return false;
  }
  *out_total_bytes = 0;

  if (width == 0 || height == 0 || mip_levels == 0 || array_layers == 0 || mip0_row_pitch_bytes == 0) {
    return false;
  }

  uint64_t layer_stride = 0;
  uint32_t level_w = width;
  uint32_t level_h = height;
  for (uint32_t level = 0; level < mip_levels; ++level) {
    const uint32_t tight_pitch = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, level_w);
    const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, level_h);
    if (tight_pitch == 0 || rows == 0) {
      return false;
    }
    const uint32_t pitch = (level == 0) ? mip0_row_pitch_bytes : tight_pitch;
    if (pitch < tight_pitch) {
      return false;
    }
    const uint64_t size_bytes = static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows);
    if (size_bytes == 0) {
      return false;
    }
    if (layer_stride > UINT64_MAX - size_bytes) {
      return false;
    }
    layer_stride += size_bytes;
    level_w = (level_w > 1) ? (level_w / 2) : 1u;
    level_h = (level_h > 1) ? (level_h / 2) : 1u;
  }

  const uint64_t total = layer_stride * static_cast<uint64_t>(array_layers);
  if (array_layers != 0 && layer_stride != 0 && total / layer_stride != array_layers) {
    return false;
  }
  *out_total_bytes = total;
  return true;
}

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

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

  // Fence value of the most recent GPU submission that writes into this resource.
  //
  // This is used for staging readback Map(READ)/Map(DO_NOT_WAIT) synchronization so
  // a read map does not spuriously fail due to unrelated in-flight work that
  // doesn't touch the resource.
  uint64_t last_gpu_write_fence = 0;

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
  aerogpu_handle_t texture = 0;
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuDepthStencilView {
  aerogpu_handle_t texture = 0;
  AeroGpuResource* resource = nullptr;
};

struct AeroGpuShaderResourceView {
  // Resource pointer is used for hazard tracking and so RotateResourceIdentities
  // (swapchain-style handle rotation) can rebind the latest resource handle.
  // The D3D runtime guarantees resources outlive views.
  AeroGpuResource* resource = nullptr;
  // If non-zero, this is a protocol texture view handle created with
  // CREATE_TEXTURE_VIEW. If zero, this view is "trivial" and bindings should use
  // `resource->handle` at bind-time.
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
// are accepted and can be bound. The state itself is encoded into the AeroGPU
// command stream when bound.
struct AeroGpuBlendState {
  aerogpu_blend_state state{};
};
struct AeroGpuRasterizerState {
  aerogpu_rasterizer_state state{};
};
struct AeroGpuDepthStencilState {
  aerogpu_depth_stencil_state state{};
};


struct AeroGpuDevice {
  // Cookie used to guard against accidental double-destroy or destroys of
  // uninitialized device private storage. DestroyDevice checks this value before
  // running the destructor.
  uint32_t destroy_cookie = kDeviceDestroyLiveCookie;
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
  std::vector<AEROGPU_WDDM_SUBMIT_ALLOCATION> referenced_allocs;
  // True if we failed to grow `referenced_allocs` due to OOM while recording
  // commands. Submitting with an incomplete allocation list is unsafe for
  // guest-backed resources because the host may not be able to resolve
  // `backing_alloc_id` references.
  bool referenced_alloc_list_oom = false;

  // Fence tracking for WDDM-backed synchronization. Higher-level D3D10/11 code (e.g. Map READ on
  // staging resources) can use these values to wait for GPU completion.
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  std::vector<AeroGpuResource*> live_resources;

  // Staging resources with CPU read access written by commands recorded since the
  // last submission. After submission, their `last_gpu_write_fence` is updated
  // to the returned fence value.
  std::vector<AeroGpuResource*> pending_staging_writes;


  // Cached state.
  //
  // D3D10/10.1 supports multiple simultaneous render targets (MRT). Track the full RTV array so
  // SetRenderTargets can faithfully bind >1 slot and so hazard resolution (SRV<->RTV aliasing)
  // can operate on all bound slots.
  uint32_t current_rtv_count = 0;
  aerogpu_handle_t current_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* current_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};

  aerogpu_handle_t current_dsv = 0;
  AeroGpuResource* current_dsv_res = nullptr;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;
  AeroGpuDepthStencilState* current_dss = nullptr;
  uint32_t current_stencil_ref = 0;
  AeroGpuRasterizerState* current_rs = nullptr;
  AeroGpuBlendState* current_bs = nullptr;
  float current_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  uint32_t current_sample_mask = kD3DSampleMaskAll;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_rtv_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_dsv_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_vb_alloc = 0;
  AEROGPU_WDDM_ALLOCATION_HANDLE current_ib_alloc = 0;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  AeroGpuResource* vs_srv_resources[kMaxShaderResourceSlots] = {};
  AeroGpuResource* ps_srv_resources[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};
};

inline void ReportDeviceErrorLocked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, HRESULT hr);

// D3D10/11 DDI entrypoints are invoked through function tables filled during
// OpenAdapter/CreateDevice. Even though we try to make hot paths allocation-free
// and to guard std::vector growth, we still defensively wrap every DDI call so a
// stray C++ exception (e.g. std::bad_alloc, std::system_error from mutex lock)
// cannot escape across the UMD ABI boundary.
template <typename... Args>
inline void ReportExceptionForArgs(HRESULT hr, const Args&... args) noexcept {
  if constexpr (sizeof...(Args) == 0) {
    return;
  } else {
    using First = std::tuple_element_t<0, std::tuple<Args...>>;
    if constexpr (std::is_same_v<std::decay_t<First>, D3D10DDI_HDEVICE>) {
      const auto tup = std::forward_as_tuple(args...);
      const auto hDevice = std::get<0>(tup);
      if (hDevice.pDrvPrivate) {
        auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
        if (dev) {
          ReportDeviceErrorLocked(dev, hDevice, hr);
        }
      }
    }
  }
}

template <auto Impl>
struct aerogpu_d3d10_11_ddi_thunk;

template <typename Ret, typename... Args, Ret(AEROGPU_APIENTRY* Impl)(Args...)>
struct aerogpu_d3d10_11_ddi_thunk<Impl> {
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
      } else {
        return Ret{};
      }
    } catch (...) {
      if constexpr (std::is_same_v<Ret, HRESULT>) {
        return E_FAIL;
      } else if constexpr (std::is_void_v<Ret>) {
        ReportExceptionForArgs(E_FAIL, args...);
        return;
      } else {
        return Ret{};
      }
    }
  }
};

#define AEROGPU_D3D10_11_DDI(fn) aerogpu_d3d10_11_ddi_thunk<&fn>::thunk

bool AddLiveResourceLocked(AeroGpuDevice* dev, AeroGpuResource* res) {
  if (!dev || !res) {
    return false;
  }
  try {
    dev->live_resources.push_back(res);
  } catch (...) {
    return false;
  }
  return true;
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

void track_alloc_for_submit_locked(AeroGpuDevice* dev, AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle, bool write) {
  if (!dev || alloc_handle == 0) {
    return;
  }
  if (dev->referenced_alloc_list_oom) {
    return;
  }

  auto& allocs = dev->referenced_allocs;
  for (auto& entry : allocs) {
    if (entry.handle == alloc_handle) {
      if (write) {
        entry.write = 1;
      }
      return;
    }
  }

  AEROGPU_WDDM_SUBMIT_ALLOCATION entry{};
  entry.handle = alloc_handle;
  entry.write = write ? 1 : 0;
  try {
    allocs.push_back(entry);
  } catch (...) {
    dev->referenced_alloc_list_oom = true;
    D3D10DDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = dev;
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
  }
}

void track_resource_alloc_for_submit_locked(AeroGpuDevice* dev, const AeroGpuResource* res, bool write) {
  if (!dev || !res) {
    return;
  }
  track_alloc_for_submit_locked(dev, res->alloc_handle, write);
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
  // Bound render targets / depth-stencil allocations are written by Draw/Clear.
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    track_resource_alloc_for_submit_locked(dev, dev->current_rtv_resources[i], /*write=*/true);
  }
  track_resource_alloc_for_submit_locked(dev, dev->current_dsv_res, /*write=*/true);
  // IA buffers are read by Draw/DrawIndexed.
  track_alloc_for_submit_locked(dev, dev->current_vb_alloc, /*write=*/false);
  track_alloc_for_submit_locked(dev, dev->current_ib_alloc, /*write=*/false);

  // Constant buffers and shader resources can be backed by guest allocations. Keep
  // them in the per-submit allocation list so the host can resolve alloc_id -> GPA
  // for resource bindings referenced by the command stream.
  for (uint32_t i = 0; i < kMaxConstantBufferSlots; i++) {
    const aerogpu_handle_t vs_handle = dev->vs_constant_buffers[i].buffer;
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, vs_handle), /*write=*/false);
    const aerogpu_handle_t ps_handle = dev->ps_constant_buffers[i].buffer;
    track_resource_alloc_for_submit_locked(dev, FindLiveResourceByHandleLocked(dev, ps_handle), /*write=*/false);
  }

  for (uint32_t i = 0; i < kMaxShaderResourceSlots; i++) {
    track_resource_alloc_for_submit_locked(dev, dev->vs_srv_resources[i], /*write=*/false);
    track_resource_alloc_for_submit_locked(dev, dev->ps_srv_resources[i], /*write=*/false);
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

bool unbind_resource_from_srvs_locked(AeroGpuDevice* dev,
                                      D3D10DDI_HDEVICE hDevice,
                                      aerogpu_handle_t resource_handle,
                                      const AeroGpuResource* res) {
  if (!dev || (resource_handle == 0 && !res)) {
    return true;
  }

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if ((resource_handle != 0 && dev->vs_srvs[slot] == resource_handle) ||
        (res && ResourcesAlias(dev->vs_srv_resources[slot], res))) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, 0)) {
        return false;
      }
      dev->vs_srvs[slot] = 0;
      dev->vs_srv_resources[slot] = nullptr;
    }
    if ((resource_handle != 0 && dev->ps_srvs[slot] == resource_handle) ||
        (res && ResourcesAlias(dev->ps_srv_resources[slot], res))) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, 0)) {
        return false;
      }
      dev->ps_srvs[slot] = 0;
      dev->ps_srv_resources[slot] = nullptr;
    }
  }
  return true;
}

// Best-effort variant used by DestroyResource.
//
// DestroyResource must guarantee that the UMD's cached state does not retain
// dangling pointers after the resource is freed, even if command emission fails
// (OOM). Therefore, this helper clears the SRV caches regardless of append
// success while still trying to emit the corresponding unbind packets.
static void UnbindResourceFromSrvsBestEffortLocked(AeroGpuDevice* dev,
                                                   D3D10DDI_HDEVICE hDevice,
                                                   aerogpu_handle_t resource_handle,
                                                   const AeroGpuResource* res) {
  if (!dev || (resource_handle == 0 && !res)) {
    return;
  }

  bool oom = false;
  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    if ((resource_handle != 0 && dev->vs_srvs[slot] == resource_handle) ||
        (res && ResourcesAlias(dev->vs_srv_resources[slot], res))) {
      if (!oom && !set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, 0)) {
        oom = true;
      }
      dev->vs_srvs[slot] = 0;
      dev->vs_srv_resources[slot] = nullptr;
    }
    if ((resource_handle != 0 && dev->ps_srvs[slot] == resource_handle) ||
        (res && ResourcesAlias(dev->ps_srv_resources[slot], res))) {
      if (!oom && !set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, 0)) {
        oom = true;
      }
      dev->ps_srvs[slot] = 0;
      dev->ps_srv_resources[slot] = nullptr;
    }
  }
}

bool unbind_resource_from_srvs_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, const AeroGpuResource* res) {
  return unbind_resource_from_srvs_locked(dev, hDevice, /*resource_handle=*/0, res);
}

bool unbind_resource_from_srvs_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, aerogpu_handle_t resource_handle) {
  return unbind_resource_from_srvs_locked(dev, hDevice, resource_handle, nullptr);
}

bool emit_set_render_targets_locked(AeroGpuDevice* dev);

// Emits an AEROGPU_CMD_SET_RENDER_TARGETS packet using the provided view handles.
// Returns false if the command could not be appended.
static bool EmitSetRenderTargetsCmdLocked(AeroGpuDevice* dev,
                                         uint32_t color_count,
                                         const aerogpu_handle_t rtvs[AEROGPU_MAX_RENDER_TARGETS],
                                         aerogpu_handle_t dsv) {
  return aerogpu::d3d10_11::EmitSetRenderTargetsCmdLocked(dev,
                                                         color_count,
                                                         rtvs,
                                                         dsv,
                                                         [](HRESULT) {});
}

bool unbind_resource_from_outputs_locked(AeroGpuDevice* dev,
                                         D3D10DDI_HDEVICE hDevice,
                                         aerogpu_handle_t resource_handle,
                                         const AeroGpuResource* res) {
  return aerogpu::d3d10_11::UnbindResourceFromOutputsLocked(dev,
                                                           resource_handle,
                                                           res,
                                                           [&](HRESULT hr) {
                                                             ReportDeviceErrorLocked(dev, hDevice, hr);
                                                           });
}

bool unbind_resource_from_outputs_locked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, const AeroGpuResource* res) {
  return unbind_resource_from_outputs_locked(dev, hDevice, /*resource_handle=*/0, res);
}

bool set_render_targets_locked(AeroGpuDevice* dev,
                               D3D10DDI_HDEVICE hDevice,
                               uint32_t rtv_count,
                               const AeroGpuRenderTargetView* const* rtvs,
                               const AeroGpuDepthStencilView* dsv) {
  if (!dev) {
    return false;
  }

  const uint32_t count = std::min<uint32_t>(rtv_count, AEROGPU_MAX_RENDER_TARGETS);

  aerogpu_handle_t new_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* new_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < count; ++i) {
    const AeroGpuRenderTargetView* view = rtvs ? rtvs[i] : nullptr;
    AeroGpuResource* res = view ? view->resource : nullptr;
    new_rtv_resources[i] = res;
    new_rtvs[i] = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
  }

  const aerogpu_handle_t dsv_handle =
      dsv ? (dsv->texture ? dsv->texture : (dsv->resource ? dsv->resource->handle : 0)) : 0;
  AeroGpuResource* dsv_res = dsv ? dsv->resource : nullptr;

  // D3D10/11 hazard rule: resources bound for output cannot simultaneously be bound as SRVs.
  for (uint32_t i = 0; i < count; ++i) {
    const AeroGpuResource* res = new_rtv_resources[i];
    if (!res) {
      continue;
    }
    // Avoid redundant scans when the same resource appears multiple times.
    bool seen = false;
    for (uint32_t j = 0; j < i; ++j) {
      if (new_rtv_resources[j] == res) {
        seen = true;
        break;
      }
    }
    if (seen) {
      continue;
    }
    if (!unbind_resource_from_srvs_locked(dev, hDevice, new_rtvs[i], res)) {
      return false;
    }
  }
  if (dsv_res && dsv_handle) {
    bool dsv_seen = false;
    for (uint32_t i = 0; i < count; ++i) {
      if (new_rtv_resources[i] == dsv_res) {
        dsv_seen = true;
        break;
      }
    }
    if (!dsv_seen && !unbind_resource_from_srvs_locked(dev, hDevice, dsv_handle, dsv_res)) {
      return false;
    }
  }

  const uint32_t prev_count = dev->current_rtv_count;
  aerogpu_handle_t prev_rtvs[AEROGPU_MAX_RENDER_TARGETS];
  AeroGpuResource* prev_rtv_resources[AEROGPU_MAX_RENDER_TARGETS];
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    prev_rtvs[i] = dev->current_rtvs[i];
    prev_rtv_resources[i] = dev->current_rtv_resources[i];
  }
  const aerogpu_handle_t prev_dsv = dev->current_dsv;
  AeroGpuResource* prev_dsv_res = dev->current_dsv_res;

  dev->current_rtv_count = count;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (i < count) {
      dev->current_rtvs[i] = new_rtvs[i];
      dev->current_rtv_resources[i] = new_rtv_resources[i];
    } else {
      dev->current_rtvs[i] = 0;
      dev->current_rtv_resources[i] = nullptr;
    }
  }
  dev->current_dsv = dsv_handle;
  dev->current_dsv_res = dsv_res;

  if (!emit_set_render_targets_locked(dev)) {
    dev->current_rtv_count = prev_count;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      dev->current_rtvs[i] = prev_rtvs[i];
      dev->current_rtv_resources[i] = prev_rtv_resources[i];
    }
    dev->current_dsv = prev_dsv;
    dev->current_dsv_res = prev_dsv_res;
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }
 
  // Render target / depth-stencil bindings are written by Draw/Clear.
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    track_resource_alloc_for_submit_locked(dev, dev->current_rtv_resources[i], /*write=*/true);
  }
  track_resource_alloc_for_submit_locked(dev, dsv_res, /*write=*/true);
  return true;
}

uint64_t submit_locked(AeroGpuDevice* dev, HRESULT* out_hr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev) {
    return 0;
  }
  if (dev->referenced_alloc_list_oom) {
    // Submitting with an incomplete allocation list is unsafe for guest-backed
    // resources because the host may not be able to resolve backing_alloc_id
    // references.
    if (out_hr) {
      *out_hr = E_OUTOFMEMORY;
    }
    dev->pending_staging_writes.clear();
    dev->referenced_allocs.clear();
    dev->cmd.reset();
    dev->referenced_alloc_list_oom = false;
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->referenced_allocs.clear();
    dev->referenced_alloc_list_oom = false;
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
    if (dev->referenced_alloc_list_oom) {
      if (out_hr) {
        *out_hr = E_OUTOFMEMORY;
      }
      dev->cmd.reset();
      dev->referenced_allocs.clear();
      dev->pending_staging_writes.clear();
      dev->referenced_alloc_list_oom = false;
      return 0;
    }

    const auto* cb = dev->device_callbacks;
    const AEROGPU_WDDM_SUBMIT_ALLOCATION* allocs = dev->referenced_allocs.empty() ? nullptr : dev->referenced_allocs.data();
    const uint32_t alloc_count = static_cast<uint32_t>(dev->referenced_allocs.size());

    uint64_t fence = 0;
    const HRESULT hr = cb->pfnSubmitCmdStream(cb->pUserContext,
                                              dev->cmd.data(),
                                              static_cast<uint32_t>(dev->cmd.size()),
                                              allocs,
                                               alloc_count,
                                               &fence);
    dev->referenced_allocs.clear();
    dev->referenced_alloc_list_oom = false;

    if (FAILED(hr)) {
      if (out_hr) {
        *out_hr = hr;
      }
      dev->cmd.reset();
      dev->pending_staging_writes.clear();
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

    for (AeroGpuResource* res : dev->pending_staging_writes) {
      if (res) {
        res->last_gpu_write_fence = fence;
      }
    }
    dev->pending_staging_writes.clear();

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

  for (AeroGpuResource* res : dev->pending_staging_writes) {
    if (res) {
      res->last_gpu_write_fence = fence;
    }
  }
  dev->pending_staging_writes.clear();

  dev->referenced_allocs.clear();
  dev->referenced_alloc_list_oom = false;
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
  return aerogpu::d3d10_11::EmitSetRenderTargetsLocked(dev, [](HRESULT) {});
}

// -------------------------------------------------------------------------------------------------
// Device DDI (plain functions to ensure the correct calling convention)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyDevice hDevice=%p", hDevice.pDrvPrivate);
  // Be robust to runtimes that destroy a device handle even when CreateDevice
  // failed or DestroyDevice is called twice.
  if (!HasLiveCookie(hDevice.pDrvPrivate, kDeviceDestroyLiveCookie)) {
    return;
  }
  const uint32_t cleared = 0;
  std::memcpy(hDevice.pDrvPrivate, &cleared, sizeof(cleared));

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERESOURCE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateResourceSize");
  return sizeof(AeroGpuResource);
}

HRESULT FailCreateResource(AeroGpuResource* res, HRESULT hr) {
  if (!res) {
    return hr;
  }
  // CreateResource can fail after constructing the private object (e.g. invalid
  // initial-data pointers). Some runtimes may still call DestroyResource on
  // failure, so ensure the private memory always ends in a constructed, default
  // state (with handle == 0) to avoid double-destroy of std::vector fields.
  res->~AeroGpuResource();
  new (res) AeroGpuResource();
  return hr;
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
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  if (!pDesc) {
    // Some runtimes may still call DestroyResource on failure; ensure the
    // private memory always ends in a safe, default-constructed state
    // (with handle == 0).
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    AEROGPU_D3D10_RET_HR(FailCreateResource(res, E_INVALIDARG));
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    AEROGPU_D3D10_RET_HR(FailCreateResource(res, E_FAIL));
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

#if defined(AEROGPU_UMD_TRACE_RESOURCES)
  trace_create_resource_desc(pDesc);
#endif

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = AllocateGlobalHandle(dev->adapter);
    res->kind = ResourceKind::Buffer;
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;
    const uint64_t padded_size_bytes = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);

    if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      return FailCreateResource(res, E_OUTOFMEMORY);
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
        return FailCreateResource(res, FAILED(hr) ? hr : E_FAIL);
      }

      res->alloc_handle = alloc_handle;
      res->backing_alloc_id = static_cast<uint32_t>(alloc_handle);
      res->alloc_offset_bytes = 0;
      res->alloc_size_bytes = alloc_size_bytes ? alloc_size_bytes : padded_size_bytes;
      if (alloc_size_bytes != 0 && alloc_size_bytes < padded_size_bytes) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
      // Resource creation references the backing allocation so the host can build
      // its `alloc_id -> GPA` table. Creation itself does not imply GPU writes.
      track_alloc_for_submit_locked(dev, alloc_handle, /*write=*/false);
    } else {
      if (padded_size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
      try {
        res->storage.resize(static_cast<size_t>(padded_size_bytes));
      } catch (...) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
    }

    const bool has_initial_data = (pDesc->pInitialData && pDesc->InitialDataCount);
    const bool is_guest_backed = (res->backing_alloc_id != 0);
    bool wddm_initial_upload = false;
    if (has_initial_data) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem || res->size_bytes == 0) {
        return FailCreateResource(res, E_INVALIDARG);
      }


      if (!res->storage.empty() && res->storage.size() >= static_cast<size_t>(res->size_bytes)) {
        std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
      }

      if (!wddm_initial_upload && res->alloc_handle != 0) {
        void* cpu_ptr = nullptr;
        HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &cpu_ptr);
        if (FAILED(hr) || !cpu_ptr) {
          return FailCreateResource(res, FAILED(hr) ? hr : E_FAIL);
        }
        std::memcpy(static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes,
                    init.pSysMem,
                    static_cast<size_t>(res->size_bytes));
        if (padded_size_bytes > res->size_bytes) {
          std::memset(static_cast<uint8_t*>(cpu_ptr) + res->alloc_offset_bytes + static_cast<size_t>(res->size_bytes),
                      0,
                      static_cast<size_t>(padded_size_bytes - res->size_bytes));
        }
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        wddm_initial_upload = true;
      }
    }

    if (!AddLiveResourceLocked(dev, res)) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return FailCreateResource(res, E_OUTOFMEMORY);
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      RemoveLiveResourceLocked(dev, res);
      return FailCreateResource(res, E_OUTOFMEMORY);
    }
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_buffer_usage_flags(res->bind_flags);
    cmd->size_bytes = padded_size_bytes;
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
          RemoveLiveResourceLocked(dev, res);
          return FailCreateResource(res, E_FAIL);
        }

        auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!dirty) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          dirty->resource_handle = res->handle;
          dirty->reserved0 = 0;
          dirty->offset_bytes = 0;
          dirty->size_bytes = res->size_bytes;
          // Guest-backed resources are updated via RESOURCE_DIRTY_RANGE; the host
          // reads guest memory to upload it.
          track_resource_alloc_for_submit_locked(dev, res, /*write=*/false);
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

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    const bool is_shared = (pDesc->MiscFlags & kD3D11ResourceMiscShared) != 0;
    const uint32_t requested_mip_levels = pDesc->MipLevels;
    const uint32_t mip_levels =
        requested_mip_levels ? requested_mip_levels : aerogpu::d3d10_11::CalcFullMipLevels(pDesc->Width, pDesc->Height);
    const uint32_t array_size = pDesc->ArraySize ? pDesc->ArraySize : 1u;
    if (is_shared && (mip_levels != 1 || array_size != 1)) {
      // MVP: shared surfaces are single-allocation only.
      auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
      return FailCreateResource(res, E_NOTIMPL);
    }

    // Keep CreateResource format handling in sync with the WDK UMDs:
    // - sRGB formats are mapped to UNORM on older host ABIs.
    // - BC formats are rejected when the host ABI does not support them.
    if (!aerogpu::d3d10_11::AerogpuSupportsDxgiFormatCompat(
            dev,
            pDesc->Format,
            aerogpu::d3d10_11::AerogpuFormatUsage::Texture2D)) {
      auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
      AEROGPU_D3D10_RET_HR(FailCreateResource(res, E_NOTIMPL));
    }

    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, pDesc->Format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
      AEROGPU_D3D10_RET_HR(FailCreateResource(res, E_NOTIMPL));
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = AllocateGlobalHandle(dev->adapter);
    res->kind = ResourceKind::Texture2D;
    res->usage = pDesc->Usage;
    res->cpu_access_flags = pDesc->CPUAccessFlags;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = mip_levels;
    res->array_size = array_size;
    res->dxgi_format = pDesc->Format;
    if (!res->width || !res->height || !res->mip_levels || !res->array_size) {
      return FailCreateResource(res, E_INVALIDARG);
    }
    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
    if (!min_row_bytes) {
      return FailCreateResource(res, E_OUTOFMEMORY);
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
        return FailCreateResource(res, FAILED(hr) ? hr : E_FAIL);
      }

      if (row_pitch_bytes) {
        res->row_pitch_bytes = row_pitch_bytes;
      }
      if (res->row_pitch_bytes < min_row_bytes) {
        return FailCreateResource(res, E_INVALIDARG);
      }

      res->alloc_handle = alloc_handle;
      res->backing_alloc_id = static_cast<uint32_t>(alloc_handle);
      res->alloc_offset_bytes = 0;
      res->alloc_size_bytes = alloc_size_bytes;
      // Resource creation references the backing allocation so the host can build
      // its `alloc_id -> GPA` table. Creation itself does not imply GPU writes.
      track_alloc_for_submit_locked(dev, alloc_handle, /*write=*/false);
    }

    uint64_t total_bytes = 0;
    if (!compute_texture2d_total_bytes(aer_fmt,
                                       res->width,
                                       res->height,
                                       res->mip_levels,
                                       res->array_size,
                                       res->row_pitch_bytes,
                                       &total_bytes)) {
      return FailCreateResource(res, E_OUTOFMEMORY);
    }

    if (res->alloc_handle != 0) {
      if (res->alloc_size_bytes == 0) {
        res->alloc_size_bytes = total_bytes;
      } else if (total_bytes > res->alloc_size_bytes) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
    } else {
      if (total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
      try {
        res->storage.resize(static_cast<size_t>(total_bytes));
      } catch (...) {
        return FailCreateResource(res, E_OUTOFMEMORY);
      }
    }

    const bool has_initial_data = (pDesc->pInitialData && pDesc->InitialDataCount);
    const bool is_guest_backed = (res->backing_alloc_id != 0);
    bool wddm_initial_upload = false;
    if (has_initial_data) {
      uint8_t* dst = res->storage.empty() ? nullptr : res->storage.data();
      void* mapped = nullptr;
      if (!wddm_initial_upload && res->alloc_handle != 0) {
        HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, res->alloc_handle, &mapped);
        if (FAILED(hr) || !mapped) {
          return FailCreateResource(res, FAILED(hr) ? hr : E_FAIL);
        }
        dst = static_cast<uint8_t*>(mapped) + res->alloc_offset_bytes;
      }
      if (!dst) {
        return FailCreateResource(res, E_FAIL);
      }

      const uint64_t subresource_count_u64 =
          static_cast<uint64_t>(res->mip_levels) * static_cast<uint64_t>(res->array_size);
      if (subresource_count_u64 == 0 || subresource_count_u64 > static_cast<uint64_t>(UINT32_MAX) ||
          pDesc->InitialDataCount != subresource_count_u64) {
        if (mapped) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        }
        return FailCreateResource(res, E_INVALIDARG);
      }

      for (uint32_t subresource = 0; subresource < static_cast<uint32_t>(subresource_count_u64); subresource++) {
        const auto& init = pDesc->pInitialData[subresource];
        if (!init.pSysMem) {
          if (mapped) {
            cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          }
          return FailCreateResource(res, E_INVALIDARG);
        }

        Texture2DSubresourceLayout layout{};
        if (!compute_texture2d_subresource_layout(aer_fmt,
                                                  res->width,
                                                  res->height,
                                                  res->mip_levels,
                                                  res->array_size,
                                                  res->row_pitch_bytes,
                                                  subresource,
                                                  &layout) ||
            layout.offset_bytes + layout.size_bytes > total_bytes) {
          if (mapped) {
            cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          }
          return FailCreateResource(res, E_INVALIDARG);
        }

        const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, layout.width);
        if (!row_bytes || row_bytes > layout.row_pitch_bytes) {
          if (mapped) {
            cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          }
          return FailCreateResource(res, E_INVALIDARG);
        }

        const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch) : static_cast<size_t>(row_bytes);
        if (static_cast<size_t>(row_bytes) > src_pitch) {
          if (mapped) {
            cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          }
          return FailCreateResource(res, E_INVALIDARG);
        }

        const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
        uint8_t* sub_dst = dst + static_cast<size_t>(layout.offset_bytes);
        for (uint32_t y = 0; y < layout.rows_in_layout; y++) {
          uint8_t* dst_row = sub_dst + static_cast<size_t>(y) * layout.row_pitch_bytes;
          std::memcpy(dst_row, src + static_cast<size_t>(y) * src_pitch, static_cast<size_t>(row_bytes));
          if (layout.row_pitch_bytes > row_bytes) {
            std::memset(dst_row + static_cast<size_t>(row_bytes), 0, layout.row_pitch_bytes - row_bytes);
          }
        }
      }
      if (mapped) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        wddm_initial_upload = true;
      }
    }

    if (!AddLiveResourceLocked(dev, res)) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return FailCreateResource(res, E_OUTOFMEMORY);
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!cmd) {
      RemoveLiveResourceLocked(dev, res);
      return FailCreateResource(res, E_OUTOFMEMORY);
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
      const uint64_t dirty_size = total_bytes;
      if (is_guest_backed) {
        if (!wddm_initial_upload) {
          RemoveLiveResourceLocked(dev, res);
          return FailCreateResource(res, E_FAIL);
        }

        auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
        if (!dirty) {
          dev->last_error = E_OUTOFMEMORY;
        } else {
          dirty->resource_handle = res->handle;
          dirty->reserved0 = 0;
          dirty->offset_bytes = 0;
          dirty->size_bytes = dirty_size;
          // Guest-backed resources are updated via RESOURCE_DIRTY_RANGE; the host
          // reads guest memory to upload it.
          track_resource_alloc_for_submit_locked(dev, res, /*write=*/false);
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

  auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
  AEROGPU_D3D10_RET_HR(FailCreateResource(res, E_NOTIMPL));
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyResource hDevice=%p hResource=%p", hDevice.pDrvPrivate, hResource.pDrvPrivate);
  if (!hResource.pDrvPrivate) {
    return;
  }

  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!res) {
    return;
  }

  // Be robust to runtimes that destroy child objects after DestroyDevice has
  // already run (device memory still allocated but no longer contains a live
  // mutex/cmd writer).
  if (!IsDeviceLive(hDevice)) {
    res->handle = kInvalidHandle;
    res->~AeroGpuResource();
    new (res) AeroGpuResource();
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    res->handle = kInvalidHandle;
    res->~AeroGpuResource();
    new (res) AeroGpuResource();
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle == kInvalidHandle) {
    return;
  }

  if (!dev->pending_staging_writes.empty()) {
    dev->pending_staging_writes.erase(
        std::remove(dev->pending_staging_writes.begin(), dev->pending_staging_writes.end(), res),
        dev->pending_staging_writes.end());
  }

  // Ensure the device state does not retain dangling pointers after the
  // resource is destroyed (portable build does not have runtime-managed
  // refcounting to prevent this).
  const aerogpu_handle_t handle = res->handle;
  bool rt_changed = false;
  for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (dev->current_rtv_resources[i] == res || dev->current_rtvs[i] == handle) {
      dev->current_rtv_resources[i] = nullptr;
      dev->current_rtvs[i] = 0;
      rt_changed = true;
    }
  }
  if (dev->current_dsv_res == res || dev->current_dsv == handle) {
    dev->current_dsv_res = nullptr;
    dev->current_dsv = 0;
    rt_changed = true;
  }
  if (rt_changed && !emit_set_render_targets_locked(dev)) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
  }
  if (handle) {
    UnbindResourceFromSrvsBestEffortLocked(dev, hDevice, handle, res);
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
  new (res) AeroGpuResource();
}

uint64_t resource_total_bytes(const AeroGpuResource* res) {
  if (!res) {
    return 0;
  }
  if (res->kind == ResourceKind::Buffer) {
    return res->size_bytes;
  }
  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu(res->dxgi_format);
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

  const bool do_not_wait = (map_flags & kD3D11MapFlagDoNotWait) != 0;

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
  if ((map_flags & ~static_cast<uint32_t>(kD3D11MapFlagDoNotWait)) != 0) {
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

  // Staging readback maps are synchronization points. Submit pending work and
  // then wait for the fence that last wrote this resource, instead of waiting
  // for the device's latest fence (which may include unrelated work).
  if (want_read) {
    const bool do_not_wait = (map_flags & kD3D11MapFlagDoNotWait) != 0;
    HRESULT submit_hr = S_OK;
    (void)submit_locked(dev, &submit_hr);
    if (FAILED(submit_hr)) {
      return submit_hr;
    }
    const uint64_t fence = res->last_gpu_write_fence;
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

  Texture2DSubresourceLayout tex_layout{};
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;
  uint32_t mapped_row_pitch = 0;
  uint32_t mapped_depth_pitch = 0;

  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_INVALIDARG;
    }
    if (!compute_texture2d_subresource_layout(aer_fmt,
                                              res->width,
                                              res->height,
                                              res->mip_levels,
                                              res->array_size,
                                              res->row_pitch_bytes,
                                              subresource,
                                              &tex_layout)) {
      return E_INVALIDARG;
    }
    mapped_offset = tex_layout.offset_bytes;
    mapped_size = tex_layout.size_bytes;
    mapped_row_pitch = tex_layout.row_pitch_bytes;
    if (mapped_size > static_cast<uint64_t>(UINT32_MAX)) {
      return E_INVALIDARG;
    }
    mapped_depth_pitch = static_cast<uint32_t>(mapped_size);
  } else {
    if (subresource != 0) {
      return E_INVALIDARG;
    }
    const uint64_t total = resource_total_bytes(res);
    if (!total) {
      return E_INVALIDARG;
    }
    mapped_offset = 0;
    mapped_size = total;
    mapped_row_pitch = 0;
    mapped_depth_pitch = 0;
  }

  const uint64_t total = resource_total_bytes(res);
  if (!total) {
    return E_INVALIDARG;
  }
  if (mapped_offset > total || mapped_size > total - mapped_offset) {
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
    if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD && mapped_size <= static_cast<uint64_t>(SIZE_MAX)) {
      // Discard contents are undefined; clear for deterministic tests.
      uint64_t clear_size = mapped_size;
      if (res->kind == ResourceKind::Buffer) {
        clear_size = AlignUpU64(mapped_offset + mapped_size, 4) - AlignDownU64(mapped_offset, 4);
      }
      if (clear_size <= static_cast<uint64_t>(SIZE_MAX)) {
        std::memset(data + static_cast<size_t>(mapped_offset), 0, static_cast<size_t>(clear_size));
      }
    }

    pMapped->pData = data + static_cast<size_t>(mapped_offset);
    pMapped->RowPitch = mapped_row_pitch;
    pMapped->DepthPitch = mapped_depth_pitch;

    res->mapped = true;
    res->mapped_write = want_write;
    res->mapped_subresource = subresource;
    res->mapped_map_type = map_type;
    res->mapped_offset_bytes = mapped_offset;
    res->mapped_size_bytes = mapped_size;
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

  pMapped->pData = res->storage.data() + static_cast<size_t>(mapped_offset);
  pMapped->RowPitch = mapped_row_pitch;
  pMapped->DepthPitch = mapped_depth_pitch;

  res->mapped = true;
  res->mapped_write = want_write;
  res->mapped_subresource = subresource;
  res->mapped_map_type = map_type;
  res->mapped_offset_bytes = mapped_offset;
  res->mapped_size_bytes = mapped_size;
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
        // Guest-backed resources are updated via RESOURCE_DIRTY_RANGE; the host
        // reads guest memory to upload it.
        track_resource_alloc_for_submit_locked(dev, res, /*write=*/false);
      }
    } else {
      // Host-owned resource: inline the bytes into the command stream.
      uint64_t upload_offset_bytes = res->mapped_offset_bytes;
      uint64_t upload_size_bytes = res->mapped_size_bytes;
      if (res->kind == ResourceKind::Buffer) {
        const uint64_t end_bytes = res->mapped_offset_bytes + res->mapped_size_bytes;
        upload_offset_bytes = AlignDownU64(res->mapped_offset_bytes, 4);
        upload_size_bytes = AlignUpU64(end_bytes, 4) - upload_offset_bytes;
      }
      if (upload_offset_bytes + upload_size_bytes <= static_cast<uint64_t>(res->storage.size())) {
        const auto offset = static_cast<size_t>(upload_offset_bytes);
        const auto size = static_cast<size_t>(upload_size_bytes);
        auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
            AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + offset, size);
        if (!upload) {
          ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        } else {
          upload->resource_handle = res->handle;
          upload->reserved0 = 0;
          upload->offset_bytes = upload_offset_bytes;
          upload->size_bytes = upload_size_bytes;
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
  const uint64_t padded_total = AlignUpU64(total ? total : 1, 4);
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
    if (discard && padded_total <= static_cast<uint64_t>(SIZE_MAX)) {
      // Discard contents are undefined; clear for deterministic tests.
      std::memset(data, 0, static_cast<size_t>(padded_total));
    }
    *ppData = data;
  } else {
    HRESULT hr = ensure_resource_storage(res, padded_total);
    if (FAILED(hr)) {
      return hr;
    }

    if (discard) {
      // Approximate DISCARD renaming by allocating a fresh CPU backing store.
      try {
        res->storage.assign(static_cast<size_t>(padded_total), 0);
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
  if ((map_flags & ~static_cast<uint32_t>(kD3D11MapFlagDoNotWait)) != 0) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (map_type == AEROGPU_DDI_MAP_WRITE_DISCARD) {
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
      if (subresource != 0) {
        return E_INVALIDARG;
      }
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
      if (subresource != 0) {
        return E_INVALIDARG;
      }
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
    if (res->bind_flags & (kD3D11BindVertexBuffer | kD3D11BindIndexBuffer)) {
      if (subresource != 0) {
        return E_INVALIDARG;
      }
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

  if (res->kind == ResourceKind::Texture2D) {
    return map_resource_locked(dev, res, subresource, map_type, map_flags, pMapped);
  }

  // Conservative: only support generic map on buffers and textures for now.
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
    uint64_t dirty_offset_bytes = 0;
    uint64_t dirty_size_bytes = resource_total_bytes(res);
    if (!dirty_size_bytes) {
      cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
      return;
    }

    if (res->kind == ResourceKind::Buffer) {
      if (dst_subresource != 0) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
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
      dirty_offset_bytes = 0;
      dirty_size_bytes = res->size_bytes;
    } else if (res->kind == ResourceKind::Texture2D) {
      const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      if (aer_fmt == AEROGPU_FORMAT_INVALID) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aer_fmt);
      Texture2DSubresourceLayout sub_layout{};
      if (!fmt_layout.valid ||
          !compute_texture2d_subresource_layout(aer_fmt,
                                                res->width,
                                                res->height,
                                                res->mip_levels,
                                                res->array_size,
                                                res->row_pitch_bytes,
                                                dst_subresource,
                                                &sub_layout)) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, sub_layout.width);
      if (min_row_bytes == 0 || sub_layout.row_pitch_bytes < min_row_bytes) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      if (sub_layout.offset_bytes > dirty_size_bytes || sub_layout.size_bytes > dirty_size_bytes - sub_layout.offset_bytes) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      dirty_offset_bytes = sub_layout.offset_bytes;
      dirty_size_bytes = sub_layout.size_bytes;

      uint8_t* tex_dst = dst + static_cast<size_t>(dirty_offset_bytes);
      const uint32_t sub_w = sub_layout.width;
      const uint32_t sub_h = sub_layout.height;
      const uint32_t sub_row_pitch = sub_layout.row_pitch_bytes;

      uint32_t copy_left = 0;
      uint32_t copy_top = 0;
      uint32_t copy_right = sub_w;
      uint32_t copy_bottom = sub_h;
      if (pDstBox) {
        if (pDstBox->front != 0 || pDstBox->back != 1) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        if (pDstBox->left >= pDstBox->right || pDstBox->top >= pDstBox->bottom) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        if (pDstBox->right > sub_w || pDstBox->bottom > sub_h) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
        copy_left = pDstBox->left;
        copy_top = pDstBox->top;
        copy_right = pDstBox->right;
        copy_bottom = pDstBox->bottom;
      }

      if (fmt_layout.block_width > 1 || fmt_layout.block_height > 1) {
        const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
          return (v % align) == 0 || v == extent;
        };
        if ((copy_left % fmt_layout.block_width) != 0 || (copy_top % fmt_layout.block_height) != 0 ||
            !aligned_or_edge(copy_right, fmt_layout.block_width, sub_w) ||
            !aligned_or_edge(copy_bottom, fmt_layout.block_height, sub_h)) {
          cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
          return;
        }
      }

      const uint32_t block_left = copy_left / fmt_layout.block_width;
      const uint32_t block_top = copy_top / fmt_layout.block_height;
      const uint32_t block_right = aerogpu_div_round_up_u32(copy_right, fmt_layout.block_width);
      const uint32_t block_bottom = aerogpu_div_round_up_u32(copy_bottom, fmt_layout.block_height);
      if (block_right < block_left || block_bottom < block_top) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }

      const uint32_t copy_width_blocks = block_right - block_left;
      const uint32_t copy_height_blocks = block_bottom - block_top;
      const uint64_t row_bytes_u64 =
          static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
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
          static_cast<uint64_t>(block_left) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
      if (dst_x_bytes_u64 > static_cast<uint64_t>(sub_row_pitch) ||
          static_cast<uint64_t>(sub_row_pitch) - dst_x_bytes_u64 < static_cast<uint64_t>(row_bytes)) {
        cb->pfnUnmapAllocation(cb->pUserContext, res->alloc_handle);
        return;
      }
      const size_t dst_x_bytes = static_cast<size_t>(dst_x_bytes_u64);
      for (uint32_t y = 0; y < copy_height_blocks; y++) {
        uint8_t* dst_row = tex_dst + (static_cast<size_t>(block_top) + y) * sub_row_pitch + dst_x_bytes;
        std::memcpy(dst_row, src + y * src_pitch, row_bytes);
      }

      // If this is a full upload, also clear any per-row padding to keep guest
      // memory deterministic for host-side uploads.
      if (!pDstBox && sub_row_pitch > row_bytes) {
        const uint32_t total_rows = aerogpu_texture_num_rows(aer_fmt, sub_h);
        for (uint32_t y = 0; y < total_rows; y++) {
          uint8_t* dst_row = tex_dst + static_cast<size_t>(y) * sub_row_pitch;
          std::memset(dst_row + row_bytes, 0, sub_row_pitch - row_bytes);
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
    dirty->offset_bytes = dirty_offset_bytes;
    dirty->size_bytes = dirty_size_bytes;
    // Guest-backed resources are updated via RESOURCE_DIRTY_RANGE; the host reads
    // guest memory to upload it.
    track_resource_alloc_for_submit_locked(dev, res, /*write=*/false);
    return;
  }

  // Host-owned resources: inline data into the command stream.
  if (!pDstBox) {
    if (res->kind == ResourceKind::Buffer) {
      if (dst_subresource != 0) {
        return;
      }
      if (res->size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        return;
      }
      const uint64_t padded_total = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
      if (padded_total > static_cast<uint64_t>(SIZE_MAX)) {
        return;
      }
      HRESULT hr = ensure_resource_storage(res, padded_total);
      if (FAILED(hr) || res->storage.size() < static_cast<size_t>(padded_total)) {
        return;
      }
      std::memcpy(res->storage.data(), pSysMem, static_cast<size_t>(res->size_bytes));
      if (padded_total > res->size_bytes) {
        std::memset(res->storage.data() + static_cast<size_t>(res->size_bytes),
                    0,
                    static_cast<size_t>(padded_total - res->size_bytes));
      }

      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data(), static_cast<size_t>(padded_total));
      if (!upload) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        return;
      }
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = 0;
      upload->size_bytes = padded_total;
      return;
    }

    if (res->kind == ResourceKind::Texture2D) {
      const uint32_t aerogpu_format = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
      const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aerogpu_format);
      Texture2DSubresourceLayout sub_layout{};
      if (!fmt_layout.valid ||
          !compute_texture2d_subresource_layout(aerogpu_format,
                                                res->width,
                                                res->height,
                                                res->mip_levels,
                                                res->array_size,
                                                res->row_pitch_bytes,
                                                dst_subresource,
                                                &sub_layout)) {
        return;
      }

      const uint32_t row_bytes = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, sub_layout.width);
      const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, sub_layout.height);
      const size_t src_pitch = SysMemPitch ? static_cast<size_t>(SysMemPitch) : static_cast<size_t>(row_bytes);
      if (row_bytes == 0 || rows == 0 || static_cast<size_t>(row_bytes) > src_pitch ||
          row_bytes > sub_layout.row_pitch_bytes) {
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
      uint8_t* dst_base = res->storage.data() + static_cast<size_t>(sub_layout.offset_bytes);
      for (uint32_t y = 0; y < rows; y++) {
        uint8_t* dst_row = dst_base + static_cast<size_t>(y) * sub_layout.row_pitch_bytes;
        std::memcpy(dst_row, src + static_cast<size_t>(y) * src_pitch, static_cast<size_t>(row_bytes));
        if (sub_layout.row_pitch_bytes > row_bytes) {
          std::memset(dst_row + static_cast<size_t>(row_bytes), 0, sub_layout.row_pitch_bytes - row_bytes);
        }
      }

      auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
          AEROGPU_CMD_UPLOAD_RESOURCE, dst_base, static_cast<size_t>(sub_layout.size_bytes));
      if (!upload) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
        return;
      }
      upload->resource_handle = res->handle;
      upload->reserved0 = 0;
      upload->offset_bytes = sub_layout.offset_bytes;
      upload->size_bytes = sub_layout.size_bytes;
      return;
    }

    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    if (dst_subresource != 0) {
      return;
    }
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

    const uint64_t padded_total = AlignUpU64(res->size_bytes ? res->size_bytes : 1, 4);
    const uint64_t upload_offset = AlignDownU64(offset, 4);
    const uint64_t upload_end = AlignUpU64(offset + size, 4);
    const uint64_t upload_size = upload_end - upload_offset;
    if (padded_total > static_cast<uint64_t>(SIZE_MAX) || upload_offset > padded_total || upload_end > padded_total ||
        upload_size > static_cast<uint64_t>(SIZE_MAX)) {
      return;
    }

    HRESULT hr = ensure_resource_storage(res, padded_total);
    if (FAILED(hr) || res->storage.size() < static_cast<size_t>(padded_total)) {
      return;
    }
    std::memcpy(res->storage.data() + static_cast<size_t>(offset), pSysMem, static_cast<size_t>(size));

    auto* upload = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
        AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + static_cast<size_t>(upload_offset), static_cast<size_t>(upload_size));
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

  if (res->kind == ResourceKind::Texture2D) {
    const uint32_t aerogpu_format = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aerogpu_format);
    Texture2DSubresourceLayout sub_layout{};
    if (!fmt_layout.valid ||
        !compute_texture2d_subresource_layout(aerogpu_format,
                                              res->width,
                                              res->height,
                                              res->mip_levels,
                                              res->array_size,
                                              res->row_pitch_bytes,
                                              dst_subresource,
                                              &sub_layout)) {
      return;
    }
    const uint32_t sub_w = sub_layout.width;
    const uint32_t sub_h = sub_layout.height;
    const uint32_t sub_row_pitch = sub_layout.row_pitch_bytes;

    if (pDstBox->front != 0 || pDstBox->back != 1) {
      return;
    }
    if (pDstBox->left >= pDstBox->right || pDstBox->top >= pDstBox->bottom) {
      return;
    }
    if (pDstBox->right > sub_w || pDstBox->bottom > sub_h) {
      return;
    }

    const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, sub_w);
    if (min_row_bytes == 0 || sub_row_pitch < min_row_bytes) {
      return;
    }

    if (fmt_layout.block_width > 1 || fmt_layout.block_height > 1) {
      const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
        return (v % align) == 0 || v == extent;
      };
      if ((pDstBox->left % fmt_layout.block_width) != 0 || (pDstBox->top % fmt_layout.block_height) != 0 ||
          !aligned_or_edge(pDstBox->right, fmt_layout.block_width, sub_w) ||
          !aligned_or_edge(pDstBox->bottom, fmt_layout.block_height, sub_h)) {
        return;
      }
    }

    const uint32_t block_left = pDstBox->left / fmt_layout.block_width;
    const uint32_t block_top = pDstBox->top / fmt_layout.block_height;
    const uint32_t block_right = aerogpu_div_round_up_u32(pDstBox->right, fmt_layout.block_width);
    const uint32_t block_bottom = aerogpu_div_round_up_u32(pDstBox->bottom, fmt_layout.block_height);
    if (block_right < block_left || block_bottom < block_top) {
      return;
    }

    const uint32_t copy_width_blocks = block_right - block_left;
    const uint32_t copy_height_blocks = block_bottom - block_top;
    const uint64_t row_bytes_u64 =
        static_cast<uint64_t>(copy_width_blocks) * static_cast<uint64_t>(fmt_layout.bytes_per_block);
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
    const size_t dst_pitch = static_cast<size_t>(sub_row_pitch);
    const size_t base_offset = static_cast<size_t>(sub_layout.offset_bytes);
    const size_t dst_x_bytes = static_cast<size_t>(block_left) * static_cast<size_t>(fmt_layout.bytes_per_block);
    for (uint32_t y = 0; y < copy_height_blocks; ++y) {
      const size_t dst_offset = base_offset + (static_cast<size_t>(block_top) + y) * dst_pitch + dst_x_bytes;
      std::memcpy(res->storage.data() + dst_offset, src + y * src_pitch, row_bytes);
    }

    // The browser executor currently only supports partial UPLOAD_RESOURCE updates for
    // tightly packed textures (row_pitch_bytes == width*4). When the texture has per-row
    // padding, keep the command stream compatible by uploading the entire subresource.
    const size_t tight_row_bytes = static_cast<size_t>(min_row_bytes);
    size_t upload_offset = base_offset + static_cast<size_t>(block_top) * dst_pitch;
    size_t upload_size = static_cast<size_t>(copy_height_blocks) * dst_pitch;
    if (dst_pitch != tight_row_bytes) {
      upload_offset = base_offset;
      upload_size = static_cast<size_t>(sub_layout.size_bytes);
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

  // Copy reads from the source resource and writes the destination resource.
  track_resource_alloc_for_submit_locked(dev, dst, /*write=*/true);
  track_resource_alloc_for_submit_locked(dev, src, /*write=*/false);

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
    TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    const uint64_t dst_padded_size_bytes = AlignUpU64(dst->size_bytes ? dst->size_bytes : 1, 4);
    const uint64_t src_padded_size_bytes = AlignUpU64(src->size_bytes ? src->size_bytes : 1, 4);
    cmd->size_bytes = std::min(dst_padded_size_bytes, src_padded_size_bytes);
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
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0 || src->width == 0 ||
        src->height == 0) {
      return;
    }
    const uint32_t aerogpu_format = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, src->dxgi_format);
    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aerogpu_format);
    if (!fmt_layout.valid) {
      return;
    }

    const uint64_t dst_total = resource_total_bytes(dst);
    const uint64_t src_total = resource_total_bytes(src);
    if (!dst_total || !src_total) {
      return;
    }

    const uint32_t flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;

    const uint32_t mip_levels = std::min(dst->mip_levels, src->mip_levels);
    const uint32_t array_layers = std::min(dst->array_size, src->array_size);

    // Emit one COPY_TEXTURE2D per subresource.
    bool tracked_dst_write = false;
    for (uint32_t layer = 0; layer < array_layers; layer++) {
      for (uint32_t mip = 0; mip < mip_levels; mip++) {
        const uint32_t dst_w = aerogpu_mip_dim(dst->width, mip);
        const uint32_t dst_h = aerogpu_mip_dim(dst->height, mip);
        const uint32_t src_w = aerogpu_mip_dim(src->width, mip);
        const uint32_t src_h = aerogpu_mip_dim(src->height, mip);
        const uint32_t copy_w = std::min(dst_w, src_w);
        const uint32_t copy_h = std::min(dst_h, src_h);
        if (copy_w == 0 || copy_h == 0) {
          continue;
        }

        auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
        if (!cmd) {
          ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
          return;
        }
        if (!tracked_dst_write) {
          TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
          tracked_dst_write = true;
        }
        cmd->dst_texture = dst->handle;
        cmd->src_texture = src->handle;
        cmd->dst_mip_level = mip;
        cmd->dst_array_layer = layer;
        cmd->src_mip_level = mip;
        cmd->src_array_layer = layer;
        cmd->dst_x = 0;
        cmd->dst_y = 0;
        cmd->src_x = 0;
        cmd->src_y = 0;
        cmd->width = copy_w;
        cmd->height = copy_h;
        cmd->flags = flags;
        cmd->reserved0 = 0;
      }
    }

    CopySimMapping src_map = map_copy_sim(src, src_total);
    CopySimMapping dst_map = map_copy_sim(dst, dst_total);
    if (src_map.data && dst_map.data) {
      for (uint32_t layer = 0; layer < array_layers; layer++) {
        for (uint32_t mip = 0; mip < mip_levels; mip++) {
          const uint32_t dst_subresource = mip + layer * dst->mip_levels;
          const uint32_t src_subresource = mip + layer * src->mip_levels;

          Texture2DSubresourceLayout dst_layout{};
          Texture2DSubresourceLayout src_layout{};
          if (!compute_texture2d_subresource_layout(aerogpu_format,
                                                    dst->width,
                                                    dst->height,
                                                    dst->mip_levels,
                                                    dst->array_size,
                                                    dst->row_pitch_bytes,
                                                    dst_subresource,
                                                    &dst_layout) ||
              !compute_texture2d_subresource_layout(aerogpu_format,
                                                    src->width,
                                                    src->height,
                                                    src->mip_levels,
                                                    src->array_size,
                                                    src->row_pitch_bytes,
                                                    src_subresource,
                                                    &src_layout)) {
            continue;
          }

          const uint32_t copy_w = std::min(dst_layout.width, src_layout.width);
          const uint32_t copy_h = std::min(dst_layout.height, src_layout.height);
          if (copy_w == 0 || copy_h == 0) {
            continue;
          }

          const uint32_t row_bytes_u32 = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, copy_w);
          const uint32_t copy_rows_u32 = aerogpu_texture_num_rows(aerogpu_format, copy_h);
          if (row_bytes_u32 == 0 || copy_rows_u32 == 0) {
            continue;
          }
          const size_t row_bytes = static_cast<size_t>(row_bytes_u32);
          const size_t copy_rows = static_cast<size_t>(copy_rows_u32);

          if (row_bytes > dst_layout.row_pitch_bytes || row_bytes > src_layout.row_pitch_bytes) {
            continue;
          }

          const size_t dst_pitch = static_cast<size_t>(dst_layout.row_pitch_bytes);
          const size_t src_pitch = static_cast<size_t>(src_layout.row_pitch_bytes);
          const size_t dst_tight_row_bytes =
              static_cast<size_t>(aerogpu_texture_min_row_pitch_bytes(aerogpu_format, dst_layout.width));
          uint8_t* dst_base = dst_map.data + static_cast<size_t>(dst_layout.offset_bytes);
          const uint8_t* src_base = src_map.data + static_cast<size_t>(src_layout.offset_bytes);

          for (size_t y = 0; y < copy_rows; y++) {
            uint8_t* dst_row = dst_base + y * dst_pitch;
            const uint8_t* src_row = src_base + y * src_pitch;
            std::memcpy(dst_row, src_row, row_bytes);
            if (dst_pitch > dst_tight_row_bytes) {
              std::memset(dst_row + dst_tight_row_bytes, 0, dst_pitch - dst_tight_row_bytes);
            }
          }
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

  // Copy reads from the source resource and writes the destination resource.
  track_resource_alloc_for_submit_locked(dev, dst, /*write=*/true);
  track_resource_alloc_for_submit_locked(dev, src, /*write=*/false);

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
    if (dst_subresource != 0 || src_subresource != 0 || dst_x != 0 || dst_y != 0 || dst_z != 0 || pSrcBox) {
      return E_NOTIMPL;
    }
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_buffer>(AEROGPU_CMD_COPY_BUFFER);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
    cmd->dst_buffer = dst->handle;
    cmd->src_buffer = src->handle;
    cmd->dst_offset_bytes = 0;
    cmd->src_offset_bytes = 0;
    const uint64_t dst_padded_size_bytes = AlignUpU64(dst->size_bytes ? dst->size_bytes : 1, 4);
    const uint64_t src_padded_size_bytes = AlignUpU64(src->size_bytes ? src->size_bytes : 1, 4);
    cmd->size_bytes = std::min(dst_padded_size_bytes, src_padded_size_bytes);
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
    if (dst->dxgi_format != src->dxgi_format || dst->width == 0 || dst->height == 0 || src->width == 0 ||
        src->height == 0) {
      return E_INVALIDARG;
    }
    if (dst_z != 0) {
      return E_INVALIDARG;
    }

    const uint64_t dst_subresource_count_u64 =
        static_cast<uint64_t>(dst->mip_levels) * static_cast<uint64_t>(dst->array_size);
    const uint64_t src_subresource_count_u64 =
        static_cast<uint64_t>(src->mip_levels) * static_cast<uint64_t>(src->array_size);
    if (dst_subresource_count_u64 == 0 || src_subresource_count_u64 == 0) {
      return E_INVALIDARG;
    }
    if (static_cast<uint64_t>(dst_subresource) >= dst_subresource_count_u64 ||
        static_cast<uint64_t>(src_subresource) >= src_subresource_count_u64) {
      return E_INVALIDARG;
    }

    const uint32_t aerogpu_format = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, src->dxgi_format);
    const AerogpuTextureFormatLayout fmt_layout = aerogpu_texture_format_layout(aerogpu_format);
    if (!fmt_layout.valid) {
      return E_INVALIDARG;
    }

    Texture2DSubresourceLayout dst_layout{};
    Texture2DSubresourceLayout src_layout{};
    if (!compute_texture2d_subresource_layout(aerogpu_format,
                                              dst->width,
                                              dst->height,
                                              dst->mip_levels,
                                              dst->array_size,
                                              dst->row_pitch_bytes,
                                              dst_subresource,
                                              &dst_layout) ||
        !compute_texture2d_subresource_layout(aerogpu_format,
                                              src->width,
                                              src->height,
                                              src->mip_levels,
                                              src->array_size,
                                              src->row_pitch_bytes,
                                              src_subresource,
                                              &src_layout)) {
      return E_INVALIDARG;
    }

    uint32_t src_x = 0;
    uint32_t src_y = 0;
    uint32_t copy_w = src_layout.width;
    uint32_t copy_h = src_layout.height;
    if (pSrcBox) {
      if (pSrcBox->front != 0 || pSrcBox->back != 1) {
        return E_INVALIDARG;
      }
      if (pSrcBox->left >= pSrcBox->right || pSrcBox->top >= pSrcBox->bottom) {
        return E_INVALIDARG;
      }
      if (pSrcBox->right > src_layout.width || pSrcBox->bottom > src_layout.height) {
        return E_INVALIDARG;
      }
      src_x = pSrcBox->left;
      src_y = pSrcBox->top;
      copy_w = pSrcBox->right - pSrcBox->left;
      copy_h = pSrcBox->bottom - pSrcBox->top;
    }

    if (copy_w == 0 || copy_h == 0) {
      return E_INVALIDARG;
    }
    if (dst_x > dst_layout.width || dst_y > dst_layout.height || dst_layout.width - dst_x < copy_w ||
        dst_layout.height - dst_y < copy_h) {
      return E_INVALIDARG;
    }

    // Validate BC alignment rules (also apply to linear formats with block size 1).
    const auto aligned_or_edge = [](uint32_t v, uint32_t align, uint32_t extent) {
      return (v % align) == 0 || v == extent;
    };
    if ((src_x % fmt_layout.block_width) != 0 || (src_y % fmt_layout.block_height) != 0 ||
        (dst_x % fmt_layout.block_width) != 0 || (dst_y % fmt_layout.block_height) != 0 ||
        !aligned_or_edge(src_x + copy_w, fmt_layout.block_width, src_layout.width) ||
        !aligned_or_edge(src_y + copy_h, fmt_layout.block_height, src_layout.height) ||
        !aligned_or_edge(dst_x + copy_w, fmt_layout.block_width, dst_layout.width) ||
        !aligned_or_edge(dst_y + copy_h, fmt_layout.block_height, dst_layout.height)) {
      return E_INVALIDARG;
    }

    const uint32_t flags =
        (dst->usage == kD3D11UsageStaging && (dst->cpu_access_flags & kD3D11CpuAccessRead) != 0 && dst->backing_alloc_id != 0)
            ? AEROGPU_COPY_FLAG_WRITEBACK_DST
            : AEROGPU_COPY_FLAG_NONE;

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_copy_texture2d>(AEROGPU_CMD_COPY_TEXTURE2D);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    TrackStagingWriteLocked(dev, dst, [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
    cmd->dst_texture = dst->handle;
    cmd->src_texture = src->handle;
    cmd->dst_mip_level = dst_layout.mip_level;
    cmd->dst_array_layer = dst_layout.array_layer;
    cmd->src_mip_level = src_layout.mip_level;
    cmd->src_array_layer = src_layout.array_layer;
    cmd->dst_x = dst_x;
    cmd->dst_y = dst_y;
    cmd->src_x = src_x;
    cmd->src_y = src_y;
    cmd->width = copy_w;
    cmd->height = copy_h;
    cmd->flags = flags;
    cmd->reserved0 = 0;

    const uint32_t row_bytes_u32 = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, copy_w);
    const uint32_t copy_rows_u32 = aerogpu_texture_num_rows(aerogpu_format, copy_h);
    if (row_bytes_u32 == 0 || copy_rows_u32 == 0) {
      return S_OK;
    }

    const uint64_t dst_total = resource_total_bytes(dst);
    const uint64_t src_total = resource_total_bytes(src);
    if (!dst_total || !src_total) {
      return S_OK;
    }

    CopySimMapping src_map = map_copy_sim(src, src_total);
    CopySimMapping dst_map = map_copy_sim(dst, dst_total);
    if (src_map.data && dst_map.data) {
      const size_t row_bytes = static_cast<size_t>(row_bytes_u32);
      const size_t copy_rows = static_cast<size_t>(copy_rows_u32);

      const size_t src_pitch = static_cast<size_t>(src_layout.row_pitch_bytes);
      const size_t dst_pitch = static_cast<size_t>(dst_layout.row_pitch_bytes);
      const size_t dst_tight_row_bytes =
          static_cast<size_t>(aerogpu_texture_min_row_pitch_bytes(aerogpu_format, dst_layout.width));

      const size_t src_x_bytes =
          static_cast<size_t>((src_x / fmt_layout.block_width) * fmt_layout.bytes_per_block);
      const size_t dst_x_bytes =
          static_cast<size_t>((dst_x / fmt_layout.block_width) * fmt_layout.bytes_per_block);
      const size_t src_row0 = static_cast<size_t>(src_y / fmt_layout.block_height);
      const size_t dst_row0 = static_cast<size_t>(dst_y / fmt_layout.block_height);

      const uint8_t* src_base = src_map.data + static_cast<size_t>(src_layout.offset_bytes);
      uint8_t* dst_base = dst_map.data + static_cast<size_t>(dst_layout.offset_bytes);
      for (size_t y = 0; y < copy_rows; y++) {
        const uint8_t* src_row = src_base + (src_row0 + y) * src_pitch + src_x_bytes;
        uint8_t* dst_row = dst_base + (dst_row0 + y) * dst_pitch + dst_x_bytes;
        std::memcpy(dst_row, src_row, row_bytes);
        if (dst_pitch > dst_tight_row_bytes) {
          std::memset(dst_base + (dst_row0 + y) * dst_pitch + dst_tight_row_bytes,
                      0,
                      dst_pitch - dst_tight_row_bytes);
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
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the private object so DestroyShader is safe even if we
  // reject the descriptor (some runtimes may still call Destroy on failure).
  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = kInvalidHandle;
  sh->stage = stage;

  if (!pDesc || !pDesc->pCode || !pDesc->CodeSize) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  sh->handle = AllocateGlobalHandle(dev->adapter);
  try {
    sh->dxbc.resize(pDesc->CodeSize);
  } catch (...) {
    sh->handle = kInvalidHandle;
    AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
  }
  std::memcpy(sh->dxbc.data(), pDesc->pCode, pDesc->CodeSize);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  if (!cmd) {
    sh->handle = kInvalidHandle;
    // Avoid leaking the DXBC blob if the runtime does not destroy on failure.
    std::vector<uint8_t>().swap(sh->dxbc);
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
  if (!hShader.pDrvPrivate) {
    return;
  }

  auto* sh = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader);
  if (!sh) {
    return;
  }

  // If the device has already been destroyed, we can't safely lock its mutex or
  // append commands. Still destruct the shader object to free its DXBC blob.
  if (!IsDeviceLive(hDevice)) {
    sh->~AeroGpuShader();
    new (sh) AeroGpuShader();
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    sh->~AeroGpuShader();
    new (sh) AeroGpuShader();
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
  new (sh) AeroGpuShader();
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
  if (!hDevice.pDrvPrivate || !hLayout.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the private object so DestroyInputLayout is safe even if
  // we reject the descriptor (some runtimes may still call Destroy on failure).
  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = kInvalidHandle;

  if (!pDesc || (!pDesc->NumElements && pDesc->pElements)) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  layout->handle = AllocateGlobalHandle(dev->adapter);

  if (pDesc->NumElements > (SIZE_MAX - sizeof(aerogpu_input_layout_blob_header)) / sizeof(aerogpu_input_layout_element_dxgi)) {
    layout->handle = kInvalidHandle;
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  const size_t blob_size = sizeof(aerogpu_input_layout_blob_header) +
                           static_cast<size_t>(pDesc->NumElements) * sizeof(aerogpu_input_layout_element_dxgi);
  try {
    layout->blob.resize(blob_size);
  } catch (...) {
    layout->handle = kInvalidHandle;
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
    // Avoid leaking the blob if the runtime does not destroy on failure.
    std::vector<uint8_t>().swap(layout->blob);
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

  auto* layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  if (!layout) {
    return;
  }

  // If the device has already been destroyed, we can't safely lock its mutex or
  // append commands. Still destruct the input layout object to free its blob.
  if (!IsDeviceLive(hDevice)) {
    layout->~AeroGpuInputLayout();
    new (layout) AeroGpuInputLayout();
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    layout->~AeroGpuInputLayout();
    new (layout) AeroGpuInputLayout();
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
  new (layout) AeroGpuInputLayout();
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
  if (!hDevice.pDrvPrivate || !hRtv.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the view object so DestroyRTV is safe even if we reject the
  // descriptor.
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->resource = nullptr;

  if (!pDesc || !pDesc->hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  if (!dev || !dev->adapter || !res) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  if (res->kind != ResourceKind::Texture2D) {
    // The portable UMD historically allowed RTVs backed by non-texture resources
    // (used by tests that validate object lifetime rules). Treat these as
    // "trivial" views that bind the underlying resource handle directly.
    rtv->resource = res;
    rtv->texture = 0;
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);

  const uint32_t view_dxgi_format = pDesc->Format ? pDesc->Format : res->dxgi_format;
  const uint32_t base_mip_level = pDesc->MipSlice;

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = res->array_size;
  if (aerogpu::d3d10_11::D3dViewDimensionIsTexture2DArray(pDesc->ViewDimension)) {
    base_array_layer = pDesc->FirstArraySlice;
    array_layer_count =
        aerogpu::d3d10_11::D3dViewCountToRemaining(base_array_layer, pDesc->ArraySize, res->array_size);
  }

  const bool format_reinterpret = (pDesc->Format != 0) && (pDesc->Format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || base_mip_level != 0 || base_array_layer != 0 || array_layer_count != res->array_size;
  if (non_trivial && !supports_views) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  if (res->mip_levels == 0 || res->array_size == 0 ||
      base_mip_level >= res->mip_levels ||
      base_array_layer >= res->array_size ||
      array_layer_count == 0 ||
      base_array_layer + array_layer_count > res->array_size) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Trivial views bind the underlying resource handle at bind-time (texture==0)
  // so RotateResourceIdentities can update the handle.
  rtv->resource = res;
  rtv->texture = 0;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = base_mip_level;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;

    rtv->texture = view_handle;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyRTV hRtv=%p", hRtv.pDrvPrivate);
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && rtv) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && rtv->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
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
  if (!hDevice.pDrvPrivate || !hDsv.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Always construct the view object so DestroyDSV is safe even if we reject the
  // descriptor.
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->resource = nullptr;

  if (!pDesc || !pDesc->hResource.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  if (!dev || !dev->adapter || !res) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  if (res->kind != ResourceKind::Texture2D) {
    // The portable UMD historically allowed DSVs backed by non-texture resources
    // (used by tests that validate object lifetime rules). Treat these as
    // "trivial" views that bind the underlying resource handle directly.
    dsv->resource = res;
    dsv->texture = 0;
    AEROGPU_D3D10_RET_HR(S_OK);
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);

  const uint32_t view_dxgi_format = pDesc->Format ? pDesc->Format : res->dxgi_format;
  const uint32_t base_mip_level = pDesc->MipSlice;

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = res->array_size;
  if (aerogpu::d3d10_11::D3dViewDimensionIsTexture2DArray(pDesc->ViewDimension)) {
    base_array_layer = pDesc->FirstArraySlice;
    array_layer_count =
        aerogpu::d3d10_11::D3dViewCountToRemaining(base_array_layer, pDesc->ArraySize, res->array_size);
  }

  const bool format_reinterpret = (pDesc->Format != 0) && (pDesc->Format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret || base_mip_level != 0 || base_array_layer != 0 || array_layer_count != res->array_size;
  if (non_trivial && !supports_views) {
    AEROGPU_D3D10_RET_HR(E_NOTIMPL);
  }

  if (res->mip_levels == 0 || res->array_size == 0 ||
      base_mip_level >= res->mip_levels ||
      base_array_layer >= res->array_size ||
      array_layer_count == 0 ||
      base_array_layer + array_layer_count > res->array_size) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Trivial views bind the underlying resource handle at bind-time (texture==0)
  // so RotateResourceIdentities can update the handle.
  dsv->resource = res;
  dsv->texture = 0;

  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      AEROGPU_D3D10_RET_HR(E_NOTIMPL);
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      AEROGPU_D3D10_RET_HR(E_OUTOFMEMORY);
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = base_mip_level;
    cmd->mip_level_count = 1;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;

    dsv->texture = view_handle;
  }

  AEROGPU_D3D10_RET_HR(S_OK);
}

void AEROGPU_APIENTRY DestroyDSV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("DestroyDSV hDsv=%p", hDsv.pDrvPrivate);
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  auto* dev = hDevice.pDrvPrivate ? FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice) : nullptr;
  if (dev && dsv) {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && dsv->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
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

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderResourceViewSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW*) {
  return sizeof(AeroGpuShaderResourceView);
}

HRESULT AEROGPU_APIENTRY CreateShaderResourceView(D3D10DDI_HDEVICE hDevice,
                                                  const AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW* pDesc,
                                                  D3D10DDI_HSHADERRESOURCEVIEW hView) {
  if (!hDevice.pDrvPrivate || !hView.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the view object so DestroyShaderResourceView is safe even
  // if we reject the descriptor.
  auto* view = new (hView.pDrvPrivate) AeroGpuShaderResourceView();
  view->texture = 0;

  if (!pDesc || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  if (!dev || !dev->adapter || !res || res->kind != ResourceKind::Texture2D) {
    return E_NOTIMPL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t view_dxgi_format = pDesc->Format ? pDesc->Format : res->dxgi_format;
  const uint32_t base_mip_level = pDesc->MostDetailedMip;
  uint32_t mip_level_count =
      aerogpu::d3d10_11::D3dViewCountToRemaining(base_mip_level, pDesc->MipLevels, res->mip_levels);

  uint32_t base_array_layer = 0;
  uint32_t array_layer_count = res->array_size;
  if (aerogpu::d3d10_11::D3dViewDimensionIsTexture2DArray(pDesc->ViewDimension)) {
    base_array_layer = pDesc->FirstArraySlice;
    array_layer_count =
        aerogpu::d3d10_11::D3dViewCountToRemaining(base_array_layer, pDesc->ArraySize, res->array_size);
  }

  const bool format_reinterpret =
      (pDesc->Format != 0) && (pDesc->Format != res->dxgi_format);
  const bool non_trivial =
      format_reinterpret ||
      base_mip_level != 0 ||
      mip_level_count != res->mip_levels ||
      base_array_layer != 0 ||
      array_layer_count != res->array_size;
  const bool supports_views = aerogpu::d3d10_11::SupportsTextureViews(dev->adapter);
  if (non_trivial && !supports_views) {
    return E_NOTIMPL;
  }

  if (res->mip_levels == 0 || res->array_size == 0 ||
      base_mip_level >= res->mip_levels ||
      mip_level_count == 0 ||
      base_mip_level + mip_level_count > res->mip_levels ||
      base_array_layer >= res->array_size ||
      array_layer_count == 0 ||
      base_array_layer + array_layer_count > res->array_size) {
    return E_INVALIDARG;
  }

  view->resource = res;
  if (non_trivial && supports_views) {
    const uint32_t aer_fmt = aerogpu::d3d10_11::dxgi_format_to_aerogpu_compat(dev, view_dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_NOTIMPL;
    }

    const aerogpu_handle_t view_handle = aerogpu::d3d10_11::AllocateGlobalHandle(dev->adapter);
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture_view>(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
    if (!cmd) {
      return E_OUTOFMEMORY;
    }
    cmd->view_handle = view_handle;
    cmd->texture_handle = res->handle;
    cmd->format = aer_fmt;
    cmd->base_mip_level = base_mip_level;
    cmd->mip_level_count = mip_level_count;
    cmd->base_array_layer = base_array_layer;
    cmd->array_layer_count = array_layer_count;
    cmd->reserved0 = 0;

    view->texture = view_handle;
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
    if (aerogpu::d3d10_11::SupportsTextureViews(dev->adapter) && view->texture) {
      auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_texture_view>(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
      if (!cmd) {
        ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
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

SIZE_T AEROGPU_APIENTRY CalcPrivateSamplerSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESAMPLER*) {
  return sizeof(AeroGpuSampler);
}

HRESULT AEROGPU_APIENTRY CreateSampler(D3D10DDI_HDEVICE hDevice,
                                       const AEROGPU_DDIARG_CREATESAMPLER* pDesc,
                                       D3D10DDI_HSAMPLER hSampler) {
  if (!hDevice.pDrvPrivate || !hSampler.pDrvPrivate) {
    return E_INVALIDARG;
  }

  // Always construct the sampler object so DestroySampler is safe even if we
  // reject the descriptor.
  auto* s = new (hSampler.pDrvPrivate) AeroGpuSampler();
  s->handle = 0;

  if (!pDesc) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  s->handle = dev->adapter->next_handle.fetch_add(1);
  InitSamplerFromCreateSamplerArg(s, pDesc);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_sampler>(AEROGPU_CMD_CREATE_SAMPLER);
  if (!cmd) {
    s->handle = 0;
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
  if (!hSampler.pDrvPrivate) {
    return;
  }

  auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(hSampler);
  if (!s) {
    return;
  }

  // If the device has already been destroyed, we can't safely lock its mutex or
  // append commands. Still destruct the sampler object.
  if (!IsDeviceLive(hDevice)) {
    s->~AeroGpuSampler();
    new (s) AeroGpuSampler();
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    s->~AeroGpuSampler();
    new (s) AeroGpuSampler();
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
  new (s) AeroGpuSampler();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEBLENDSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateBlendStateSize");
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const AEROGPU_DDIARG_CREATEBLENDSTATE* pDesc,
                                          D3D10DDI_HBLENDSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateBlendState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  // Default to D3D10 defaults (blending disabled; write RGBA).
  aerogpu_blend_state state{};
  state.enable = 0;
  state.src_factor = AEROGPU_BLEND_ONE;
  state.dst_factor = AEROGPU_BLEND_ZERO;
  state.blend_op = AEROGPU_BLEND_OP_ADD;
  state.color_write_mask = kD3DColorWriteMaskAll;
  state.reserved0[0] = 0;
  state.reserved0[1] = 0;
  state.reserved0[2] = 0;
  state.src_factor_alpha = AEROGPU_BLEND_ONE;
  state.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  state.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  state.blend_constant_rgba_f32[0] = 0;
  state.blend_constant_rgba_f32[1] = 0;
  state.blend_constant_rgba_f32[2] = 0;
  state.blend_constant_rgba_f32[3] = 0;
  state.sample_mask = kD3DSampleMaskAll;

  // Always construct the state object so DestroyBlendState is safe even if we
  // reject the descriptor (some runtimes may still call Destroy on failure).
  auto* s = new (hState.pDrvPrivate) AeroGpuBlendState();
  s->state = state;

  if (pDesc) {
    if (pDesc->enable > 1u) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if ((pDesc->color_write_mask & ~kD3DColorWriteMaskAll) != 0) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if (pDesc->src_factor > AEROGPU_BLEND_INV_CONSTANT || pDesc->dst_factor > AEROGPU_BLEND_INV_CONSTANT ||
        pDesc->src_factor_alpha > AEROGPU_BLEND_INV_CONSTANT || pDesc->dst_factor_alpha > AEROGPU_BLEND_INV_CONSTANT) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if (pDesc->blend_op > AEROGPU_BLEND_OP_MAX || pDesc->blend_op_alpha > AEROGPU_BLEND_OP_MAX) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    state.enable = pDesc->enable ? 1u : 0u;
    state.src_factor = pDesc->src_factor;
    state.dst_factor = pDesc->dst_factor;
    state.blend_op = pDesc->blend_op;
    state.color_write_mask = static_cast<uint8_t>(pDesc->color_write_mask & kD3DColorWriteMaskAll);
    state.src_factor_alpha = pDesc->src_factor_alpha;
    state.dst_factor_alpha = pDesc->dst_factor_alpha;
    state.blend_op_alpha = pDesc->blend_op_alpha;
    s->state = state;
  }
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
  new (s) AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERASTERIZERSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateRasterizerStateSize");
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const AEROGPU_DDIARG_CREATERASTERIZERSTATE* pDesc,
                                               D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateRasterizerState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  // Default to D3D10 defaults (solid fill, cull back, depth clip enabled).
  aerogpu_rasterizer_state state{};
  state.fill_mode = AEROGPU_FILL_SOLID;
  state.cull_mode = AEROGPU_CULL_BACK;
  state.front_ccw = 0u;
  state.scissor_enable = 0u;
  state.depth_bias = 0;
  state.flags = AEROGPU_RASTERIZER_FLAG_NONE;

  // Always construct the state object so DestroyRasterizerState is safe even if
  // we reject the descriptor.
  auto* s = new (hState.pDrvPrivate) AeroGpuRasterizerState();
  s->state = state;

  if (pDesc) {
    if (pDesc->fill_mode > AEROGPU_FILL_WIREFRAME) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if (pDesc->cull_mode > AEROGPU_CULL_BACK) {
      // Cull mode is serialized into the AeroGPU command stream. Reject unknown values so we
      // do not emit invalid protocol enums.
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if (pDesc->front_ccw > 1u || pDesc->scissor_enable > 1u || pDesc->depth_clip_enable > 1u) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    state.fill_mode = pDesc->fill_mode;
    state.cull_mode = pDesc->cull_mode;
    state.front_ccw = pDesc->front_ccw ? 1u : 0u;
    state.scissor_enable = pDesc->scissor_enable ? 1u : 0u;
    state.depth_bias = pDesc->depth_bias;
    state.flags = pDesc->depth_clip_enable ? AEROGPU_RASTERIZER_FLAG_NONE
                                          : AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE;
    s->state = state;
  }
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
  new (s) AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE,
                                                         const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CalcPrivateDepthStencilStateSize");
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE* pDesc,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("CreateDepthStencilState hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }
  aerogpu_depth_stencil_state state{};
  state.depth_enable = 1u;
  state.depth_write_enable = 1u;
  state.depth_func = AEROGPU_COMPARE_LESS;
  state.stencil_enable = 0u;
  state.stencil_read_mask = kD3DStencilMaskAll;
  state.stencil_write_mask = kD3DStencilMaskAll;
  state.reserved0[0] = 0;
  state.reserved0[1] = 0;

  // Always construct the state object so DestroyDepthStencilState is safe even
  // if we reject the descriptor (some runtimes may still call Destroy on
  // failure).
  auto* s = new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  s->state = state;

  if (pDesc) {
    if (pDesc->depth_enable > 1u || pDesc->depth_write_enable > 1u || pDesc->stencil_enable > 1u) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }
    if (pDesc->depth_func > AEROGPU_COMPARE_ALWAYS) {
      AEROGPU_D3D10_RET_HR(E_INVALIDARG);
    }

    state.depth_enable = pDesc->depth_enable ? 1u : 0u;
    // D3D10/11 semantics: DepthWriteMask is ignored when depth testing is disabled.
    state.depth_write_enable = (state.depth_enable && pDesc->depth_write_enable) ? 1u : 0u;
    state.depth_func = pDesc->depth_func;
    state.stencil_enable = pDesc->stencil_enable ? 1u : 0u;
    state.stencil_read_mask = pDesc->stencil_read_mask;
    state.stencil_write_mask = pDesc->stencil_write_mask;
    s->state = state;
  }
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
  new (s) AeroGpuDepthStencilState();
}

void AEROGPU_APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice,
                                       uint32_t num_views,
                                       const D3D10DDI_HRENDERTARGETVIEW* ph_views,
                                       D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetRenderTargets hDevice=%p num_views=%u rtv0=%p hDsv=%p",
                                hDevice.pDrvPrivate,
                                static_cast<unsigned>(num_views),
                                (ph_views && num_views) ? ph_views[0].pDrvPrivate : nullptr,
                                hDsv.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (num_views != 0 && !ph_views) {
    ReportDeviceErrorLocked(dev, hDevice, E_INVALIDARG);
    return;
  }

  const uint32_t count = std::min<uint32_t>(num_views, AEROGPU_MAX_RENDER_TARGETS);
  const AeroGpuRenderTargetView* rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < count; ++i) {
    if (ph_views && ph_views[i].pDrvPrivate) {
      rtvs[i] = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(ph_views[i]);
    }
  }

  const AeroGpuDepthStencilView* dsv = nullptr;
  if (hDsv.pDrvPrivate) {
    dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  }

  if (!set_render_targets_locked(dev, hDevice, count, (count ? rtvs : nullptr), dsv)) {
    return;
  }
}

void AEROGPU_APIENTRY ClearRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hRtv, const float rgba[4]) {
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

  // Software fallback: update CPU-visible backing so deterministic staging readback
  // tests can observe the cleared pixels without a real GPU/host compositor.
  AeroGpuResource* rt = nullptr;
  if (hRtv.pDrvPrivate) {
    auto* view = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
    rt = view ? view->resource : nullptr;
  }
  if (!rt) {
    // D3D10/10.1 ClearRTV always provides an explicit RTV handle, but keep a
    // fallback for tests/tools that call into the portable UMD without a view.
    for (uint32_t i = 0; i < dev->current_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      if (dev->current_rtv_resources[i]) {
        rt = dev->current_rtv_resources[i];
        break;
      }
    }
  }

  if (rt && rt->kind == ResourceKind::Texture2D && rt->width != 0 && rt->height != 0) {
    // Resolve the backing pointer (prefer allocation-backed resources when the harness provides mapping callbacks).
    uint8_t* base = nullptr;
    AEROGPU_WDDM_ALLOCATION_HANDLE mapped_alloc = 0;
    void* mapped_ptr = nullptr;
    const auto* cb = dev->device_callbacks;
    if (cb && cb->pfnMapAllocation && cb->pfnUnmapAllocation && rt->alloc_handle != 0) {
      HRESULT hr = cb->pfnMapAllocation(cb->pUserContext, rt->alloc_handle, &mapped_ptr);
      if (SUCCEEDED(hr) && mapped_ptr) {
        base = static_cast<uint8_t*>(mapped_ptr) + rt->alloc_offset_bytes;
        mapped_alloc = rt->alloc_handle;
      } else if (SUCCEEDED(hr) && !mapped_ptr) {
        cb->pfnUnmapAllocation(cb->pUserContext, rt->alloc_handle);
      }
    }

    uint32_t bytes_per_pixel = 0;
    bool is_16bpp = false;
    uint8_t px32[4] = {};
    uint16_t px16 = 0;

    auto float_to_unorm = [](float v, uint32_t max) -> uint32_t {
      // Treat NaNs as zero via ordered comparisons.
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

    const uint8_t out_r = static_cast<uint8_t>(float_to_unorm(rgba[0], 255));
    const uint8_t out_g = static_cast<uint8_t>(float_to_unorm(rgba[1], 255));
    const uint8_t out_b = static_cast<uint8_t>(float_to_unorm(rgba[2], 255));
    const uint8_t out_a = static_cast<uint8_t>(float_to_unorm(rgba[3], 255));

    switch (rt->dxgi_format) {
      case aerogpu::d3d10_11::kDxgiFormatB5G6R5Unorm: {
        const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
        const uint16_t g6 = static_cast<uint16_t>(float_to_unorm(rgba[1], 63));
        const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
        px16 = static_cast<uint16_t>((r5 << 11) | (g6 << 5) | b5);
        bytes_per_pixel = 2;
        is_16bpp = true;
        break;
      }
      case aerogpu::d3d10_11::kDxgiFormatB5G5R5A1Unorm: {
        const uint16_t r5 = static_cast<uint16_t>(float_to_unorm(rgba[0], 31));
        const uint16_t g5 = static_cast<uint16_t>(float_to_unorm(rgba[1], 31));
        const uint16_t b5 = static_cast<uint16_t>(float_to_unorm(rgba[2], 31));
        const uint16_t a1 = static_cast<uint16_t>(float_to_unorm(rgba[3], 1));
        px16 = static_cast<uint16_t>((a1 << 15) | (r5 << 10) | (g5 << 5) | b5);
        bytes_per_pixel = 2;
        is_16bpp = true;
        break;
      }
      case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Unorm:
      case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8UnormSrgb:
      case aerogpu::d3d10_11::kDxgiFormatR8G8B8A8Typeless:
        px32[0] = out_r;
        px32[1] = out_g;
        px32[2] = out_b;
        px32[3] = out_a;
        bytes_per_pixel = 4;
        break;
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Unorm:
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8UnormSrgb:
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8X8Typeless:
        px32[0] = out_b;
        px32[1] = out_g;
        px32[2] = out_r;
        px32[3] = 255;
        bytes_per_pixel = 4;
        break;
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm:
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8UnormSrgb:
      case aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Typeless:
        px32[0] = out_b;
        px32[1] = out_g;
        px32[2] = out_r;
        px32[3] = out_a;
        bytes_per_pixel = 4;
        break;
      default:
        break;
    }

    if (bytes_per_pixel != 0) {
      if (rt->row_pitch_bytes == 0) {
        rt->row_pitch_bytes = rt->width * bytes_per_pixel;
      }
      if (rt->row_pitch_bytes >= rt->width * bytes_per_pixel) {
        const uint64_t required_bytes =
            static_cast<uint64_t>(rt->row_pitch_bytes) * static_cast<uint64_t>(rt->height);
        if (required_bytes != 0 && required_bytes <= static_cast<uint64_t>(SIZE_MAX)) {
          if (!base) {
            if (SUCCEEDED(ensure_resource_storage(rt, required_bytes))) {
              base = rt->storage.data();
            }
          }
          if (base) {
            for (uint32_t y = 0; y < rt->height; ++y) {
              uint8_t* row = base + static_cast<size_t>(y) * rt->row_pitch_bytes;
              for (uint32_t x = 0; x < rt->width; ++x) {
                uint8_t* dst = row + static_cast<size_t>(x) * bytes_per_pixel;
                if (is_16bpp) {
                  std::memcpy(dst, &px16, sizeof(px16));
                } else {
                  std::memcpy(dst, px32, sizeof(px32));
                }
              }
            }
          }
        }
      }
    }

    if (mapped_alloc != 0 && cb && cb->pfnUnmapAllocation) {
      cb->pfnUnmapAllocation(cb->pUserContext, mapped_alloc);
    }
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  // Clear writes the currently bound render targets. Track them now so their
  // backing allocations are included even if the targets are later unbound
  // before submission.
  track_current_state_allocs_for_submit_locked(dev);
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
  if (flags != 0) {
    // Clear writes the currently bound depth-stencil target. Track it now so its
    // backing allocation is included even if it is later unbound before
    // submission.
    track_current_state_allocs_for_submit_locked(dev);
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
  // Vertex buffers are read by Draw.
  track_alloc_for_submit_locked(dev, vb_alloc, /*write=*/false);
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
  // Index buffers are read by DrawIndexed.
  track_alloc_for_submit_locked(dev, ib_alloc, /*write=*/false);
}

void AEROGPU_APIENTRY SetViewports(D3D10DDI_HDEVICE hDevice,
                                   uint32_t num_viewports,
                                   const AEROGPU_DDI_VIEWPORT* pViewports) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetViewports hDevice=%p num=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(num_viewports));
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  validate_and_emit_viewports_locked(dev,
                                    num_viewports,
                                    pViewports,
                                    [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
}

void AEROGPU_APIENTRY SetScissorRects(D3D10DDI_HDEVICE hDevice,
                                      uint32_t num_rects,
                                      const AEROGPU_DDI_RECT* pRects) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetScissorRects hDevice=%p num=%u",
                               hDevice.pDrvPrivate,
                               static_cast<unsigned>(num_rects));
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  validate_and_emit_scissor_rects_locked(dev,
                                         num_rects,
                                         pRects,
                                         [&](HRESULT hr) { ReportDeviceErrorLocked(dev, hDevice, hr); });
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

  auto* cmd = dev->cmd.bind_shaders(vs, ps, /*cs=*/0);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  dev->current_vs = vs;
  dev->current_ps = ps;
}

static bool EmitRasterizerStateLocked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, const AeroGpuRasterizerState* rs) {
  if (!dev) {
    return false;
  }

  aerogpu_rasterizer_state state{};
  state.fill_mode = AEROGPU_FILL_SOLID;
  state.cull_mode = AEROGPU_CULL_BACK;
  state.front_ccw = 0u;
  state.scissor_enable = 0u;
  state.depth_bias = 0;
  state.flags = AEROGPU_RASTERIZER_FLAG_NONE;
  if (rs) {
    state = rs->state;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_rasterizer_state>(AEROGPU_CMD_SET_RASTERIZER_STATE);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }
  cmd->state = state;
  return true;
}

static bool EmitBlendStateLocked(AeroGpuDevice* dev,
                                 D3D10DDI_HDEVICE hDevice,
                                 const AeroGpuBlendState* bs,
                                 const float blend_factor[4],
                                 uint32_t sample_mask) {
  if (!dev) {
    return false;
  }

  aerogpu_blend_state state{};
  state.enable = 0;
  state.src_factor = AEROGPU_BLEND_ONE;
  state.dst_factor = AEROGPU_BLEND_ZERO;
  state.blend_op = AEROGPU_BLEND_OP_ADD;
  state.color_write_mask = kD3DColorWriteMaskAll;
  state.reserved0[0] = 0;
  state.reserved0[1] = 0;
  state.reserved0[2] = 0;
  state.src_factor_alpha = AEROGPU_BLEND_ONE;
  state.dst_factor_alpha = AEROGPU_BLEND_ZERO;
  state.blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  if (bs) {
    state = bs->state;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_blend_state>(AEROGPU_CMD_SET_BLEND_STATE);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }
  state.reserved0[0] = 0;
  state.reserved0[1] = 0;
  state.reserved0[2] = 0;
  state.blend_constant_rgba_f32[0] = blend_factor ? f32_bits(blend_factor[0]) : f32_bits(1.0f);
  state.blend_constant_rgba_f32[1] = blend_factor ? f32_bits(blend_factor[1]) : f32_bits(1.0f);
  state.blend_constant_rgba_f32[2] = blend_factor ? f32_bits(blend_factor[2]) : f32_bits(1.0f);
  state.blend_constant_rgba_f32[3] = blend_factor ? f32_bits(blend_factor[3]) : f32_bits(1.0f);
  state.sample_mask = sample_mask;
  cmd->state = state;
  return true;
}

static bool EmitDepthStencilStateLocked(AeroGpuDevice* dev, D3D10DDI_HDEVICE hDevice, const AeroGpuDepthStencilState* dss) {
  if (!dev) {
    return false;
  }

  aerogpu_depth_stencil_state state{};
  state.depth_enable = 1u;
  state.depth_write_enable = 1u;
  state.depth_func = AEROGPU_COMPARE_LESS;
  state.stencil_enable = 0u;
  state.stencil_read_mask = kD3DStencilMaskAll;
  state.stencil_write_mask = kD3DStencilMaskAll;
  state.reserved0[0] = 0;
  state.reserved0[1] = 0;
  if (dss) {
    state = dss->state;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return false;
  }
  cmd->state = state;
  return true;
}

void AEROGPU_APIENTRY SetBlendState(D3D10DDI_HDEVICE hDevice,
                                    D3D10DDI_HBLENDSTATE hState,
                                    const float blend_factor[4],
                                    UINT sample_mask) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetBlendState");
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  AeroGpuBlendState* new_bs = hState.pDrvPrivate ? FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState) : nullptr;
  float new_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  if (blend_factor) {
    std::memcpy(new_blend_factor, blend_factor, sizeof(new_blend_factor));
  }
  const uint32_t new_sample_mask = sample_mask;

  if (!EmitBlendStateLocked(dev, hDevice, new_bs, new_blend_factor, new_sample_mask)) {
    return;
  }

  dev->current_bs = new_bs;
  std::memcpy(dev->current_blend_factor, new_blend_factor, sizeof(dev->current_blend_factor));
  dev->current_sample_mask = new_sample_mask;
}

void AEROGPU_APIENTRY SetRasterizerState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRASTERIZERSTATE hState) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetRasterizerState");
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  AeroGpuRasterizerState* new_rs =
      hState.pDrvPrivate ? FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState) : nullptr;
  if (!EmitRasterizerStateLocked(dev, hDevice, new_rs)) {
    return;
  }
  dev->current_rs = new_rs;
}

void AEROGPU_APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HDEPTHSTENCILSTATE hState, UINT stencil_ref) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("SetDepthStencilState");
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  std::lock_guard<std::mutex> lock(dev->mutex);
  AeroGpuDepthStencilState* new_dss =
      hState.pDrvPrivate ? FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState) : nullptr;
  const uint32_t new_stencil_ref = stencil_ref;
  if (!EmitDepthStencilStateLocked(dev, hDevice, new_dss)) {
    return;
  }
  dev->current_dss = new_dss;
  dev->current_stencil_ref = new_stencil_ref;
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

  // Avoid std::vector allocations (can throw std::bad_alloc). The slot count is small and bounded.
  aerogpu_constant_buffer_binding bindings[kMaxConstantBufferSlots] = {};
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
        b.size_bytes = aerogpu::d3d10_11::ClampU64ToU32(res->size_bytes);
      }
    }

    bindings[i] = b;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings, static_cast<size_t>(count) * sizeof(bindings[0]));
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

  aerogpu_constant_buffer_binding bindings[kMaxConstantBufferSlots] = {};
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
        b.size_bytes = aerogpu::d3d10_11::ClampU64ToU32(res->size_bytes);
      }
    }

    bindings[i] = b;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, bindings, static_cast<size_t>(count) * sizeof(bindings[0]));
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

  // D3D10/11 hazard rule: resources bound as SRVs cannot simultaneously be bound
  // as render targets / depth-stencil outputs. Unbind any aliased outputs before
  // installing the SRVs.
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    AeroGpuResource* res = nullptr;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      if (view) {
        res = view->resource;
        tex = view->texture ? view->texture : (res ? res->handle : 0);
      }
    }
    if ((tex || res) && !unbind_resource_from_outputs_locked(dev, hDevice, tex, res)) {
      return;
    }
  }

  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    AeroGpuResource* res = nullptr;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      res = view ? view->resource : nullptr;
      tex = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
    }
    const uint32_t slot = start_slot + i;
    if (dev->vs_srvs[slot] == tex && dev->vs_srv_resources[slot] == res) {
      continue;
    }
    if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, tex)) {
      return;
    }
    dev->vs_srvs[slot] = tex;
    dev->vs_srv_resources[slot] = tex ? res : nullptr;
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

  // D3D10/11 hazard rule: resources bound as SRVs cannot simultaneously be bound
  // as render targets / depth-stencil outputs. Unbind any aliased outputs before
  // installing the SRVs.
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    AeroGpuResource* res = nullptr;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      if (view) {
        res = view->resource;
        tex = view->texture ? view->texture : (res ? res->handle : 0);
      }
    }
    if ((tex || res) && !unbind_resource_from_outputs_locked(dev, hDevice, tex, res)) {
      return;
    }
  }

  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t tex = 0;
    AeroGpuResource* res = nullptr;
    if (pViews && pViews[i].pDrvPrivate) {
      auto* view = FromHandle<D3D10DDI_HSHADERRESOURCEVIEW, AeroGpuShaderResourceView>(pViews[i]);
      res = view ? view->resource : nullptr;
      tex = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
    }
    const uint32_t slot = start_slot + i;
    if (dev->ps_srvs[slot] == tex && dev->ps_srv_resources[slot] == res) {
      continue;
    }
    if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, tex)) {
      return;
    }
    dev->ps_srvs[slot] = tex;
    dev->ps_srv_resources[slot] = tex ? res : nullptr;
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

  // Avoid std::vector allocations (can throw std::bad_alloc). Sampler slots are small and bounded.
  aerogpu_handle_t handles[kMaxSamplerSlots] = {};
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t h = 0;
    if (pSamplers && pSamplers[i].pDrvPrivate) {
      auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(pSamplers[i]);
      h = s ? s->handle : 0;
    }
    handles[i] = h;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles, static_cast<size_t>(count) * sizeof(handles[0]));
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

  aerogpu_handle_t handles[kMaxSamplerSlots] = {};
  for (uint32_t i = 0; i < count; i++) {
    aerogpu_handle_t h = 0;
    if (pSamplers && pSamplers[i].pDrvPrivate) {
      auto* s = FromHandle<D3D10DDI_HSAMPLER, AeroGpuSampler>(pSamplers[i]);
      h = s ? s->handle : 0;
    }
    handles[i] = h;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, handles, static_cast<size_t>(count) * sizeof(handles[0]));
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
  if (vertex_count != 0) {
    // Track allocations referenced by the draw's *current* state so the eventual
    // submission's allocation list includes resources used earlier in the command
    // buffer even if they are later unbound.
    track_current_state_allocs_for_submit_locked(dev);
  }
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawInstanced(D3D10DDI_HDEVICE hDevice,
                                    uint32_t vertex_count_per_instance,
                                    uint32_t instance_count,
                                    uint32_t start_vertex,
                                    uint32_t start_instance) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawInstanced hDevice=%p vcpi=%u inst=%u start_v=%u start_i=%u",
                               hDevice.pDrvPrivate,
                               vertex_count_per_instance,
                               instance_count,
                               start_vertex,
                               start_instance);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  if (vertex_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  track_current_state_allocs_for_submit_locked(dev);
  cmd->vertex_count = vertex_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = start_instance;
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
  if (index_count != 0) {
    track_current_state_allocs_for_submit_locked(dev);
  }
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexedInstanced(D3D10DDI_HDEVICE hDevice,
                                           uint32_t index_count_per_instance,
                                           uint32_t instance_count,
                                           uint32_t start_index,
                                           int32_t base_vertex,
                                           uint32_t start_instance) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawIndexedInstanced hDevice=%p icpi=%u inst=%u start=%u base=%d start_i=%u",
                               hDevice.pDrvPrivate,
                               index_count_per_instance,
                               instance_count,
                               start_index,
                               base_vertex,
                               start_instance);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }
  if (index_count_per_instance == 0 || instance_count == 0) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  track_current_state_allocs_for_submit_locked(dev);
  cmd->index_count = index_count_per_instance;
  cmd->instance_count = instance_count;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = start_instance;
}

void AEROGPU_APIENTRY DrawAuto(D3D10DDI_HDEVICE hDevice) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF_VERBOSE("DrawAuto hDevice=%p", hDevice.pDrvPrivate);
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  // Repository builds do not implement stream output yet, so `DrawAuto` cannot
  // determine a vertex count. Emit a no-op draw so command stream consumers can
  // still observe the call without crashing the runtime.
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  if (!cmd) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  cmd->vertex_count = 0;
  cmd->instance_count = 1;
  cmd->first_vertex = 0;
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
    // Present reads from the backbuffer (scanout source).
    track_resource_alloc_for_submit_locked(dev, backbuffer, /*write=*/false);
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
  try {
    resources.reserve(numResources);
  } catch (...) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
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
    try {
      resources.push_back(res);
    } catch (...) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
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

  // Treat RotateResourceIdentities as a transaction: if any required rebinding
  // packet cannot be appended (OOM), roll back the command stream and undo the
  // rotation so the runtime-visible state remains unchanged.
  const auto cmd_checkpoint = dev->cmd.checkpoint();
  const uint32_t prev_rtv_count = dev->current_rtv_count;
  aerogpu_handle_t prev_rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  AeroGpuResource* prev_rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    prev_rtvs[i] = dev->current_rtvs[i];
    prev_rtv_resources[i] = dev->current_rtv_resources[i];
  }
  const aerogpu_handle_t prev_dsv = dev->current_dsv;
  AeroGpuResource* prev_dsv_res = dev->current_dsv_res;
  aerogpu_handle_t prev_vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t prev_ps_srvs[kMaxShaderResourceSlots] = {};
  std::memcpy(prev_vs_srvs, dev->vs_srvs, sizeof(prev_vs_srvs));
  std::memcpy(prev_ps_srvs, dev->ps_srvs, sizeof(prev_ps_srvs));

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
    uint64_t last_gpu_write_fence = 0;
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
    id.last_gpu_write_fence = res->last_gpu_write_fence;
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
    res->last_gpu_write_fence = id.last_gpu_write_fence;
    res->mapped = id.mapped;
    res->mapped_write = id.mapped_write;
    res->mapped_subresource = id.mapped_subresource;
    res->mapped_map_type = id.mapped_map_type;
    res->mapped_offset_bytes = id.mapped_offset_bytes;
    res->mapped_size_bytes = id.mapped_size_bytes;
  };

  auto rollback_rotation = [&](bool report_oom) {
    dev->cmd.rollback(cmd_checkpoint);

    // Undo the rotation (rotate right by one).
    ResourceIdentity undo_saved = take_identity(resources[numResources - 1]);
    for (uint32_t i = numResources - 1; i > 0; --i) {
      put_identity(resources[i], take_identity(resources[i - 1]));
    }
    put_identity(resources[0], std::move(undo_saved));

    dev->current_rtv_count = prev_rtv_count;
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      dev->current_rtvs[i] = prev_rtvs[i];
      dev->current_rtv_resources[i] = prev_rtv_resources[i];
    }
    dev->current_dsv = prev_dsv;
    dev->current_dsv_res = prev_dsv_res;
    std::memcpy(dev->vs_srvs, prev_vs_srvs, sizeof(prev_vs_srvs));
    std::memcpy(dev->ps_srvs, prev_ps_srvs, sizeof(prev_ps_srvs));

    if (report_oom) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    }
  };

  // Capture the pre-rotation AeroGPU handles so we can remap bound SRV slots
  // (which store raw handles, not resource pointers).
  std::vector<aerogpu_handle_t> old_handles;
  try {
    old_handles.reserve(resources.size());
  } catch (...) {
    ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
    return;
  }
  for (auto* res : resources) {
    try {
      old_handles.push_back(res ? res->handle : 0);
    } catch (...) {
      ReportDeviceErrorLocked(dev, hDevice, E_OUTOFMEMORY);
      return;
    }
  }

  // Rotate the full resource identity bundle. This matches Win7 DXGI's
  // expectation that the *logical* backbuffer resource (buffer[0]) continues to
  // be used by the app across frames while the underlying allocation identity
  // flips.
  ResourceIdentity saved = take_identity(resources[0]);
  for (uint32_t i = 0; i + 1 < numResources; ++i) {
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

  // If the current render targets refer to a rotated resource, re-emit the bind
  // command so the next frame targets the new backbuffer identity.
  bool needs_rebind = false;
  for (AeroGpuResource* r : resources) {
    if (dev->current_dsv_res == r) {
      needs_rebind = true;
      break;
    }
    for (uint32_t slot = 0; slot < dev->current_rtv_count && slot < AEROGPU_MAX_RENDER_TARGETS; ++slot) {
      if (dev->current_rtv_resources[slot] == r) {
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
    for (uint32_t i = 0; i < prev_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      new_rtvs[i] = prev_rtvs[i];
    }
    for (uint32_t i = 0; i < prev_rtv_count && i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      new_rtvs[i] = remap_handle(new_rtvs[i]);
    }
    const aerogpu_handle_t new_dsv = remap_handle(prev_dsv);

    if (!EmitSetRenderTargetsCmdLocked(dev, prev_rtv_count, new_rtvs, new_dsv)) {
      rollback_rotation(/*report_oom=*/true);
      return;
    }
    for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
      dev->current_rtvs[i] = new_rtvs[i];
    }
    dev->current_dsv = new_dsv;
  }

  for (uint32_t slot = 0; slot < kMaxShaderResourceSlots; ++slot) {
    const aerogpu_handle_t new_vs = remap_handle(dev->vs_srvs[slot]);
    if (new_vs != dev->vs_srvs[slot]) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_VERTEX, slot, new_vs)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->vs_srvs[slot] = new_vs;
    }
    const aerogpu_handle_t new_ps = remap_handle(dev->ps_srvs[slot]);
    if (new_ps != dev->ps_srvs[slot]) {
      if (!set_texture_locked(dev, hDevice, AEROGPU_SHADER_STAGE_PIXEL, slot, new_ps)) {
        rollback_rotation(/*report_oom=*/false);
        return;
      }
      dev->ps_srvs[slot] = new_ps;
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
    AEROGPU_D3D10_RET_HR(E_INVALIDARG);
  }

  // Populate the device function table up-front so callers can still invoke
  // DestroyDevice even if CreateDevice fails (some runtimes call Destroy on
  // failure).
  AEROGPU_D3D10_11_DEVICEFUNCS funcs = {};
  funcs.pfnDestroyDevice = AEROGPU_D3D10_11_DDI(DestroyDevice);

  funcs.pfnCalcPrivateResourceSize = AEROGPU_D3D10_11_DDI(CalcPrivateResourceSize);
  funcs.pfnCreateResource = AEROGPU_D3D10_11_DDI(CreateResource);
  funcs.pfnDestroyResource = AEROGPU_D3D10_11_DDI(DestroyResource);

  funcs.pfnCalcPrivateShaderSize = AEROGPU_D3D10_11_DDI(CalcPrivateShaderSize);
  funcs.pfnCreateVertexShader = AEROGPU_D3D10_11_DDI(CreateVertexShader);
  funcs.pfnCreatePixelShader = AEROGPU_D3D10_11_DDI(CreatePixelShader);
  funcs.pfnDestroyShader = AEROGPU_D3D10_11_DDI(DestroyShader);

  funcs.pfnCalcPrivateInputLayoutSize = AEROGPU_D3D10_11_DDI(CalcPrivateInputLayoutSize);
  funcs.pfnCreateInputLayout = AEROGPU_D3D10_11_DDI(CreateInputLayout);
  funcs.pfnDestroyInputLayout = AEROGPU_D3D10_11_DDI(DestroyInputLayout);

  funcs.pfnCalcPrivateRTVSize = AEROGPU_D3D10_11_DDI(CalcPrivateRTVSize);
  funcs.pfnCreateRTV = AEROGPU_D3D10_11_DDI(CreateRTV);
  funcs.pfnDestroyRTV = AEROGPU_D3D10_11_DDI(DestroyRTV);

  funcs.pfnCalcPrivateDSVSize = AEROGPU_D3D10_11_DDI(CalcPrivateDSVSize);
  funcs.pfnCreateDSV = AEROGPU_D3D10_11_DDI(CreateDSV);
  funcs.pfnDestroyDSV = AEROGPU_D3D10_11_DDI(DestroyDSV);

  funcs.pfnCalcPrivateShaderResourceViewSize = AEROGPU_D3D10_11_DDI(CalcPrivateShaderResourceViewSize);
  funcs.pfnCreateShaderResourceView = AEROGPU_D3D10_11_DDI(CreateShaderResourceView);
  funcs.pfnDestroyShaderResourceView = AEROGPU_D3D10_11_DDI(DestroyShaderResourceView);

  funcs.pfnCalcPrivateSamplerSize = AEROGPU_D3D10_11_DDI(CalcPrivateSamplerSize);
  funcs.pfnCreateSampler = AEROGPU_D3D10_11_DDI(CreateSampler);
  funcs.pfnDestroySampler = AEROGPU_D3D10_11_DDI(DestroySampler);

  funcs.pfnCalcPrivateBlendStateSize = AEROGPU_D3D10_11_DDI(CalcPrivateBlendStateSize);
  funcs.pfnCreateBlendState = AEROGPU_D3D10_11_DDI(CreateBlendState);
  funcs.pfnDestroyBlendState = AEROGPU_D3D10_11_DDI(DestroyBlendState);

  funcs.pfnCalcPrivateRasterizerStateSize = AEROGPU_D3D10_11_DDI(CalcPrivateRasterizerStateSize);
  funcs.pfnCreateRasterizerState = AEROGPU_D3D10_11_DDI(CreateRasterizerState);
  funcs.pfnDestroyRasterizerState = AEROGPU_D3D10_11_DDI(DestroyRasterizerState);

  funcs.pfnCalcPrivateDepthStencilStateSize = AEROGPU_D3D10_11_DDI(CalcPrivateDepthStencilStateSize);
  funcs.pfnCreateDepthStencilState = AEROGPU_D3D10_11_DDI(CreateDepthStencilState);
  funcs.pfnDestroyDepthStencilState = AEROGPU_D3D10_11_DDI(DestroyDepthStencilState);

  funcs.pfnSetRenderTargets = AEROGPU_D3D10_11_DDI(SetRenderTargets);
  funcs.pfnClearRTV = AEROGPU_D3D10_11_DDI(ClearRTV);
  funcs.pfnClearDSV = AEROGPU_D3D10_11_DDI(ClearDSV);

  funcs.pfnSetInputLayout = AEROGPU_D3D10_11_DDI(SetInputLayout);
  funcs.pfnSetVertexBuffer = AEROGPU_D3D10_11_DDI(SetVertexBuffer);
  funcs.pfnSetIndexBuffer = AEROGPU_D3D10_11_DDI(SetIndexBuffer);
  funcs.pfnSetViewport = AEROGPU_D3D10_11_DDI(SetViewport);
  funcs.pfnSetViewports = AEROGPU_D3D10_11_DDI(SetViewports);
  funcs.pfnSetScissorRects = AEROGPU_D3D10_11_DDI(SetScissorRects);
  funcs.pfnSetDrawState = AEROGPU_D3D10_11_DDI(SetDrawState);
  funcs.pfnSetBlendState = AEROGPU_D3D10_11_DDI(SetBlendState);
  funcs.pfnSetRasterizerState = AEROGPU_D3D10_11_DDI(SetRasterizerState);
  funcs.pfnSetDepthStencilState = AEROGPU_D3D10_11_DDI(SetDepthStencilState);
  funcs.pfnSetPrimitiveTopology = AEROGPU_D3D10_11_DDI(SetPrimitiveTopology);

  funcs.pfnVsSetConstantBuffers = AEROGPU_D3D10_11_DDI(VsSetConstantBuffers);
  funcs.pfnPsSetConstantBuffers = AEROGPU_D3D10_11_DDI(PsSetConstantBuffers);
  funcs.pfnVsSetShaderResources = AEROGPU_D3D10_11_DDI(VsSetShaderResources);
  funcs.pfnPsSetShaderResources = AEROGPU_D3D10_11_DDI(PsSetShaderResources);
  funcs.pfnVsSetSamplers = AEROGPU_D3D10_11_DDI(VsSetSamplers);
  funcs.pfnPsSetSamplers = AEROGPU_D3D10_11_DDI(PsSetSamplers);

  funcs.pfnDraw = AEROGPU_D3D10_11_DDI(Draw);
  funcs.pfnDrawIndexed = AEROGPU_D3D10_11_DDI(DrawIndexed);
  funcs.pfnDrawInstanced = AEROGPU_D3D10_11_DDI(DrawInstanced);
  funcs.pfnDrawIndexedInstanced = AEROGPU_D3D10_11_DDI(DrawIndexedInstanced);
  funcs.pfnDrawAuto = AEROGPU_D3D10_11_DDI(DrawAuto);
  // Map/Unmap has both a "classic" D3D10-style signature and a D3D11-style
  // {args} pointer signature in this file; cast to disambiguate.
  funcs.pfnMap = aerogpu_d3d10_11_ddi_thunk<static_cast<PFNAEROGPU_DDI_MAP>(&Map)>::thunk;
  funcs.pfnUnmap = aerogpu_d3d10_11_ddi_thunk<static_cast<PFNAEROGPU_DDI_UNMAP>(&Unmap)>::thunk;
  funcs.pfnPresent = AEROGPU_D3D10_11_DDI(Present);
  funcs.pfnFlush = AEROGPU_D3D10_11_DDI(Flush);
  funcs.pfnRotateResourceIdentities = AEROGPU_D3D10_11_DDI(RotateResourceIdentities);
  funcs.pfnUpdateSubresourceUP = AEROGPU_D3D10_11_DDI(UpdateSubresourceUP);
  funcs.pfnCopyResource = AEROGPU_D3D10_11_DDI(CopyResource);
  funcs.pfnCopySubresourceRegion = AEROGPU_D3D10_11_DDI(CopySubresourceRegion);

  // Map/unmap. Win7 D3D11 runtimes may use specialized entrypoints.
  funcs.pfnStagingResourceMap = AEROGPU_D3D10_11_DDI(StagingResourceMap);
  funcs.pfnStagingResourceUnmap = AEROGPU_D3D10_11_DDI(StagingResourceUnmap);
  funcs.pfnDynamicIABufferMapDiscard = AEROGPU_D3D10_11_DDI(DynamicIABufferMapDiscard);
  funcs.pfnDynamicIABufferMapNoOverwrite = AEROGPU_D3D10_11_DDI(DynamicIABufferMapNoOverwrite);
  funcs.pfnDynamicIABufferUnmap = AEROGPU_D3D10_11_DDI(DynamicIABufferUnmap);
  funcs.pfnDynamicConstantBufferMapDiscard = AEROGPU_D3D10_11_DDI(DynamicConstantBufferMapDiscard);
  funcs.pfnDynamicConstantBufferUnmap = AEROGPU_D3D10_11_DDI(DynamicConstantBufferUnmap);

  // The runtime-provided device function table is typically a superset of the
  // subset we populate here. Ensure the full table is zeroed first so any
  // unimplemented entrypoints are nullptr (instead of uninitialized garbage),
  // then copy the implemented prefix.
  std::memset(pCreateDevice->pDeviceFuncs, 0, sizeof(*pCreateDevice->pDeviceFuncs));
  std::memcpy(out_funcs, &funcs, sizeof(funcs));

  // Always construct the private device object so DestroyDevice is safe even if
  // we later reject the adapter handle.
  auto* device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = nullptr;
  device->device_callbacks = pCreateDevice->pDeviceCallbacks;

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    AEROGPU_D3D10_RET_HR(E_FAIL);
  }
  device->adapter = adapter;
  // Initialize the command stream header now that device creation is confirmed.
  device->cmd.reset();
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

HRESULT AEROGPU_APIENTRY GetCaps(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_GETCAPS* pGetCaps) {
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

  const auto* adapter = hAdapter.pDrvPrivate ? FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter) : nullptr;

  switch (type) {
    case kD3D11DdiCapsTypeFeatureLevels: {
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

    case kD3D11DdiCapsTypeThreading:
    case kD3D11DdiCapsTypeDoubles:
    case kD3D11DdiCapsTypeD3D10XHardwareOptions:
    case kD3D11DdiCapsTypeD3D11Options:
    case kD3D11DdiCapsTypeArchitectureInfo:
    case kD3D11DdiCapsTypeD3D9Options: {
      // Conservative: report "not supported" for everything (all fields zero).
      std::memset(data, 0, data_size);
      return S_OK;
    }

    case kD3D11DdiCapsTypeFormatSupport: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT)) {
        return E_INVALIDARG;
      }
      auto* fs = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT*>(data);
      fs->OutFormatSupport = d3d11_format_support_flags(adapter, fs->InFormat);
      return S_OK;
    }

    case kD3D11DdiCapsTypeFormatSupport2: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT2)) {
        return E_INVALIDARG;
      }
      auto* fs = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_FORMAT_SUPPORT2*>(data);
      fs->OutFormatSupport2 = 0;
      return S_OK;
    }

    case kD3D11DdiCapsTypeMultisampleQualityLevels: {
      if (data_size < sizeof(AEROGPU_D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS)) {
        return E_INVALIDARG;
      }
      auto* ms = reinterpret_cast<AEROGPU_D3D11_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS*>(data);
      // No MSAA support yet; report only the implicit 1x case.
      const bool supported_format = aerogpu::d3d10_11::AerogpuSupportsMultisampleQualityLevels(adapter, ms->Format);
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
  aerogpu::d3d10_11::LogModulePathOnce();
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

  // Portable build: assume we are running against an in-repo AeroGPU host that
  // supports the current protocol ABI. This keeps format support (BC/sRGB) and
  // transfer/copy feature gates in sync with the WDK UMDs.
  adapter->umd_private_valid = true;
  adapter->umd_private.size_bytes = sizeof(adapter->umd_private);
  adapter->umd_private.struct_version = AEROGPU_UMDPRIV_STRUCT_VERSION_V1;
  adapter->umd_private.device_mmio_magic = 0;
  adapter->umd_private.device_abi_version_u32 = AEROGPU_ABI_VERSION_U32;
  adapter->umd_private.device_features = AEROGPU_UMDPRIV_FEATURE_TRANSFER;
  adapter->umd_private.flags = 0;
 
 
  D3D10DDI_ADAPTERFUNCS funcs = {};
  funcs.pfnGetCaps = AEROGPU_D3D10_11_DDI(GetCaps);
  funcs.pfnCalcPrivateDeviceSize = AEROGPU_D3D10_11_DDI(CalcPrivateDeviceSize);
  funcs.pfnCreateDevice = AEROGPU_D3D10_11_DDI(CreateDevice);
  funcs.pfnCloseAdapter = AEROGPU_D3D10_11_DDI(CloseAdapter);

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
  try {
    return OpenAdapterCommon(pOpenData);
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter10_2");
  try {
    return OpenAdapterCommon(pOpenData);
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  AEROGPU_D3D10_11_LOG_CALL();
  AEROGPU_D3D10_TRACEF("OpenAdapter11");
  try {
    return OpenAdapterCommon(pOpenData);
  } catch (const std::bad_alloc&) {
    return E_OUTOFMEMORY;
  } catch (...) {
    return E_FAIL;
  }
}

} // extern "C"


#endif // WDK build exclusion guard (this TU is portable-only)
