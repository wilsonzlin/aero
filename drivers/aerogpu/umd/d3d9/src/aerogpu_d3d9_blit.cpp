#include "aerogpu_d3d9_blit.h"

#include <algorithm>
#include <cstdint>
#include <cstring>

#include "aerogpu_d3d9_builtin_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_submit.h"
#include "aerogpu_log.h"

namespace aerogpu {
namespace {

// D3D9 format subset (numeric values from d3d9types.h).
constexpr uint32_t kD3d9FmtA8R8G8B8 = 21u;
constexpr uint32_t kD3d9FmtX8R8G8B8 = 22u;
constexpr uint32_t kD3d9FmtA8B8G8R8 = 32u;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatR32G32B32A32Float = 2;
constexpr uint32_t kDxgiFormatR32G32Float = 16;

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
constexpr uint32_t kD3d9BlendOpAdd = 1;
constexpr uint32_t kD3d9CullNone = 1;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

uint32_t hash_semantic_name(const char* s) {
  // FNV-1a 32-bit hash (matches D3D10/11 UMD helper).
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
  if (!dev) {
    return false;
  }

  if (dev->cmd.bytes_remaining() >= bytes_needed) {
    return true;
  }

  // Flush the current submission and retry. This allows the blit helper to run
  // against span-backed DMA buffers with bounded capacity.
  if (!dev->cmd.empty()) {
    (void)submit_locked(dev);
  }

  return dev->cmd.bytes_remaining() >= bytes_needed;
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
                                 aerogpu_handle_t resource_handle,
                                 uint64_t offset_bytes,
                                 const uint8_t* data,
                                 size_t size_bytes) {
  if (!dev || !resource_handle || !data) {
    return false;
  }

  size_t remaining = size_bytes;
  uint64_t cur_offset = offset_bytes;
  const uint8_t* src = data;

  while (remaining) {
    // Ensure we can at least fit a minimal upload packet (header + 1 byte).
    const size_t min_needed = align_up(sizeof(aerogpu_cmd_upload_resource) + 1, 4);
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
    if (!chunk) {
      // Extremely small DMA buffer: force a submit and retry.
      (void)submit_locked(dev);
      continue;
    }

    auto* cmd = append_with_payload_locked<aerogpu_cmd_upload_resource>(dev, AEROGPU_CMD_UPLOAD_RESOURCE, src, chunk);
    if (!cmd) {
      return false;
    }

    cmd->resource_handle = resource_handle;
    cmd->reserved0 = 0;
    cmd->offset_bytes = cur_offset;
    cmd->size_bytes = chunk;

    src += chunk;
    cur_offset += chunk;
    remaining -= chunk;
  }

  return true;
}

bool emit_set_render_targets_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_render_targets>(dev, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }
  cmd->color_count = 4;
  cmd->depth_stencil = dev->depth_stencil ? dev->depth_stencil->handle : 0;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    cmd->colors[i] = 0;
  }
  for (uint32_t i = 0; i < 4; ++i) {
    cmd->colors[i] = dev->render_targets[i] ? dev->render_targets[i]->handle : 0;
  }
  return true;
}

bool emit_bind_shaders_locked(Device* dev) {
  auto* cmd = append_fixed_locked<aerogpu_cmd_bind_shaders>(dev, AEROGPU_CMD_BIND_SHADERS);
  if (!cmd) {
    return false;
  }
  cmd->vs = dev->vs ? dev->vs->handle : 0;
  cmd->ps = dev->ps ? dev->ps->handle : 0;
  cmd->cs = 0;
  cmd->reserved0 = 0;
  return true;
}

