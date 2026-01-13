#pragma once

#include <atomic>
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <limits>
#include <condition_variable>
#include <deque>
#include <memory>
#include <mutex>
#include <unordered_map>
#include <vector>

#include "../include/aerogpu_d3d9_umd.h"

#include "aerogpu_kmd_query.h"
#include "aerogpu_cmd_writer.h"
#include "aerogpu_d3d9_shared_resource.h"
#include "aerogpu_wddm_context.h"
#include "aerogpu_wddm_alloc_list.h"

namespace aerogpu {

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Surface = 2,
  Texture2D = 3,
};

// Device-lost reason code (best-effort diagnostic). Once the device enters a
// lost state, key DDIs return a stable device-lost HRESULT (D3DERR_DEVICELOST)
// and command submission stops.
enum class DeviceLostReason : uint32_t {
  None = 0,
  // WDDM submission callback failure for a render submission.
  WddmSubmitRender = 1,
  // WDDM submission callback failure for a present submission.
  WddmSubmitPresent = 2,
};

inline uint32_t bytes_per_pixel(D3DDDIFORMAT d3d9_format) {
  // Conservative: handle the formats DWM/typical D3D9 samples use.
  // For unknown formats we assume 4 bytes to avoid undersizing.
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8 / D3DFMT_A8B8G8R8
    case 21u:
    case 22u:
    case 32u:
      return 4;
    // D3DFMT_R5G6B5 / D3DFMT_X1R5G5B5 / D3DFMT_A1R5G5B5
    case 23u:
    case 24u:
    case 25u:
      return 2;
    // D3DFMT_A8
    case 28u:
      return 1;
    // D3DFMT_D24S8
    case 75u:
      return 4;
    default:
      return 4;
  }
}

// D3D9 compressed texture formats are defined as FOURCC codes (D3DFORMAT values).
// Keep local definitions so portable builds don't require the Windows SDK/WDK.
inline constexpr uint32_t d3d9_make_fourcc(char a, char b, char c, char d) {
  return static_cast<uint32_t>(static_cast<uint8_t>(a)) |
         (static_cast<uint32_t>(static_cast<uint8_t>(b)) << 8) |
         (static_cast<uint32_t>(static_cast<uint8_t>(c)) << 16) |
         (static_cast<uint32_t>(static_cast<uint8_t>(d)) << 24);
}

inline constexpr D3DDDIFORMAT kD3dFmtDxt1 = static_cast<D3DDDIFORMAT>(d3d9_make_fourcc('D', 'X', 'T', '1')); // D3DFMT_DXT1
inline constexpr D3DDDIFORMAT kD3dFmtDxt2 = static_cast<D3DDDIFORMAT>(d3d9_make_fourcc('D', 'X', 'T', '2')); // D3DFMT_DXT2 (premul alpha)
inline constexpr D3DDDIFORMAT kD3dFmtDxt3 = static_cast<D3DDDIFORMAT>(d3d9_make_fourcc('D', 'X', 'T', '3')); // D3DFMT_DXT3
inline constexpr D3DDDIFORMAT kD3dFmtDxt4 = static_cast<D3DDDIFORMAT>(d3d9_make_fourcc('D', 'X', 'T', '4')); // D3DFMT_DXT4 (premul alpha)
inline constexpr D3DDDIFORMAT kD3dFmtDxt5 = static_cast<D3DDDIFORMAT>(d3d9_make_fourcc('D', 'X', 'T', '5')); // D3DFMT_DXT5

inline bool is_block_compressed_format(D3DDDIFORMAT d3d9_format) {
  switch (static_cast<uint32_t>(d3d9_format)) {
    case static_cast<uint32_t>(kD3dFmtDxt1):
    case static_cast<uint32_t>(kD3dFmtDxt2):
    case static_cast<uint32_t>(kD3dFmtDxt3):
    case static_cast<uint32_t>(kD3dFmtDxt4):
    case static_cast<uint32_t>(kD3dFmtDxt5):
      return true;
    default:
      return false;
  }
}

// Returns the number of bytes per 4x4 block for BC/DXT formats, or 0 if the
// format is not block-compressed.
inline uint32_t block_bytes_per_4x4(D3DDDIFORMAT d3d9_format) {
  switch (static_cast<uint32_t>(d3d9_format)) {
    case static_cast<uint32_t>(kD3dFmtDxt1):
      return 8; // BC1/DXT1
    case static_cast<uint32_t>(kD3dFmtDxt2): // BC2/DXT3 family (premul alpha not represented in protocol format)
    case static_cast<uint32_t>(kD3dFmtDxt3):
    case static_cast<uint32_t>(kD3dFmtDxt4): // BC3/DXT5 family (premul alpha not represented in protocol format)
    case static_cast<uint32_t>(kD3dFmtDxt5):
      return 16; // BC2/BC3
    default:
      return 0;
  }
}

