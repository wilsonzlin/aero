#pragma once

#include <atomic>
#include <cstdint>
#include <condition_variable>
#include <deque>
#include <mutex>
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

inline uint32_t bytes_per_pixel(uint32_t d3d9_format) {
  // Conservative: handle the formats DWM/typical D3D9 samples use.
  // For unknown formats we assume 4 bytes to avoid undersizing.
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8 / D3DFMT_A8B8G8R8
    case 21u:
    case 22u:
    case 32u:
      return 4;
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

struct Resource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;
  uint32_t type = 0;
  uint32_t format = 0;
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

  // WDDM allocation handle for this resource's backing store (per-process).
  // The stable ID referenced in command buffers is `backing_alloc_id`.
  WddmAllocationHandle wddm_hAllocation = 0;

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
  AEROGPU_D3D9DDI_SHADER_STAGE stage = AEROGPU_D3D9DDI_SHADER_STAGE_VS;
  std::vector<uint8_t> bytecode;
};

struct VertexDecl {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct Query {
  uint32_t type = 0;
  std::atomic<uint64_t> fence_value{0};
  std::atomic<bool> issued{false};
  std::atomic<bool> completion_logged{false};
};

struct Adapter {
  // The adapter LUID used for caching/reuse when the runtime opens the same
  // adapter multiple times (common with D3D9Ex + DWM).
  LUID luid = {};

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

  // 64-bit token generator for shared-surface interop (EXPORT/IMPORT_SHARED_SURFACE).
  ShareTokenAllocator share_token_allocator;

  // Different D3D9 runtimes/headers may use different numeric encodings for the
  // EVENT query type at the DDI boundary. Once we observe the first EVENT query
  // type value we lock it in per-adapter, so we don't accidentally treat other
  // query types (e.g. pipeline stats) as EVENT.
  std::atomic<bool> event_query_type_known{false};
  std::atomic<uint32_t> event_query_type{0};

  // Monotonic cross-process ID allocator used for shared-allocation bookkeeping
  // (e.g. generating stable alloc_id values). The D3D9 UMD may be loaded into
  // multiple guest processes (DWM + apps), so we must coordinate ID allocation
  // cross-process. See aerogpu_d3d9_driver.cpp.
  std::mutex share_token_mutex;
  HANDLE share_token_mapping = nullptr;
  void* share_token_view = nullptr;
  std::atomic<uint64_t> next_share_token{1}; // Fallback if cross-process allocator fails.

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t last_submitted_fence = 0;
  uint64_t completed_fence = 0;

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
  uint32_t primary_rotation = AEROGPU_D3D9DDI_ROTATION_IDENTITY;
};

struct DeviceStateStream {
  Resource* vb = nullptr;
  uint32_t offset_bytes = 0;
  uint32_t stride_bytes = 0;
};

struct Device {
  explicit Device(Adapter* adapter) : adapter(adapter) {
    cmd.reset();

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
  }

  Adapter* adapter = nullptr;
  std::mutex mutex;

  // WDDM state (only populated in real Win7/WDDM builds).
  WddmDeviceCallbacks wddm_callbacks{};
  WddmHandle wddm_device = 0;
  WddmContext wddm_context{};

  CmdWriter cmd;
  AllocationListTracker alloc_list_tracker;

  // D3D9Ex throttling + present statistics.
  //
  // These fields model the D3D9Ex "maximum frame latency" behavior used by DWM:
  // we allow up to max_frame_latency in-flight presents, each tracked by a KMD
  // fence ID (or a bring-up stub fence in non-WDDM builds).
  int32_t gpu_thread_priority = 0; // clamped to [-7, 7]
  uint32_t max_frame_latency = 3;
  std::deque<uint64_t> inflight_present_fences;
  uint32_t present_count = 0;
  uint64_t last_present_qpc = 0;
  std::vector<SwapChain*> swapchains;
  SwapChain* current_swapchain = nullptr;

  // Cached pipeline state.
  Resource* render_targets[4] = {nullptr, nullptr, nullptr, nullptr};
  Resource* depth_stencil = nullptr;
  Resource* textures[16] = {};
  DeviceStateStream streams[16] = {};
  Resource* index_buffer = nullptr;
  AEROGPU_D3D9DDI_INDEX_FORMAT index_format = AEROGPU_D3D9DDI_INDEX_FORMAT_U16;
  uint32_t index_offset_bytes = 0;
  uint32_t topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  Shader* vs = nullptr;
  Shader* ps = nullptr;
  VertexDecl* vertex_decl = nullptr;

  AEROGPU_D3D9DDI_VIEWPORT viewport = {0, 0, 0, 0, 0.0f, 1.0f};
  RECT scissor_rect = {0, 0, 0, 0};
  BOOL scissor_enabled = FALSE;

  // D3D9 state caches used by helper paths (blits, color fills) so they can
  // temporarily override state and restore it afterwards.
  //
  // D3D9 state IDs are sparse, but the commonly-used ranges fit comfortably in
  // 0..255 and the values are cheap to track.
  uint32_t render_states[256] = {};
  uint32_t sampler_states[16][16] = {};

  // Shader float constant register caches (float4 registers).
  float vs_consts_f[256 * 4] = {};
  float ps_consts_f[256 * 4] = {};

  // Built-in resources used for blit/copy operations (StretchRect/Blt).
  Shader* builtin_copy_vs = nullptr;
  Shader* builtin_copy_ps = nullptr;
  VertexDecl* builtin_copy_decl = nullptr;
  Resource* builtin_copy_vb = nullptr;
};

} // namespace aerogpu