bool emit_set_viewport_locked(Device* dev) {
  const auto& vp = dev->viewport;
  auto* cmd = append_fixed_locked<aerogpu_cmd_set_viewport>(dev, AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    return false;
  }
  cmd->x_f32 = f32_bits(vp.x);
  cmd->y_f32 = f32_bits(vp.y);
  cmd->width_f32 = f32_bits(vp.w);
  cmd->height_f32 = f32_bits(vp.h);
  cmd->min_depth_f32 = f32_bits(vp.min_z);
  cmd->max_depth_f32 = f32_bits(vp.max_z);
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
                               AEROGPU_D3D9DDI_SHADER_STAGE stage,
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
  cmd->stage = (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? AEROGPU_SHADER_STAGE_VERTEX : AEROGPU_SHADER_STAGE_PIXEL;
  cmd->start_register = start_reg;
  cmd->vec4_count = vec4_count;
  cmd->reserved0 = 0;

  float* dst = (stage == AEROGPU_D3D9DDI_SHADER_STAGE_VS) ? dev->vs_consts_f : dev->ps_consts_f;
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
    auto* sh = new Shader();
    sh->handle = dev->adapter->next_handle.fetch_add(1);
    sh->stage = AEROGPU_D3D9DDI_SHADER_STAGE_VS;
    sh->bytecode.assign(builtin_d3d9_shaders::kCopyVsDxbc,
                        builtin_d3d9_shaders::kCopyVsDxbc + builtin_d3d9_shaders::kCopyVsDxbcSize);

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
    auto* sh = new Shader();
    sh->handle = dev->adapter->next_handle.fetch_add(1);
    sh->stage = AEROGPU_D3D9DDI_SHADER_STAGE_PS;
    sh->bytecode.assign(builtin_d3d9_shaders::kCopyPsDxbc,
                        builtin_d3d9_shaders::kCopyPsDxbc + builtin_d3d9_shaders::kCopyPsDxbcSize);

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
    auto* decl = new VertexDecl();
    decl->handle = dev->adapter->next_handle.fetch_add(1);

    const size_t blob_size = sizeof(aerogpu_input_layout_blob_header) + 2 * sizeof(aerogpu_input_layout_element_dxgi);
    decl->blob.resize(blob_size);

    auto* hdr = reinterpret_cast<aerogpu_input_layout_blob_header*>(decl->blob.data());
    hdr->magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
    hdr->version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
    hdr->element_count = 2;
    hdr->reserved0 = 0;

    auto* elems = reinterpret_cast<aerogpu_input_layout_element_dxgi*>(decl->blob.data() + sizeof(*hdr));
    elems[0].semantic_name_hash = hash_semantic_name("POSITION");
    elems[0].semantic_index = 0;
    elems[0].dxgi_format = kDxgiFormatR32G32B32A32Float;
    elems[0].input_slot = 0;
    elems[0].aligned_byte_offset = 0;
    elems[0].input_slot_class = 0;
    elems[0].instance_data_step_rate = 0;

    elems[1].semantic_name_hash = hash_semantic_name("TEXCOORD");
    elems[1].semantic_index = 0;
    elems[1].dxgi_format = kDxgiFormatR32G32Float;
    elems[1].input_slot = 0;
    elems[1].aligned_byte_offset = 16;
    elems[1].input_slot_class = 0;
    elems[1].instance_data_step_rate = 0;

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
    auto* vb = new Resource();
    vb->handle = dev->adapter->next_handle.fetch_add(1);
    vb->kind = ResourceKind::Buffer;
    vb->size_bytes = sizeof(BlitVertex) * 4;
    vb->storage.resize(vb->size_bytes);

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

HRESULT blit_locked(Device* dev,
                    Resource* dst,
                    const RECT* dst_rect_in,
                    Resource* src,
                    const RECT* src_rect_in,
                    uint32_t filter) {
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
  const AEROGPU_D3D9DDI_VIEWPORT saved_vp = dev->viewport;
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

  // Configure a conservative copy state.
  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, TRUE) ||
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
  if (!set_shader_const_f_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_VS, 0, ident, 4)) {
    return E_OUTOFMEMORY;
  }

  // Pixel shader multiplier: 1.0 (pass through sampled texel).
  const float one[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  if (!set_shader_const_f_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_PS, 0, one, 1)) {
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
                                    dev->builtin_copy_vb->handle,
                                    /*offset_bytes=*/0,
                                    reinterpret_cast<const uint8_t*>(verts),
                                    sizeof(verts))) {
    return E_OUTOFMEMORY;
  }

  // Draw.
  auto* draw = append_fixed_locked<aerogpu_cmd_draw>(dev, AEROGPU_CMD_DRAW);
  if (!draw) {
    return E_OUTOFMEMORY;
  }
  draw->vertex_count = 4;
  draw->instance_count = 1;
  draw->first_vertex = 0;
  draw->first_instance = 0;

  // Restore state.
  dev->streams[0] = saved_stream0;
  if (!emit_set_vertex_buffer_locked(dev, 0)) {
    return E_OUTOFMEMORY;
  }

  dev->vertex_decl = saved_decl;
  if (!emit_set_input_layout_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->textures[0] = saved_tex0;
  if (!emit_set_texture_locked(dev, 0)) {
    return E_OUTOFMEMORY;
  }

  dev->vs = saved_vs;
  dev->ps = saved_ps;
  if (!emit_bind_shaders_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->render_targets[0] = saved_rts[0];
  dev->render_targets[1] = saved_rts[1];
  dev->render_targets[2] = saved_rts[2];
  dev->render_targets[3] = saved_rts[3];
  dev->depth_stencil = saved_ds;
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->viewport = saved_vp;
  if (!emit_set_viewport_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->scissor_rect = saved_scissor;
  dev->scissor_enabled = saved_scissor_enabled;
  if (!emit_set_scissor_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  if (!emit_set_topology_locked(dev, saved_topology)) {
    return E_OUTOFMEMORY;
  }

  // Restore constants.
  if (!set_shader_const_f_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_VS, 0, saved_vs_c0_3, 4) ||
      !set_shader_const_f_locked(dev, AEROGPU_D3D9DDI_SHADER_STAGE_PS, 0, saved_ps_c0, 1)) {
    return E_OUTOFMEMORY;
  }

  // Restore sampler states.
  if (!set_sampler_state_locked(dev, 0, kD3d9SampAddressU, saved_samp_u) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampAddressV, saved_samp_v) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMinFilter, saved_samp_min) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMagFilter, saved_samp_mag) ||
      !set_sampler_state_locked(dev, 0, kD3d9SampMipFilter, saved_samp_mip)) {
    return E_OUTOFMEMORY;
  }

  // Restore render states.
  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, saved_rs_scissor) ||
      !set_render_state_locked(dev, kD3d9RsAlphaBlendEnable, saved_rs_alpha_blend) ||
      !set_render_state_locked(dev, kD3d9RsSeparateAlphaBlendEnable, saved_rs_sep_alpha_blend) ||
      !set_render_state_locked(dev, kD3d9RsSrcBlend, saved_rs_src_blend) ||
      !set_render_state_locked(dev, kD3d9RsDestBlend, saved_rs_dst_blend) ||
      !set_render_state_locked(dev, kD3d9RsBlendOp, saved_rs_blend_op) ||
      !set_render_state_locked(dev, kD3d9RsColorWriteEnable, saved_rs_color_write) ||
      !set_render_state_locked(dev, kD3d9RsZEnable, saved_rs_z_enable) ||
      !set_render_state_locked(dev, kD3d9RsZWriteEnable, saved_rs_z_write) ||
      !set_render_state_locked(dev, kD3d9RsCullMode, saved_rs_cull)) {
    return E_OUTOFMEMORY;
  }

  return S_OK;
}