// Maps a D3D9 format (D3DFORMAT / D3DDDIFORMAT numeric value) to an AeroGPU
// protocol format (`enum aerogpu_format`).
//
// NOTE: Portable builds do not include the Windows SDK/WDK, so callers should
// pass the numeric D3DFORMAT value (e.g. 21u for D3DFMT_A8R8G8B8).
inline uint32_t d3d9_format_to_aerogpu(uint32_t d3d9_format) {
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8
    case 21u:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case 22u:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    // D3DFMT_R5G6B5
    case 23u:
      return AEROGPU_FORMAT_B5G6R5_UNORM;
    // D3DFMT_X1R5G5B5 / D3DFMT_A1R5G5B5
    //
    // Note: X1R5G5B5 has no alpha channel; map it to B5G5R5A1 and treat the
    // alpha bit as "opaque" (D3D9 semantics are equivalent to alpha=1). The UMD
    // also fixes up CPU writes for X1 formats to set the top bit so texture sampling observes
    // opaque alpha.
    case 24u:
    case 25u:
      return AEROGPU_FORMAT_B5G5R5A1_UNORM;
    // D3DFMT_A8B8G8R8
    case 32u:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    // D3DFMT_D24S8
    case 75u:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    // D3DFMT_DXT1/DXT2/DXT3/DXT4/DXT5 (FOURCC codes; see d3d9_make_fourcc above).
    case static_cast<uint32_t>(kD3dFmtDxt1):
      return AEROGPU_FORMAT_BC1_RGBA_UNORM;
    // DXT2 is the premultiplied-alpha variant of DXT3. AeroGPU does not encode
    // alpha-premultiplication at the format level, so treat it as BC2.
    case static_cast<uint32_t>(kD3dFmtDxt2):
    case static_cast<uint32_t>(kD3dFmtDxt3):
      return AEROGPU_FORMAT_BC2_RGBA_UNORM;
    // DXT4 is the premultiplied-alpha variant of DXT5. AeroGPU does not encode
    // alpha-premultiplication at the format level, so treat it as BC3.
    case static_cast<uint32_t>(kD3dFmtDxt4):
    case static_cast<uint32_t>(kD3dFmtDxt5):
      return AEROGPU_FORMAT_BC3_RGBA_UNORM;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

struct Texture2dLayout {
  uint32_t row_pitch_bytes = 0;
  uint32_t slice_pitch_bytes = 0;
  uint64_t total_size_bytes = 0;
};

// D3D9 CreateTexture semantics: MipLevels=0 means "allocate the full mip chain".
// For 2D textures that is:
//   floor(log2(max(width, height))) + 1
// Clamped to at least 1.
inline uint32_t calc_full_mip_chain_levels_2d(uint32_t width, uint32_t height) {
  const uint32_t max_dim = std::max(width, height);
  uint32_t levels = 0;
  uint32_t v = max_dim;
  while (v) {
    ++levels;
    v >>= 1;
  }
  return std::max(1u, levels);
}

struct Texture2dMipLevelLayout {
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t row_pitch_bytes = 0;
  uint32_t slice_pitch_bytes = 0;
  uint64_t offset_bytes = 0;
};
// Computes the packed linear layout for a 2D texture mip chain (as used by the
// AeroGPU protocol).
//
// - For uncompressed formats: row_pitch = width * bytes_per_pixel.
// - For block-compressed formats: row_pitch is measured in 4x4 blocks.
//
// Returns false on overflow / invalid inputs.
inline bool calc_texture2d_layout(
    D3DDDIFORMAT format,
    uint32_t width,
    uint32_t height,
    uint32_t mip_levels,
    uint32_t depth,
    Texture2dLayout* out) {
  if (!out) {
    return false;
  }

  width = std::max(1u, width);
  height = std::max(1u, height);
  mip_levels = std::max(1u, mip_levels);
  depth = std::max(1u, depth);

  uint32_t w = width;
  uint32_t h = height;
  uint64_t total = 0;
  uint32_t row0 = 0;
  uint32_t slice0 = 0;

  for (uint32_t level = 0; level < mip_levels; ++level) {
    uint64_t row_pitch = 0;
    uint64_t slice_pitch = 0;

    if (is_block_compressed_format(format)) {
      const uint32_t block_bytes = block_bytes_per_4x4(format);
      if (block_bytes == 0) {
        return false;
      }

      const uint32_t blocks_w = std::max(1u, (w + 3u) / 4u);
      const uint32_t blocks_h = std::max(1u, (h + 3u) / 4u);

      row_pitch = static_cast<uint64_t>(blocks_w) * block_bytes;
      if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
        return false;
      }
      slice_pitch = row_pitch * blocks_h;
    } else {
      const uint32_t bpp = bytes_per_pixel(format);
      row_pitch = static_cast<uint64_t>(w) * bpp;
      if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
        return false;
      }
      slice_pitch = row_pitch * h;
    }

    if (slice_pitch == 0 || slice_pitch > 0xFFFFFFFFull) {
      return false;
    }

    if (level == 0) {
      row0 = static_cast<uint32_t>(row_pitch);
      slice0 = static_cast<uint32_t>(slice_pitch);
    }

    if (total > UINT64_MAX - slice_pitch) {
      return false;
    }
    total += slice_pitch;
    w = std::max(1u, w / 2);
    h = std::max(1u, h / 2);
  }

  if (depth != 0 && total > UINT64_MAX / static_cast<uint64_t>(depth)) {
    return false;
  }
  total *= static_cast<uint64_t>(depth);

  out->row_pitch_bytes = row0;
  out->slice_pitch_bytes = slice0;
  out->total_size_bytes = total;
  return true;
}

