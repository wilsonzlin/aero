#include "aerogpu_d3d9_blit.h"

#include <array>
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <new>

#include "aerogpu_d3d9_builtin_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_submit.h"
#include "aerogpu_log.h"
#include "aerogpu_wddm_alloc.h"

namespace aerogpu {

namespace {

// D3D9 format subset (numeric values from d3d9types.h).
constexpr uint32_t kD3d9FmtA8R8G8B8 = 21u;
constexpr uint32_t kD3d9FmtX8R8G8B8 = 22u;
constexpr uint32_t kD3d9FmtA8B8G8R8 = 32u;

// D3DLOCK_* flags (numeric values from d3d9.h). Only the bits we care about are
// defined here to keep the blit helper self-contained.
constexpr uint32_t kD3DLOCK_READONLY = 0x00000010u;

// D3D9 sampler state IDs (numeric values from d3d9types.h).
constexpr uint32_t kD3d9SampAddressU = 1;
constexpr uint32_t kD3d9SampAddressV = 2;
constexpr uint32_t kD3d9SampMagFilter = 5;
constexpr uint32_t kD3d9SampMinFilter = 6;
constexpr uint32_t kD3d9SampMipFilter = 7;

// D3DTEXTUREADDRESS / D3DTEXTUREFILTERTYPE subset.
constexpr uint32_t kD3d9TexAddressClamp = 3;
constexpr uint32_t kD3d9TexFilterNone = 0;
constexpr uint32_t kD3d9TexFilterPoint = 1;
constexpr uint32_t kD3d9TexFilterLinear = 2;

// D3D9 render state IDs (numeric values from d3d9types.h).
constexpr uint32_t kD3d9RsZEnable = 7;
constexpr uint32_t kD3d9RsZWriteEnable = 14;
constexpr uint32_t kD3d9RsAlphaBlendEnable = 27;
constexpr uint32_t kD3d9RsSrcBlend = 19;
constexpr uint32_t kD3d9RsDestBlend = 20;
constexpr uint32_t kD3d9RsCullMode = 22;
constexpr uint32_t kD3d9RsScissorTestEnable = 174;
constexpr uint32_t kD3d9RsBlendOp = 171;
constexpr uint32_t kD3d9RsColorWriteEnable = 168;
constexpr uint32_t kD3d9RsSeparateAlphaBlendEnable = 206;

// D3DBLEND / D3DBLENDOP / D3DCULL subset.
constexpr uint32_t kD3d9BlendZero = 1;
constexpr uint32_t kD3d9BlendOne = 2;
constexpr uint32_t kD3d9BlendSrcAlpha = 5;
constexpr uint32_t kD3d9BlendInvSrcAlpha = 6;
constexpr uint32_t kD3d9BlendOpAdd = 1;
constexpr uint32_t kD3d9CullNone = 1;

template <typename F>
struct ScopeExit {
  F f;
  ~ScopeExit() noexcept {
    try {
      f();
    } catch (...) {
    }
  }
};

template <typename F>
ScopeExit<F> make_scope_exit(F f) {
  return ScopeExit<F>{f};
}

// D3D9 shader stage values at the DDI boundary.
constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

bool convert_pixel_4bpp(uint32_t src_format, uint32_t dst_format, const uint8_t* src, uint8_t* dst) {
  if (!src || !dst) {
    return false;
  }

  if (src_format == dst_format) {
    std::memcpy(dst, src, 4);
    return true;
  }

  // NOTE: D3D9 A8R8G8B8 and X8R8G8B8 have identical byte ordering (B,G,R,A/X).
  // A8B8G8R8 differs in that it stores bytes as (R,G,B,A).
  const bool src_is_argb = (src_format == kD3d9FmtA8R8G8B8);
  const bool src_is_xrgb = (src_format == kD3d9FmtX8R8G8B8);
  const bool src_is_abgr = (src_format == kD3d9FmtA8B8G8R8);
  const bool dst_is_argb = (dst_format == kD3d9FmtA8R8G8B8);
  const bool dst_is_xrgb = (dst_format == kD3d9FmtX8R8G8B8);
  const bool dst_is_abgr = (dst_format == kD3d9FmtA8B8G8R8);

  if (!((src_is_argb || src_is_xrgb || src_is_abgr) && (dst_is_argb || dst_is_xrgb || dst_is_abgr))) {
    return false;
  }

  uint8_t r = 0;
  uint8_t g = 0;
  uint8_t b = 0;
  uint8_t a = 0xFF;
  if (src_is_abgr) {
    // Bytes: R,G,B,A.
    r = src[0];
    g = src[1];
    b = src[2];
    a = src[3];
  } else {
    // Bytes: B,G,R,A/X.
    b = src[0];
    g = src[1];
    r = src[2];
    a = src_is_argb ? src[3] : 0xFF;
  }

  if (dst_is_abgr) {
    dst[0] = r;
    dst[1] = g;
    dst[2] = b;
    dst[3] = a;
    return true;
  }

  dst[0] = b;
  dst[1] = g;
  dst[2] = r;
  dst[3] = dst_is_argb ? a : 0xFF;
  return true;
}

struct BlitVertex {
  float x, y, z, w;
  float u, v;
};

bool clamp_rect(const RECT* in, uint32_t width, uint32_t height, RECT* out) {
  if (!out || width == 0 || height == 0) {
    return false;
  }

  RECT r{};
  if (in) {
    r = *in;
  } else {
    r.left = 0;
    r.top = 0;
    r.right = static_cast<long>(width);
    r.bottom = static_cast<long>(height);
  }

  const long max_x = static_cast<long>(width);
  const long max_y = static_cast<long>(height);

  r.left = std::clamp(r.left, 0l, max_x);
  r.right = std::clamp(r.right, 0l, max_x);
  r.top = std::clamp(r.top, 0l, max_y);
  r.bottom = std::clamp(r.bottom, 0l, max_y);

  if (r.right <= r.left || r.bottom <= r.top) {
    return false;
  }
  *out = r;
  return true;
}

bool ensure_cmd_space(Device* dev, size_t bytes_needed) {
  return ensure_cmd_space_locked(dev, bytes_needed);
}

HRESULT track_resource_allocation_locked(Device* dev, Resource* res, bool write) {
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  // Only track allocations when running on the WDDM path. Portable builds do
  // not use WDDM allocation handles or runtime-provided allocation lists.
  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

// Ensure we have a valid WDDM allocation list bound before we try to write
// entries into it. In real Win7 builds, `ensure_cmd_space()` will acquire
// runtime-provided DMA buffers + allocation lists (CreateContext persistent
// buffers or AllocateCb/GetCommandBufferCb fallback) without forcing a submit
// as long as we request space for at least one command header.
#if defined(_WIN32)
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  if (!ensure_cmd_space(dev, min_packet)) {
    return E_FAIL;
  }
#endif

  // Allocation tracking requires a bound allocation-list buffer. In portable
  // builds/tests we may toggle `hContext` without wiring a list; treat that as
  // "tracking disabled" so unit tests focused on other behavior keep working.
  if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
    return E_FAIL;
#else
    return S_OK;
#endif
  }

  if (res->backing_alloc_id == 0) {
    // backing_alloc_id==0 denotes a host-allocated resource (no guest allocation
    // table entry required).
    return S_OK;
  }

  if (res->wddm_hAllocation == 0) {
    logf("aerogpu-d3d9: missing WDDM hAllocation for blit resource handle=%u alloc_id=%u\n",
         res->handle,
         res->backing_alloc_id);
    return E_FAIL;
  }

  AllocRef ref{};
  if (write) {
    ref = dev->alloc_list_tracker.track_render_target_write(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  } else if (res->kind == ResourceKind::Buffer) {
    ref = dev->alloc_list_tracker.track_buffer_read(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  } else {
    ref = dev->alloc_list_tracker.track_texture_read(
        res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
  }

  if (ref.status == AllocRefStatus::kNeedFlush) {
    // Split the submission and retry.
    (void)submit_locked(dev);

#if defined(_WIN32)
    // AllocateCb/DeallocateCb runtimes deallocate the active allocation list on
    // every submit, so reacquire/rebind before retrying.
    const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
    if (!ensure_cmd_space(dev, min_packet)) {
      return E_FAIL;
    }
#endif

    if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
      return E_FAIL;
#else
      return S_OK;
#endif
    }

    if (write) {
      ref = dev->alloc_list_tracker.track_render_target_write(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    } else if (res->kind == ResourceKind::Buffer) {
      ref = dev->alloc_list_tracker.track_buffer_read(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    } else {
      ref = dev->alloc_list_tracker.track_texture_read(
          res->wddm_hAllocation, res->backing_alloc_id, res->share_token);
    }
  }

  if (ref.status != AllocRefStatus::kOk) {
    logf("aerogpu-d3d9: failed to track blit allocation (handle=%u alloc_id=%u status=%u)\n",
         res->handle,
         res->backing_alloc_id,
         static_cast<uint32_t>(ref.status));
    if (ref.status == AllocRefStatus::kOutOfMemory) {
      return E_OUTOFMEMORY;
    }
    return E_FAIL;
  }

  return S_OK;
}

HRESULT track_blit_draw_state_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

  // Pre-split if the current allocation list can't accommodate the full set of
  // allocations required by the blit draw. This matches the main draw path
  // (`track_draw_state_locked`) and avoids a mid-tracking split that would reset
  // the allocation list and drop earlier tracked resources.
  //
  // Callers are expected to have already emitted any state-setting packets; the
  // GPU context state persists across submissions, so it is safe to split before
  // the final draw packet as long as we track allocations for the draw in the
  // new submission.
#if defined(_WIN32)
  const size_t min_packet = align_up(sizeof(aerogpu_cmd_hdr), 4);
  if (!ensure_cmd_space(dev, min_packet)) {
    return E_FAIL;
  }
#endif

  if (!dev->alloc_list_tracker.list_base() || dev->alloc_list_tracker.list_capacity_effective() == 0) {
#if defined(_WIN32)
    return E_FAIL;
#else
    return S_OK;
#endif
  }

  std::array<UINT, 4 + 1 + 1 + 1> unique_allocs{};
  size_t unique_alloc_len = 0;
  auto add_alloc = [&unique_allocs, &unique_alloc_len](const Resource* res) {
    if (!res) {
      return;
    }
    if (res->backing_alloc_id == 0) {
      return;
    }
    if (res->wddm_hAllocation == 0) {
      return;
    }
    const UINT alloc_id = res->backing_alloc_id;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (unique_allocs[i] == alloc_id) {
        return;
      }
    }
    unique_allocs[unique_alloc_len++] = alloc_id;
  };

  // Render targets + depth are treated as write destinations.
  for (uint32_t i = 0; i < 4; ++i) {
    add_alloc(dev->render_targets[i]);
  }
  add_alloc(dev->depth_stencil);
  // For blits we only sample from stage 0 today.
  add_alloc(dev->textures[0]);
  // Vertex buffer (builtin quad) is read-only.
  add_alloc(dev->streams[0].vb);

  const UINT needed_total = static_cast<UINT>(unique_alloc_len);
  if (needed_total != 0) {
    const UINT cap = dev->alloc_list_tracker.list_capacity_effective();
    if (needed_total > cap) {
      logf("aerogpu-d3d9: blit draw requires %u allocations but allocation list capacity is %u\n",
           static_cast<unsigned>(needed_total),
           static_cast<unsigned>(cap));
      return E_FAIL;
    }

    UINT needed_new = 0;
    for (size_t i = 0; i < unique_alloc_len; ++i) {
      if (!dev->alloc_list_tracker.contains_alloc_id(unique_allocs[i])) {
        needed_new++;
      }
    }
    const UINT existing = dev->alloc_list_tracker.list_len();
    if (existing > cap || needed_new > cap - existing) {
      (void)submit_locked(dev);
    }
  }

  // Render targets + depth are treated as write destinations.
  for (uint32_t i = 0; i < 4; ++i) {
    if (!dev->render_targets[i]) {
      continue;
    }
    HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
    if (FAILED(hr)) {
      return hr;
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (FAILED(hr)) {
      return hr;
    }
  }

  // For blits we only sample from stage 0 today.
  if (dev->textures[0]) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->textures[0], /*write=*/false);
    if (FAILED(hr)) {
      return hr;
    }
  }

  // Vertex buffer (builtin quad) is read-only.
  if (dev->streams[0].vb) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->streams[0].vb, /*write=*/false);
    if (FAILED(hr)) {
      return hr;
    }
  }

