#include "../include/aerogpu_d3d9_umd.h"

#include <algorithm>
#include <chrono>
#include <cstring>
#include <memory>
#include <thread>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_log.h"
#include "aerogpu_wddm_alloc.h"

namespace aerogpu {
namespace {

// D3DERR_INVALIDCALL from d3d9.h (returned by UMD for invalid arguments).
constexpr HRESULT kD3DErrInvalidCall = 0x8876086CUL;

// -----------------------------------------------------------------------------
// Minimal caps structure (compat only)
// -----------------------------------------------------------------------------
// The Windows 7 D3D9Ex runtime consumes D3DCAPS9. We intentionally do not embed
// the full public D3DCAPS9 here because this repository is not built with the
// Windows SDK/WDK by default. When integrating into a real driver build,
// implement GetCaps using the real D3DCAPS9.
//
// Fields included here are the ones commonly consulted by the D3D9 runtime and
// DWM to decide whether to enable the advanced composition pipeline.
struct Caps {
  uint32_t max_texture_width;
  uint32_t max_texture_height;
  uint32_t max_volume_extent;
  uint32_t max_simultaneous_textures;
  uint32_t max_streams;

  uint32_t vertex_shader_version;
  uint32_t pixel_shader_version;

  uint32_t presentation_intervals; // bitmask: 1=immediate, 2=one

  uint32_t raster_caps;
  uint32_t texture_caps;
  uint32_t texture_filter_caps;
  uint32_t texture_address_caps;
  uint32_t alpha_cmp_caps;
  uint32_t src_blend_caps;
  uint32_t dest_blend_caps;
  uint32_t shade_caps;
  uint32_t stencil_caps;
};

constexpr uint32_t D3DVS_VERSION(uint32_t major, uint32_t minor) {
  return 0xFFFE0000u | (major << 8) | minor;
}
constexpr uint32_t D3DPS_VERSION(uint32_t major, uint32_t minor) {
  return 0xFFFF0000u | (major << 8) | minor;
}

// D3D9 API/UMD query constants (numeric values from d3d9types.h).
constexpr uint32_t kD3DQueryTypeEvent = 8u;
constexpr uint32_t kD3DIssueEnd = 0x1u;
constexpr uint32_t kD3DGetDataFlush = 0x1u;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// D3DPRESENT_* flags (numeric values from d3d9.h). We only need DONOTWAIT for
// max-frame-latency throttling.
constexpr uint32_t kD3dPresentDoNotWait = 0x00000001u; // D3DPRESENT_DONOTWAIT

// D3DERR_WASSTILLDRAWING (0x8876021C). Returned by PresentEx when DONOTWAIT is
// specified and the present is throttled.
constexpr HRESULT kD3dErrWasStillDrawing = static_cast<HRESULT>(-2005532132);

constexpr uint32_t kMaxFrameLatencyMin = 1;
constexpr uint32_t kMaxFrameLatencyMax = 16;

// Bounded wait for PresentEx throttling. This must be finite to avoid hangs in
// DWM/PresentEx call sites if the GPU stops making forward progress.
constexpr uint32_t kPresentThrottleMaxWaitMs = 100;

uint64_t monotonic_ms() {
#if defined(_WIN32)
  return static_cast<uint64_t>(GetTickCount64());
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

uint64_t qpc_now() {
#if defined(_WIN32)
  LARGE_INTEGER li;
  QueryPerformanceCounter(&li);
  return static_cast<uint64_t>(li.QuadPart);
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

void sleep_ms(uint32_t ms) {
#if defined(_WIN32)
  Sleep(ms);
#else
  std::this_thread::sleep_for(std::chrono::milliseconds(ms));
#endif
}

struct FenceSnapshot {
  uint64_t last_submitted = 0;
  uint64_t last_completed = 0;
};

FenceSnapshot refresh_fence_snapshot(Adapter* adapter) {
  FenceSnapshot snap{};
  if (!adapter) {
    return snap;
  }

#if defined(_WIN32)
  // DWM and many D3D9Ex clients poll EVENT queries in tight loops. Querying the
  // KMD fence counter requires a D3DKMTEscape call, so throttle it to at most
  // once per millisecond tick to avoid burning CPU in the kernel.
  const uint64_t now_ms = monotonic_ms();
  bool should_query_kmd = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->last_kmd_fence_query_ms != now_ms) {
      adapter->last_kmd_fence_query_ms = now_ms;
      should_query_kmd = true;
    }
  }

  if (should_query_kmd) {
    uint64_t submitted = 0;
    uint64_t completed = 0;
    if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
      std::lock_guard<std::mutex> lock(adapter->fence_mutex);
      adapter->last_submitted_fence = std::max<uint64_t>(adapter->last_submitted_fence, submitted);
      adapter->completed_fence = std::max<uint64_t>(adapter->completed_fence, completed);
    }
  }
#endif

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    snap.last_submitted = adapter->last_submitted_fence;
    snap.last_completed = adapter->completed_fence;
  }
  return snap;
}

void retire_completed_presents_locked(Device* dev) {
  if (!dev || !dev->adapter) {
    return;
  }

  const uint64_t completed = refresh_fence_snapshot(dev->adapter).last_completed;
  while (!dev->inflight_present_fences.empty() && dev->inflight_present_fences.front() <= completed) {
    dev->inflight_present_fences.pop_front();
  }
}

bool wait_for_fence(Adapter* adapter, uint64_t fence, uint32_t timeout_ms) {
  if (!adapter || !fence) {
    return true;
  }

  const uint64_t deadline = monotonic_ms() + timeout_ms;
  while (monotonic_ms() < deadline) {
    if (refresh_fence_snapshot(adapter).last_completed >= fence) {
      return true;
    }
    sleep_ms(1);
  }
  return refresh_fence_snapshot(adapter).last_completed >= fence;
}

enum class FenceWaitResult {
  Complete,
  NotReady,
  Failed,
};

#if defined(_WIN32)
using AerogpuNtStatus = LONG;

constexpr AerogpuNtStatus kStatusSuccess = 0x00000000L;
constexpr AerogpuNtStatus kStatusTimeout = 0x00000102L;

struct AerogpuD3DKMTWaitForSynchronizationObject {
  UINT ObjectCount;
  const WddmHandle* ObjectHandleArray;
  const uint64_t* FenceValueArray;
  uint64_t Timeout;
};

using PFND3DKMTWaitForSynchronizationObject =
    AerogpuNtStatus(WINAPI*)(AerogpuD3DKMTWaitForSynchronizationObject* pData);