// Computes the packed linear layout for a specific mip level of a 2D texture mip
// chain.
//
// Returns false on overflow / invalid inputs.
//
// Notes:
// - `offset_bytes` is the byte offset within the *first* array layer (depth slice)
//   of the texture. For depth/array-layer counts > 1, callers can treat the
//   packed resource as:
//     layer_offset = layer_index * layer_size_bytes
//     subresource_offset = layer_offset + level.offset_bytes
inline bool calc_texture2d_mip_level_layout(
    D3DDDIFORMAT format,
    uint32_t width,
    uint32_t height,
    uint32_t mip_levels,
    uint32_t depth,
    uint32_t level,
    Texture2dMipLevelLayout* out) {
  if (!out) {
    return false;
  }

  width = std::max(1u, width);
  height = std::max(1u, height);
  mip_levels = std::max(1u, mip_levels);
  depth = std::max(1u, depth);

  if (level >= mip_levels) {
    return false;
  }

  uint32_t w = width;
  uint32_t h = height;
  uint64_t offset = 0;

  for (uint32_t cur_level = 0; cur_level < mip_levels; ++cur_level) {
    uint64_t row_pitch = 0;
    uint64_t slice_pitch = 0;

    if (is_block_compressed_format(format)) {
      const uint32_t block_bytes = block_bytes_per_4x4(format);
      if (block_bytes == 0) {
        return false;
      }

      const uint32_t blocks_w = std::max(1u, (w + 3u) / 4u);
      const uint32_t blocks_h = std::max(1u, (h + 3u) / 4u);

      row_pitch = static_cast<uint64_t>(blocks_w) * block_bytes;
      if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
        return false;
      }
      slice_pitch = row_pitch * blocks_h;
    } else {
      const uint32_t bpp = bytes_per_pixel(format);
      row_pitch = static_cast<uint64_t>(w) * bpp;
      if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
        return false;
      }
      slice_pitch = row_pitch * h;
    }

    if (slice_pitch == 0 || slice_pitch > 0xFFFFFFFFull) {
      return false;
    }

    if (cur_level == level) {
      out->width = w;
      out->height = h;
      out->row_pitch_bytes = static_cast<uint32_t>(row_pitch);
      out->slice_pitch_bytes = static_cast<uint32_t>(slice_pitch);
      out->offset_bytes = offset;
      return true;
    }

    if (offset > std::numeric_limits<uint64_t>::max() - slice_pitch) {
      return false;
    }
    offset += slice_pitch;
    w = std::max(1u, w / 2);
    h = std::max(1u, h / 2);
  }

  return false;
}

struct Texture2dSubresourceLayout {
  uint32_t row_pitch_bytes = 0;
  uint32_t slice_pitch_bytes = 0;
  uint64_t subresource_start_bytes = 0;
  uint64_t subresource_end_bytes = 0;
};

// Computes the row/slice pitch for the texture subresource that contains
// `offset_bytes` in the packed linear layout used by the AeroGPU protocol.
//
// This is required for LockRect on mipmapped and/or layered textures: the D3D9
// runtime expects RowPitch/SlicePitch to match the mip level being locked, not
// always mip 0.
inline bool calc_texture2d_subresource_layout_for_offset(
    D3DDDIFORMAT format,
    uint32_t width,
    uint32_t height,
    uint32_t mip_levels,
    uint32_t array_layers,
    uint64_t offset_bytes,
    Texture2dSubresourceLayout* out) {
  if (!out) {
    return false;
  }

  width = std::max(1u, width);
  height = std::max(1u, height);
  mip_levels = std::max(1u, mip_levels);
  array_layers = std::max(1u, array_layers);

  uint64_t layer_base = 0;
  for (uint32_t layer = 0; layer < array_layers; ++layer) {
    uint32_t w = width;
    uint32_t h = height;

    uint64_t level_base = layer_base;
    for (uint32_t level = 0; level < mip_levels; ++level) {
      uint64_t row_pitch = 0;
      uint64_t slice_pitch = 0;

      if (is_block_compressed_format(format)) {
        const uint32_t block_bytes = block_bytes_per_4x4(format);
        if (block_bytes == 0) {
          return false;
        }
        const uint32_t blocks_w = std::max(1u, (w + 3u) / 4u);
        const uint32_t blocks_h = std::max(1u, (h + 3u) / 4u);
        row_pitch = static_cast<uint64_t>(blocks_w) * block_bytes;
        slice_pitch = row_pitch * blocks_h;
      } else {
        const uint32_t bpp = bytes_per_pixel(format);
        row_pitch = static_cast<uint64_t>(w) * bpp;
        slice_pitch = row_pitch * h;
      }

      if (row_pitch == 0 || slice_pitch == 0) {
        return false;
      }
      if (row_pitch > 0xFFFFFFFFull || slice_pitch > 0xFFFFFFFFull) {
        return false;
      }

      const uint64_t start = level_base;
      if (start > std::numeric_limits<uint64_t>::max() - slice_pitch) {
        return false;
      }
      const uint64_t end = start + slice_pitch;
      if (offset_bytes >= start && offset_bytes < end) {
        out->row_pitch_bytes = static_cast<uint32_t>(row_pitch);
        out->slice_pitch_bytes = static_cast<uint32_t>(slice_pitch);
        out->subresource_start_bytes = start;
        out->subresource_end_bytes = end;
        return true;
      }

      level_base = end;
      w = std::max(1u, w / 2);
      h = std::max(1u, h / 2);
    }

    layer_base = level_base;
  }
  return false;
}