  return S_OK;
}

HRESULT track_blit_render_targets_locked(Device* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }

  if (dev->wddm_context.hContext == 0) {
    return S_OK;
  }

  for (uint32_t i = 0; i < 4; ++i) {
    if (!dev->render_targets[i]) {
      continue;
    }
    HRESULT hr = track_resource_allocation_locked(dev, dev->render_targets[i], /*write=*/true);
    if (FAILED(hr)) {
      return hr;
    }
  }

  if (dev->depth_stencil) {
    HRESULT hr = track_resource_allocation_locked(dev, dev->depth_stencil, /*write=*/true);
    if (FAILED(hr)) {
      return hr;
    }
  }

  return S_OK;
}

template <typename T>
T* append_fixed_locked(Device* dev, uint32_t opcode) {
  const size_t needed = align_up(sizeof(T), 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendFixed<T>(opcode);
}

template <typename HeaderT>
HeaderT* append_with_payload_locked(Device* dev, uint32_t opcode, const void* payload, size_t payload_size) {
  const size_t needed = align_up(sizeof(HeaderT) + payload_size, 4);
  if (!ensure_cmd_space(dev, needed)) {
    return nullptr;
  }
  return dev->cmd.TryAppendWithPayload<HeaderT>(opcode, payload, payload_size);
}

bool upload_resource_bytes_locked(Device* dev,
                                  Resource* res,
                                  uint64_t offset_bytes,
                                  const uint8_t* data,
                                  size_t size_bytes) {
  if (!dev || !res || !res->handle) {
    return false;
  }
  if (size_bytes == 0) {
    return true;
  }
  if (!data) {
    return false;
  }
  if (res->backing_alloc_id != 0) {
    // Host-side validation rejects UPLOAD_RESOURCE for guest-backed resources.
    // Callers must write into the guest allocation memory (e.g. via WDDM LockCb)
    // and then notify the host using RESOURCE_DIRTY_RANGE.
    logf("aerogpu-d3d9: upload_resource_bytes_locked called on guest-backed resource handle=%u alloc_id=%u\n",
         static_cast<unsigned>(res->handle),
         static_cast<unsigned>(res->backing_alloc_id));
    return false;
  }

  const uint64_t res_size = res->size_bytes;
  const uint64_t size_u64 = static_cast<uint64_t>(size_bytes);
  if (offset_bytes > res_size || size_u64 > res_size - offset_bytes) {
    return false;
  }

  const bool is_buffer = (res->kind == ResourceKind::Buffer);
  uint64_t upload_offset = offset_bytes;
  size_t upload_size = size_bytes;
  const uint8_t* upload_src = data;

  if (is_buffer) {
    // WebGPU buffer copies require 4-byte alignment for both offsets and sizes.
    // For host-backed buffers we can do a read-modify-write using the CPU shadow
    // storage to preserve any bytes outside of the caller's range.
    if (res->storage.size() < res->size_bytes) {
      try {
        res->storage.resize(res->size_bytes, 0);
      } catch (...) {
        return false;
      }
    }

    std::memmove(res->storage.data() + static_cast<size_t>(offset_bytes), data, size_bytes);

    const uint64_t start = upload_offset & ~3ull;
    const uint64_t end = (upload_offset + size_u64 + 3ull) & ~3ull;
    if (end > res_size || end < start) {
      return false;
    }
    upload_offset = start;
    upload_size = static_cast<size_t>(end - start);
    upload_src = res->storage.data() + static_cast<size_t>(start);
  }

  if (is_buffer) {
    size_t remaining = upload_size;
    uint64_t cur_offset = upload_offset;
    const uint8_t* src = upload_src;

    while (remaining) {
      // Ensure we can at least fit a minimal upload packet (header + N bytes).
      const size_t min_payload = 4;
      const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + min_payload, 4);
      if (!ensure_cmd_space(dev, min_needed)) {
        return false;
      }

      // Uploads write into the destination resource. Track the backing allocation
      // so the KMD alloc table contains the required (alloc_id -> GPA) mapping even
      // when the patch-location list is empty.
      HRESULT hr = track_resource_allocation_locked(dev, res, /*write=*/true);
      if (FAILED(hr)) {
        return false;
      }

      // Allocation tracking may have split/flushed the submission; re-validate the
      // command buffer capacity before computing the chunk size.
      if (!ensure_cmd_space(dev, min_needed)) {
        return false;
      }

      const size_t avail = dev->cmd.bytes_remaining();
      size_t chunk = 0;
      if (avail > sizeof(aerogpu_cmd_upload_resource)) {
        chunk = std::min(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
      }

      chunk &= ~static_cast<size_t>(3);
      if (!chunk) {
        // Extremely small DMA buffer: force a submit and retry.
        (void)submit_locked(dev);
        continue;
      }

      auto* cmd =
          append_with_payload_locked<aerogpu_cmd_upload_resource>(dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
      if (!cmd) {
        return false;
      }

      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = cur_offset;
      cmd->size_bytes = chunk;

      src += chunk;
      cur_offset += chunk;
      remaining -= chunk;
    }

    return true;
  }

  // Texture uploads must be aligned to whole rows within the destination
  // subresource. The host validates that UPLOAD_RESOURCE ranges for textures are
  // row-aligned (offset and size are multiples of row_pitch_bytes within each
  // subresource).
  const uint32_t array_layers = std::max(1u, res->depth);
  if (res->storage.size() < res->size_bytes) {
    try {
      res->storage.resize(res->size_bytes, 0);
    } catch (...) {
      return false;
    }
  }

  // Copy the caller's bytes into the CPU shadow storage so we can safely expand
  // the upload to whole rows without needing the caller to provide padding.
  std::memmove(res->storage.data() + static_cast<size_t>(offset_bytes), data, size_bytes);

  const uint64_t range_start = offset_bytes;
  const uint64_t range_end = range_start + static_cast<uint64_t>(size_bytes);

  auto align_up_to_multiple = [](uint64_t v, uint64_t a) -> uint64_t {
    if (a == 0) {
      return v;
    }
    const uint64_t rem = v % a;
    return rem ? (v + (a - rem)) : v;
  };

  uint64_t cur = range_start;
  while (cur < range_end) {
    Texture2dSubresourceLayout sub{};
    if (!calc_texture2d_subresource_layout_for_offset(
            res->format,
            res->width,
            res->height,
            res->mip_levels,
            array_layers,
            cur,
            &sub)) {
      return false;
    }

    const uint64_t sub_start = sub.subresource_start_bytes;
    const uint64_t sub_end = sub.subresource_end_bytes;
    if (sub_start >= sub_end) {
      return false;
    }

    const uint64_t inter_start = std::max(cur, sub_start);
    const uint64_t inter_end = std::min(range_end, sub_end);
    if (inter_start >= inter_end) {
      cur = inter_end;
      continue;
    }

    const uint64_t row_pitch = static_cast<uint64_t>(sub.row_pitch_bytes);
    if (row_pitch == 0) {
      return false;
    }

    const uint64_t rel_start = inter_start - sub_start;
    const uint64_t rel_end = inter_end - sub_start;
    const uint64_t aligned_rel_start = (rel_start / row_pitch) * row_pitch;
    uint64_t aligned_rel_end = align_up_to_multiple(rel_end, row_pitch);

    const uint64_t aligned_start = sub_start + aligned_rel_start;
    uint64_t aligned_end = sub_start + aligned_rel_end;
    if (aligned_end > sub_end) {
      aligned_end = sub_end;
    }
    if (aligned_end <= aligned_start) {
      return false;
    }

    const size_t aligned_end_sz = static_cast<size_t>(aligned_end);
    if (res->storage.size() < aligned_end_sz) {
      return false;
    }

    const size_t row_pitch_sz = static_cast<size_t>(row_pitch);
    const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + row_pitch_sz, 4);

    const uint8_t* src = res->storage.data() + static_cast<size_t>(aligned_start);
    size_t remaining = static_cast<size_t>(aligned_end - aligned_start);
    uint64_t cur_offset = aligned_start;

    while (remaining) {
      if (!ensure_cmd_space(dev, min_needed)) {
        return false;
      }

      HRESULT hr = track_resource_allocation_locked(dev, res, /*write=*/true);
      if (FAILED(hr)) {
        return false;
      }

      if (!ensure_cmd_space(dev, min_needed)) {
        return false;
      }

      const size_t avail = dev->cmd.bytes_remaining();
      size_t chunk = 0;
      if (avail > sizeof(aerogpu_cmd_upload_resource)) {
        chunk = std::min(remaining, avail - sizeof(aerogpu_cmd_upload_resource));
      }
      // Account for 4-byte alignment padding at the end of the packet.
      while (chunk && align_up(sizeof(aerogpu_cmd_upload_resource) + chunk, 4) > avail) {
        chunk--;
      }

      chunk = (chunk / row_pitch_sz) * row_pitch_sz;
      if (!chunk) {
        (void)submit_locked(dev);
        continue;
      }

      auto* cmd =
          append_with_payload_locked<aerogpu_cmd_upload_resource>(dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
      if (!cmd) {
        return false;
      }

      cmd->resource_handle = res->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = cur_offset;
      cmd->size_bytes = chunk;

      src += chunk;
      cur_offset += static_cast<uint64_t>(chunk);
      remaining -= chunk;
    }

    cur = inter_end;
  }

  return true;
}

bool emit_set_render_targets_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_targets>(dev, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }
  uint32_t color_count = 0;
  while (color_count < 4 && dev->render_targets[color_count]) {
    color_count++;
  }
  for (uint32_t i = color_count; i < 4; ++i) {
    dev->render_targets[i] = nullptr;
  }

  cmd->color_count = color_count;
  cmd->depth_stencil = dev->depth_stencil ? dev->depth_stencil->handle : 0;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    cmd->colors[i] = 0;
  }
  for (uint32_t i = 0; i < color_count; ++i) {
    cmd->colors[i] = dev->render_targets[i] ? dev->render_targets[i]->handle : 0;
  }
  return true;
}

