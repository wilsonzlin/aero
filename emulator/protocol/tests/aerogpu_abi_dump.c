#include <stddef.h>
#include <stdio.h>

#include "drivers/aerogpu/protocol/aerogpu_alloc.h"
#include "drivers/aerogpu/protocol/aerogpu_cmd.h"
#include "drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h"
#include "drivers/aerogpu/protocol/aerogpu_escape.h"
#include "drivers/aerogpu/protocol/aerogpu_ring.h"
#include "drivers/aerogpu/protocol/aerogpu_umd_private.h"
#include "drivers/aerogpu/protocol/aerogpu_wddm_alloc.h"

#define PRINT_SIZE(name, type) printf("SIZE %s %zu\n", name, sizeof(type))
#define PRINT_OFF(name, type, field) printf("OFF %s %s %zu\n", name, #field, offsetof(type, field))
#define PRINT_CONST(name) printf("CONST %s %llu\n", #name, (unsigned long long)(name))

int main(void) {
  /* ------------------------------- Struct sizes -------------------------- */
  PRINT_SIZE("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header);
  PRINT_SIZE("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr);

  PRINT_SIZE("aerogpu_cmd_create_buffer", struct aerogpu_cmd_create_buffer);
  PRINT_SIZE("aerogpu_cmd_create_texture2d", struct aerogpu_cmd_create_texture2d);
  PRINT_SIZE("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource);
  PRINT_SIZE("aerogpu_cmd_resource_dirty_range", struct aerogpu_cmd_resource_dirty_range);
  PRINT_SIZE("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource);
  PRINT_SIZE("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer);
  PRINT_SIZE("aerogpu_cmd_copy_texture2d", struct aerogpu_cmd_copy_texture2d);
  PRINT_SIZE("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc);
  PRINT_SIZE("aerogpu_cmd_destroy_shader", struct aerogpu_cmd_destroy_shader);
  PRINT_SIZE("aerogpu_cmd_bind_shaders", struct aerogpu_cmd_bind_shaders);
  PRINT_SIZE("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f);
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
  PRINT_SIZE("aerogpu_cmd_clear", struct aerogpu_cmd_clear);
  PRINT_SIZE("aerogpu_cmd_draw", struct aerogpu_cmd_draw);
  PRINT_SIZE("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed);
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
  PRINT_SIZE("aerogpu_escape_dump_ring_inout", aerogpu_escape_dump_ring_inout);
  PRINT_SIZE("aerogpu_escape_dump_ring_v2_inout", aerogpu_escape_dump_ring_v2_inout);
  PRINT_SIZE("aerogpu_escape_selftest_inout", aerogpu_escape_selftest_inout);
  PRINT_SIZE("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out);
  PRINT_SIZE("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout);

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

  /* Variable-length packet headers. */
  PRINT_OFF("aerogpu_cmd_create_shader_dxbc", struct aerogpu_cmd_create_shader_dxbc, dxbc_size_bytes);
  PRINT_OFF("aerogpu_cmd_set_shader_constants_f", struct aerogpu_cmd_set_shader_constants_f, vec4_count);
  PRINT_OFF("aerogpu_cmd_create_input_layout", struct aerogpu_cmd_create_input_layout, blob_size_bytes);
  PRINT_OFF("aerogpu_cmd_set_vertex_buffers", struct aerogpu_cmd_set_vertex_buffers, buffer_count);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, offset_bytes);
  PRINT_OFF("aerogpu_cmd_upload_resource", struct aerogpu_cmd_upload_resource, size_bytes);

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

  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, hdr);
  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, resource_handle);
  PRINT_OFF("aerogpu_cmd_destroy_resource", struct aerogpu_cmd_destroy_resource, reserved0);

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

  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, hdr);
  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, state);
  PRINT_OFF("aerogpu_cmd_set_render_state", struct aerogpu_cmd_set_render_state, value);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, dst_buffer);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, src_buffer);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, dst_offset_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, src_offset_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, size_bytes);
  PRINT_OFF("aerogpu_cmd_copy_buffer", struct aerogpu_cmd_copy_buffer, flags);
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

  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, detected_mmio_magic);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, abi_version_u32);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, features_lo);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, features_hi);
  PRINT_OFF("aerogpu_escape_query_device_v2_out", aerogpu_escape_query_device_v2_out, reserved0);

  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vidpn_source_id);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, irq_enable);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, irq_status);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, flags);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_seq);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, last_vblank_time_ns);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_period_ns);
  PRINT_OFF("aerogpu_escape_query_vblank_out", aerogpu_escape_query_vblank_out, vblank_interrupt_type);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, shared_handle);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, debug_token);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, share_token);
  PRINT_OFF("aerogpu_escape_map_shared_handle_inout", aerogpu_escape_map_shared_handle_inout, reserved0);

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
  PRINT_CONST(AEROGPU_CMD_CREATE_SHADER_DXBC);
  PRINT_CONST(AEROGPU_CMD_DESTROY_SHADER);
  PRINT_CONST(AEROGPU_CMD_BIND_SHADERS);
  PRINT_CONST(AEROGPU_CMD_SET_SHADER_CONSTANTS_F);
  PRINT_CONST(AEROGPU_CMD_CREATE_INPUT_LAYOUT);
  PRINT_CONST(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  PRINT_CONST(AEROGPU_CMD_SET_INPUT_LAYOUT);
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
  PRINT_CONST(AEROGPU_CMD_CLEAR);
  PRINT_CONST(AEROGPU_CMD_DRAW);
  PRINT_CONST(AEROGPU_CMD_DRAW_INDEXED);
  PRINT_CONST(AEROGPU_CMD_PRESENT);
  PRINT_CONST(AEROGPU_CMD_PRESENT_EX);
  PRINT_CONST(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_RELEASE_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_FLUSH);

  PRINT_CONST(AEROGPU_SHADER_STAGE_VERTEX);
  PRINT_CONST(AEROGPU_SHADER_STAGE_PIXEL);
  PRINT_CONST(AEROGPU_SHADER_STAGE_COMPUTE);

  PRINT_CONST(AEROGPU_INDEX_FORMAT_UINT16);
  PRINT_CONST(AEROGPU_INDEX_FORMAT_UINT32);

  PRINT_CONST(AEROGPU_TOPOLOGY_POINTLIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLEFAN);

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
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_IS_LEGACY);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_VBLANK);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE);

  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_MAGIC);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_VERSION);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_VERSION_2);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_ID_UMD_MAX);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_ID_KMD_MIN);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_UNKNOWN);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_BUFFER);
  PRINT_CONST(AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D);
 
  PRINT_CONST(AEROGPU_ESCAPE_VERSION);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_DEVICE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2);
  PRINT_CONST(AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE);

  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_FENCE);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_RING);
  PRINT_CONST(AEROGPU_ESCAPE_OP_SELFTEST);
  PRINT_CONST(AEROGPU_ESCAPE_OP_QUERY_VBLANK);
  PRINT_CONST(AEROGPU_ESCAPE_OP_DUMP_RING_V2);

  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN);
  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_LEGACY);
  PRINT_CONST(AEROGPU_DBGCTL_RING_FORMAT_AGPU);

  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED);
  PRINT_CONST(AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID);

  return 0;
}