struct Resource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;
  uint32_t type = 0;
  D3DDDIFORMAT format = static_cast<D3DDDIFORMAT>(0);
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t depth = 0;
  uint32_t mip_levels = 1;
  uint32_t usage = 0;
  uint32_t pool = 0;
  uint32_t size_bytes = 0;
  uint32_t row_pitch = 0;
  uint32_t slice_pitch = 0;

  // Host-visible backing allocation ID carried in per-allocation private driver
  // data (aerogpu_wddm_alloc_priv). 0 means "host allocated" (no
  // allocation-table entry).
  uint32_t backing_alloc_id = 0;

  // Optional offset into the backing allocation (bytes). Most D3D9Ex shared
  // surfaces are a single allocation with offset 0, but keeping this explicit
  // makes it possible to alias suballocations later.
  uint32_t backing_offset_bytes = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  bool is_shared = false;
  bool is_shared_alias = false;

  bool locked = false;
  uint32_t locked_offset = 0;
  uint32_t locked_size = 0;
  uint32_t locked_flags = 0;
  void* locked_ptr = nullptr;

  // WDDM allocation handle for this resource's backing store (per-process).
  // The stable ID referenced in command buffers is `backing_alloc_id`.
  WddmAllocationHandle wddm_hAllocation = 0;

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  // Legacy resource properties (cached only, not currently emitted to the
  // AeroGPU command stream).
  uint32_t priority = 0;
  uint32_t auto_gen_filter_type = 2u; // D3DTEXF_LINEAR
#endif

  std::vector<uint8_t> storage;
  std::vector<uint8_t> shared_private_driver_data;
};

struct SwapChain {
  aerogpu_handle_t handle = 0;
  HWND hwnd = nullptr;

  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t format = 0;
  uint32_t sync_interval = 0;
  uint32_t swap_effect = 0;
  uint32_t flags = 0;

  std::vector<Resource*> backbuffers;

  uint64_t present_count = 0;
  uint64_t last_present_fence = 0;
};

struct Shader {
  aerogpu_handle_t handle = 0;
  uint32_t stage = AEROGPU_SHADER_STAGE_VERTEX;
  std::vector<uint8_t> bytecode;
};

struct VertexDecl {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct Query {
  uint32_t type = 0;
  std::atomic<uint64_t> fence_value{0};
  // True once the query is eligible to observe its `fence_value` via GetData.
  //
  // For D3D9Ex EVENT queries, `Issue(END)` does not necessarily flush commands
  // to the kernel. DWM relies on polling `GetData(DONOTFLUSH)` without forcing
  // a submission; in that state the query must report "not ready" even if the
  // GPU is idle. We therefore keep EVENT queries "unsubmitted" until an
  // explicit flush/submission boundary (Flush/Present/etc) marks them ready.
  //
  // Note: in some paths we may already know the fence value (because the UMD
  // submitted work for other reasons), but we still keep the query unsubmitted
  // so the first DONOTFLUSH poll reports not-ready.
  std::atomic<bool> submitted{false};
  std::atomic<bool> issued{false};
  std::atomic<bool> completion_logged{false};
};

// Forward declaration for D3D9 state-block support (defined in
// aerogpu_d3d9_driver.cpp). State blocks are lifetime-managed by the D3D9
// runtime via CreateStateBlock/DeleteStateBlock and BeginStateBlock/EndStateBlock.
struct StateBlock;

struct Adapter {
  // The adapter LUID used for caching/reuse when the runtime opens the same
  // adapter multiple times (common with D3D9Ex + DWM).
  LUID luid = {};

  // Best-effort VidPnSourceId corresponding to the active display output for
  // this adapter. Populated when available via D3DKMTOpenAdapterFromHdc.
  //
  // Used to improve vblank waits (D3DKMTGetScanLine). If unknown, code should
  // fall back to a time-based sleep.
  uint32_t vid_pn_source_id = 0;
  bool vid_pn_source_id_valid = false;

  // Reference count for OpenAdapter* / CloseAdapter bookkeeping.
  std::atomic<uint32_t> open_count{0};

  // Runtime callback tables provided during OpenAdapter*.
  // Stored as raw pointers; the tables live for the lifetime of the runtime.
  D3DDDI_ADAPTERCALLBACKS* adapter_callbacks = nullptr;
  D3DDDI_ADAPTERCALLBACKS2* adapter_callbacks2 = nullptr;
  // Also store by-value copies so adapter code can safely reference callbacks
  // even if the runtime decides to re-home the tables (observed on some
  // configurations).
  D3DDDI_ADAPTERCALLBACKS adapter_callbacks_copy = {};
  D3DDDI_ADAPTERCALLBACKS2 adapter_callbacks2_copy = {};
  bool adapter_callbacks_valid = false;
  bool adapter_callbacks2_valid = false;