bool emit_bind_shaders_locked(Device* dev) {
  const size_t needed = align_up(sizeof(aerogpu_cmd_bind_shaders), 4);
  if (!ensure_cmd_space(dev, needed)) {
    return false;
  }
  auto* cmd = dev->cmd.bind_shaders(
      /*vs=*/dev->vs ? dev->vs->handle : 0,
      /*ps=*/dev->ps ? dev->ps->handle : 0,
      /*cs=*/0);
  return cmd != nullptr;
}

bool emit_set_viewport_locked(Device* dev) {
  const auto& vp = dev->viewport;
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_viewport>(dev, AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    return false;
  }
  cmd->x_f32 = f32_bits(vp.X);
  cmd->y_f32 = f32_bits(vp.Y);
  cmd->width_f32 = f32_bits(vp.Width);
  cmd->height_f32 = f32_bits(vp.Height);
  cmd->min_depth_f32 = f32_bits(vp.MinZ);
  cmd->max_depth_f32 = f32_bits(vp.MaxZ);
  return true;
}

bool emit_set_scissor_locked(Device* dev) {
  int32_t x = 0;
  int32_t y = 0;
  int32_t w = 0x7FFFFFFF;
  int32_t h = 0x7FFFFFFF;
  if (dev->scissor_enabled) {
    x = static_cast<int32_t>(dev->scissor_rect.left);
    y = static_cast<int32_t>(dev->scissor_rect.top);
    w = static_cast<int32_t>(dev->scissor_rect.right - dev->scissor_rect.left);
    h = static_cast<int32_t>(dev->scissor_rect.bottom - dev->scissor_rect.top);
  }

  auto* cmd = append_fixed_locked<aerogpu_cmd_set_scissor>(dev, AEROGPU_CMD_SET_SCISSOR);
  if (!cmd) {
    return false;
  }
  cmd->x = x;
  cmd->y = y;
  cmd->width = w;
  cmd->height = h;
  return true;
}

bool emit_set_texture_locked(Device* dev, uint32_t stage) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_texture>(dev, AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    return false;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->texture = dev->textures[stage] ? dev->textures[stage]->handle : 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_input_layout_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_input_layout>(dev, AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    return false;
  }
  cmd->input_layout_handle = dev->vertex_decl ? dev->vertex_decl->handle : 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_vertex_buffer_locked(Device* dev, uint32_t stream) {
  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = dev->streams[stream].vb ? dev->streams[stream].vb->handle : 0;
  binding.stride_bytes = dev->streams[stream].stride_bytes;
  binding.offset_bytes = dev->streams[stream].offset_bytes;
  binding.reserved0 = 0;

  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_vertex_buffers>(
      dev, AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
  if (!cmd) {
    return false;
  }
  cmd->start_slot = stream;
  cmd->buffer_count = 1;
  return true;
}

bool emit_set_topology_locked(Device* dev, uint32_t topology) {
  if (dev->topology == topology) {
    return true;
  }
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_primitive_topology>(dev, AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    return false;
  }
  dev->topology = topology;
  cmd->topology = topology;
  cmd->reserved0 = 0;
  return true;
}

bool set_render_state_locked(Device* dev, uint32_t state, uint32_t value) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_state>(dev, AEROGPU_CMD_SET_RENDER_STATE);
  if (!cmd) {
    return false;
  }
  if (state < 256) {
    dev->render_states[state] = value;
  }
  cmd->state = state;
  cmd->value = value;
  return true;
}

bool set_sampler_state_locked(Device* dev, uint32_t stage, uint32_t state, uint32_t value) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_sampler_state>(dev, AEROGPU_CMD_SET_SAMPLER_STATE);
  if (!cmd) {
    return false;
  }
  if (stage < 16 && state < 16) {
    dev->sampler_states[stage][state] = value;
  }
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->state = state;
  cmd->value = value;
  return true;
}

bool set_shader_const_f_locked(Device* dev,
                               uint32_t stage,
                               uint32_t start_reg,
                               const float* data,
                               uint32_t vec4_count) {
  if (!data || vec4_count == 0) {
    return true;
  }

  const size_t payload_size = static_cast<size_t>(vec4_count) * 4 * sizeof(float);
  auto* cmd = append_with_payload_locked<aerogpu_cmd_set_shader_constants_f>(
      dev, AEROGPU_CMD_SET_SHADER_CONSTANTS_F, data, payload_size);
  if (!cmd) {
    return false;
  }
  cmd->stage = (stage == kD3d9ShaderStageVs) ? AEROGPU_SHADER_STAGE_VERTEX : AEROGPU_SHADER_STAGE_PIXEL;
  cmd->start_register = start_reg;
  cmd->vec4_count = vec4_count;
  cmd->reserved0 = 0;

  float* dst = (stage == kD3d9ShaderStageVs) ? dev->vs_consts_f : dev->ps_consts_f;
  const uint32_t max_regs = 256;
  if (start_reg < max_regs) {
    const uint32_t write_regs = std::min(vec4_count, max_regs - start_reg);
    std::memcpy(dst + start_reg * 4, data, static_cast<size_t>(write_regs) * 4 * sizeof(float));
  }
  return true;
}

HRESULT ensure_blit_objects_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  if (!dev->builtin_copy_vs) {
    auto* sh = new (std::nothrow) Shader();
    if (!sh) {
      return E_OUTOFMEMORY;
    }
    sh->handle = allocate_global_handle(dev->adapter);
    sh->stage = kD3d9ShaderStageVs;
    try {
      sh->bytecode.assign(builtin_d3d9_shaders::kCopyVsDxbc,
                          builtin_d3d9_shaders::kCopyVsDxbc + builtin_d3d9_shaders::kCopyVsDxbcSize);
    } catch (...) {
      delete sh;
      return E_OUTOFMEMORY;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_create_shader_dxbc>(
        dev, AEROGPU_CMD_CREATE_SHADER_DXBC, sh->bytecode.data(), sh->bytecode.size());
    if (!cmd) {
      delete sh;
      return E_OUTOFMEMORY;
    }
    cmd->shader_handle = sh->handle;
    cmd->stage = AEROGPU_SHADER_STAGE_VERTEX;
    cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->bytecode.size());
    cmd->reserved0 = 0;

    dev->builtin_copy_vs = sh;
  }

  if (!dev->builtin_copy_ps) {
    auto* sh = new (std::nothrow) Shader();
    if (!sh) {
      return E_OUTOFMEMORY;
    }
    sh->handle = allocate_global_handle(dev->adapter);
    sh->stage = kD3d9ShaderStagePs;
    try {
      sh->bytecode.assign(builtin_d3d9_shaders::kCopyPsDxbc,
                          builtin_d3d9_shaders::kCopyPsDxbc + builtin_d3d9_shaders::kCopyPsDxbcSize);
    } catch (...) {
      delete sh;
      return E_OUTOFMEMORY;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_create_shader_dxbc>(
        dev, AEROGPU_CMD_CREATE_SHADER_DXBC, sh->bytecode.data(), sh->bytecode.size());
    if (!cmd) {
      delete sh;
      return E_OUTOFMEMORY;
    }
    cmd->shader_handle = sh->handle;
    cmd->stage = AEROGPU_SHADER_STAGE_PIXEL;
    cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->bytecode.size());
    cmd->reserved0 = 0;

    dev->builtin_copy_ps = sh;
  }

  if (!dev->builtin_copy_decl) {
    auto* decl = new (std::nothrow) VertexDecl();
    if (!decl) {
      return E_OUTOFMEMORY;
    }
    decl->handle = allocate_global_handle(dev->adapter);

    // D3D9 vertex declaration (D3DVERTEXELEMENT9[]), little-endian:
    //   POSITION0: float4 at stream 0 offset 0
    //   TEXCOORD0: float2 at stream 0 offset 16
    //   end marker
    try {
      decl->blob = {
          0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
          0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x05, 0x00,
          0xff, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00,
      };
    } catch (...) {
      delete decl;
      return E_OUTOFMEMORY;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_create_input_layout>(
        dev, AEROGPU_CMD_CREATE_INPUT_LAYOUT, decl->blob.data(), decl->blob.size());
    if (!cmd) {
      delete decl;
      return E_OUTOFMEMORY;
    }
    cmd->input_layout_handle = decl->handle;
    cmd->blob_size_bytes = static_cast<uint32_t>(decl->blob.size());
    cmd->reserved0 = 0;

    dev->builtin_copy_decl = decl;
  }

  if (!dev->builtin_copy_vb) {
    auto* vb = new (std::nothrow) Resource();
    if (!vb) {
      return E_OUTOFMEMORY;
    }
    vb->handle = allocate_global_handle(dev->adapter);
    vb->kind = ResourceKind::Buffer;
    vb->size_bytes = sizeof(BlitVertex) * 4;
    try {
      vb->storage.resize(vb->size_bytes);
    } catch (...) {
      delete vb;
      return E_OUTOFMEMORY;
    }

    auto* cmd = append_fixed_locked<aerogpu_cmd_create_buffer>(dev, AEROGPU_CMD_CREATE_BUFFER);
    if (!cmd) {
      delete vb;
      return E_OUTOFMEMORY;
    }
    cmd->buffer_handle = vb->handle;
    cmd->usage_flags = AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    cmd->size_bytes = vb->size_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    dev->builtin_copy_vb = vb;
  }

  return S_OK;
}

} // namespace

