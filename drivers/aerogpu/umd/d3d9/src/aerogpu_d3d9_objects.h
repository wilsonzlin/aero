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
  uint32_t size_bytes = 0;
  uint32_t row_pitch = 0;
  uint32_t slice_pitch = 0;

  // Host-visible backing allocation ID. Written by the KMD into per-allocation
  // private driver data (aerogpu_wddm_alloc_priv). 0 means "host allocated"
  // (no allocation-table entry).
  uint32_t backing_alloc_id = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  bool is_shared = false;
  bool is_shared_alias = false;

  bool locked = false;
  uint32_t locked_offset = 0;
  uint32_t locked_size = 0;

  WddmAllocationHandle wddm_hAllocation = 0;
  uint32_t wddm_alloc_id = 0;
  uint32_t wddm_alloc_offset_bytes = 0;

  std::vector<uint8_t> storage;
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
  uint64_t fence_value = 0;
  bool issued = false;
};

struct Adapter {
  std::atomic<uint32_t> next_handle{1};
  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t last_submitted_fence = 0;
  uint64_t completed_fence = 0;

  // Optional best-effort KMD query path (Win7 user-mode D3DKMTEscape).
  AerogpuKmdQuery kmd_query;
};

struct DeviceStateStream {
  Resource* vb = nullptr;
  uint32_t offset_bytes = 0;
  uint32_t stride_bytes = 0;
};

struct Device {
  explicit Device(Adapter* adapter) : adapter(adapter) {
    cmd.reset();
  }

  Adapter* adapter = nullptr;
  std::mutex mutex;

  CmdWriter cmd;

  // D3D9Ex throttling + present statistics.
  //
  // These fields model the D3D9Ex "maximum frame latency" behavior used by DWM:
  // we allow up to max_frame_latency in-flight presents, each tracked by a KMD
  // fence ID (or a bring-up stub fence in non-WDDM builds).
  uint32_t max_frame_latency = 3;
  std::deque<uint64_t> inflight_present_fences;
  uint32_t present_count = 0;
  uint64_t last_present_qpc = 0;

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
};

} // namespace aerogpu