  UINT interface_version = 0;
  UINT umd_version = 0;

  std::atomic<uint32_t> next_handle{1};
  // UMD-owned allocation IDs used in WDDM allocation private driver data
  // (aerogpu_wddm_alloc_priv.alloc_id).
  std::atomic<uint32_t> next_alloc_id{1};
  // KMD-advertised max allocation-list slot-id (DXGK_DRIVERCAPS::MaxAllocationListSlotId).
  // AeroGPU's Win7 KMD currently reports 0xFFFF.
  uint32_t max_allocation_list_slot_id = 0xFFFFu;
  // Logging guard so we only emit the driver-caps-derived value once per adapter.
  std::atomic<bool> max_allocation_list_slot_id_logged{false};

  // 64-bit token generator for shared-surface interop (EXPORT/IMPORT_SHARED_SURFACE).
  ShareTokenAllocator share_token_allocator;

  // Different D3D9 runtimes/headers may use different numeric encodings for the
  // EVENT query type at the DDI boundary. Once we observe the first EVENT query
  // type value we lock it in per-adapter, so we don't accidentally treat other
  // query types (e.g. pipeline stats) as EVENT.
  std::atomic<bool> event_query_type_known{false};
  std::atomic<uint32_t> event_query_type{0};

  // Monotonic cross-process token allocator used to derive stable IDs across
  // guest processes. The D3D9 UMD uses it primarily to derive stable 31-bit
  // `alloc_id` values for shared allocations.
  //
  // The D3D9 UMD may be loaded into multiple guest processes (DWM + apps), so we
  // coordinate token allocation cross-process via a named file mapping (see
  // aerogpu_d3d9_driver.cpp).
  std::mutex share_token_mutex;
  HANDLE share_token_mapping = nullptr;
  void* share_token_view = nullptr;
  std::atomic<uint64_t> next_share_token{1}; // Fallback if cross-process allocator fails.

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t last_submitted_fence = 0;
  uint64_t completed_fence = 0;
  // Diagnostics: number of non-empty submissions issued by the UMD. These are
  // tracked under `fence_mutex` so host-side tests can assert submit ordering
  // (render vs present) without relying solely on fence deltas.
  uint64_t render_submit_count = 0;
  uint64_t present_submit_count = 0;

  // Optional best-effort KMD query path (Win7 user-mode D3DKMTEscape).
  // NOTE: Querying via D3DKMTEscape is relatively expensive; callers should use
  // a cached snapshot unless they truly need to refresh.
  std::atomic<bool> kmd_query_available{false};
  uint64_t last_kmd_fence_query_ms = 0;
  AerogpuKmdQuery kmd_query;

  // Cached KMD UMDRIVERPRIVATE discovery blob (queried via D3DKMTQueryAdapterInfo).
  // If this is populated, the UMD can make runtime decisions based on the active
  // AeroGPU MMIO ABI (legacy "ARGP" vs new "AGPU") and the reported feature bits.
  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;
  // Primary display mode as reported via GetDisplayModeEx. Initialized when the
  // runtime opens the adapter from an HDC (best-effort).
  uint32_t primary_width = 1024;
  uint32_t primary_height = 768;
  uint32_t primary_refresh_hz = 60;
  uint32_t primary_format = 22u; // D3DFMT_X8R8G8B8
  uint32_t primary_rotation = D3DDDI_ROTATION_IDENTITY;
};

struct DeviceStateStream {
  Resource* vb = nullptr;
  uint32_t offset_bytes = 0;
  uint32_t stride_bytes = 0;
};

// Per-device patch handle cache for DrawRectPatch/DrawTriPatch.
//
// D3D9 patch handles are app-supplied integers that the driver can use as an
// optional cache key to avoid re-tessellating patches when the handle is reused
// with identical parameters.
enum class PatchKind : uint8_t {
  Rect = 0,
  Tri = 1,
};

struct PatchCacheSignature {
  PatchKind kind = PatchKind::Rect;
  uint32_t fvf = 0;
  uint32_t stride_bytes = 0;

  uint32_t start_vertex_offset = 0;
  uint32_t num_vertices = 0;
  uint32_t basis = 0;
  uint32_t degree = 0;

  // Bitwise float encodings of the segment-count array (rect: 4, tri: 3).
  uint32_t seg_bits[4] = {};