PFND3DKMTWaitForSynchronizationObject load_d3dkmt_wait_for_sync_object() {
  static PFND3DKMTWaitForSynchronizationObject fn = []() -> PFND3DKMTWaitForSynchronizationObject {
    HMODULE gdi32 = GetModuleHandleW(L"gdi32.dll");
    if (!gdi32) {
      gdi32 = LoadLibraryW(L"gdi32.dll");
    }
    if (!gdi32) {
      return nullptr;
    }
    return reinterpret_cast<PFND3DKMTWaitForSynchronizationObject>(
        GetProcAddress(gdi32, "D3DKMTWaitForSynchronizationObject"));
  }();
  return fn;
}
#endif

FenceWaitResult wait_for_fence(Device* dev, uint64_t fence_value, uint64_t timeout_ms) {
  if (!dev || !dev->adapter) {
    return FenceWaitResult::Failed;
  }
  if (fence_value == 0) {
    return FenceWaitResult::Complete;
  }

  Adapter* adapter = dev->adapter;

  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (adapter->completed_fence >= fence_value) {
      return FenceWaitResult::Complete;
    }
  }

#if defined(_WIN32)
  const WddmHandle sync_object = dev->wddm_context.hSyncObject;
  if (sync_object != 0) {
    auto* wait_fn = load_d3dkmt_wait_for_sync_object();
    if (wait_fn) {
      const WddmHandle handles[1] = {sync_object};
      const uint64_t fences[1] = {fence_value};

      AerogpuD3DKMTWaitForSynchronizationObject args{};
      args.ObjectCount = 1;
      args.ObjectHandleArray = handles;
      args.FenceValueArray = fences;
      args.Timeout = timeout_ms;

      const AerogpuNtStatus st = wait_fn(&args);
      if (st == kStatusSuccess) {
        {
          std::lock_guard<std::mutex> lock(adapter->fence_mutex);
          adapter->completed_fence = std::max(adapter->completed_fence, fence_value);
        }
        adapter->fence_cv.notify_all();
        return FenceWaitResult::Complete;
      }
      if (st == kStatusTimeout) {
        return FenceWaitResult::NotReady;
      }
    }
  }
#endif

  if (timeout_ms == 0) {
    return (refresh_fence_snapshot(adapter).last_completed >= fence_value) ? FenceWaitResult::Complete
                                                                          : FenceWaitResult::NotReady;
  }

  const bool complete = wait_for_fence(adapter, fence_value, static_cast<uint32_t>(timeout_ms));
  return complete ? FenceWaitResult::Complete : FenceWaitResult::NotReady;
}

HRESULT throttle_presents_locked(Device* dev, uint32_t d3d9_present_flags) {
  if (!dev) {
    return E_INVALIDARG;
  }
  if (!dev->adapter) {
    return E_FAIL;
  }

  // Clamp in case callers pass unexpected values.
  if (dev->max_frame_latency < kMaxFrameLatencyMin) {
    dev->max_frame_latency = kMaxFrameLatencyMin;
  }
  if (dev->max_frame_latency > kMaxFrameLatencyMax) {
    dev->max_frame_latency = kMaxFrameLatencyMax;
  }

  retire_completed_presents_locked(dev);

  if (dev->inflight_present_fences.size() < dev->max_frame_latency) {
    return S_OK;
  }

  const bool dont_wait = (d3d9_present_flags & kD3dPresentDoNotWait) != 0;
  if (dont_wait) {
    return kD3dErrWasStillDrawing;
  }

  // Wait for at least one present fence to retire, but never indefinitely.
  const uint64_t deadline = monotonic_ms() + kPresentThrottleMaxWaitMs;
  while (dev->inflight_present_fences.size() >= dev->max_frame_latency) {
    const uint64_t now = monotonic_ms();
    if (now >= deadline) {
      // Forward progress failed; drop the oldest fence to ensure PresentEx
      // returns quickly. This preserves overall system responsiveness at the
      // expense of perfect throttling accuracy under GPU hangs.
      dev->inflight_present_fences.pop_front();
      break;
    }

    const uint64_t oldest = dev->inflight_present_fences.front();
    const uint32_t time_left = static_cast<uint32_t>(std::min<uint64_t>(deadline - now, kPresentThrottleMaxWaitMs));
    (void)wait_for_fence(dev->adapter, oldest, time_left);
    retire_completed_presents_locked(dev);
  }

  return S_OK;
}