static HRESULT blit_locked_impl(Device* dev,
                                Resource* dst,
                                const RECT* dst_rect_in,
                                Resource* src,
                                const RECT* src_rect_in,
                                uint32_t filter,
                                bool alpha_blend) {
  if (!dev || !dst || !src) {
    return E_INVALIDARG;
  }

  HRESULT hr = ensure_blit_objects_locked(dev);
  if (hr < 0) {
    return hr;
  }

  RECT dst_rect{};
  RECT src_rect{};
  if (!clamp_rect(dst_rect_in, dst->width, dst->height, &dst_rect) ||
      !clamp_rect(src_rect_in, src->width, src->height, &src_rect)) {
    // Treat empty rects as no-op to match common driver behavior and keep the
    // DWM path resilient.
    return S_OK;
  }

  // Save state we overwrite.
  Resource* saved_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};
  Resource* saved_ds = dev->depth_stencil;
  Shader* saved_vs = dev->vs;
  Shader* saved_ps = dev->ps;
  VertexDecl* saved_decl = dev->vertex_decl;
  Resource* saved_tex0 = dev->textures[0];
  DeviceStateStream saved_stream0 = dev->streams[0];
  const uint32_t saved_topology = dev->topology;
  const D3DDDIVIEWPORTINFO saved_vp = dev->viewport;
  const RECT saved_scissor = dev->scissor_rect;
  const BOOL saved_scissor_enabled = dev->scissor_enabled;

  const uint32_t saved_rs_scissor = dev->render_states[kD3d9RsScissorTestEnable];
  const uint32_t saved_rs_alpha_blend = dev->render_states[kD3d9RsAlphaBlendEnable];
  const uint32_t saved_rs_sep_alpha_blend = dev->render_states[kD3d9RsSeparateAlphaBlendEnable];
  const uint32_t saved_rs_src_blend = dev->render_states[kD3d9RsSrcBlend];
  const uint32_t saved_rs_dst_blend = dev->render_states[kD3d9RsDestBlend];
  const uint32_t saved_rs_blend_op = dev->render_states[kD3d9RsBlendOp];
  const uint32_t saved_rs_color_write = dev->render_states[kD3d9RsColorWriteEnable];
  const uint32_t saved_rs_z_enable = dev->render_states[kD3d9RsZEnable];
  const uint32_t saved_rs_z_write = dev->render_states[kD3d9RsZWriteEnable];
  const uint32_t saved_rs_cull = dev->render_states[kD3d9RsCullMode];

  const uint32_t saved_samp_u = dev->sampler_states[0][kD3d9SampAddressU];
  const uint32_t saved_samp_v = dev->sampler_states[0][kD3d9SampAddressV];
  const uint32_t saved_samp_min = dev->sampler_states[0][kD3d9SampMinFilter];
  const uint32_t saved_samp_mag = dev->sampler_states[0][kD3d9SampMagFilter];
  const uint32_t saved_samp_mip = dev->sampler_states[0][kD3d9SampMipFilter];

  float saved_vs_c0_3[16];
  std::memcpy(saved_vs_c0_3, dev->vs_consts_f, sizeof(saved_vs_c0_3));
  float saved_ps_c0[4];
  std::memcpy(saved_ps_c0, dev->ps_consts_f, sizeof(saved_ps_c0));

  auto restore = make_scope_exit([&] {
    dev->streams[0] = saved_stream0;
    (void)emit_set_vertex_buffer_locked(dev, 0);

    dev->vertex_decl = saved_decl;
    (void)emit_set_input_layout_locked(dev);

    dev->textures[0] = saved_tex0;
    (void)emit_set_texture_locked(dev, 0);

    // The host command stream validator rejects null shader binds. If the caller
    // did not have a pipeline bound (common early in device bring-up), keep the
    // current internal shaders bound instead of restoring a null pair. The next
    // draw/blit will re-bind the appropriate pipeline.
    if (saved_vs && saved_ps) {
      dev->vs = saved_vs;
      dev->ps = saved_ps;
      (void)emit_bind_shaders_locked(dev);
    }

    dev->render_targets[0] = saved_rts[0];
    dev->render_targets[1] = saved_rts[1];
    dev->render_targets[2] = saved_rts[2];
    dev->render_targets[3] = saved_rts[3];
    dev->depth_stencil = saved_ds;
    (void)emit_set_render_targets_locked(dev);

    dev->viewport = saved_vp;
    (void)emit_set_viewport_locked(dev);

    dev->scissor_rect = saved_scissor;
    dev->scissor_enabled = saved_scissor_enabled;
    (void)emit_set_scissor_locked(dev);

    (void)emit_set_topology_locked(dev, saved_topology);

    (void)set_shader_const_f_locked(dev, kD3d9ShaderStageVs, 0, saved_vs_c0_3, 4);
    (void)set_shader_const_f_locked(dev, kD3d9ShaderStagePs, 0, saved_ps_c0, 1);

    (void)set_sampler_state_locked(dev, 0, kD3d9SampAddressU, saved_samp_u);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampAddressV, saved_samp_v);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMinFilter, saved_samp_min);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMagFilter, saved_samp_mag);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMipFilter, saved_samp_mip);

    (void)set_render_state_locked(dev, kD3d9RsScissorTestEnable, saved_rs_scissor);
    (void)set_render_state_locked(dev, kD3d9RsAlphaBlendEnable, saved_rs_alpha_blend);
    (void)set_render_state_locked(dev, kD3d9RsSeparateAlphaBlendEnable, saved_rs_sep_alpha_blend);
    (void)set_render_state_locked(dev, kD3d9RsSrcBlend, saved_rs_src_blend);
    (void)set_render_state_locked(dev, kD3d9RsDestBlend, saved_rs_dst_blend);
    (void)set_render_state_locked(dev, kD3d9RsBlendOp, saved_rs_blend_op);
    (void)set_render_state_locked(dev, kD3d9RsColorWriteEnable, saved_rs_color_write);
    (void)set_render_state_locked(dev, kD3d9RsZEnable, saved_rs_z_enable);
    (void)set_render_state_locked(dev, kD3d9RsZWriteEnable, saved_rs_z_write);
    (void)set_render_state_locked(dev, kD3d9RsCullMode, saved_rs_cull);
  });

  // Configure a conservative copy state.
  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, TRUE) ||
      !set_render_state_locked(dev, kD3d9RsAlphaBlendEnable, alpha_blend ? TRUE : FALSE) ||
      !set_render_state_locked(dev, kD3d9RsSeparateAlphaBlendEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsSrcBlend, alpha_blend ? kD3d9BlendSrcAlpha : kD3d9BlendOne) ||
      !set_render_state_locked(dev, kD3d9RsDestBlend, alpha_blend ? kD3d9BlendInvSrcAlpha : kD3d9BlendZero) ||
      !set_render_state_locked(dev, kD3d9RsBlendOp, kD3d9BlendOpAdd) ||
      !set_render_state_locked(dev, kD3d9RsColorWriteEnable, 0xFu) ||
      !set_render_state_locked(dev, kD3d9RsZEnable, 0u) ||
      !set_render_state_locked(dev, kD3d9RsZWriteEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsCullMode, kD3d9CullNone)) {
    return E_OUTOFMEMORY;
  }

  if (!set_sampler_state_locked(dev, 0, kD3d9SampAddressU, kD3d9TexAddressClamp) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampAddressV, kD3d9TexAddressClamp) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMipFilter, kD3d9TexFilterNone)) {
    return E_OUTOFMEMORY;
  }

  const uint32_t effective_filter = (filter == kD3d9TexFilterLinear) ? kD3d9TexFilterLinear : kD3d9TexFilterPoint;
  if (!set_sampler_state_locked(dev, 0, kD3d9SampMinFilter, effective_filter) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMagFilter, effective_filter)) {
    return E_OUTOFMEMORY;
  }

  // Bind destination as render target.
  dev->render_targets[0] = dst;
  dev->render_targets[1] = nullptr;
  dev->render_targets[2] = nullptr;
  dev->render_targets[3] = nullptr;
  dev->depth_stencil = nullptr;
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  // Full-surface viewport for correct NDC mapping.
  dev->viewport = {0.0f, 0.0f, static_cast<float>(dst->width), static_cast<float>(dst->height), 0.0f, 1.0f};
  if (!emit_set_viewport_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->scissor_rect = dst_rect;
  dev->scissor_enabled = TRUE;
  if (!emit_set_scissor_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  // Bind copy shaders + constants.
  dev->vs = dev->builtin_copy_vs;
  dev->ps = dev->builtin_copy_ps;
  if (!emit_bind_shaders_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  // Vertex shader matrix: identity (so vertices can be provided in clip-space).
  const float ident[16] = {
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  if (!set_shader_const_f_locked(dev, kD3d9ShaderStageVs, 0, ident, 4)) {
    return E_OUTOFMEMORY;
  }

  // Pixel shader multiplier: 1.0 (pass through sampled texel).
  const float one[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  if (!set_shader_const_f_locked(dev, kD3d9ShaderStagePs, 0, one, 1)) {
    return E_OUTOFMEMORY;
  }

  // Bind source texture.
  dev->textures[0] = src;
  if (!emit_set_texture_locked(dev, 0)) {
    return E_OUTOFMEMORY;
  }

  // Bind input layout + vertex buffer.
  dev->vertex_decl = dev->builtin_copy_decl;
  if (!emit_set_input_layout_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->streams[0].vb = dev->builtin_copy_vb;
  dev->streams[0].offset_bytes = 0;
  dev->streams[0].stride_bytes = sizeof(BlitVertex);
  if (!emit_set_vertex_buffer_locked(dev, 0) ||
      !emit_set_topology_locked(dev, AEROGPU_TOPOLOGY_TRIANGLESTRIP)) {
    return E_OUTOFMEMORY;
  }

  // Build quad vertices.
  const float dst_w = static_cast<float>(dst->width);
  const float dst_h = static_cast<float>(dst->height);
  const float src_w = static_cast<float>(src->width);
  const float src_h = static_cast<float>(src->height);

  const float x0 = (2.0f * static_cast<float>(dst_rect.left) / dst_w) - 1.0f;
  const float x1 = (2.0f * static_cast<float>(dst_rect.right) / dst_w) - 1.0f;
  const float y0 = 1.0f - (2.0f * static_cast<float>(dst_rect.top) / dst_h);
  const float y1 = 1.0f - (2.0f * static_cast<float>(dst_rect.bottom) / dst_h);

  const float u0 = static_cast<float>(src_rect.left) / src_w;
  const float u1 = static_cast<float>(src_rect.right) / src_w;
  const float v0 = static_cast<float>(src_rect.top) / src_h;
  const float v1 = static_cast<float>(src_rect.bottom) / src_h;

  BlitVertex verts[4] = {
      {x0, y0, 0.0f, 1.0f, u0, v0},
      {x0, y1, 0.0f, 1.0f, u0, v1},
      {x1, y0, 0.0f, 1.0f, u1, v0},
      {x1, y1, 0.0f, 1.0f, u1, v1},
  };

  // Upload vertices (bring-up path uses UPLOAD_RESOURCE so the host doesn't need
  // to dereference guest allocations).
  if (!upload_resource_bytes_locked(dev,
                                    dev->builtin_copy_vb,
                                    /*offset_bytes=*/0,
                                    reinterpret_cast<const uint8_t*>(verts),
                                    sizeof(verts))) {
    return E_OUTOFMEMORY;
  }

  // Draw.
  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    return E_OUTOFMEMORY;
  }
  hr = track_blit_draw_state_locked(dev);
  if (FAILED(hr)) {
    return hr;
  }
  auto* draw = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!draw) {
    return E_OUTOFMEMORY;
  }
  draw->vertex_count = 4;
  draw->instance_count = 1;
  draw->first_vertex = 0;
  draw->first_instance = 0;
  (void)restore;
  return S_OK;
}

HRESULT blit_locked(Device* dev,
                    Resource* dst,
                    const RECT* dst_rect_in,
                    Resource* src,
                    const RECT* src_rect_in,
                    uint32_t filter) {
  return blit_locked_impl(dev, dst, dst_rect_in, src, src_rect_in, filter, /*alpha_blend=*/false);
}

HRESULT blit_alpha_locked(Device* dev,
                          Resource* dst,
                          const RECT* dst_rect_in,
                          Resource* src,
                          const RECT* src_rect_in,
                          uint32_t filter) {
  return blit_locked_impl(dev, dst, dst_rect_in, src, src_rect_in, filter, /*alpha_blend=*/true);
}

HRESULT color_fill_locked(Device* dev, Resource* dst, const RECT* dst_rect_in, uint32_t color_argb) {
  if (!dev || !dst) {
    return E_INVALIDARG;
  }

  HRESULT hr = ensure_blit_objects_locked(dev);
  if (hr < 0) {
    return hr;
  }

  RECT dst_rect{};
  if (!clamp_rect(dst_rect_in, dst->width, dst->height, &dst_rect)) {
    return S_OK;
  }

  // Save state we overwrite.
  Resource* saved_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};
  Resource* saved_ds = dev->depth_stencil;
  Shader* saved_vs = dev->vs;
  Shader* saved_ps = dev->ps;
  VertexDecl* saved_decl = dev->vertex_decl;
  Resource* saved_tex0 = dev->textures[0];
  DeviceStateStream saved_stream0 = dev->streams[0];
  const uint32_t saved_topology = dev->topology;
  const D3DDDIVIEWPORTINFO saved_vp = dev->viewport;
  const RECT saved_scissor = dev->scissor_rect;
  const BOOL saved_scissor_enabled = dev->scissor_enabled;
  const uint32_t saved_rs_scissor = dev->render_states[kD3d9RsScissorTestEnable];
  const uint32_t saved_rs_alpha_blend = dev->render_states[kD3d9RsAlphaBlendEnable];
  const uint32_t saved_rs_sep_alpha_blend = dev->render_states[kD3d9RsSeparateAlphaBlendEnable];
  const uint32_t saved_rs_src_blend = dev->render_states[kD3d9RsSrcBlend];
  const uint32_t saved_rs_dst_blend = dev->render_states[kD3d9RsDestBlend];
  const uint32_t saved_rs_blend_op = dev->render_states[kD3d9RsBlendOp];
  const uint32_t saved_rs_color_write = dev->render_states[kD3d9RsColorWriteEnable];
  const uint32_t saved_rs_z_enable = dev->render_states[kD3d9RsZEnable];
  const uint32_t saved_rs_z_write = dev->render_states[kD3d9RsZWriteEnable];
  const uint32_t saved_rs_cull = dev->render_states[kD3d9RsCullMode];

  const uint32_t saved_samp_u = dev->sampler_states[0][kD3d9SampAddressU];
  const uint32_t saved_samp_v = dev->sampler_states[0][kD3d9SampAddressV];
  const uint32_t saved_samp_min = dev->sampler_states[0][kD3d9SampMinFilter];
  const uint32_t saved_samp_mag = dev->sampler_states[0][kD3d9SampMagFilter];
  const uint32_t saved_samp_mip = dev->sampler_states[0][kD3d9SampMipFilter];

  float saved_ps_c0[4];
  std::memcpy(saved_ps_c0, dev->ps_consts_f, sizeof(saved_ps_c0));

  auto restore = make_scope_exit([&] {
    dev->streams[0] = saved_stream0;
    (void)emit_set_vertex_buffer_locked(dev, 0);

    dev->vertex_decl = saved_decl;
    (void)emit_set_input_layout_locked(dev);

    dev->textures[0] = saved_tex0;
    (void)emit_set_texture_locked(dev, 0);

    // The host command stream validator rejects null shader binds. If the caller
    // did not have a pipeline bound (common early in device bring-up), keep the
    // current internal shaders bound instead of restoring a null pair. The next
    // draw/blit will re-bind the appropriate pipeline.
    if (saved_vs && saved_ps) {
      dev->vs = saved_vs;
      dev->ps = saved_ps;
      (void)emit_bind_shaders_locked(dev);
    }

    dev->render_targets[0] = saved_rts[0];
    dev->render_targets[1] = saved_rts[1];
    dev->render_targets[2] = saved_rts[2];
    dev->render_targets[3] = saved_rts[3];
    dev->depth_stencil = saved_ds;
    (void)emit_set_render_targets_locked(dev);

    dev->viewport = saved_vp;
    (void)emit_set_viewport_locked(dev);

    dev->scissor_rect = saved_scissor;
    dev->scissor_enabled = saved_scissor_enabled;
    (void)emit_set_scissor_locked(dev);

    (void)emit_set_topology_locked(dev, saved_topology);

    (void)set_shader_const_f_locked(dev, kD3d9ShaderStagePs, 0, saved_ps_c0, 1);

    (void)set_sampler_state_locked(dev, 0, kD3d9SampAddressU, saved_samp_u);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampAddressV, saved_samp_v);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMinFilter, saved_samp_min);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMagFilter, saved_samp_mag);
    (void)set_sampler_state_locked(dev, 0, kD3d9SampMipFilter, saved_samp_mip);

    (void)set_render_state_locked(dev, kD3d9RsScissorTestEnable, saved_rs_scissor);
    (void)set_render_state_locked(dev, kD3d9RsAlphaBlendEnable, saved_rs_alpha_blend);
    (void)set_render_state_locked(dev, kD3d9RsSeparateAlphaBlendEnable, saved_rs_sep_alpha_blend);
    (void)set_render_state_locked(dev, kD3d9RsSrcBlend, saved_rs_src_blend);
    (void)set_render_state_locked(dev, kD3d9RsDestBlend, saved_rs_dst_blend);
    (void)set_render_state_locked(dev, kD3d9RsBlendOp, saved_rs_blend_op);
    (void)set_render_state_locked(dev, kD3d9RsColorWriteEnable, saved_rs_color_write);
    (void)set_render_state_locked(dev, kD3d9RsZEnable, saved_rs_z_enable);
    (void)set_render_state_locked(dev, kD3d9RsZWriteEnable, saved_rs_z_write);
    (void)set_render_state_locked(dev, kD3d9RsCullMode, saved_rs_cull);
  });

  // Configure a conservative fill state.
  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsAlphaBlendEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsSeparateAlphaBlendEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsSrcBlend, kD3d9BlendOne) ||
      !set_render_state_locked(dev, kD3d9RsDestBlend, kD3d9BlendZero) ||
      !set_render_state_locked(dev, kD3d9RsBlendOp, kD3d9BlendOpAdd) ||
      !set_render_state_locked(dev, kD3d9RsColorWriteEnable, 0xFu) ||
      !set_render_state_locked(dev, kD3d9RsZEnable, 0u) ||
      !set_render_state_locked(dev, kD3d9RsZWriteEnable, FALSE) ||
      !set_render_state_locked(dev, kD3d9RsCullMode, kD3d9CullNone)) {
    return E_OUTOFMEMORY;
  }

  if (!set_sampler_state_locked(dev, 0, kD3d9SampAddressU, kD3d9TexAddressClamp) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampAddressV, kD3d9TexAddressClamp) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMinFilter, kD3d9TexFilterPoint) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMagFilter, kD3d9TexFilterPoint) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMipFilter, kD3d9TexFilterNone)) {
    return E_OUTOFMEMORY;
  }

  const float a = static_cast<float>((color_argb >> 24) & 0xFF) / 255.0f;
  const float r = static_cast<float>((color_argb >> 16) & 0xFF) / 255.0f;
  const float g = static_cast<float>((color_argb >> 8) & 0xFF) / 255.0f;
  const float b = static_cast<float>((color_argb >> 0) & 0xFF) / 255.0f;

  dev->render_targets[0] = dst;
  dev->render_targets[1] = nullptr;
  dev->render_targets[2] = nullptr;
  dev->render_targets[3] = nullptr;
  dev->depth_stencil = nullptr;
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->viewport = {0.0f, 0.0f, static_cast<float>(dst->width), static_cast<float>(dst->height), 0.0f, 1.0f};
  if (!emit_set_viewport_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  // Disable scissor rect clipping (quad already matches dst_rect).
  dev->scissor_enabled = FALSE;
  if (!emit_set_scissor_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  // Bind copy shaders.
  dev->vs = dev->builtin_copy_vs;
  dev->ps = dev->builtin_copy_ps;
  if (!emit_bind_shaders_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  const float color[4] = {r, g, b, a};
  if (!set_shader_const_f_locked(dev, kD3d9ShaderStagePs, 0, color, 1)) {
    return E_OUTOFMEMORY;
  }

  // Bind dummy texture (host substitutes a 1x1 white texture for handle 0).
  dev->textures[0] = nullptr;
  if (!emit_set_texture_locked(dev, 0)) {
    return E_OUTOFMEMORY;
  }

  // Bind input layout + vertex buffer.
  dev->vertex_decl = dev->builtin_copy_decl;
  if (!emit_set_input_layout_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->streams[0].vb = dev->builtin_copy_vb;
  dev->streams[0].offset_bytes = 0;
  dev->streams[0].stride_bytes = sizeof(BlitVertex);
  if (!emit_set_vertex_buffer_locked(dev, 0) ||
      !emit_set_topology_locked(dev, AEROGPU_TOPOLOGY_TRIANGLESTRIP)) {
    return E_OUTOFMEMORY;
  }

  const float dst_w = static_cast<float>(dst->width);
  const float dst_h = static_cast<float>(dst->height);

  const float x0 = (2.0f * static_cast<float>(dst_rect.left) / dst_w) - 1.0f;
  const float x1 = (2.0f * static_cast<float>(dst_rect.right) / dst_w) - 1.0f;
  const float y0 = 1.0f - (2.0f * static_cast<float>(dst_rect.top) / dst_h);
  const float y1 = 1.0f - (2.0f * static_cast<float>(dst_rect.bottom) / dst_h);

  BlitVertex verts[4] = {
      {x0, y0, 0.0f, 1.0f, 0.0f, 0.0f},
      {x0, y1, 0.0f, 1.0f, 0.0f, 0.0f},
      {x1, y0, 0.0f, 1.0f, 0.0f, 0.0f},
      {x1, y1, 0.0f, 1.0f, 0.0f, 0.0f},
  };

  if (!upload_resource_bytes_locked(dev,
                                    dev->builtin_copy_vb,
                                    /*offset_bytes=*/0,
                                    reinterpret_cast<const uint8_t*>(verts),
                                    sizeof(verts))) {
    return E_OUTOFMEMORY;
  }

  // Draw.
  // Ensure the command buffer has space before we track allocations; tracking
  // may force a submission split, and command-buffer splits must not occur
  // after tracking or the allocation list would be out of sync.
  if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_draw), 4))) {
    return E_OUTOFMEMORY;
  }
  hr = track_blit_draw_state_locked(dev);
  if (FAILED(hr)) {
    return hr;
  }
  auto* draw = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!draw) {
    return E_OUTOFMEMORY;
  }
  draw->vertex_count = 4;
  draw->instance_count = 1;
  draw->first_vertex = 0;
  draw->first_instance = 0;
  (void)restore;
  return S_OK;
}

