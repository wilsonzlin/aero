#include <stddef.h>
#include <stdio.h>
#include <string.h>

#include "drivers/aerogpu/protocol/aerogpu_alloc.h"
#include "drivers/aerogpu/protocol/aerogpu_cmd.h"
#include "drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h"
#include "drivers/aerogpu/protocol/aerogpu_escape.h"
#include "drivers/aerogpu/protocol/aerogpu_ring.h"
#include "drivers/aerogpu/protocol/aerogpu_umd_private.h"
#include "drivers/aerogpu/protocol/aerogpu_wddm_alloc.h"

/* When multiple branches extend this file in parallel, merges can accidentally introduce duplicate
 * PRINT_* lines. This helper keeps the dump stable by deduping identical keys at runtime, while
 * still erroring if a duplicate key maps to a different value. */

#define AEROGPU_ABI_DUMP_MAX_SIZES 512
#define AEROGPU_ABI_DUMP_MAX_OFFS 4096
#define AEROGPU_ABI_DUMP_MAX_CONSTS 4096

typedef struct {
  const char* name;
  size_t size;
} aerogpu_abi_dump_size_entry;

typedef struct {
  const char* ty;
  const char* field;
  size_t off;
} aerogpu_abi_dump_off_entry;

typedef struct {
  const char* name;
  unsigned long long value;
} aerogpu_abi_dump_const_entry;

static aerogpu_abi_dump_size_entry g_sizes[AEROGPU_ABI_DUMP_MAX_SIZES];
static size_t g_sizes_count = 0;

static aerogpu_abi_dump_off_entry g_offs[AEROGPU_ABI_DUMP_MAX_OFFS];
static size_t g_offs_count = 0;

static aerogpu_abi_dump_const_entry g_consts[AEROGPU_ABI_DUMP_MAX_CONSTS];
static size_t g_consts_count = 0;

static int print_size_once(const char* name, size_t size) {
  for (size_t i = 0; i < g_sizes_count; i++) {
    if (strcmp(g_sizes[i].name, name) == 0) {
      if (g_sizes[i].size != size) {
        fprintf(stderr, "ABI dump duplicate SIZE mismatch for %s: %zu vs %zu\n", name, g_sizes[i].size, size);
        return 1;
      }
      return 0;
    }
  }

  if (g_sizes_count >= AEROGPU_ABI_DUMP_MAX_SIZES) {
    fprintf(stderr, "ABI dump exceeded max SIZE entries (%d)\n", AEROGPU_ABI_DUMP_MAX_SIZES);
    return 1;
  }
  g_sizes[g_sizes_count++] = (aerogpu_abi_dump_size_entry){ name, size };
  printf("SIZE %s %zu\n", name, size);
  return 0;
}

static int print_off_once(const char* ty, const char* field, size_t off) {
  for (size_t i = 0; i < g_offs_count; i++) {
    if (strcmp(g_offs[i].ty, ty) == 0 && strcmp(g_offs[i].field, field) == 0) {
      if (g_offs[i].off != off) {
        fprintf(stderr,
                "ABI dump duplicate OFF mismatch for %s.%s: %zu vs %zu\n",
                ty,
                field,
                g_offs[i].off,
                off);
        return 1;
      }
      return 0;
    }
  }

  if (g_offs_count >= AEROGPU_ABI_DUMP_MAX_OFFS) {
    fprintf(stderr, "ABI dump exceeded max OFF entries (%d)\n", AEROGPU_ABI_DUMP_MAX_OFFS);
    return 1;
  }
  g_offs[g_offs_count++] = (aerogpu_abi_dump_off_entry){ ty, field, off };
  printf("OFF %s %s %zu\n", ty, field, off);
  return 0;
}

static int print_const_once(const char* name, unsigned long long value) {
  for (size_t i = 0; i < g_consts_count; i++) {
    if (strcmp(g_consts[i].name, name) == 0) {
      if (g_consts[i].value != value) {
        fprintf(
            stderr, "ABI dump duplicate CONST mismatch for %s: %llu vs %llu\n", name, g_consts[i].value, value);
        return 1;
      }
      return 0;
    }
  }

  if (g_consts_count >= AEROGPU_ABI_DUMP_MAX_CONSTS) {
    fprintf(stderr, "ABI dump exceeded max CONST entries (%d)\n", AEROGPU_ABI_DUMP_MAX_CONSTS);
    return 1;
  }
  g_consts[g_consts_count++] = (aerogpu_abi_dump_const_entry){ name, value };
  printf("CONST %s %llu\n", name, value);
  return 0;
}

#define PRINT_SIZE(name, type) \
  do {                         \
    if (print_size_once(name, sizeof(type))) return 1; \
  } while (0)