HRESULT color_fill_locked(Device* dev, Resource* dst, const RECT* dst_rect_in, uint32_t color_argb) {
  if (!dev || !dst) {
    return E_INVALIDARG;
  }

  RECT dst_rect{};
  if (!clamp_rect(dst_rect_in, dst->width, dst->height, &dst_rect)) {
    return S_OK;
  }

  // Save state.
  Resource* saved_rts[4] = {dev->render_targets[0], dev->render_targets[1], dev->render_targets[2], dev->render_targets[3]};
  Resource* saved_ds = dev->depth_stencil;
  const AEROGPU_D3D9DDI_VIEWPORT saved_vp = dev->viewport;
  const RECT saved_scissor = dev->scissor_rect;
  const BOOL saved_scissor_enabled = dev->scissor_enabled;
  const uint32_t saved_rs_scissor = dev->render_states[kD3d9RsScissorTestEnable];

  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, TRUE)) {
    return E_OUTOFMEMORY;
  }

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

  dev->scissor_rect = dst_rect;
  dev->scissor_enabled = TRUE;
  if (!emit_set_scissor_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  const float a = static_cast<float>((color_argb >> 24) & 0xFF) / 255.0f;
  const float r = static_cast<float>((color_argb >> 16) & 0xFF) / 255.0f;
  const float g = static_cast<float>((color_argb >> 8) & 0xFF) / 255.0f;
  const float b = static_cast<float>((color_argb >> 0) & 0xFF) / 255.0f;

  auto* cmd = append_fixed_locked<aerogpu_cmd_clear>(dev, AEROGPU_CMD_CLEAR);
  if (!cmd) {
    return E_OUTOFMEMORY;
  }
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(r);
  cmd->color_rgba_f32[1] = f32_bits(g);
  cmd->color_rgba_f32[2] = f32_bits(b);
  cmd->color_rgba_f32[3] = f32_bits(a);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;

  // Restore state.
  dev->render_targets[0] = saved_rts[0];
  dev->render_targets[1] = saved_rts[1];
  dev->render_targets[2] = saved_rts[2];
  dev->render_targets[3] = saved_rts[3];
  dev->depth_stencil = saved_ds;
  if (!emit_set_render_targets_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->viewport = saved_vp;
  if (!emit_set_viewport_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  dev->scissor_rect = saved_scissor;
  dev->scissor_enabled = saved_scissor_enabled;
  if (!emit_set_scissor_locked(dev)) {
    return E_OUTOFMEMORY;
  }

  if (!set_render_state_locked(dev, kD3d9RsScissorTestEnable, saved_rs_scissor)) {
    return E_OUTOFMEMORY;
  }

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

  for (uint32_t y = 0; y < copy_h; ++y) {
    const uint32_t src_off = (static_cast<uint32_t>(src_top) + y) * src->row_pitch +
                             static_cast<uint32_t>(src_left) * bpp;
    const uint32_t dst_off = (static_cast<uint32_t>(dst_top) + y) * dst->row_pitch +
                             static_cast<uint32_t>(dst_left) * bpp;
    if (src_off + row_bytes > src->storage.size() || dst_off + row_bytes > dst->storage.size()) {
      return E_INVALIDARG;
    }

    uint8_t* dst_row = dst->storage.data() + dst_off;
    const uint8_t* src_row = src->storage.data() + src_off;
    if (can_fast_copy) {
      std::memcpy(dst_row, src_row, row_bytes);
    } else {
      // 4-byte format conversion (ARGB/XRGB/ABGR).
      for (uint32_t x = 0; x < copy_w; ++x) {
        const uint8_t* s = src_row + static_cast<size_t>(x) * 4;
        uint8_t* d = dst_row + static_cast<size_t>(x) * 4;
        if (!convert_pixel_4bpp(src->format, dst->format, s, d)) {
          return D3DERR_INVALIDCALL;
        }
      }
    }

    if (!upload_resource_bytes_locked(dev,
                                      dst->handle,
                                      /*offset_bytes=*/dst_off,
                                      dst->storage.data() + dst_off,
                                      row_bytes)) {
      return E_OUTOFMEMORY;
    }
  }

  return S_OK;
}

HRESULT update_texture_locked(Device* dev, Resource* src, Resource* dst) {
  if (!dev || !src || !dst) {
    return E_INVALIDARG;
  }
  if (src->width != dst->width || src->height != dst->height ||
      src->mip_levels != dst->mip_levels || src->size_bytes != dst->size_bytes) {
    return D3DERR_INVALIDCALL;
  }

  if (src->format != dst->format) {
    // Only support conversions between a small subset of 32bpp formats.
    const bool can_convert =
        (bytes_per_pixel(src->format) == 4u) && (bytes_per_pixel(dst->format) == 4u) &&
        ((src->format == kD3d9FmtA8R8G8B8 || src->format == kD3d9FmtX8R8G8B8 || src->format == kD3d9FmtA8B8G8R8) &&
         (dst->format == kD3d9FmtA8R8G8B8 || dst->format == kD3d9FmtX8R8G8B8 || dst->format == kD3d9FmtA8B8G8R8));
    if (!can_convert) {
      return D3DERR_INVALIDCALL;
    }

    dst->storage.resize(src->storage.size());
    for (size_t i = 0; i + 3 < src->storage.size(); i += 4) {
      if (!convert_pixel_4bpp(src->format, dst->format, &src->storage[i], &dst->storage[i])) {
        return D3DERR_INVALIDCALL;
      }
    }
  } else {
    dst->storage = src->storage;
  }
  if (dst->handle == 0) {
    return E_INVALIDARG;
  }

  if (!upload_resource_bytes_locked(dev,
                                    dst->handle,
                                    /*offset_bytes=*/0,
                                    dst->storage.data(),
                                    dst->storage.size())) {
    return E_OUTOFMEMORY;
  }
  return S_OK;
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