HRESULT update_surface_locked(Device* dev,
                              Resource* src,
                              const RECT* src_rect_in,
                              Resource* dst,
                              const POINT* dst_point_in) {
  if (!dev || !src || !dst) {
    return E_INVALIDARG;
  }
  const bool can_fast_copy = (src->format == dst->format);
  const bool can_convert_4bpp =
      (bytes_per_pixel(src->format) == 4u) && (bytes_per_pixel(dst->format) == 4u) &&
      ((src->format == kD3d9FmtA8R8G8B8 || src->format == kD3d9FmtX8R8G8B8 || src->format == kD3d9FmtA8B8G8R8) &&
       (dst->format == kD3d9FmtA8R8G8B8 || dst->format == kD3d9FmtX8R8G8B8 || dst->format == kD3d9FmtA8B8G8R8));
  if (!can_fast_copy && !can_convert_4bpp) {
    // UpdateSurface requires compatible formats; return INVALIDCALL-style failure
    // rather than E_NOTIMPL (which can cause callers to assume the DDI is missing).
    return D3DERR_INVALIDCALL;
  }
  if (dst->handle == 0) {
    // System-memory pool surfaces are CPU-only and do not have a backing GPU
    // resource handle to upload into.
    return E_INVALIDARG;
  }

  RECT src_rect{};
  if (!clamp_rect(src_rect_in, src->width, src->height, &src_rect)) {
    return S_OK;
  }

  int64_t dst_x = 0;
  int64_t dst_y = 0;
  if (dst_point_in) {
    dst_x = static_cast<int64_t>(dst_point_in->x);
    dst_y = static_cast<int64_t>(dst_point_in->y);
  }

  // D3D9 UpdateSurface specifies a destination point (top-left corner). Build a
  // destination rect by translating the source rect and clip it to the dst
  // surface bounds. Any out-of-bounds portions are treated as a no-op for
  // resilience in compositor paths.
  int64_t src_left = static_cast<int64_t>(src_rect.left);
  int64_t src_top = static_cast<int64_t>(src_rect.top);
  int64_t src_right = static_cast<int64_t>(src_rect.right);
  int64_t src_bottom = static_cast<int64_t>(src_rect.bottom);

  int64_t dst_left = dst_x;
  int64_t dst_top = dst_y;

  const int64_t dst_w = static_cast<int64_t>(dst->width);
  const int64_t dst_h = static_cast<int64_t>(dst->height);
  if (dst_w <= 0 || dst_h <= 0) {
    return S_OK;
  }

  const int64_t src_w = src_right - src_left;
  const int64_t src_h = src_bottom - src_top;

  // Clip negative offsets by advancing the source rect.
  if (dst_left < 0) {
    // Compute abs(dst_left) without triggering signed overflow for INT64_MIN.
    const uint64_t shift_u = static_cast<uint64_t>(-(dst_left + 1)) + 1;
    if (shift_u >= static_cast<uint64_t>(src_w)) {
      return S_OK;
    }
    src_left += static_cast<int64_t>(shift_u);
    dst_left = 0;
  }
  if (dst_top < 0) {
    const uint64_t shift_u = static_cast<uint64_t>(-(dst_top + 1)) + 1;
    if (shift_u >= static_cast<uint64_t>(src_h)) {
      return S_OK;
    }
    src_top += static_cast<int64_t>(shift_u);
    dst_top = 0;
  }

  // Entirely out-of-bounds destination.
  if (dst_left >= dst_w || dst_top >= dst_h) {
    return S_OK;
  }

  int64_t dst_right = dst_left + (src_right - src_left);
  int64_t dst_bottom = dst_top + (src_bottom - src_top);

  // Clip to destination bounds.
  if (dst_right > dst_w) {
    src_right -= dst_right - dst_w;
    dst_right = dst_w;
  }
  if (dst_bottom > dst_h) {
    src_bottom -= dst_bottom - dst_h;
    dst_bottom = dst_h;
  }

  if (src_right <= src_left || src_bottom <= src_top) {
    return S_OK;
  }

  const uint32_t copy_w = static_cast<uint32_t>(src_right - src_left);
  const uint32_t copy_h = static_cast<uint32_t>(src_bottom - src_top);
  if (!copy_w || !copy_h) {
    return S_OK;
  }

  const uint32_t bpp = bytes_per_pixel(src->format);
  const uint32_t row_bytes = copy_w * bpp;
  if (src->row_pitch == 0 || dst->row_pitch == 0) {
    return E_FAIL;
  }

  const uint64_t dst_bytes = static_cast<uint64_t>(dst->row_pitch) * dst->height;
  if (dst_bytes == 0 || dst_bytes > dst->size_bytes) {
    return E_FAIL;
  }

  // Compat path: update CPU shadow storage. Host-owned resources are updated by
  // embedding raw bytes in the command stream; guest-backed resources must use
  // RESOURCE_DIRTY_RANGE so the host re-uploads from the guest allocation table.
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id == 0 && dst->storage.size() >= dst_bytes) {
    // Host-backed destination: update CPU shadow storage and emit UPLOAD_RESOURCE.
    // The source may still be guest-backed (systemmem/readback surfaces), so map
    // its WDDM allocation when necessary.
    const uint64_t src_bytes = static_cast<uint64_t>(src->row_pitch) * src->height;
    if (src_bytes == 0 || src_bytes > src->size_bytes) {
      return E_FAIL;
    }

    const uint8_t* src_base = nullptr;
    void* src_ptr = nullptr;
    bool src_locked = false;

    auto unlock_src = make_scope_exit([&]() {
      if (src_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
    });

    bool use_src_storage = src->storage.size() >= src_bytes;
    // Guest-backed resources may still allocate CPU shadow storage (e.g. shared
    // resources opened via OpenResource). In WDDM builds the authoritative bytes
    // are in the allocation mapping, so prefer LockCb.
    if (src->backing_alloc_id != 0) {
      use_src_storage = false;
    }

    if (use_src_storage) {
      src_base = src->storage.data();
    } else {
      if (src->wddm_hAllocation == 0 || dev->wddm_device == 0) {
        return E_FAIL;
      }
      const HRESULT src_lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                       dev->wddm_device,
                                                       src->wddm_hAllocation,
                                                       0,
                                                       src_bytes,
                                                       kD3DLOCK_READONLY,
                                                       &src_ptr,
                                                       dev->wddm_context.hContext);
      if (FAILED(src_lock_hr) || !src_ptr) {
        return FAILED(src_lock_hr) ? src_lock_hr : E_FAIL;
      }
      src_locked = true;
      src_base = static_cast<const uint8_t*>(src_ptr);
    }

    // First pass: copy/convert into dst->storage while holding any source mapping.
    for (uint32_t y = 0; y < copy_h; ++y) {
      const uint64_t src_off = (static_cast<uint64_t>(src_top) + y) * src->row_pitch +
                               static_cast<uint64_t>(src_left) * bpp;
      const uint64_t dst_off = (static_cast<uint64_t>(dst_top) + y) * dst->row_pitch +
                               static_cast<uint64_t>(dst_left) * bpp;
      if (src_off + row_bytes > src_bytes || dst_off + row_bytes > dst->storage.size()) {
        return E_INVALIDARG;
      }

      uint8_t* dst_row = dst->storage.data() + static_cast<size_t>(dst_off);
      const uint8_t* src_row = src_base + static_cast<size_t>(src_off);
      if (can_fast_copy) {
        std::memcpy(dst_row, src_row, row_bytes);
      } else {
        for (uint32_t x = 0; x < copy_w; ++x) {
          const uint8_t* s = src_row + static_cast<size_t>(x) * 4;
          uint8_t* d = dst_row + static_cast<size_t>(x) * 4;
          if (!convert_pixel_4bpp(src->format, dst->format, s, d)) {
            return D3DERR_INVALIDCALL;
          }
        }
      }
    }

    // Unlock allocations before any call that can trigger a command-buffer split
    // (ensure_cmd_space / allocation tracking).
    (void)unlock_src;
    if (src_locked) {
      (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                   dev->wddm_device,
                                   src->wddm_hAllocation,
                                   dev->wddm_context.hContext);
      src_locked = false;
    }

    // Second pass: upload the updated rows from CPU storage.
    for (uint32_t y = 0; y < copy_h; ++y) {
      const uint64_t dst_off = (static_cast<uint64_t>(dst_top) + y) * dst->row_pitch +
                               static_cast<uint64_t>(dst_left) * bpp;
      uint8_t* dst_row = dst->storage.data() + static_cast<size_t>(dst_off);
      if (!upload_resource_bytes_locked(dev,
                                        dst,
                                        /*offset_bytes=*/dst_off,
                                        dst_row,
                                        row_bytes)) {
        return E_OUTOFMEMORY;
      }
    }

    return S_OK;
  }