#define PRINT_OFF(name, type, field) \
  do {                               \
    if (print_off_once(name, #field, offsetof(type, field))) return 1; \
  } while (0)
#define PRINT_CONST(name) \
  do {                    \
    if (print_const_once(#name, (unsigned long long)(name))) return 1; \
  } while (0)

int main(void) {
  /* ------------------------------- Struct sizes -------------------------- */
  PRINT_SIZE("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header);
  PRINT_SIZE("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr);

  PRINT_SIZE("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer);
  PRINT_SIZE("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d);
  PRINT_SIZE("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view);
  PRINT_SIZE("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource);
  PRINT_SIZE("aerogpu_cmd_destroy_texture_view", struct aerogpu_cmd_destroy_texture_view);
  PRINT_SIZE("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range);
  PRINT_SIZE("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource);
  PRINT_SIZE("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer);
  PRINT_SIZE("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d);
  PRINT_SIZE("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc);
  PRINT_SIZE("aerogpu_cmd_destroy_shader", struct aerogpu_cmd_destroy_shader);
  PRINT_SIZE("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders);
  PRINT_SIZE("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f);
  PRINT_SIZE("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i);
  PRINT_SIZE("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b);
  PRINT_SIZE("aerogpu_input_layout_blob_header", struct aerogpu_input_layout_blob_header);
  PRINT_SIZE("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi);
  PRINT_SIZE("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout);
  PRINT_SIZE("aerogpu_cmd_destroy_input_layout", struct aerogpu_cmd_destroy_input_layout);
  PRINT_SIZE("aerogpu_cmd_set_input_layout", struct aerogpu_cmd_set_input_layout);
  PRINT_SIZE("aerogpu_blend_state", struct aerogpu_blend_state);
  PRINT_SIZE("aerogpu_cmd_set_blend_state", struct aerogpu_cmd_set_blend_state);
  PRINT_SIZE("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state);
  PRINT_SIZE("aerogpu_cmd_set_depth_stencil_state", struct aerogpu_cmd_set_depth_stencil_state);
  PRINT_SIZE("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state);
  PRINT_SIZE("aerogpu_cmd_set_rasterizer_state", struct aerogpu_cmd_set_rasterizer_state);
  PRINT_SIZE("aerogpu_cmd_set_render_targets", struct aerogpu_cmd_set_render_targets);
  PRINT_SIZE("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport);
  PRINT_SIZE("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor);
  PRINT_SIZE("aerogpu_vertex_buffer_binding", struct aerogpu_vertex_buffer_binding);
  PRINT_SIZE("aerogpu_cmd_set_vertex_buffers", struct aerogpu_cmd_set_vertex_buffers);
  PRINT_SIZE("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer);
  PRINT_SIZE("aerogpu_cmd_set_primitive_topology", struct aerogpu_cmd_set_primitive_topology);
  PRINT_SIZE("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture);
  PRINT_SIZE("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state);
  PRINT_SIZE("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state);
  PRINT_SIZE("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler);
  PRINT_SIZE("aerogpu_cmd_destroy_sampler", struct aerogpu_cmd_destroy_sampler);
  PRINT_SIZE("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers);
  PRINT_SIZE("aerogpu_constant_buffer_binding", struct aerogpu_constant_buffer_binding);
  PRINT_SIZE("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers);
  PRINT_SIZE("aerogpu_shader_resource_buffer_binding", struct aerogpu_shader_resource_buffer_binding);
  PRINT_SIZE("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers);
  PRINT_SIZE("aerogpu_unordered_access_buffer_binding", struct aerogpu_unordered_access_buffer_binding);
  PRINT_SIZE("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers);
  PRINT_SIZE("aerogpu_cmd_clear", struct aerogpu_cmd_clear);
  PRINT_SIZE("aerogpu_cmd_draw", struct aerogpu_cmd_draw);
  PRINT_SIZE("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed);
  PRINT_SIZE("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch);
  PRINT_SIZE("aerogpu_cmd_present", struct aerogpu_cmd_present);
  PRINT_SIZE("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex);
  PRINT_SIZE("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface);
  PRINT_SIZE("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface);
  PRINT_SIZE("aerogpu_cmd_release_shared_surface", struct aerogpu_cmd_release_shared_surface);
  PRINT_SIZE("aerogpu_cmd_flush", struct aerogpu_cmd_flush);

  PRINT_SIZE("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header);
  PRINT_SIZE("aerogpu_alloc_entry", struct aerogpu_alloc_entry);
  PRINT_SIZE("aerogpu_submit_desc", struct aerogpu_submit_desc);
  PRINT_SIZE("aerogpu_ring_header", struct aerogpu_ring_header);
  PRINT_SIZE("aerogpu_fence_page", struct aerogpu_fence_page);
 
  PRINT_SIZE("aerogpu_umd_private_v1", aerogpu_umd_private_v1);
  PRINT_SIZE("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv);
  PRINT_SIZE("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2);
 
  PRINT_SIZE("aerogpu_escape_header", aerogpu_escape_header);
  PRINT_SIZE("aerogpu_escape_query_device_out", aerogpu_escape_query_device_out);
  PRINT_SIZE("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out);
  PRINT_SIZE("aerogpu_escape_query_fence_out", aerogpu_escape_query_fence_out);
  PRINT_SIZE("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out);
  PRINT_SIZE("aerogpu_dbgctl_ring_desc", aerogpu_dbgctl_ring_desc);
  PRINT_SIZE("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2);
  PRINT_SIZE("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout);
  PRINT_SIZE("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout);
  PRINT_SIZE("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout);
  PRINT_SIZE("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out);
  PRINT_SIZE("aerogpu_escape_dump_vblank_inout", aerogpu_escape_dump_vblank_inout);
  PRINT_SIZE("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out);
  PRINT_SIZE("aerogpu_escape_query_scanout_out_v2", aerogpu_escape_query_scanout_out_v2);
  PRINT_SIZE("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out);
  PRINT_SIZE("aerogpu_escape_set_cursor_position_in", aerogpu_escape_set_cursor_position_in);
  PRINT_SIZE("aerogpu_escape_set_cursor_visibility_in", aerogpu_escape_set_cursor_visibility_in);
  PRINT_SIZE("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in);
  PRINT_SIZE("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out);
  PRINT_SIZE("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout);
  PRINT_SIZE("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout);
  PRINT_SIZE("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc);
  PRINT_SIZE("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout);

  /* -------------------------------- Offsets ------------------------------ */
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, magic);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, abi_version);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, size_bytes);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, flags);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, reserved0);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, reserved1);

  PRINT_OFF("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr, opcode);
  PRINT_OFF("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr, size_bytes);

  PRINT_OFF("aerogpu_input_layout_blob_header", struct aerogpu_input_layout_blob_header, magic);
  PRINT_OFF("aerogpu_input_layout_blob_header", struct aerogpu_input_layout_blob_header, version);
  PRINT_OFF("aerogpu_input_layout_blob_header", struct aerogpu_input_layout_blob_header, element_count);
  PRINT_OFF("aerogpu_input_layout_blob_header", struct aerogpu_input_layout_blob_header, reserved0);

  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, semantic_name_hash);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, semantic_index);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, dxgi_format);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, input_slot);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, aligned_byte_offset);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, input_slot_class);
  PRINT_OFF("aerogpu_input_layout_element_dxgi", struct aerogpu_input_layout_element_dxgi, instance_data_step_rate);

  /* Fixed-layout packet field offsets (helps catch accidental field reordering). */
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, hdr);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, buffer_handle);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, usage_flags);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, size_bytes);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, backing_alloc_id);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, backing_offset_bytes);
  PRINT_OFF("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer, reserved0);

  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, hdr);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, texture_handle);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, usage_flags);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, format);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, width);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, height);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, mip_levels);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, array_layers);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, row_pitch_bytes);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, backing_alloc_id);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, backing_offset_bytes);
  PRINT_OFF("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d, reserved0);

  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, hdr);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, view_handle);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, texture_handle);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, format);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, base_mip_level);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, mip_level_count);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, base_array_layer);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, array_layer_count);
  PRINT_OFF("aerogpu_cmd_create_texture_view", struct aerogpu_cmd_create_texture_view, reserved0);

  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, resource_handle);
  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, reserved0);

  PRINT_OFF("aerogpu_cmd_destroy_texture_view", struct aerogpu_cmd_destroy_texture_view, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_texture_view", struct aerogpu_cmd_destroy_texture_view, view_handle);
  PRINT_OFF("aerogpu_cmd_destroy_texture_view", struct aerogpu_cmd_destroy_texture_view, reserved0);

  PRINT_OFF("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range, hdr);
  PRINT_OFF("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range, resource_handle);
  PRINT_OFF("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range, reserved0);
  PRINT_OFF("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range, offset_bytes);
  PRINT_OFF("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range, size_bytes);

  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, hdr);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, resource_handle);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, reserved0);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, offset_bytes);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, size_bytes);

  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, hdr);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, dst_buffer);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, src_buffer);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, dst_offset_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, src_offset_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, size_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, flags);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, reserved0);

  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, hdr);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, dst_texture);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, src_texture);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, dst_mip_level);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, dst_array_layer);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, src_mip_level);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, src_array_layer);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, dst_x);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, dst_y);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, src_x);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, src_y);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, width);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, height);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, flags);
  PRINT_OFF("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d, reserved0);

  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, hdr);
  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, shader_handle);
  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, stage);
  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, dxbc_size_bytes);
  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, reserved0);

  PRINT_OFF("aerogpu_cmd_destroy_shader", struct aerogpu_cmd_destroy_shader, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_shader", struct aerogpu_cmd_destroy_shader, shader_handle);
  PRINT_OFF("aerogpu_cmd_destroy_shader", struct aerogpu_cmd_destroy_shader, reserved0);

  PRINT_OFF("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders, hdr);
  PRINT_OFF("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders, vs);
  PRINT_OFF("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders, ps);
  PRINT_OFF("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders, cs);
  PRINT_OFF("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders, reserved0);

  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, stage);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, start_register);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, vec4_count);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, reserved0);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, stage);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, start_register);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, vec4_count);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, reserved0);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, stage);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, start_register);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, bool_count);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, reserved0);

  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, stage);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, start_register);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, vec4_count);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_i", struct aerogpu_cmd_set_shader_constants_i, reserved0);

  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, stage);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, start_register);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, bool_count);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_b", struct aerogpu_cmd_set_shader_constants_b, reserved0);

  PRINT_OFF("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout, hdr);
  PRINT_OFF("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout, input_layout_handle);
  PRINT_OFF("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout, blob_size_bytes);
  PRINT_OFF("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout, reserved0);

  PRINT_OFF("aerogpu_cmd_destroy_input_layout", struct aerogpu_cmd_destroy_input_layout, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_input_layout", struct aerogpu_cmd_destroy_input_layout, input_layout_handle);
  PRINT_OFF("aerogpu_cmd_destroy_input_layout", struct aerogpu_cmd_destroy_input_layout, reserved0);

  PRINT_OFF("aerogpu_cmd_set_input_layout", struct aerogpu_cmd_set_input_layout, hdr);
  PRINT_OFF("aerogpu_cmd_set_input_layout", struct aerogpu_cmd_set_input_layout, input_layout_handle);
  PRINT_OFF("aerogpu_cmd_set_input_layout", struct aerogpu_cmd_set_input_layout, reserved0);

  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, enable);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, src_factor);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, dst_factor);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, blend_op);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, color_write_mask);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, reserved0);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, src_factor_alpha);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, dst_factor_alpha);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, blend_op_alpha);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, blend_constant_rgba_f32);
  PRINT_OFF("aerogpu_blend_state", struct aerogpu_blend_state, sample_mask);

  PRINT_OFF("aerogpu_cmd_set_blend_state", struct aerogpu_cmd_set_blend_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_blend_state", struct aerogpu_cmd_set_blend_state, state);

  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, depth_enable);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, depth_write_enable);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, depth_func);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, stencil_enable);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, stencil_read_mask);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, stencil_write_mask);
  PRINT_OFF("aerogpu_depth_stencil_state", struct aerogpu_depth_stencil_state, reserved0);

  PRINT_OFF("aerogpu_cmd_set_depth_stencil_state", struct aerogpu_cmd_set_depth_stencil_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_depth_stencil_state", struct aerogpu_cmd_set_depth_stencil_state, state);

  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, fill_mode);
  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, cull_mode);
  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, front_ccw);
  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, scissor_enable);
  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, depth_bias);
  PRINT_OFF("aerogpu_rasterizer_state", struct aerogpu_rasterizer_state, flags);

  PRINT_OFF("aerogpu_cmd_set_rasterizer_state", struct aerogpu_cmd_set_rasterizer_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_rasterizer_state", struct aerogpu_cmd_set_rasterizer_state, state);

  PRINT_OFF("aerogpu_cmd_set_render_targets", struct aerogpu_cmd_set_render_targets, hdr);
  PRINT_OFF("aerogpu_cmd_set_render_targets", struct aerogpu_cmd_set_render_targets, color_count);
  PRINT_OFF("aerogpu_cmd_set_render_targets", struct aerogpu_cmd_set_render_targets, depth_stencil);
  PRINT_OFF("aerogpu_cmd_set_render_targets", struct aerogpu_cmd_set_render_targets, colors);

  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, hdr);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, x_f32);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, y_f32);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, width_f32);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, height_f32);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, min_depth_f32);
  PRINT_OFF("aerogpu_cmd_set_viewport", struct aerogpu_cmd_set_viewport, max_depth_f32);

  PRINT_OFF("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor, hdr);
  PRINT_OFF("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor, x);
  PRINT_OFF("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor, y);
  PRINT_OFF("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor, width);
  PRINT_OFF("aerogpu_cmd_set_scissor", struct aerogpu_cmd_set_scissor, height);

  PRINT_OFF("aerogpu_vertex_buffer_binding", struct aerogpu_vertex_buffer_binding, buffer);
  PRINT_OFF("aerogpu_vertex_buffer_binding", struct aerogpu_vertex_buffer_binding, stride_bytes);
  PRINT_OFF("aerogpu_vertex_buffer_binding", struct aerogpu_vertex_buffer_binding, offset_bytes);
  PRINT_OFF("aerogpu_vertex_buffer_binding", struct aerogpu_vertex_buffer_binding, reserved0);

  PRINT_OFF("aerogpu_cmd_set_vertex_buffers", struct aerogpu_cmd_set_vertex_buffers, hdr);
  PRINT_OFF("aerogpu_cmd_set_vertex_buffers", struct aerogpu_cmd_set_vertex_buffers, start_slot);
  PRINT_OFF("aerogpu_cmd_set_vertex_buffers", struct aerogpu_cmd_set_vertex_buffers, buffer_count);

  PRINT_OFF("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer, hdr);
  PRINT_OFF("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer, buffer);
  PRINT_OFF("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer, format);
  PRINT_OFF("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer, offset_bytes);
  PRINT_OFF("aerogpu_cmd_set_index_buffer", struct aerogpu_cmd_set_index_buffer, reserved0);

  PRINT_OFF("aerogpu_cmd_set_primitive_topology", struct aerogpu_cmd_set_primitive_topology, hdr);
  PRINT_OFF("aerogpu_cmd_set_primitive_topology", struct aerogpu_cmd_set_primitive_topology, topology);
  PRINT_OFF("aerogpu_cmd_set_primitive_topology", struct aerogpu_cmd_set_primitive_topology, reserved0);

  PRINT_OFF("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture, hdr);
  PRINT_OFF("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture, slot);
  PRINT_OFF("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture, texture);
  PRINT_OFF("aerogpu_cmd_set_texture", struct aerogpu_cmd_set_texture, reserved0);

  PRINT_OFF("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state, slot);
  PRINT_OFF("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state, state);
  PRINT_OFF("aerogpu_cmd_set_sampler_state", struct aerogpu_cmd_set_sampler_state, value);

  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, hdr);
  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, sampler_handle);
  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, filter);
  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, address_u);
  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, address_v);
  PRINT_OFF("aerogpu_cmd_create_sampler", struct aerogpu_cmd_create_sampler, address_w);

  PRINT_OFF("aerogpu_cmd_destroy_sampler", struct aerogpu_cmd_destroy_sampler, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_sampler", struct aerogpu_cmd_destroy_sampler, sampler_handle);
  PRINT_OFF("aerogpu_cmd_destroy_sampler", struct aerogpu_cmd_destroy_sampler, reserved0);

  PRINT_OFF("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers, hdr);
  PRINT_OFF("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers, start_slot);
  PRINT_OFF("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers, sampler_count);
  PRINT_OFF("aerogpu_cmd_set_samplers", struct aerogpu_cmd_set_samplers, reserved0);

  PRINT_OFF("aerogpu_constant_buffer_binding", struct aerogpu_constant_buffer_binding, buffer);
  PRINT_OFF("aerogpu_constant_buffer_binding", struct aerogpu_constant_buffer_binding, offset_bytes);
  PRINT_OFF("aerogpu_constant_buffer_binding", struct aerogpu_constant_buffer_binding, size_bytes);
  PRINT_OFF("aerogpu_constant_buffer_binding", struct aerogpu_constant_buffer_binding, reserved0);

  PRINT_OFF("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers, hdr);
  PRINT_OFF("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers, start_slot);
  PRINT_OFF("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers, buffer_count);
  PRINT_OFF("aerogpu_cmd_set_constant_buffers", struct aerogpu_cmd_set_constant_buffers, reserved0);
  
  PRINT_OFF("aerogpu_shader_resource_buffer_binding", struct aerogpu_shader_resource_buffer_binding, buffer);
  PRINT_OFF("aerogpu_shader_resource_buffer_binding", struct aerogpu_shader_resource_buffer_binding, offset_bytes);
  PRINT_OFF("aerogpu_shader_resource_buffer_binding", struct aerogpu_shader_resource_buffer_binding, size_bytes);
  PRINT_OFF("aerogpu_shader_resource_buffer_binding", struct aerogpu_shader_resource_buffer_binding, reserved0);
  
  PRINT_OFF("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers, hdr);
  PRINT_OFF("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers, start_slot);
  PRINT_OFF("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers, buffer_count);
  PRINT_OFF("aerogpu_cmd_set_shader_resource_buffers", struct aerogpu_cmd_set_shader_resource_buffers, reserved0);
  
  PRINT_OFF("aerogpu_unordered_access_buffer_binding", struct aerogpu_unordered_access_buffer_binding, buffer);
  PRINT_OFF("aerogpu_unordered_access_buffer_binding", struct aerogpu_unordered_access_buffer_binding, offset_bytes);
  PRINT_OFF("aerogpu_unordered_access_buffer_binding", struct aerogpu_unordered_access_buffer_binding, size_bytes);
  PRINT_OFF("aerogpu_unordered_access_buffer_binding", struct aerogpu_unordered_access_buffer_binding, initial_count);
  
  PRINT_OFF("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers, hdr);
  PRINT_OFF("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers, shader_stage);
  PRINT_OFF("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers, start_slot);
  PRINT_OFF("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers, uav_count);
  PRINT_OFF("aerogpu_cmd_set_unordered_access_buffers", struct aerogpu_cmd_set_unordered_access_buffers, reserved0);

  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, state);
  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, value);

  PRINT_OFF("aerogpu_cmd_clear", struct aerogpu_cmd_clear, hdr);
  PRINT_OFF("aerogpu_cmd_clear", struct aerogpu_cmd_clear, flags);
  PRINT_OFF("aerogpu_cmd_clear", struct aerogpu_cmd_clear, color_rgba_f32);
  PRINT_OFF("aerogpu_cmd_clear", struct aerogpu_cmd_clear, depth_f32);
  PRINT_OFF("aerogpu_cmd_clear", struct aerogpu_cmd_clear, stencil);

  PRINT_OFF("aerogpu_cmd_draw", struct aerogpu_cmd_draw, hdr);
  PRINT_OFF("aerogpu_cmd_draw", struct aerogpu_cmd_draw, vertex_count);
  PRINT_OFF("aerogpu_cmd_draw", struct aerogpu_cmd_draw, instance_count);
  PRINT_OFF("aerogpu_cmd_draw", struct aerogpu_cmd_draw, first_vertex);
  PRINT_OFF("aerogpu_cmd_draw", struct aerogpu_cmd_draw, first_instance);

  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, hdr);
  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, index_count);
  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, instance_count);
  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, first_index);
  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, base_vertex);
  PRINT_OFF("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed, first_instance);
  
  PRINT_OFF("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch, hdr);
  PRINT_OFF("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch, group_count_x);
  PRINT_OFF("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch, group_count_y);
  PRINT_OFF("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch, group_count_z);
  PRINT_OFF("aerogpu_cmd_dispatch", struct aerogpu_cmd_dispatch, reserved0);

  PRINT_OFF("aerogpu_cmd_present", struct aerogpu_cmd_present, hdr);
  PRINT_OFF("aerogpu_cmd_present", struct aerogpu_cmd_present, scanout_id);
  PRINT_OFF("aerogpu_cmd_present", struct aerogpu_cmd_present, flags);

  PRINT_OFF("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex, hdr);
  PRINT_OFF("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex, scanout_id);
  PRINT_OFF("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex, flags);
  PRINT_OFF("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex, d3d9_present_flags);
  PRINT_OFF("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex, reserved0);

  PRINT_OFF("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface, hdr);
  PRINT_OFF("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface, resource_handle);
  PRINT_OFF("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface, reserved0);
  PRINT_OFF("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface, share_token);

  PRINT_OFF("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface, hdr);
  PRINT_OFF("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface, out_resource_handle);
  PRINT_OFF("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface, reserved0);
  PRINT_OFF("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface, share_token);

  PRINT_OFF("aerogpu_cmd_release_shared_surface", struct aerogpu_cmd_release_shared_surface, hdr);
  PRINT_OFF("aerogpu_cmd_release_shared_surface", struct aerogpu_cmd_release_shared_surface, share_token);
  PRINT_OFF("aerogpu_cmd_release_shared_surface", struct aerogpu_cmd_release_shared_surface, reserved0);

  PRINT_OFF("aerogpu_cmd_flush", struct aerogpu_cmd_flush, hdr);
  PRINT_OFF("aerogpu_cmd_flush", struct aerogpu_cmd_flush, reserved0);
  PRINT_OFF("aerogpu_cmd_flush", struct aerogpu_cmd_flush, reserved1);

  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, magic);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, abi_version);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, size_bytes);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, entry_count);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, entry_stride_bytes);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, reserved0);

  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, alloc_id);
  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, flags);
  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, gpa);
  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, size_bytes);
  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, reserved0);

  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, desc_size_bytes);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, flags);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, context_id);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, engine_id);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, cmd_gpa);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, cmd_size_bytes);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, cmd_reserved0);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, alloc_table_gpa);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, alloc_table_size_bytes);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, alloc_table_reserved0);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, signal_fence);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, reserved0);

  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, magic);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, abi_version);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, size_bytes);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, entry_count);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, entry_stride_bytes);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, flags);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, head);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, tail);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, reserved0);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, reserved1);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, reserved2);

  PRINT_OFF("aerogpu_fence_page", struct aerogpu_fence_page, magic);
  PRINT_OFF("aerogpu_fence_page", struct aerogpu_fence_page, abi_version);
  PRINT_OFF("aerogpu_fence_page", struct aerogpu_fence_page, completed_fence);
  PRINT_OFF("aerogpu_fence_page", struct aerogpu_fence_page, reserved0);

  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, size_bytes);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, struct_version);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_mmio_magic);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_abi_version_u32);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, reserved0);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_features);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, flags);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, reserved1);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, reserved2);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, reserved3);

  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, magic);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, version);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, alloc_id);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, flags);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, share_token);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, size_bytes);
  PRINT_OFF("aerogpu_wddm_alloc_priv", aerogpu_wddm_alloc_priv, reserved0);
 
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, magic);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, version);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, alloc_id);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, flags);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, share_token);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, size_bytes);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, reserved0);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, kind);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, width);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, height);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, format);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, row_pitch_bytes);
  PRINT_OFF("aerogpu_wddm_alloc_priv_v2", aerogpu_wddm_alloc_priv_v2, reserved1);
 
  PRINT_OFF("aerogpu_escape_header", aerogpu_escape_header, version);
  PRINT_OFF("aerogpu_escape_header", aerogpu_escape_header, op);
  PRINT_OFF("aerogpu_escape_header", aerogpu_escape_header, size);
  PRINT_OFF("aerogpu_escape_header", aerogpu_escape_header, reserved0);

  PRINT_OFF("aerogpu_escape_query_device_out", aerogpu_escape_query_device_out, mmio_version);
  PRINT_OFF("aerogpu_escape_query_device_out", aerogpu_escape_query_device_out, reserved0);

  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, detected_mmio_magic);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, abi_version_u32);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, features_lo);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, features_hi);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, reserved0);

  PRINT_OFF("aerogpu_escape_query_fence_out", aerogpu_escape_query_fence_out, last_submitted_fence);
  PRINT_OFF("aerogpu_escape_query_fence_out", aerogpu_escape_query_fence_out, last_completed_fence);
  PRINT_OFF("aerogpu_escape_query_fence_out", aerogpu_escape_query_fence_out, error_irq_count);
  PRINT_OFF("aerogpu_escape_query_fence_out", aerogpu_escape_query_fence_out, last_error_fence);
  
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, last_submitted_fence);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, last_completed_fence);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, ring0_size_bytes);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, ring0_entry_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, ring0_head);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, ring0_tail);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, total_submissions);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, total_presents);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, total_render_submits);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, total_internal_submits);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, irq_fence_delivered);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, irq_vblank_delivered);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, irq_spurious);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, reset_from_timeout_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, last_reset_time_100ns);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, vblank_seq);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, last_vblank_time_ns);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, vblank_period_ns);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, flags);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, pending_meta_handle_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, pending_meta_handle_reserved0);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, pending_meta_handle_bytes);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, error_irq_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, last_error_fence);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, ring_push_failures);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, selftest_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, selftest_last_error_code);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, reserved0);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, get_scanline_cache_hits);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, get_scanline_mmio_polls);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, contig_pool_hit);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, contig_pool_miss);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, contig_pool_bytes_saved);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, alloc_table_count);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, alloc_table_entries);
  PRINT_OFF("aerogpu_escape_query_perf_out", aerogpu_escape_query_perf_out, alloc_table_readonly_entries);

  PRINT_OFF("aerogpu_dbgctl_ring_desc", aerogpu_dbgctl_ring_desc, signal_fence);
  PRINT_OFF("aerogpu_dbgctl_ring_desc", aerogpu_dbgctl_ring_desc, cmd_gpa);
  PRINT_OFF("aerogpu_dbgctl_ring_desc", aerogpu_dbgctl_ring_desc, cmd_size_bytes);
  PRINT_OFF("aerogpu_dbgctl_ring_desc", aerogpu_dbgctl_ring_desc, flags);

  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, ring_id);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, ring_size_bytes);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, head);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, tail);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, desc_count);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, desc_capacity);
  PRINT_OFF("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout, desc);

  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, fence);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, cmd_gpa);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, cmd_size_bytes);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, flags);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, alloc_table_gpa);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, alloc_table_size_bytes);
  PRINT_OFF("aerogpu_dbgctl_ring_desc_v2", aerogpu_dbgctl_ring_desc_v2, reserved0);

  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, ring_id);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, ring_format);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, ring_size_bytes);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, head);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, tail);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, desc_count);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, desc_capacity);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, reserved0);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, reserved1);
  PRINT_OFF("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout, desc);

  PRINT_OFF("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout, timeout_ms);
  PRINT_OFF("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout, passed);
  PRINT_OFF("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout, error_code);
  PRINT_OFF("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout, reserved0);

  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vidpn_source_id);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, irq_enable);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, irq_status);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, flags);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_seq);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, last_vblank_time_ns);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_period_ns);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_interrupt_type);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, vidpn_source_id);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, reserved0);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, cached_enable);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, cached_width);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, cached_height);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, cached_format);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, cached_pitch_bytes);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_enable);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_width);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_height);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_format);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_pitch_bytes);
  PRINT_OFF("aerogpu_escape_query_scanout_out", aerogpu_escape_query_scanout_out, mmio_fb_gpa);
  PRINT_OFF("aerogpu_escape_query_scanout_out_v2", aerogpu_escape_query_scanout_out_v2, cached_fb_gpa);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, flags);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, reserved0);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, enable);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, x);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, y);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, hot_x);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, hot_y);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, width);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, height);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, format);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, fb_gpa);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, pitch_bytes);
  PRINT_OFF("aerogpu_escape_query_cursor_out", aerogpu_escape_query_cursor_out, reserved1);

  PRINT_OFF("aerogpu_escape_set_cursor_position_in", aerogpu_escape_set_cursor_position_in, x);
  PRINT_OFF("aerogpu_escape_set_cursor_position_in", aerogpu_escape_set_cursor_position_in, y);
  PRINT_OFF("aerogpu_escape_set_cursor_visibility_in", aerogpu_escape_set_cursor_visibility_in, visible);
  PRINT_OFF("aerogpu_escape_set_cursor_visibility_in", aerogpu_escape_set_cursor_visibility_in, reserved0);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, width);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, height);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, hot_x);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, hot_y);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, pitch_bytes);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, format);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, reserved0);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, reserved1);
  PRINT_OFF("aerogpu_escape_set_cursor_shape_in", aerogpu_escape_set_cursor_shape_in, pixels);
  PRINT_OFF("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out, flags);
  PRINT_OFF("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out, error_code);
  PRINT_OFF("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out, error_fence);
  PRINT_OFF("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out, error_count);
  PRINT_OFF("aerogpu_escape_query_error_out", aerogpu_escape_query_error_out, reserved0);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, shared_handle);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, debug_token);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, share_token);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, reserved0);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, gpa);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, size_bytes);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, reserved0);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, status);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, bytes_copied);
  PRINT_OFF("aerogpu_escape_read_gpa_inout", aerogpu_escape_read_gpa_inout, data);

  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, seq);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, call_seq);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, alloc_index);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, num_allocations);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, create_flags);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, alloc_id);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, priv_flags);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, pitch_bytes);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, share_token);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, size_bytes);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, flags_in);
  PRINT_OFF("aerogpu_dbgctl_createallocation_desc", aerogpu_dbgctl_createallocation_desc, flags_out);

  PRINT_OFF("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout, write_index);
  PRINT_OFF("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout, entry_count);
  PRINT_OFF("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout, entry_capacity);
  PRINT_OFF("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout, reserved0);
  PRINT_OFF("aerogpu_escape_dump_createallocation_inout", aerogpu_escape_dump_createallocation_inout, entries);

  /* ------------------------------ Constants ------------------------------- */
  PRINT_CONST(AEROGPU_ABI_MAJOR);
  PRINT_CONST(AEROGPU_ABI_MINOR);
  PRINT_CONST(AEROGPU_ABI_VERSION_U32);

  /* PCI identity / BAR layout. */
  PRINT_CONST(AEROGPU_PCI_VENDOR_ID);
  PRINT_CONST(AEROGPU_PCI_DEVICE_ID);
  PRINT_CONST(AEROGPU_PCI_SUBSYSTEM_VENDOR_ID);
  PRINT_CONST(AEROGPU_PCI_SUBSYSTEM_ID);
  PRINT_CONST(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER);
  PRINT_CONST(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE);
  PRINT_CONST(AEROGPU_PCI_PROG_IF);
  PRINT_CONST(AEROGPU_PCI_BAR0_INDEX);
  PRINT_CONST(AEROGPU_PCI_BAR0_SIZE_BYTES);
  PRINT_CONST(AEROGPU_PCI_BAR1_INDEX);
  PRINT_CONST(AEROGPU_PCI_BAR1_SIZE_BYTES);
  PRINT_CONST(AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES);

  /* MMIO register map. */
  PRINT_CONST(AEROGPU_MMIO_REG_MAGIC);
  PRINT_CONST(AEROGPU_MMIO_REG_ABI_VERSION);
  PRINT_CONST(AEROGPU_MMIO_REG_FEATURES_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_FEATURES_HI);

  PRINT_CONST(AEROGPU_MMIO_MAGIC);
  PRINT_CONST(AEROGPU_MMIO_REG_RING_GPA_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_RING_GPA_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_RING_SIZE_BYTES);
  PRINT_CONST(AEROGPU_MMIO_REG_RING_CONTROL);
  PRINT_CONST(AEROGPU_MMIO_REG_FENCE_GPA_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_FENCE_GPA_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_COMPLETED_FENCE_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_COMPLETED_FENCE_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_DOORBELL);

  PRINT_CONST(AEROGPU_MMIO_REG_IRQ_STATUS);
  PRINT_CONST(AEROGPU_MMIO_REG_IRQ_ENABLE);
  PRINT_CONST(AEROGPU_MMIO_REG_IRQ_ACK);
  PRINT_CONST(AEROGPU_MMIO_REG_ERROR_CODE);
  PRINT_CONST(AEROGPU_MMIO_REG_ERROR_FENCE_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_ERROR_FENCE_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_ERROR_COUNT);

  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_ENABLE);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_WIDTH);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_HEIGHT);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_FORMAT);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI);

  PRINT_CONST(AEROGPU_FEATURE_FENCE_PAGE);
  PRINT_CONST(AEROGPU_FEATURE_CURSOR);
  PRINT_CONST(AEROGPU_FEATURE_SCANOUT);
  PRINT_CONST(AEROGPU_FEATURE_VBLANK);
  PRINT_CONST(AEROGPU_FEATURE_TRANSFER);
  PRINT_CONST(AEROGPU_FEATURE_ERROR_INFO);
  PRINT_CONST(AEROGPU_RING_CONTROL_ENABLE);
  PRINT_CONST(AEROGPU_RING_CONTROL_RESET);
  PRINT_CONST(AEROGPU_IRQ_FENCE);
  PRINT_CONST(AEROGPU_IRQ_SCANOUT_VBLANK);
  PRINT_CONST(AEROGPU_IRQ_ERROR);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);

  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_ENABLE);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_X);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_Y);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_HOT_X);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_HOT_Y);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_WIDTH);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_HEIGHT);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_FORMAT);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI);
  PRINT_CONST(AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES);

  PRINT_CONST(AEROGPU_CMD_STREAM_MAGIC);
  PRINT_CONST(AEROGPU_CMD_STREAM_FLAG_NONE);
  PRINT_CONST(AEROGPU_STAGE_EX_MIN_ABI_MINOR);
  PRINT_CONST(AEROGPU_ALLOC_TABLE_MAGIC);
  PRINT_CONST(AEROGPU_RING_MAGIC);
  PRINT_CONST(AEROGPU_FENCE_PAGE_MAGIC);

  PRINT_CONST(AEROGPU_RESOURCE_USAGE_NONE);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_TEXTURE);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_SCANOUT);
  PRINT_CONST(AEROGPU_RESOURCE_USAGE_STORAGE);

  PRINT_CONST(AEROGPU_COPY_FLAG_NONE);
  PRINT_CONST(AEROGPU_COPY_FLAG_WRITEBACK_DST);

  PRINT_CONST(AEROGPU_MAX_RENDER_TARGETS);

  PRINT_CONST(AEROGPU_CMD_NOP);
  PRINT_CONST(AEROGPU_CMD_DEBUG_MARKER);
  PRINT_CONST(AEROGPU_CMD_CREATE_BUFFER);
  PRINT_CONST(AEROGPU_CMD_CREATE_TEXTURE2D);
  PRINT_CONST(AEROGPU_CMD_DESTROY_RESOURCE);
  PRINT_CONST(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  PRINT_CONST(AEROGPU_CMD_UPLOAD_RESOURCE);
  PRINT_CONST(AEROGPU_CMD_COPY_BUFFER);
  PRINT_CONST(AEROGPU_CMD_COPY_TEXTURE2D);
  PRINT_CONST(AEROGPU_CMD_CREATE_TEXTURE_VIEW);
  PRINT_CONST(AEROGPU_CMD_DESTROY_TEXTURE_VIEW);
  PRINT_CONST(AEROGPU_CMD_CREATE_SHADER_DXBC);
  PRINT_CONST(AEROGPU_CMD_DESTROY_SHADER);
  PRINT_CONST(AEROGPU_CMD_BIND_SHADERS);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_F);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_I);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_B);
  PRINT_CONST(AEROGPU_CMD_CREATE_INPUT_LAYOUT);
  PRINT_CONST(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  PRINT_CONST(AEROGPU_CMD_SET_INPUT_LAYOUT);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_I);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_B);
  PRINT_CONST(AEROGPU_CMD_SET_BLEND_STATE);
  PRINT_CONST(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  PRINT_CONST(AEROGPU_CMD_SET_RASTERIZER_STATE);
  PRINT_CONST(AEROGPU_CMD_SET_RENDER_TARGETS);
  PRINT_CONST(AEROGPU_CMD_SET_VIEWPORT);
  PRINT_CONST(AEROGPU_CMD_SET_SCISSOR);
  PRINT_CONST(AEROGPU_CMD_SET_VERTEX_BUFFERS);
  PRINT_CONST(AEROGPU_CMD_SET_INDEX_BUFFER);
  PRINT_CONST(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  PRINT_CONST(AEROGPU_CMD_SET_TEXTURE);
  PRINT_CONST(AEROGPU_CMD_SET_SAMPLER_STATE);
  PRINT_CONST(AEROGPU_CMD_SET_RENDER_STATE);
  PRINT_CONST(AEROGPU_CMD_CREATE_SAMPLER);
  PRINT_CONST(AEROGPU_CMD_DESTROY_SAMPLER);
  PRINT_CONST(AEROGPU_CMD_SET_SAMPLERS);
  PRINT_CONST(AEROGPU_CMD_SET_CONSTANT_BUFFERS);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS);
  PRINT_CONST(AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS);
  PRINT_CONST(AEROGPU_CMD_CLEAR);
  PRINT_CONST(AEROGPU_CMD_DRAW);
  PRINT_CONST(AEROGPU_CMD_DRAW_INDEXED);
  PRINT_CONST(AEROGPU_CMD_DISPATCH);
  PRINT_CONST(AEROGPU_CMD_PRESENT);
  PRINT_CONST(AEROGPU_CMD_PRESENT_EX);
  PRINT_CONST(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_RELEASE_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_FLUSH);

  PRINT_CONST(AEROGPU_SHADER_STAGE_VERTEX);
  PRINT_CONST(AEROGPU_SHADER_STAGE_PIXEL);
  PRINT_CONST(AEROGPU_SHADER_STAGE_COMPUTE);
  PRINT_CONST(AEROGPU_SHADER_STAGE_GEOMETRY);

  PRINT_CONST(AEROGPU_STAGE_EX_MIN_ABI_MINOR);

  PRINT_CONST(AEROGPU_SHADER_STAGE_EX_NONE);
  PRINT_CONST(AEROGPU_SHADER_STAGE_EX_GEOMETRY);
  PRINT_CONST(AEROGPU_SHADER_STAGE_EX_HULL);
  PRINT_CONST(AEROGPU_SHADER_STAGE_EX_DOMAIN);
  PRINT_CONST(AEROGPU_SHADER_STAGE_EX_COMPUTE);

  PRINT_CONST(AEROGPU_INDEX_FORMAT_UINT16);
  PRINT_CONST(AEROGPU_INDEX_FORMAT_UINT32);

  PRINT_CONST(AEROGPU_TOPOLOGY_POINTLIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLEFAN);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINELIST_ADJ);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINESTRIP_ADJ);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLELIST_ADJ);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLESTRIP_ADJ);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_1);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_2);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_3);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_4);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_5);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_6);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_7);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_8);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_9);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_10);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_11);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_12);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_13);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_14);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_15);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_16);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_17);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_18);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_19);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_20);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_21);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_22);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_23);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_24);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_25);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_26);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_27);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_28);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_29);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_30);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_31);
  PRINT_CONST(AEROGPU_TOPOLOGY_PATCHLIST_32);

  PRINT_CONST(AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
  PRINT_CONST(AEROGPU_INPUT_LAYOUT_BLOB_VERSION);

  PRINT_CONST(AEROGPU_CLEAR_COLOR);
  PRINT_CONST(AEROGPU_CLEAR_DEPTH);
  PRINT_CONST(AEROGPU_CLEAR_STENCIL);

  PRINT_CONST(AEROGPU_PRESENT_FLAG_NONE);
  PRINT_CONST(AEROGPU_PRESENT_FLAG_VSYNC);

  PRINT_CONST(AEROGPU_BLEND_ZERO);
  PRINT_CONST(AEROGPU_BLEND_ONE);
  PRINT_CONST(AEROGPU_BLEND_SRC_ALPHA);
  PRINT_CONST(AEROGPU_BLEND_INV_SRC_ALPHA);
  PRINT_CONST(AEROGPU_BLEND_DEST_ALPHA);
  PRINT_CONST(AEROGPU_BLEND_INV_DEST_ALPHA);
  PRINT_CONST(AEROGPU_BLEND_CONSTANT);
  PRINT_CONST(AEROGPU_BLEND_INV_CONSTANT);

  PRINT_CONST(AEROGPU_BLEND_OP_ADD);
  PRINT_CONST(AEROGPU_BLEND_OP_SUBTRACT);
  PRINT_CONST(AEROGPU_BLEND_OP_REV_SUBTRACT);
  PRINT_CONST(AEROGPU_BLEND_OP_MIN);
  PRINT_CONST(AEROGPU_BLEND_OP_MAX);

  PRINT_CONST(AEROGPU_COMPARE_NEVER);
  PRINT_CONST(AEROGPU_COMPARE_LESS);
  PRINT_CONST(AEROGPU_COMPARE_EQUAL);
  PRINT_CONST(AEROGPU_COMPARE_LESS_EQUAL);
  PRINT_CONST(AEROGPU_COMPARE_GREATER);
  PRINT_CONST(AEROGPU_COMPARE_NOT_EQUAL);
  PRINT_CONST(AEROGPU_COMPARE_GREATER_EQUAL);
  PRINT_CONST(AEROGPU_COMPARE_ALWAYS);

  PRINT_CONST(AEROGPU_FILL_SOLID);
  PRINT_CONST(AEROGPU_FILL_WIREFRAME);

  PRINT_CONST(AEROGPU_CULL_NONE);
  PRINT_CONST(AEROGPU_CULL_FRONT);
  PRINT_CONST(AEROGPU_CULL_BACK);
  PRINT_CONST(AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE);

  PRINT_CONST(AEROGPU_ERROR_NONE);
  PRINT_CONST(AEROGPU_ERROR_CMD_DECODE);
  PRINT_CONST(AEROGPU_ERROR_OOB);
  PRINT_CONST(AEROGPU_ERROR_BACKEND);
  PRINT_CONST(AEROGPU_ERROR_INTERNAL);

  PRINT_CONST(AEROGPU_FORMAT_INVALID);
  PRINT_CONST(AEROGPU_FORMAT_B8G8R8A8_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_B8G8R8X8_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_R8G8B8A8_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_R8G8B8X8_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_B5G6R5_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_B5G5R5A1_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_D24_UNORM_S8_UINT);
  PRINT_CONST(AEROGPU_FORMAT_D32_FLOAT);
  PRINT_CONST(AEROGPU_FORMAT_BC1_RGBA_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_BC2_RGBA_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_BC3_RGBA_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB);
  PRINT_CONST(AEROGPU_FORMAT_BC7_RGBA_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB);

  PRINT_CONST(AEROGPU_SUBMIT_FLAG_NONE);
  PRINT_CONST(AEROGPU_SUBMIT_FLAG_PRESENT);
  PRINT_CONST(AEROGPU_SUBMIT_FLAG_NO_IRQ);

  PRINT_CONST(AEROGPU_ENGINE_0);

  PRINT_CONST(AEROGPU_ALLOC_FLAG_NONE);
  PRINT_CONST(AEROGPU_ALLOC_FLAG_READONLY);

  PRINT_CONST(AEROGPU_UMDPRIV_STRUCT_VERSION_V1);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_REG_MAGIC);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_CURSOR);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_SCANOUT);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_VBLANK);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_TRANSFER);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_ERROR_INFO);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_IS_LEGACY);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_VBLANK);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE);

  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_VERSION);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_ID_KMD_MIN);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_UNKNOWN);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_BUFFER);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D);
  
  PRINT_CONST(AEROGPU_ESCAPE_VERSION);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_DEVICE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_FENCE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_RING);
  PRINT_CONST(AEROGPU_ESCAPE_OP_SELFTEST);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_VBLANK);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_VBLANK);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_RING_V2);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2);
  PRINT_CONST(AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_SCANOUT);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_CURSOR);
  PRINT_CONST(AEROGPU_ESCAPE_OP_SET_CURSOR_SHAPE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_SET_CURSOR_POSITION);
  PRINT_CONST(AEROGPU_ESCAPE_OP_SET_CURSOR_VISIBILITY);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_PERF);
  PRINT_CONST(AEROGPU_ESCAPE_OP_READ_GPA);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_ERROR);
  PRINT_CONST(AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS);
  PRINT_CONST(AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS);
  PRINT_CONST(AEROGPU_DBGCTL_READ_GPA_MAX_BYTES);

  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN);
  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_LEGACY);
  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_AGPU);

  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_OK);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED);
  PRINT_CONST(AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED);

  PRINT_CONST(AEROGPU_DBGCTL_QUERY_PERF_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_PERF_FLAG_RING_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_PERF_FLAG_VBLANK_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_PERF_FLAG_GETSCANLINE_COUNTERS_VALID);

  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_LATCHED);

  return 0;
}