  uint64_t control_point_hash = 0;
};

struct PatchCacheEntry {
  PatchCacheSignature sig{};
  std::vector<uint8_t> vertices;     // tessellated vertices in the source vertex format
  std::vector<uint16_t> indices_u16; // triangle-list indices
};

struct Device {
  explicit Device(Adapter* adapter) : adapter(adapter) {
    // In WDK builds the runtime provides the DMA command buffer later during
    // device/context creation, so defer command stream initialization until the
    // buffer is bound (avoid any std::vector allocation in the WDDM path).
#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
    cmd.reset();
#endif

    // Initialize D3D9 state caches to API defaults so helper paths can save and
    // restore state even if the runtime never explicitly sets it.
    //
    // Render state defaults (numeric values from d3d9types.h).
    // - COLORWRITEENABLE = 0xF (RGBA)
    // - SRCBLEND = ONE (2)
    // - DESTBLEND = ZERO (1)
    // - BLENDOP = ADD (1)
    // - ZENABLE = TRUE (1)
    // - ZWRITEENABLE = TRUE (1)
    // - CULLMODE = CCW (3)
    render_states[168] = 0xFu; // D3DRS_COLORWRITEENABLE
    render_states[19] = 2u;    // D3DRS_SRCBLEND
    render_states[20] = 1u;    // D3DRS_DESTBLEND
    render_states[171] = 1u;   // D3DRS_BLENDOP
    render_states[7] = 1u;     // D3DRS_ZENABLE
    render_states[14] = 1u;    // D3DRS_ZWRITEENABLE
    render_states[22] = 3u;    // D3DRS_CULLMODE

    // Sampler defaults per stage:
    // - ADDRESSU/V = WRAP (1)
    // - MIN/MAG = POINT (1)
    // - MIP = NONE (0)
    for (uint32_t stage = 0; stage < 16; ++stage) {
      sampler_states[stage][1] = 1u; // D3DSAMP_ADDRESSU
      sampler_states[stage][2] = 1u; // D3DSAMP_ADDRESSV
      sampler_states[stage][5] = 1u; // D3DSAMP_MAGFILTER
      sampler_states[stage][6] = 1u; // D3DSAMP_MINFILTER
      sampler_states[stage][7] = 0u; // D3DSAMP_MIPFILTER
    }

    // Texture stage state defaults (numeric values from d3d9types.h).
    //
    // These are fixed-function states. Most are cached-only (GetTextureStageState
    // + state blocks), but stage0 is consulted by the UMD's minimal fixed-function
    // fallback path to select a pixel shader variant.
    //
    // D3DTEXTUREOP:
    // - DISABLE = 1
    // - SELECTARG1 = 2
    // - MODULATE = 4
    //
    // D3DTA_* source selector:
    // - DIFFUSE = 0
    // - TEXTURE = 2
    constexpr uint32_t kD3dTssColorOp = 1u;
    constexpr uint32_t kD3dTssColorArg1 = 2u;
    constexpr uint32_t kD3dTssColorArg2 = 3u;
    constexpr uint32_t kD3dTssAlphaOp = 4u;
    constexpr uint32_t kD3dTssAlphaArg1 = 5u;
    constexpr uint32_t kD3dTssAlphaArg2 = 6u;

    constexpr uint32_t kD3dTopDisable = 1u;
    constexpr uint32_t kD3dTopSelectArg1 = 2u;
    constexpr uint32_t kD3dTopModulate = 4u;

    constexpr uint32_t kD3dTaDiffuse = 0u;
    constexpr uint32_t kD3dTaTexture = 2u;

    for (uint32_t stage = 0; stage < 16; ++stage) {
      const bool stage0 = (stage == 0);
      texture_stage_states[stage][kD3dTssColorOp] = stage0 ? kD3dTopModulate : kD3dTopDisable;
      texture_stage_states[stage][kD3dTssColorArg1] = kD3dTaTexture;
      texture_stage_states[stage][kD3dTssColorArg2] = kD3dTaDiffuse;
      texture_stage_states[stage][kD3dTssAlphaOp] = stage0 ? kD3dTopSelectArg1 : kD3dTopDisable;
      texture_stage_states[stage][kD3dTssAlphaArg1] = kD3dTaTexture;
      texture_stage_states[stage][kD3dTssAlphaArg2] = kD3dTaDiffuse;
    }

    // Default stream source frequency is 1 (no instancing).
    for (uint32_t stream = 0; stream < 16; ++stream) {
      stream_source_freq[stream] = 1u;
    }

    // Default transform state is identity for all cached slots.
    for (uint32_t i = 0; i < kTransformCacheCount; ++i) {
      float* m = transform_matrices[i];
      m[0] = 1.0f;
      m[5] = 1.0f;
      m[10] = 1.0f;
      m[15] = 1.0f;
    }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    // Default fixed-function material is white.
    std::memset(&material, 0, sizeof(material));
    material.Diffuse.r = 1.0f;
    material.Diffuse.g = 1.0f;
    material.Diffuse.b = 1.0f;
    material.Diffuse.a = 1.0f;
    material.Ambient = material.Diffuse;
    material_valid = true;

    for (uint32_t i = 0; i < kMaxLights; ++i) {
      std::memset(&lights[i], 0, sizeof(lights[i]));
      light_valid[i] = false;
      light_enabled[i] = FALSE;
    }

    // Default gamma ramp is identity.
    std::memset(&gamma_ramp, 0, sizeof(gamma_ramp));
    WORD* ramp_words = reinterpret_cast<WORD*>(&gamma_ramp);
    for (uint32_t i = 0; i < 256; ++i) {
      const WORD v = static_cast<WORD>(i * 257u);
      ramp_words[i] = v;
      ramp_words[256u + i] = v;
      ramp_words[512u + i] = v;
    }
    gamma_ramp_valid = true;

    // Clip status and palettes start out as "unset" (zeroes).
    std::memset(&clip_status, 0, sizeof(clip_status));
    clip_status_valid = false;

    for (uint32_t p = 0; p < kMaxPalettes; ++p) {
      std::memset(&palette_entries[p][0], 0, sizeof(palette_entries[p]));
      palette_valid[p] = false;
    }
    current_texture_palette = 0;
#endif
  }