#else
  if (dst->storage.size() >= dst_bytes) {
    for (uint32_t y = 0; y < copy_h; ++y) {
      const uint64_t src_off = (static_cast<uint64_t>(src_top) + y) * src->row_pitch +
                               static_cast<uint64_t>(src_left) * bpp;
      const uint64_t dst_off = (static_cast<uint64_t>(dst_top) + y) * dst->row_pitch +
                               static_cast<uint64_t>(dst_left) * bpp;
      if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
        return E_INVALIDARG;
      }

      uint8_t* dst_row = dst->storage.data() + static_cast<size_t>(dst_off);
      const uint8_t* src_row = src->storage.data() + static_cast<size_t>(src_off);
      if (can_fast_copy) {
        std::memcpy(dst_row, src_row, row_bytes);
      } else {
        for (uint32_t x = 0; x < copy_w; ++x) {
          const uint8_t* s = src_row + static_cast<size_t>(x) * 4;
          uint8_t* d = dst_row + static_cast<size_t>(x) * 4;
          if (!convert_pixel_4bpp(src->format, dst->format, s, d)) {
            return D3DERR_INVALIDCALL;
          }
        }
      }

      if (dst->backing_alloc_id == 0) {
        if (!upload_resource_bytes_locked(dev,
                                          dst,
                                          /*offset_bytes=*/dst_off,
                                          dst_row,
                                          row_bytes)) {
          return E_OUTOFMEMORY;
        }
      }
    }

    if (dst->backing_alloc_id != 0) {
      const uint64_t dirty_offset = static_cast<uint64_t>(dst_top) * dst->row_pitch +
                                    static_cast<uint64_t>(dst_left) * bpp;
      const uint64_t dirty_size = static_cast<uint64_t>(copy_h - 1) * dst->row_pitch + row_bytes;
      if (dirty_offset + dirty_size > dst_bytes) {
        return E_INVALIDARG;
      }

      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
        return E_OUTOFMEMORY;
      }
      const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
      if (FAILED(track_hr)) {
        return track_hr;
      }
      auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->resource_handle = dst->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = dirty_offset;
      cmd->size_bytes = dirty_size;
    }
    return S_OK;
  }