uint32_t d3d9_format_to_aerogpu(uint32_t d3d9_format) {
  switch (d3d9_format) {
    // D3DFMT_A8R8G8B8 / D3DFMT_X8R8G8B8
    case 21u:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case 22u:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    // D3DFMT_A8B8G8R8
    case 32u:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    // D3DFMT_D24S8
    case 75u:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

uint32_t d3d9_stage_to_aerogpu_stage(AEROGPU_D3D9DDI_SHADER_STAGE stage) {
  return (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? AEROGPU_SHADER_STAGE_VERTEX : AEROGPU_SHADER_STAGE_PIXEL;
}

uint32_t d3d9_index_format_to_aerogpu(AEROGPU_D3D9DDI_INDEX_FORMAT fmt) {
  return (fmt == AEROGPU_D3D9DDI_INDEX_FORMAT_U32) ? AEROGPU_INDEX_FORMAT_UINT32 : AEROGPU_INDEX_FORMAT_UINT16;
}

// D3DUSAGE_* subset (numeric values from d3d9types.h).
constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;

uint32_t d3d9_usage_to_aerogpu_usage_flags(uint32_t usage) {
  uint32_t flags = AEROGPU_RESOURCE_USAGE_TEXTURE;
  if (usage & kD3DUsageRenderTarget) {
    flags |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (usage & kD3DUsageDepthStencil) {
    flags |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return flags;
}

uint32_t d3d9_prim_to_topology(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim) {
  switch (prim) {
    case AEROGPU_D3D9DDI_PRIM_POINTLIST:
      return AEROGPU_TOPOLOGY_POINTLIST;
    case AEROGPU_D3D9DDI_PRIM_LINELIST:
      return AEROGPU_TOPOLOGY_LINELIST;
    case AEROGPU_D3D9DDI_PRIM_LINESTRIP:
      return AEROGPU_TOPOLOGY_LINESTRIP;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLESTRIP:
      return AEROGPU_TOPOLOGY_TRIANGLESTRIP;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLEFAN:
      return AEROGPU_TOPOLOGY_TRIANGLEFAN;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLELIST:
    default:
      return AEROGPU_TOPOLOGY_TRIANGLELIST;
  }
}

uint32_t vertex_count_from_primitive(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim, uint32_t primitive_count) {
  switch (prim) {
    case AEROGPU_D3D9DDI_PRIM_POINTLIST:
      return primitive_count;
    case AEROGPU_D3D9DDI_PRIM_LINELIST:
      return primitive_count * 2;
    case AEROGPU_D3D9DDI_PRIM_LINESTRIP:
      return primitive_count + 1;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLELIST:
      return primitive_count * 3;
    case AEROGPU_D3D9DDI_PRIM_TRIANGLESTRIP:
    case AEROGPU_D3D9DDI_PRIM_TRIANGLEFAN:
      return primitive_count + 2;
    default:
      return primitive_count * 3;
  }
}

uint32_t index_count_from_primitive(AEROGPU_D3D9DDI_PRIMITIVE_TYPE prim, uint32_t primitive_count) {
  // Indexed draws follow the same primitive->index expansion rules.
  return vertex_count_from_primitive(prim, primitive_count);
}

// -----------------------------------------------------------------------------
// Handle helpers
// -----------------------------------------------------------------------------

Adapter* as_adapter(AEROGPU_D3D9DDI_HADAPTER hAdapter) {
  return reinterpret_cast<Adapter*>(hAdapter);
}

Device* as_device(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  return reinterpret_cast<Device*>(hDevice);
}

Resource* as_resource(AEROGPU_D3D9DDI_HRESOURCE hRes) {
  return reinterpret_cast<Resource*>(hRes);
}

Shader* as_shader(AEROGPU_D3D9DDI_HSHADER hShader) {
  return reinterpret_cast<Shader*>(hShader);
}

VertexDecl* as_vertex_decl(AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  return reinterpret_cast<VertexDecl*>(hDecl);
}

Query* as_query(AEROGPU_D3D9DDI_HQUERY hQuery) {
  return reinterpret_cast<Query*>(hQuery);
}

// -----------------------------------------------------------------------------
// Command emission helpers (protocol: drivers/aerogpu/protocol/aerogpu_cmd.h)
// -----------------------------------------------------------------------------

void emit_set_render_targets_locked(Device* dev) {
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = 4;
  cmd->depth_stencil = dev->depth_stencil ? dev->depth_stencil->handle : 0;

  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  for (uint32_t i = 0; i < 4; i++) {
    cmd->colors[i] = dev->render_targets[i] ? dev->render_targets[i]->handle : 0;
  }
}

void emit_bind_shaders_locked(Device* dev) {
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = dev->vs ? dev->vs->handle : 0;
  cmd->ps = dev->ps ? dev->ps->handle : 0;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void emit_set_topology_locked(Device* dev, uint32_t topology) {
  if (dev->topology == topology) {
    return;
  }
  dev->topology = topology;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = topology;
  cmd->reserved0 = 0;
}

void emit_create_resource_locked(Device* dev, Resource* res) {
  if (!dev || !res) {
    return;
  }

  if (res->kind == ResourceKind::Buffer) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER | AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;
    return;
  }

  if (res->kind == ResourceKind::Surface || res->kind == ResourceKind::Texture2D) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = d3d9_usage_to_aerogpu_usage_flags(res->usage);
    cmd->format = d3d9_format_to_aerogpu(res->format);
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch;
    cmd->backing_alloc_id = res->backing_alloc_id;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;
    return;
  }
}

void emit_destroy_resource_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
  cmd->resource_handle = handle;
  cmd->reserved0 = 0;
}

void emit_export_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
}

void emit_import_shared_surface_locked(Device* dev, const Resource* res) {
  if (!dev || !res || !res->handle || !res->share_token) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_import_shared_surface>(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  cmd->out_resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->share_token = res->share_token;
}

void emit_create_shader_locked(Device* dev, Shader* sh) {
  if (!dev || !sh) {
    return;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->bytecode.data(), sh->bytecode.size());
  cmd->shader_handle = sh->handle;
  cmd->stage = d3d9_stage_to_aerogpu_stage(sh->stage);
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->bytecode.size());
  cmd->reserved0 = 0;
}

void emit_destroy_shader_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
  cmd->shader_handle = handle;
  cmd->reserved0 = 0;
}

void emit_create_input_layout_locked(Device* dev, VertexDecl* decl) {
  if (!dev || !decl) {
    return;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, decl->blob.data(), decl->blob.size());
  cmd->input_layout_handle = decl->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(decl->blob.size());
  cmd->reserved0 = 0;
}

void emit_destroy_input_layout_locked(Device* dev, aerogpu_handle_t handle) {
  if (!dev || !handle) {
    return;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
}

// -----------------------------------------------------------------------------
// KMD submission stub
// -----------------------------------------------------------------------------

uint64_t submit(Device* dev) {
  // In the initial bring-up implementation we treat submission as synchronous:
  // once the command buffer is "submitted", we immediately mark it complete.
  // A real driver would forward the command buffer GPA/size to the KMD, which
  // would then place a submit descriptor into the shared ring.

  if (!dev) {
    return 0;
  }

  Adapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  if (dev->cmd.empty()) {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    return adapter->last_submitted_fence;
  }

  dev->cmd.finalize();

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->last_submitted_fence = fence;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  // Light logging so we can confirm command flow during integration.
  logf("aerogpu-d3d9: submit cmd_bytes=%zu fence=%llu\n",
       dev->cmd.size(),
       static_cast<unsigned long long>(fence));

  dev->cmd.reset();
  return fence;
}

HRESULT flush_locked(Device* dev) {
  // Flushing an empty command buffer should be a no-op. This matters for
  // D3DGETDATA_FLUSH polling loops (e.g. DWM EVENT queries): if we submit an
  // empty buffer every poll we can flood the KMD/emulator with redundant
  // submissions and increase CPU usage.
  if (!dev || dev->cmd.empty()) {
    return S_OK;
  }
  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_flush>(AEROGPU_CMD_FLUSH);
  cmd->reserved0 = 0;
  cmd->reserved1 = 0;
  submit(dev);
  return S_OK;
}

// -----------------------------------------------------------------------------
// Adapter DDIs
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL adapter_close(AEROGPU_D3D9DDI_HADAPTER hAdapter) {
  auto* adapter = as_adapter(hAdapter);
  delete adapter;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL adapter_get_caps(AEROGPU_D3D9DDI_HADAPTER, void* pCaps, uint32_t caps_size) {
  if (!pCaps) {
    return E_INVALIDARG;
  }
  if (caps_size < sizeof(Caps)) {
    return E_INVALIDARG;
  }

  Caps caps{};
  caps.max_texture_width = 4096;
  caps.max_texture_height = 4096;
  caps.max_volume_extent = 256;
  caps.max_simultaneous_textures = 8;
  caps.max_streams = 16;

  // Aero on Win7 requires at least SM2.0. Keep conservative.
  caps.vertex_shader_version = D3DVS_VERSION(2, 0);
  caps.pixel_shader_version = D3DPS_VERSION(2, 0);

  // Present intervals: immediate (bit0) + one (bit1).
  caps.presentation_intervals = 0x1u | 0x2u;

  // Conservative but sufficient to express typical DWM state.
  caps.raster_caps = 0;
  caps.texture_caps = 0;
  caps.texture_filter_caps = 0;
  caps.texture_address_caps = 0;
  caps.alpha_cmp_caps = 0;
  caps.src_blend_caps = 0;
  caps.dest_blend_caps = 0;
  caps.shade_caps = 0;
  caps.stencil_caps = 0;

  std::memcpy(pCaps, &caps, sizeof(caps));
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    AEROGPU_D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    AEROGPU_D3D9DDI_DEVICEFUNCS* pDeviceFuncs);

// -----------------------------------------------------------------------------
// Device DDIs
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL device_destroy(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  auto* dev = as_device(hDevice);
  if (!dev) {
    return S_OK;
  }

  // Best-effort: flush any outstanding work so we don't abandon a partially
  // constructed submission stream.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    flush_locked(dev);
  }

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  dev->wddm_context.destroy(dev->wddm_callbacks);
  wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
  dev->wddm_device = 0;
#endif

  delete dev;
  return S_OK;
}

static void consume_wddm_alloc_priv(Resource* res,
                                   const void* priv_data,
                                   uint32_t priv_data_size,
                                   bool is_shared_resource) {
  if (!res || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return;
  }

  aerogpu_wddm_alloc_priv priv{};
  std::memcpy(&priv, priv_data, sizeof(priv));

  if (priv.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC || priv.version != AEROGPU_WDDM_ALLOC_PRIV_VERSION) {
    return;
  }

  res->backing_alloc_id = priv.alloc_id;
  res->share_token = priv.share_token;
  if (priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) {
    res->is_shared = true;
  }

  // For compatibility, derive a stable token if share_token is missing.
  if (is_shared_resource && res->share_token == 0 && res->backing_alloc_id != 0) {
    res->share_token = static_cast<uint64_t>(res->backing_alloc_id);
  }
}

HRESULT AEROGPU_D3D9_CALL device_create_resource(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_CREATERESOURCE* pCreateResource) {
  if (!hDevice || !pCreateResource) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  const bool is_shared = (pCreateResource->pSharedHandle != NULL);
  const uint32_t mip_levels = std::max(1u, pCreateResource->mip_levels);
  if (is_shared && mip_levels != 1) {
    // MVP: shared surfaces must be single-allocation (no mip chains/arrays).
    return kD3DErrInvalidCall;
  }

  auto res = std::make_unique<Resource>();
  res->handle = dev->adapter->next_handle.fetch_add(1);
  res->type = pCreateResource->type;
  res->format = pCreateResource->format;
  res->width = pCreateResource->width;
  res->height = pCreateResource->height;
  res->depth = std::max(1u, pCreateResource->depth);
  res->mip_levels = mip_levels;
  res->usage = pCreateResource->usage;

  const bool wants_shared = (pCreateResource->pSharedHandle != nullptr);
  const bool open_existing_shared = wants_shared && (*pCreateResource->pSharedHandle != nullptr);
  res->is_shared = wants_shared;
  res->is_shared_alias = open_existing_shared;

  consume_wddm_alloc_priv(res.get(),
                          pCreateResource->pKmdAllocPrivateData,
                          pCreateResource->KmdAllocPrivateDataSize,
                          res->is_shared);

  // Heuristic: if size is provided, treat as buffer; otherwise treat as a 2D image.
  if (pCreateResource->size) {
    res->kind = ResourceKind::Buffer;
    res->size_bytes = pCreateResource->size;
    res->row_pitch = 0;
    res->slice_pitch = 0;
  } else if (res->width && res->height) {
    // Surface/Texture2D share the same storage layout for now.
    res->kind = (res->mip_levels > 1) ? ResourceKind::Texture2D : ResourceKind::Surface;

    const uint32_t bpp = bytes_per_pixel(res->format);
    uint32_t w = std::max(1u, res->width);
    uint32_t h = std::max(1u, res->height);

    res->row_pitch = w * bpp;
    res->slice_pitch = res->row_pitch * h;

    uint64_t total = 0;
    for (uint32_t level = 0; level < res->mip_levels; level++) {
      total += static_cast<uint64_t>(std::max(1u, w)) * static_cast<uint64_t>(std::max(1u, h)) * bpp;
      w = std::max(1u, w / 2);
      h = std::max(1u, h / 2);
    }
    total *= res->depth;
    if (total > 0x7FFFFFFFu) {
      return E_OUTOFMEMORY;
    }
    res->size_bytes = static_cast<uint32_t>(total);
  } else {
    return E_INVALIDARG;
  }

  try {
    res->storage.resize(res->size_bytes);
  } catch (...) {
    return E_OUTOFMEMORY;
  }

  if (wants_shared && !open_existing_shared) {
    if (!pCreateResource->pKmdAllocPrivateData ||
        pCreateResource->KmdAllocPrivateDataSize < sizeof(aerogpu_wddm_alloc_priv)) {
      logf("aerogpu-d3d9: Create shared resource missing private data buffer (have=%u need=%zu)\n",
           pCreateResource->KmdAllocPrivateDataSize,
           sizeof(aerogpu_wddm_alloc_priv));
      return kD3DErrInvalidCall;
    }

    // Generate a stable UMD-owned alloc_id + share_token and persist them in
    // allocation private data so they survive OpenResource in another process.
    uint32_t alloc_id = dev->adapter->next_alloc_id.fetch_add(1, std::memory_order_relaxed);
    alloc_id &= AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
    if (alloc_id == 0) {
      alloc_id = 1;
    }

#if defined(_WIN32)
    const uint64_t share_token =
        (static_cast<uint64_t>(GetCurrentProcessId()) << 32) | static_cast<uint64_t>(alloc_id);
#else
    const uint64_t share_token = static_cast<uint64_t>(alloc_id);
#endif

    aerogpu_wddm_alloc_priv priv{};
    priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
    priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
    priv.alloc_id = alloc_id;
    priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
    priv.share_token = share_token;
    priv.size_bytes = static_cast<aerogpu_wddm_u64>(res->size_bytes);
    priv.reserved0 = 0;
    std::memcpy(pCreateResource->pKmdAllocPrivateData, &priv, sizeof(priv));

    res->backing_alloc_id = alloc_id;
    res->share_token = share_token;
  }

  if (open_existing_shared) {
    if (!res->share_token) {
      logf("aerogpu-d3d9: Open shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      return E_FAIL;
    }
    // Shared surface open (D3D9Ex): the host already has the original resource,
    // so we only create a new alias handle and IMPORT it.
    emit_import_shared_surface_locked(dev, res.get());
  } else {
    emit_create_resource_locked(dev, res.get());

    if (wants_shared) {
      if (!res->share_token) {
        logf("aerogpu-d3d9: Create shared resource missing share_token (alloc_id=%u)\n", res->backing_alloc_id);
      } else {
        // Shared surface create (D3D9Ex): export exactly once so other guest
        // processes can IMPORT using the same stable share_token.
        emit_export_shared_surface_locked(dev, res.get());
      }
    }
  }

  pCreateResource->hResource = res.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_resource(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hResource) {
  auto* dev = as_device(hDevice);
  auto* res = as_resource(hResource);
  if (!dev || !res) {
    delete res;
    return S_OK;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  // NOTE: For now we emit DESTROY_RESOURCE for both original resources and
  // shared-surface aliases. The host command processor is expected to normalize
  // alias lifetimes, but proper cross-process refcounting may be needed later.
  emit_destroy_resource_locked(dev, res->handle);
  delete res;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_lock(
    AEROGPU_D3D9DDI_HDEVICE,
    const AEROGPU_D3D9DDIARG_LOCK* pLock,
    AEROGPU_D3D9DDI_LOCKED_BOX* pLockedBox) {
  if (!pLock || !pLockedBox) {
    return E_INVALIDARG;
  }
  auto* res = as_resource(pLock->hResource);
  if (!res) {
    return E_INVALIDARG;
  }
  if (res->locked) {
    return E_FAIL;
  }

  uint32_t offset = pLock->offset_bytes;
  uint32_t size = pLock->size_bytes ? pLock->size_bytes : (res->size_bytes - offset);
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return E_INVALIDARG;
  }

  res->locked = true;
  res->locked_offset = offset;
  res->locked_size = size;

  pLockedBox->pData = res->storage.data() + offset;
  pLockedBox->rowPitch = res->row_pitch;
  pLockedBox->slicePitch = res->slice_pitch;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_unlock(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_UNLOCK* pUnlock) {
  if (!hDevice || !pUnlock) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  auto* res = as_resource(pUnlock->hResource);
  if (!dev || !res) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (!res->locked) {
    return E_FAIL;
  }

  uint32_t offset = pUnlock->offset_bytes ? pUnlock->offset_bytes : res->locked_offset;
  uint32_t size = pUnlock->size_bytes ? pUnlock->size_bytes : res->locked_size;
  if (offset > res->size_bytes || size > res->size_bytes - offset) {
    return E_INVALIDARG;
  }

  res->locked = false;

  // For bring-up we inline resource updates directly into the command stream so
  // the host/emulator does not need to dereference guest allocations.
  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_upload_resource>(
      AEROGPU_CMD_UPLOAD_RESOURCE, res->storage.data() + offset, size);
  cmd->resource_handle = res->handle;
  cmd->reserved0 = 0;
  cmd->offset_bytes = offset;
  cmd->size_bytes = size;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_render_target(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t slot,
    AEROGPU_D3D9DDI_HRESOURCE hSurface) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  if (slot >= 4) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->render_targets[slot] == surf) {
    return S_OK;
  }
  dev->render_targets[slot] = surf;
  emit_set_render_targets_locked(dev);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_depth_stencil(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hSurface) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  auto* surf = as_resource(hSurface);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->depth_stencil == surf) {
    return S_OK;
  }
  dev->depth_stencil = surf;
  emit_set_render_targets_locked(dev);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_viewport(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDI_VIEWPORT* pViewport) {
  if (!hDevice || !pViewport) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->viewport = *pViewport;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(pViewport->x);
  cmd->y_f32 = f32_bits(pViewport->y);
  cmd->width_f32 = f32_bits(pViewport->w);
  cmd->height_f32 = f32_bits(pViewport->h);
  cmd->min_depth_f32 = f32_bits(pViewport->min_z);
  cmd->max_depth_f32 = f32_bits(pViewport->max_z);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_scissor(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const RECT* pRect,
    BOOL enabled) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (pRect) {
    dev->scissor_rect = *pRect;
  }
  dev->scissor_enabled = enabled;

  int32_t x = 0;
  int32_t y = 0;
  int32_t w = 0x7FFFFFFF;
  int32_t h = 0x7FFFFFFF;
  if (enabled && pRect) {
    x = static_cast<int32_t>(pRect->left);
    y = static_cast<int32_t>(pRect->top);
    w = static_cast<int32_t>(pRect->right - pRect->left);
    h = static_cast<int32_t>(pRect->bottom - pRect->top);
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  cmd->x = x;
  cmd->y = y;
  cmd->width = w;
  cmd->height = h;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_texture(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stage,
    AEROGPU_D3D9DDI_HRESOURCE hTexture) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  if (stage >= 16) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* tex = as_resource(hTexture);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->textures[stage] == tex) {
    return S_OK;
  }
  dev->textures[stage] = tex;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->texture = tex ? tex->handle : 0;
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_sampler_state(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  if (stage >= 16) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_sampler_state>(AEROGPU_CMD_SET_SAMPLER_STATE);
  cmd->shader_stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->slot = stage;
  cmd->state = state;
  cmd->value = value;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_render_state(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t state,
    uint32_t value) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_state>(AEROGPU_CMD_SET_RENDER_STATE);
  cmd->state = state;
  cmd->value = value;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_create_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const void* pDecl,
    uint32_t decl_size,
    AEROGPU_D3D9DDI_HVERTEXDECL* phDecl) {
  if (!hDevice || !pDecl || !phDecl || decl_size == 0) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto decl = std::make_unique<VertexDecl>();
  decl->handle = dev->adapter->next_handle.fetch_add(1);
  decl->blob.resize(decl_size);
  std::memcpy(decl->blob.data(), pDecl, decl_size);

  emit_create_input_layout_locked(dev, decl.get());

  *phDecl = decl.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->vertex_decl == decl) {
    return S_OK;
  }
  dev->vertex_decl = decl;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  cmd->input_layout_handle = decl ? decl->handle : 0;
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_vertex_decl(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HVERTEXDECL hDecl) {
  auto* dev = as_device(hDevice);
  auto* decl = as_vertex_decl(hDecl);
  if (!dev || !decl) {
    delete decl;
    return S_OK;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  emit_destroy_input_layout_locked(dev, decl->handle);
  delete decl;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_create_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    const void* pBytecode,
    uint32_t bytecode_size,
    AEROGPU_D3D9DDI_HSHADER* phShader) {
  if (!hDevice || !pBytecode || !phShader || bytecode_size == 0) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto sh = std::make_unique<Shader>();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  sh->stage = stage;
  sh->bytecode.resize(bytecode_size);
  std::memcpy(sh->bytecode.data(), pBytecode, bytecode_size);

  emit_create_shader_locked(dev, sh.get());

  *phShader = sh.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    AEROGPU_D3D9DDI_HSHADER hShader) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);

  std::lock_guard<std::mutex> lock(dev->mutex);

  Shader** slot = (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? &dev->vs : &dev->ps;
  if (*slot == sh) {
    return S_OK;
  }
  *slot = sh;

  emit_bind_shaders_locked(dev);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_shader(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HSHADER hShader) {
  auto* dev = as_device(hDevice);
  auto* sh = as_shader(hShader);
  if (!dev || !sh) {
    delete sh;
    return S_OK;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  emit_destroy_shader_locked(dev, sh->handle);
  delete sh;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_shader_const_f(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_SHADER_STAGE stage,
    uint32_t start_reg,
    const float* pData,
    uint32_t vec4_count) {
  if (!hDevice || !pData || vec4_count == 0) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  const size_t payload_size = static_cast<size_t>(vec4_count) * 4 * sizeof(float);
  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_shader_constants_f>(
      AEROGPU_CMD_SET_SHADER_CONSTANTS_F, pData, payload_size);
  cmd->stage = d3d9_stage_to_aerogpu_stage(stage);
  cmd->start_register = start_reg;
  cmd->vec4_count = vec4_count;
  cmd->reserved0 = 0;

  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_stream_source(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t stream,
    AEROGPU_D3D9DDI_HRESOURCE hVb,
    uint32_t offset_bytes,
    uint32_t stride_bytes) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  if (stream >= 16) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* vb = as_resource(hVb);

  std::lock_guard<std::mutex> lock(dev->mutex);

  DeviceStateStream& ss = dev->streams[stream];
  ss.vb = vb;
  ss.offset_bytes = offset_bytes;
  ss.stride_bytes = stride_bytes;

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = vb ? vb->handle : 0;
  binding.stride_bytes = stride_bytes;
  binding.offset_bytes = offset_bytes;
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, &binding, sizeof(binding));
  cmd->start_slot = stream;
  cmd->buffer_count = 1;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_indices(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_HRESOURCE hIb,
    AEROGPU_D3D9DDI_INDEX_FORMAT fmt,
    uint32_t offset_bytes) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  auto* ib = as_resource(hIb);

  std::lock_guard<std::mutex> lock(dev->mutex);

  dev->index_buffer = ib;
  dev->index_format = fmt;
  dev->index_offset_bytes = offset_bytes;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = ib ? ib->handle : 0;
  cmd->format = d3d9_index_format_to_aerogpu(fmt);
  cmd->offset_bytes = offset_bytes;
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_clear(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t flags,
    uint32_t color_rgba8,
    float depth,
    uint32_t stencil) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  const float a = static_cast<float>((color_rgba8 >> 24) & 0xFF) / 255.0f;
  const float r = static_cast<float>((color_rgba8 >> 16) & 0xFF) / 255.0f;
  const float g = static_cast<float>((color_rgba8 >> 8) & 0xFF) / 255.0f;
  const float b = static_cast<float>((color_rgba8 >> 0) & 0xFF) / 255.0f;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = flags;
  cmd->color_rgba_f32[0] = f32_bits(r);
  cmd->color_rgba_f32[1] = f32_bits(g);
  cmd->color_rgba_f32[2] = f32_bits(b);
  cmd->color_rgba_f32[3] = f32_bits(a);
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRIMITIVE_TYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t topology = d3d9_prim_to_topology(type);
  emit_set_topology_locked(dev, topology);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = vertex_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRIMITIVE_TYPE type,
    int32_t base_vertex,
    uint32_t /*min_index*/,
    uint32_t /*num_vertices*/,
    uint32_t start_index,
    uint32_t primitive_count) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  const uint32_t topology = d3d9_prim_to_topology(type);
  emit_set_topology_locked(dev, topology);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = index_count_from_primitive(type, primitive_count);
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_present_ex(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_PRESENTEX* pPresentEx) {
  if (!hDevice || !pPresentEx) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  HRESULT hr = throttle_presents_locked(dev, pPresentEx->d3d9_present_flags);
  if (hr != S_OK) {
    return hr;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present_ex>(AEROGPU_CMD_PRESENT_EX);
  cmd->scanout_id = 0;
  cmd->flags = (pPresentEx->sync_interval != 0) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
  cmd->d3d9_present_flags = pPresentEx->d3d9_present_flags;
  cmd->reserved0 = 0;

  const uint64_t submit_fence = submit(dev);
  const uint64_t present_fence = std::max<uint64_t>(submit_fence, refresh_fence_snapshot(dev->adapter).last_submitted);
  if (present_fence) {
    dev->inflight_present_fences.push_back(present_fence);
  }

  dev->present_count++;
  dev->last_present_qpc = qpc_now();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_present(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_PRESENT* pPresent) {
  if (!hDevice || !pPresent) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  HRESULT hr = throttle_presents_locked(dev, pPresent->flags);
  if (hr != S_OK) {
    return hr;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present_ex>(AEROGPU_CMD_PRESENT_EX);
  cmd->scanout_id = 0;
  cmd->flags = (pPresent->sync_interval != 0) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;
  cmd->d3d9_present_flags = pPresent->flags;
  cmd->reserved0 = 0;

  const uint64_t submit_fence = submit(dev);
  const uint64_t present_fence = std::max<uint64_t>(submit_fence, refresh_fence_snapshot(dev->adapter).last_submitted);
  if (present_fence) {
    dev->inflight_present_fences.push_back(present_fence);
  }

  dev->present_count++;
  dev->last_present_qpc = qpc_now();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_set_maximum_frame_latency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t max_frame_latency) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  if (max_frame_latency == 0) {
    return E_INVALIDARG;
  }
  dev->max_frame_latency = std::clamp(max_frame_latency, kMaxFrameLatencyMin, kMaxFrameLatencyMax);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_get_maximum_frame_latency(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t* pMaxFrameLatency) {
  if (!hDevice || !pMaxFrameLatency) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pMaxFrameLatency = dev->max_frame_latency;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_get_present_stats(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDI_PRESENTSTATS* pStats) {
  if (!hDevice || !pStats) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);

  std::memset(pStats, 0, sizeof(*pStats));
  pStats->PresentCount = dev->present_count;
  pStats->PresentRefreshCount = dev->present_count;
  pStats->SyncRefreshCount = dev->present_count;
  pStats->SyncQPCTime = static_cast<int64_t>(dev->last_present_qpc);
  pStats->SyncGPUTime = 0;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_get_last_present_count(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    uint32_t* pLastPresentCount) {
  if (!hDevice || !pLastPresentCount) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  *pLastPresentCount = dev->present_count;
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_flush(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  if (!hDevice) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  std::lock_guard<std::mutex> lock(dev->mutex);
  return flush_locked(dev);
}

HRESULT AEROGPU_D3D9_CALL device_create_query(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    AEROGPU_D3D9DDIARG_CREATEQUERY* pCreateQuery) {
  if (!hDevice || !pCreateQuery) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  Adapter* adapter = dev->adapter;
  bool is_event = false;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (!adapter->event_query_type_known) {
      // Accept both the public D3DQUERYTYPE_EVENT (8) encoding and the DDI-style
      // encoding where EVENT is the first enum entry (0). Once observed, lock
      // in the value so we don't accidentally treat other query types as EVENT.
      if (pCreateQuery->type == 0u || pCreateQuery->type == kD3DQueryTypeEvent) {
        adapter->event_query_type_known = true;
        adapter->event_query_type = pCreateQuery->type;
      }
    }
    is_event = adapter->event_query_type_known && (pCreateQuery->type == adapter->event_query_type);
  }

  if (!is_event) {
    return D3DERR_NOTAVAILABLE;
  }

  auto q = std::make_unique<Query>();
  q->type = pCreateQuery->type;
  pCreateQuery->hQuery = q.release();
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_destroy_query(
    AEROGPU_D3D9DDI_HDEVICE,
    AEROGPU_D3D9DDI_HQUERY hQuery) {
  delete as_query(hQuery);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_issue_query(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_ISSUEQUERY* pIssueQuery) {
  if (!hDevice || !pIssueQuery) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pIssueQuery->hQuery);
  if (!q) {
    return E_INVALIDARG;
  }
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  Adapter* adapter = dev->adapter;
  const bool is_event = adapter->event_query_type_known ? (q->type == adapter->event_query_type)
                                                        : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return D3DERR_NOTAVAILABLE;
  }

  // Event queries only care about END. BEGIN is ignored.
  if ((pIssueQuery->flags & kD3DIssueEnd) == 0) {
    return S_OK;
  }

  // Ensure all prior GPU work is submitted and capture the submission fence.
  uint64_t fence = submit(dev);
  if (fence != 0) {
    std::lock_guard<std::mutex> fence_lock(adapter->fence_mutex);
    adapter->last_submitted_fence = std::max(adapter->last_submitted_fence, fence);
  }

  // Preferred: use the fence returned by submit(). Fallback: query the KMD's
  // last_submitted_fence (useful when END happens with no pending cmd buffer).
  uint64_t fence_value = fence;
  if (fence_value == 0) {
    fence_value = refresh_fence_snapshot(adapter).last_submitted;
  }

  q->fence_value.store(fence_value, std::memory_order_release);
  q->issued.store(true, std::memory_order_release);
  return S_OK;
}

HRESULT AEROGPU_D3D9_CALL device_get_query_data(
    AEROGPU_D3D9DDI_HDEVICE hDevice,
    const AEROGPU_D3D9DDIARG_GETQUERYDATA* pGetQueryData) {
  if (!hDevice || !pGetQueryData) {
    return E_INVALIDARG;
  }
  auto* dev = as_device(hDevice);
  auto* q = as_query(pGetQueryData->hQuery);
  if (!q) {
    return E_INVALIDARG;
  }

  if (!dev || !dev->adapter) {
    return E_FAIL;
  }
  Adapter* adapter = dev->adapter;

  const bool is_event = adapter->event_query_type_known ? (q->type == adapter->event_query_type)
                                                        : (q->type == 0u || q->type == kD3DQueryTypeEvent);
  if (!is_event) {
    return D3DERR_NOTAVAILABLE;
  }
  if (!q->issued.load(std::memory_order_acquire)) {
    return S_FALSE;
  }

  // If no output buffer provided, just report readiness via HRESULT.
  const bool need_data = (pGetQueryData->pData != nullptr) && (pGetQueryData->data_size != 0);

  const uint64_t fence_value = q->fence_value.load(std::memory_order_acquire);

  FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/0);
  if (wait_res == FenceWaitResult::NotReady && (pGetQueryData->flags & kD3DGetDataFlush)) {
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      (void)submit(dev);
    }
    wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/16);
  }

  if (wait_res == FenceWaitResult::Complete) {
    if (need_data) {
      // D3DQUERYTYPE_EVENT expects a BOOL-like result.
      if (pGetQueryData->data_size < sizeof(uint32_t)) {
        return kD3DErrInvalidCall;
      }
      *reinterpret_cast<uint32_t*>(pGetQueryData->pData) = TRUE;
    }
    return S_OK;
  }
  if (wait_res == FenceWaitResult::Failed) {
    return E_FAIL;
  }
  return S_FALSE;
}

HRESULT AEROGPU_D3D9_CALL device_wait_for_idle(AEROGPU_D3D9DDI_HDEVICE hDevice) {
  if (!hDevice) {
    return E_INVALIDARG;
  }

  auto* dev = as_device(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  uint64_t fence_value = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    fence_value = submit(dev);
  }

  for (;;) {
    const FenceWaitResult wait_res = wait_for_fence(dev, fence_value, /*timeout_ms=*/250);
    if (wait_res == FenceWaitResult::Complete) {
      return S_OK;
    }
    if (wait_res == FenceWaitResult::Failed) {
      return E_FAIL;
    }
  }
}

HRESULT AEROGPU_D3D9_CALL adapter_create_device(
    AEROGPU_D3D9DDIARG_CREATEDEVICE* pCreateDevice,
    AEROGPU_D3D9DDI_DEVICEFUNCS* pDeviceFuncs) {
  if (!pCreateDevice || !pDeviceFuncs) {
    return E_INVALIDARG;
  }
  auto* adapter = as_adapter(pCreateDevice->hAdapter);
  if (!adapter) {
    return E_INVALIDARG;
  }

  auto dev = std::make_unique<Device>(adapter);

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI)
  if (!pCreateDevice->pCallbacks) {
    return E_INVALIDARG;
  }

  dev->wddm_callbacks = *reinterpret_cast<const WddmDeviceCallbacks*>(pCreateDevice->pCallbacks);

  HRESULT hr = wddm_create_device(dev->wddm_callbacks, pCreateDevice->hAdapter, &dev->wddm_device);
  if (FAILED(hr)) {
    return hr;
  }

  hr = wddm_create_context(dev->wddm_callbacks, dev->wddm_device, &dev->wddm_context);
  if (FAILED(hr)) {
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    return hr;
  }

  // Validate the runtime-provided submission buffers. These must be present for
  // DMA buffer construction; later milestones will build commands directly into
  // them.
  if (!dev->wddm_context.pCommandBuffer ||
      dev->wddm_context.CommandBufferSize < sizeof(aerogpu_cmd_stream_header) ||
      !dev->wddm_context.pAllocationList || dev->wddm_context.AllocationListSize == 0 ||
      !dev->wddm_context.pPatchLocationList || dev->wddm_context.PatchLocationListSize == 0 ||
      dev->wddm_context.hSyncObject == 0) {
    logf("aerogpu-d3d9: WDDM CreateContext returned invalid buffers "
         "cmd=%p size=%u alloc=%p size=%u patch=%p size=%u sync=%llu\n",
         dev->wddm_context.pCommandBuffer,
         dev->wddm_context.CommandBufferSize,
         dev->wddm_context.pAllocationList,
         dev->wddm_context.AllocationListSize,
         dev->wddm_context.pPatchLocationList,
         dev->wddm_context.PatchLocationListSize,
         static_cast<unsigned long long>(dev->wddm_context.hSyncObject));

    dev->wddm_context.destroy(dev->wddm_callbacks);
    wddm_destroy_device(dev->wddm_callbacks, dev->wddm_device);
    dev->wddm_device = 0;
    return E_FAIL;
  }

  logf("aerogpu-d3d9: WDDM ctx device=%llu context=%llu sync=%llu "
       "cmd=%p bytes=%u alloc_list=%p entries=%u patch_list=%p entries=%u\n",
       static_cast<unsigned long long>(dev->wddm_device),
       static_cast<unsigned long long>(dev->wddm_context.hContext),
       static_cast<unsigned long long>(dev->wddm_context.hSyncObject),
       dev->wddm_context.pCommandBuffer,
       dev->wddm_context.CommandBufferSize,
       dev->wddm_context.pAllocationList,
       dev->wddm_context.AllocationListSize,
       dev->wddm_context.pPatchLocationList,
       dev->wddm_context.PatchLocationListSize);
#endif

  pCreateDevice->hDevice = dev.get();

  std::memset(pDeviceFuncs, 0, sizeof(*pDeviceFuncs));
  pDeviceFuncs->pfnDestroyDevice = device_destroy;
  pDeviceFuncs->pfnCreateResource = device_create_resource;
  pDeviceFuncs->pfnDestroyResource = device_destroy_resource;
  pDeviceFuncs->pfnLock = device_lock;
  pDeviceFuncs->pfnUnlock = device_unlock;

  pDeviceFuncs->pfnSetRenderTarget = device_set_render_target;
  pDeviceFuncs->pfnSetDepthStencil = device_set_depth_stencil;
  pDeviceFuncs->pfnSetViewport = device_set_viewport;
  pDeviceFuncs->pfnSetScissorRect = device_set_scissor;
  pDeviceFuncs->pfnSetTexture = device_set_texture;
  pDeviceFuncs->pfnSetSamplerState = device_set_sampler_state;
  pDeviceFuncs->pfnSetRenderState = device_set_render_state;

  pDeviceFuncs->pfnCreateVertexDecl = device_create_vertex_decl;
  pDeviceFuncs->pfnSetVertexDecl = device_set_vertex_decl;
  pDeviceFuncs->pfnDestroyVertexDecl = device_destroy_vertex_decl;

  pDeviceFuncs->pfnCreateShader = device_create_shader;
  pDeviceFuncs->pfnSetShader = device_set_shader;
  pDeviceFuncs->pfnDestroyShader = device_destroy_shader;
  pDeviceFuncs->pfnSetShaderConstF = device_set_shader_const_f;

  pDeviceFuncs->pfnSetStreamSource = device_set_stream_source;
  pDeviceFuncs->pfnSetIndices = device_set_indices;

  pDeviceFuncs->pfnClear = device_clear;
  pDeviceFuncs->pfnDrawPrimitive = device_draw_primitive;
  pDeviceFuncs->pfnDrawIndexedPrimitive = device_draw_indexed_primitive;
  pDeviceFuncs->pfnPresent = device_present;
  pDeviceFuncs->pfnPresentEx = device_present_ex;
  pDeviceFuncs->pfnFlush = device_flush;
  pDeviceFuncs->pfnSetMaximumFrameLatency = device_set_maximum_frame_latency;
  pDeviceFuncs->pfnGetMaximumFrameLatency = device_get_maximum_frame_latency;
  pDeviceFuncs->pfnGetPresentStats = device_get_present_stats;
  pDeviceFuncs->pfnGetLastPresentCount = device_get_last_present_count;

  pDeviceFuncs->pfnCreateQuery = device_create_query;
  pDeviceFuncs->pfnDestroyQuery = device_destroy_query;
  pDeviceFuncs->pfnIssueQuery = device_issue_query;
  pDeviceFuncs->pfnGetQueryData = device_get_query_data;
  pDeviceFuncs->pfnWaitForIdle = device_wait_for_idle;

  dev.release();
  return S_OK;
}

} // namespace
} // namespace aerogpu

// -----------------------------------------------------------------------------
// Public entrypoints
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL OpenAdapter(
    AEROGPU_D3D9DDIARG_OPENADAPTER* pOpenAdapter,
    AEROGPU_D3D9DDI_ADAPTERFUNCS* pAdapterFuncs) {
  return OpenAdapter2(pOpenAdapter, pAdapterFuncs);
}

HRESULT AEROGPU_D3D9_CALL OpenAdapter2(
    AEROGPU_D3D9DDIARG_OPENADAPTER* pOpenAdapter,
    AEROGPU_D3D9DDI_ADAPTERFUNCS* pAdapterFuncs) {
  if (!pOpenAdapter || !pAdapterFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = new aerogpu::Adapter();
  pOpenAdapter->hAdapter = adapter;

  std::memset(pAdapterFuncs, 0, sizeof(*pAdapterFuncs));
  pAdapterFuncs->pfnCloseAdapter = aerogpu::adapter_close;
  pAdapterFuncs->pfnGetCaps = aerogpu::adapter_get_caps;
  pAdapterFuncs->pfnCreateDevice = aerogpu::adapter_create_device;

  aerogpu::logf("aerogpu-d3d9: OpenAdapter2 interface_version=%u\n", pOpenAdapter->interface_version);

#if defined(_WIN32)
  // Best-effort wiring for Win7 bring-up: initialize the KMD fence query helper
  // so we can observe real submission/completion fences via D3DKMTEscape.
  //
  // Failure is non-fatal; the UMD can fall back to conservative CPU-side
  // behavior when the query path is unavailable.
  if (pOpenAdapter->hDc && adapter->kmd_query.InitFromHdc(pOpenAdapter->hDc)) {
    uint64_t submitted = 0;
    uint64_t completed = 0;
    if (adapter->kmd_query.QueryFence(&submitted, &completed)) {
      aerogpu::logf("aerogpu-d3d9: KMD fence submitted=%llu completed=%llu\n",
                    static_cast<unsigned long long>(submitted),
                    static_cast<unsigned long long>(completed));
    }
  }
#endif

  return S_OK;
}