  Adapter* adapter = nullptr;
  std::mutex mutex;

  // Device-lost tracking (sticky).
  //
  // In WDDM builds, if the runtime submission callback (Render/Present/SubmitCommand)
  // fails, the UMD marks the device as lost so DWM/apps observe a stable failure
  // code instead of spinning on fence==0 / "trivially complete" queries.
  std::atomic<bool> device_lost{false};
  // HRESULT returned by the failing submission callback.
  std::atomic<int32_t> device_lost_hr{S_OK};
  std::atomic<uint32_t> device_lost_reason{static_cast<uint32_t>(DeviceLostReason::None)};
  // Log guard so the "device lost" transition is emitted once per device.
  std::atomic<bool> device_lost_logged{false};

  // Active state-block recording session (BeginStateBlock -> EndStateBlock).
  // When non-null, state-setting DDIs record the subset of state they touch
  // into this object.
  StateBlock* recording_state_block = nullptr;

  // WDDM state (only populated in real Win7/WDDM builds).
  WddmDeviceCallbacks wddm_callbacks{};
  WddmHandle wddm_device = 0;
  WddmContext wddm_context{};
  std::unique_ptr<AllocationListTracker> wddm_alloc_tracker;

  CmdWriter cmd;
  AllocationListTracker alloc_list_tracker;

  // Last submission fence ID returned by the D3D9 runtime callback for this
  // device/context. This is required to correctly wait for "our own" work under
  // multi-device / multi-process workloads (DWM + apps).
  uint64_t last_submission_fence = 0;

  // D3D9Ex EVENT queries are tracked as "pending" until the next submission
  // boundary stamps them with a fence value (see `Query::submitted`).
  std::vector<Query*> pending_event_queries;

  // D3D9Ex throttling + present statistics.
  //
  // These fields model the D3D9Ex "maximum frame latency" behavior used by DWM:
  // we allow up to max_frame_latency in-flight presents, each tracked by a KMD
  // fence ID (or a bring-up stub fence in non-WDDM builds).
  int32_t gpu_thread_priority = 0; // clamped to [-7, 7]
  uint32_t max_frame_latency = 3;
  std::deque<uint64_t> inflight_present_fences;
  uint32_t present_count = 0;
  uint32_t present_refresh_count = 0;
  uint32_t sync_refresh_count = 0;
  uint64_t last_present_qpc = 0;
  std::vector<SwapChain*> swapchains;
  SwapChain* current_swapchain = nullptr;

  // Cached pipeline state.
  Resource* render_targets[4] = {nullptr, nullptr, nullptr, nullptr};
  Resource* depth_stencil = nullptr;
  Resource* textures[16] = {};
  DeviceStateStream streams[16] = {};
  uint32_t stream_source_freq[16] = {};
  Resource* index_buffer = nullptr;
  D3DDDIFORMAT index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  uint32_t index_offset_bytes = 0;
  uint32_t topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  // "User" shaders are the ones explicitly set via the D3D9 runtime.
  // `vs`/`ps` below track what is currently bound in the AeroGPU command stream
  // (may be a fixed-function fallback shader).
  Shader* user_vs = nullptr;
  Shader* user_ps = nullptr;

  Shader* vs = nullptr;
  Shader* ps = nullptr;
  VertexDecl* vertex_decl = nullptr;

  // Fixed-function (FVF) fallback state.
  uint32_t fvf = 0;
  // Internal vertex declarations synthesized for common FVFs (SetFVF).
  VertexDecl* fvf_vertex_decl = nullptr;
  VertexDecl* fvf_vertex_decl_tex1 = nullptr;
  VertexDecl* fvf_vertex_decl_tex1_nodiffuse = nullptr;
  VertexDecl* fvf_vertex_decl_xyz_diffuse = nullptr;
  VertexDecl* fvf_vertex_decl_xyz_diffuse_tex1 = nullptr;
  VertexDecl* fvf_vertex_decl_xyz_tex1 = nullptr;
  Shader* fixedfunc_vs = nullptr;
  Shader* fixedfunc_vs_xyz_diffuse = nullptr;
  Shader* fixedfunc_ps = nullptr;
  Shader* fixedfunc_vs_tex1 = nullptr;
  Shader* fixedfunc_vs_tex1_nodiffuse = nullptr;
  Shader* fixedfunc_ps_tex1 = nullptr;
  Shader* fixedfunc_vs_xyz_diffuse_tex1 = nullptr;
  Shader* fixedfunc_ps_xyz_diffuse_tex1 = nullptr;
  Shader* fixedfunc_vs_xyz_tex1 = nullptr;
  // True when cached world/view/proj transforms changed and the fixed-function
  // fallback needs to re-upload the combined matrix constant registers.
  bool fixedfunc_matrix_dirty = true;

  // Fixed-function "interop" fallbacks used when exactly one shader stage is
  // explicitly bound by the app (D3D9 allows VS-only or PS-only draws).
  //
  // - If `user_vs != nullptr` and `user_ps == nullptr`, we bind an internal
  //   stage0 fixed-function pixel shader to `ps` at draw time.
  // - If `user_vs == nullptr` and `user_ps != nullptr`, we reuse the existing
  //   fixed-function VS variants (`fixedfunc_vs*`) as a draw-time fallback.
  Shader* fixedfunc_ps_interop = nullptr;