#endif

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id != 0 && dst->wddm_hAllocation != 0 && dev->wddm_device != 0) {
    const uint64_t src_bytes = static_cast<uint64_t>(src->row_pitch) * src->height;
    if (src_bytes == 0 || src_bytes > src->size_bytes) {
      return E_FAIL;
    }

    const uint8_t* src_base = nullptr;
    void* src_ptr = nullptr;
    bool src_locked = false;

    bool use_src_storage = src->storage.size() >= src_bytes;
    // Guest-backed resources may still allocate CPU shadow storage (e.g. shared
    // resources opened via OpenResource). In WDDM builds the authoritative bytes
    // are in the allocation mapping, so prefer LockCb.
    if (src->backing_alloc_id != 0) {
      use_src_storage = false;
    }

    if (use_src_storage) {
      src_base = src->storage.data();
    } else {
      if (src->wddm_hAllocation == 0) {
        return E_FAIL;
      }
      const HRESULT src_lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                       dev->wddm_device,
                                                       src->wddm_hAllocation,
                                                       0,
                                                       src_bytes,
                                                       kD3DLOCK_READONLY,
                                                       &src_ptr,
                                                       dev->wddm_context.hContext);
      if (FAILED(src_lock_hr) || !src_ptr) {
        return FAILED(src_lock_hr) ? src_lock_hr : E_FAIL;
      }
      src_locked = true;
      src_base = static_cast<const uint8_t*>(src_ptr);
    }

    void* dst_ptr = nullptr;
    bool dst_locked = false;
    auto unlock = make_scope_exit([&]() {
      if (dst_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     dst->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
      if (src_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
    });

    const HRESULT lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                 dev->wddm_device,
                                                 dst->wddm_hAllocation,
                                                 0,
                                                 dst_bytes,
                                                 &dst_ptr,
                                                 dev->wddm_context.hContext);
    if (FAILED(lock_hr) || !dst_ptr) {
      return FAILED(lock_hr) ? lock_hr : E_FAIL;
    }
    dst_locked = true;

    auto* dst_base = static_cast<uint8_t*>(dst_ptr);
    for (uint32_t y = 0; y < copy_h; ++y) {
      const uint64_t src_off = (static_cast<uint64_t>(src_top) + y) * src->row_pitch +
                               static_cast<uint64_t>(src_left) * bpp;
      const uint64_t dst_off = (static_cast<uint64_t>(dst_top) + y) * dst->row_pitch +
                               static_cast<uint64_t>(dst_left) * bpp;
      if (src_off + row_bytes > src_bytes || dst_off + row_bytes > dst_bytes) {
        return E_INVALIDARG;
      }

      const uint8_t* src_row = src_base + static_cast<size_t>(src_off);
      uint8_t* dst_row = dst_base + dst_off;
      if (can_fast_copy) {
        std::memcpy(dst_row, src_row, row_bytes);
      } else {
        for (uint32_t x = 0; x < copy_w; ++x) {
          const uint8_t* s = src_row + static_cast<size_t>(x) * 4;
          uint8_t* d = dst_row + static_cast<size_t>(x) * 4;
          if (!convert_pixel_4bpp(src->format, dst->format, s, d)) {
            return D3DERR_INVALIDCALL;
          }
        }
      }
    }

    // Unlock allocations before any call that can trigger a command-buffer split
    // (ensure_cmd_space / allocation tracking).
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 dst->wddm_hAllocation,
                                 dev->wddm_context.hContext);
    dst_locked = false;
    if (src_locked) {
      (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                   dev->wddm_device,
                                   src->wddm_hAllocation,
                                   dev->wddm_context.hContext);
      src_locked = false;
    }

    if (dst->handle != 0 && dst->backing_alloc_id != 0) {
      const uint64_t dirty_offset = static_cast<uint64_t>(dst_top) * dst->row_pitch +
                                    static_cast<uint64_t>(dst_left) * bpp;
      const uint64_t dirty_size = static_cast<uint64_t>(copy_h - 1) * dst->row_pitch + row_bytes;
      if (dirty_offset + dirty_size > dst_bytes) {
        return E_INVALIDARG;
      }

      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
        return E_OUTOFMEMORY;
      }
      const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
      if (FAILED(track_hr)) {
        return track_hr;
      }
      auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->resource_handle = dst->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = dirty_offset;
      cmd->size_bytes = dirty_size;
    }

    return S_OK;
  }
#endif

  return E_FAIL;
}

HRESULT update_texture_locked(Device* dev, Resource* src, Resource* dst) {
  if (!dev || !src || !dst) {
    return E_INVALIDARG;
  }
  if (src->width != dst->width || src->height != dst->height ||
      src->mip_levels != dst->mip_levels || src->size_bytes != dst->size_bytes) {
    return D3DERR_INVALIDCALL;
  }

  if (dst->handle == 0) {
    return E_INVALIDARG;
  }

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
  if (src->storage.size() < dst->size_bytes) {
    return E_INVALIDARG;
  }
#endif

  const bool can_fast_copy = (src->format == dst->format);
  const bool can_convert_4bpp =
      (bytes_per_pixel(src->format) == 4u) && (bytes_per_pixel(dst->format) == 4u) &&
      ((src->format == kD3d9FmtA8R8G8B8 || src->format == kD3d9FmtX8R8G8B8 || src->format == kD3d9FmtA8B8G8R8) &&
       (dst->format == kD3d9FmtA8R8G8B8 || dst->format == kD3d9FmtX8R8G8B8 || dst->format == kD3d9FmtA8B8G8R8));
  if (!can_fast_copy && !can_convert_4bpp) {
    return D3DERR_INVALIDCALL;
  }

  // Compat path: update CPU shadow storage. Host-owned resources are updated by
  // embedding raw bytes via UPLOAD_RESOURCE. Guest-backed resources must use
  // RESOURCE_DIRTY_RANGE so the host re-uploads from guest memory.
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id == 0 && dst->storage.size() >= dst->size_bytes && dst->size_bytes) {
#else
  if (dst->storage.size() >= dst->size_bytes) {
#endif
    const uint8_t* src_bytes = src->storage.data();
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    void* src_ptr = nullptr;
    bool src_locked = false;
    auto unlock_src = make_scope_exit([&]() {
      if (src_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
    });

    bool use_src_storage = src->storage.size() >= dst->size_bytes;
    // Guest-backed resources may still allocate CPU shadow storage; prefer
    // mapping the authoritative allocation.
    if (src->backing_alloc_id != 0) {
      use_src_storage = false;
    }
    if (!use_src_storage) {
      if (src->wddm_hAllocation == 0 || dev->wddm_device == 0) {
        return E_FAIL;
      }
      const HRESULT src_lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                       dev->wddm_device,
                                                       src->wddm_hAllocation,
                                                       0,
                                                       dst->size_bytes,
                                                       kD3DLOCK_READONLY,
                                                       &src_ptr,
                                                       dev->wddm_context.hContext);
      if (FAILED(src_lock_hr) || !src_ptr) {
        return FAILED(src_lock_hr) ? src_lock_hr : E_FAIL;
      }
      src_locked = true;
      src_bytes = static_cast<const uint8_t*>(src_ptr);
    }
#endif

    if (can_fast_copy) {
      std::memcpy(dst->storage.data(), src_bytes, dst->size_bytes);
    } else {
      if ((dst->size_bytes & 3u) != 0) {
        return D3DERR_INVALIDCALL;
      }
      for (size_t i = 0; i + 3 < static_cast<size_t>(dst->size_bytes); i += 4) {
        if (!convert_pixel_4bpp(src->format, dst->format, src_bytes + i, &dst->storage[i])) {
          return D3DERR_INVALIDCALL;
        }
      }
    }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    // Unlock allocations before any call that can trigger a command-buffer split
    // (ensure_cmd_space / allocation tracking).
    (void)unlock_src;
    if (src_locked) {
      (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                   dev->wddm_device,
                                   src->wddm_hAllocation,
                                   dev->wddm_context.hContext);
      src_locked = false;
    }
#endif

#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
    if (dst->backing_alloc_id == 0) {
#endif
      if (!upload_resource_bytes_locked(dev,
                                        dst,
                                        /*offset_bytes=*/0,
                                        dst->storage.data(),
                                        dst->size_bytes)) {
        return E_OUTOFMEMORY;
      }
#if !(defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)
    } else {
      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
        return E_OUTOFMEMORY;
      }
      const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
      if (FAILED(track_hr)) {
        return track_hr;
      }
      auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->resource_handle = dst->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = 0;
      cmd->size_bytes = dst->size_bytes;
    }
#endif
    return S_OK;
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
  if (dst->backing_alloc_id != 0 && dst->wddm_hAllocation != 0 && dev->wddm_device != 0 && dst->size_bytes) {
    const uint8_t* src_bytes = nullptr;
    void* src_ptr = nullptr;
    bool src_locked = false;

    void* dst_ptr = nullptr;
    bool dst_locked = false;

    auto unlock = make_scope_exit([&]() {
      if (dst_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     dst->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
      if (src_locked) {
        (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                     dev->wddm_device,
                                     src->wddm_hAllocation,
                                     dev->wddm_context.hContext);
      }
    });

    bool use_src_storage = src->storage.size() >= dst->size_bytes;
    // Guest-backed resources may still allocate CPU shadow storage; prefer
    // mapping the authoritative allocation.
    if (src->backing_alloc_id != 0) {
      use_src_storage = false;
    }

    if (use_src_storage) {
      src_bytes = src->storage.data();
    } else {
      if (src->wddm_hAllocation == 0) {
        return E_FAIL;
      }
      const HRESULT src_lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                       dev->wddm_device,
                                                       src->wddm_hAllocation,
                                                       0,
                                                       dst->size_bytes,
                                                       kD3DLOCK_READONLY,
                                                       &src_ptr,
                                                       dev->wddm_context.hContext);
      if (FAILED(src_lock_hr) || !src_ptr) {
        return FAILED(src_lock_hr) ? src_lock_hr : E_FAIL;
      }
      src_locked = true;
      src_bytes = static_cast<const uint8_t*>(src_ptr);
    }

    const HRESULT lock_hr = wddm_lock_allocation(dev->wddm_callbacks,
                                                 dev->wddm_device,
                                                 dst->wddm_hAllocation,
                                                 0,
                                                 dst->size_bytes,
                                                 &dst_ptr,
                                                 dev->wddm_context.hContext);
    if (FAILED(lock_hr) || !dst_ptr) {
      return FAILED(lock_hr) ? lock_hr : E_FAIL;
    }
    dst_locked = true;

    if (can_fast_copy) {
      std::memcpy(dst_ptr, src_bytes, dst->size_bytes);
    } else {
      if ((dst->size_bytes & 3u) != 0) {
        return D3DERR_INVALIDCALL;
      }
      auto* dst_bytes = static_cast<uint8_t*>(dst_ptr);
      for (size_t i = 0; i + 3 < static_cast<size_t>(dst->size_bytes); i += 4) {
        if (!convert_pixel_4bpp(src->format, dst->format, src_bytes + i, &dst_bytes[i])) {
          return D3DERR_INVALIDCALL;
        }
      }
    }

    // Unlock allocations before any call that can trigger a command-buffer split
    // (ensure_cmd_space / allocation tracking).
    (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                 dev->wddm_device,
                                 dst->wddm_hAllocation,
                                 dev->wddm_context.hContext);
    dst_locked = false;
    if (src_locked) {
      (void)wddm_unlock_allocation(dev->wddm_callbacks,
                                   dev->wddm_device,
                                   src->wddm_hAllocation,
                                   dev->wddm_context.hContext);
      src_locked = false;
    }

    if (dst->backing_alloc_id != 0) {
      if (!ensure_cmd_space(dev, align_up(sizeof(aerogpu_cmd_resource_dirty_range), 4))) {
        return E_OUTOFMEMORY;
      }
      const HRESULT track_hr = track_resource_allocation_locked(dev, dst, /*write=*/false);
      if (FAILED(track_hr)) {
        return track_hr;
      }
      auto* cmd = append_fixed_locked<aerogpu_cmd_resource_dirty_range>(dev, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      if (!cmd) {
        return E_OUTOFMEMORY;
      }
      cmd->resource_handle = dst->handle;
      cmd->reserved0 = 0;
      cmd->offset_bytes = 0;
      cmd->size_bytes = dst->size_bytes;
    }
    return S_OK;
  }
#endif

  return E_FAIL;
}

void destroy_blit_objects_locked(Device* dev) {
  if (!dev) {
    return;
  }

  if (dev->builtin_copy_vb) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_resource>(dev, AEROGPU_CMD_DESTROY_RESOURCE)) {
      cmd->resource_handle = dev->builtin_copy_vb->handle;
      cmd->reserved0 = 0;
    }
#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI
    if (dev->builtin_copy_vb->wddm_hAllocation != 0 && dev->wddm_device != 0) {
      (void)wddm_destroy_allocation(dev->wddm_callbacks,
                                    dev->wddm_device,
                                    dev->builtin_copy_vb->wddm_hAllocation,
                                    dev->wddm_context.hContext);
      dev->builtin_copy_vb->wddm_hAllocation = 0;
    }
#endif
    delete dev->builtin_copy_vb;
    dev->builtin_copy_vb = nullptr;
  }

  if (dev->builtin_copy_decl) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_input_layout>(dev, AEROGPU_CMD_DESTROY_INPUT_LAYOUT)) {
      cmd->input_layout_handle = dev->builtin_copy_decl->handle;
      cmd->reserved0 = 0;
    }
    delete dev->builtin_copy_decl;
    dev->builtin_copy_decl = nullptr;
  }

  if (dev->builtin_copy_vs) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_shader>(dev, AEROGPU_CMD_DESTROY_SHADER)) {
      cmd->shader_handle = dev->builtin_copy_vs->handle;
      cmd->reserved0 = 0;
    }
    delete dev->builtin_copy_vs;
    dev->builtin_copy_vs = nullptr;
  }

  if (dev->builtin_copy_ps) {
    if (auto* cmd = append_fixed_locked<aerogpu_cmd_destroy_shader>(dev, AEROGPU_CMD_DESTROY_SHADER)) {
      cmd->shader_handle = dev->builtin_copy_ps->handle;
      cmd->reserved0 = 0;
    }
    delete dev->builtin_copy_ps;
    dev->builtin_copy_ps = nullptr;
  }
}

} // namespace aerogpu