  // Scratch vertex buffer used to emulate DrawPrimitiveUP and fixed-function
  // transformed vertex uploads.
  Resource* up_vertex_buffer = nullptr;

  // Scratch index buffer used to emulate DrawIndexedPrimitiveUP-style paths.
  Resource* up_index_buffer = nullptr;

  // Patch tessellation cache (keyed by D3D9 patch handle).
  //
  // This cache is optional (handle==0 disables caching) but storing it per-device
  // matches D3D9 handle semantics: patch handles are scoped to an IDirect3DDevice9.
  std::unordered_map<uint32_t, PatchCacheEntry> patch_cache;
  uint64_t patch_tessellate_count = 0;
  uint64_t patch_cache_hit_count = 0;

  // Scene bracketing (BeginScene/EndScene). Depth allows the runtime to nest
  // scenes in some edge cases; we treat BeginScene/EndScene as a no-op beyond
  // tracking nesting.
  uint32_t scene_depth = 0;

  D3DDDIVIEWPORTINFO viewport = {0, 0, 0, 0, 0.0f, 1.0f};
  RECT scissor_rect = {0, 0, 0, 0};
  // Track whether the scissor rect was explicitly set by the app (via SetScissorRect).
  // Some runtimes enable scissor testing before ever calling SetScissorRect, so
  // leaving the default (all-zero) rect would clip everything. When scissor is
  // enabled and the rect is still unset, the UMD can fall back to a viewport-sized
  // rect to match common D3D9 behavior.
  bool scissor_rect_user_set = false;
  BOOL scissor_enabled = FALSE;

  // Misc fixed-function / legacy state (cached for Get*/state-block compatibility).
  BOOL software_vertex_processing = FALSE;
  float n_patch_mode = 0.0f;

  // Transform state cache for GetTransform/SetTransform. D3D9 transform state
  // enums are sparse (WORLD matrices start at 256), so keep a conservative fixed
  // cache that covers common values.
  static constexpr uint32_t kTransformCacheCount = 512u;
  float transform_matrices[kTransformCacheCount][16] = {};

  // Clip plane cache for GetClipPlane/SetClipPlane.
  float clip_planes[6][4] = {};

  // D3D9 state caches used by helper paths (blits, color fills) so they can
  // temporarily override state and restore it afterwards.
  //
  // D3D9 state IDs are sparse, but the commonly-used ranges fit comfortably in
  // 0..255 and the values are cheap to track.
  uint32_t render_states[256] = {};
  uint32_t sampler_states[16][16] = {};
  uint32_t texture_stage_states[16][256] = {};

  // Shader float constant register caches (float4 registers).
  float vs_consts_f[256 * 4] = {};
  float ps_consts_f[256 * 4] = {};
  // Shader int constant register caches (int4 registers).
  int32_t vs_consts_i[256 * 4] = {};
  int32_t ps_consts_i[256 * 4] = {};
  // Shader bool constant register caches (scalar bool registers).
  uint8_t vs_consts_b[256] = {};
  uint8_t ps_consts_b[256] = {};

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  // Fixed-function lighting/material state (cached only).
  D3DMATERIAL9 material = {};
  bool material_valid = false;
  static constexpr uint32_t kMaxLights = 16u;
  D3DLIGHT9 lights[kMaxLights] = {};
  bool light_valid[kMaxLights] = {};
  BOOL light_enabled[kMaxLights] = {};

  // Misc legacy state not currently emitted to the AeroGPU command stream.
  D3DGAMMARAMP gamma_ramp = {};
  bool gamma_ramp_valid = false;
  D3DCLIPSTATUS9 clip_status = {};
  bool clip_status_valid = false;
  static constexpr uint32_t kMaxPalettes = 256u;
  PALETTEENTRY palette_entries[kMaxPalettes][256] = {};
  bool palette_valid[kMaxPalettes] = {};
  uint32_t current_texture_palette = 0;
#endif

  // D3D9 device cursor state (software cursor composited at Present time).
  //
  // Win7-era D3D9 applications frequently rely on IDirect3DDevice9 cursor APIs
  // (SetCursorProperties/SetCursorPosition/ShowCursor) instead of the Win32
  // cursor. AeroGPU implements these as a minimal software overlay in the UMD.
  BOOL cursor_visible = FALSE;
  int32_t cursor_x = 0;
  int32_t cursor_y = 0;
  uint32_t cursor_hot_x = 0;
  uint32_t cursor_hot_y = 0;
  Resource* cursor_bitmap = nullptr;
  uint64_t cursor_bitmap_serial = 0;

  // Built-in resources used for blit/copy operations (StretchRect/Blt).
  Shader* builtin_copy_vs = nullptr;
  Shader* builtin_copy_ps = nullptr;
  VertexDecl* builtin_copy_decl = nullptr;
  Resource* builtin_copy_vb = nullptr;
};

aerogpu_handle_t allocate_global_handle(Adapter* adapter);

} // namespace aerogpu
