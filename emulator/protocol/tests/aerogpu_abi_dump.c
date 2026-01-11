#include <stddef.h>
#include <stdio.h>

#include "drivers/aerogpu/protocol/aerogpu_cmd.h"
#include "drivers/aerogpu/protocol/aerogpu_ring.h"
#include "drivers/aerogpu/protocol/aerogpu_umd_private.h"

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
  PRINT_SIZE("aerogpu_cmd_clear", struct aerogpu_cmd_clear);
  PRINT_SIZE("aerogpu_cmd_draw", struct aerogpu_cmd_draw);
  PRINT_SIZE("aerogpu_cmd_draw_indexed", struct aerogpu_cmd_draw_indexed);
  PRINT_SIZE("aerogpu_cmd_present", struct aerogpu_cmd_present);
  PRINT_SIZE("aerogpu_cmd_present_ex", struct aerogpu_cmd_present_ex);
  PRINT_SIZE("aerogpu_cmd_export_shared_surface", struct aerogpu_cmd_export_shared_surface);
  PRINT_SIZE("aerogpu_cmd_import_shared_surface", struct aerogpu_cmd_import_shared_surface);
  PRINT_SIZE("aerogpu_cmd_flush", struct aerogpu_cmd_flush);

  PRINT_SIZE("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header);
  PRINT_SIZE("aerogpu_alloc_entry", struct aerogpu_alloc_entry);
  PRINT_SIZE("aerogpu_submit_desc", struct aerogpu_submit_desc);
  PRINT_SIZE("aerogpu_ring_header", struct aerogpu_ring_header);
  PRINT_SIZE("aerogpu_fence_page", struct aerogpu_fence_page);

  PRINT_SIZE("aerogpu_umd_private_v1", aerogpu_umd_private_v1);

  /* -------------------------------- Offsets ------------------------------ */
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, magic);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, abi_version);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, size_bytes);
  PRINT_OFF("aerogpu_cmd_stream_header", struct aerogpu_cmd_stream_header, flags);

  PRINT_OFF("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr, opcode);
  PRINT_OFF("aerogpu_cmd_hdr", struct aerogpu_cmd_hdr, size_bytes);

  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, magic);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, abi_version);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, size_bytes);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, entry_count);
  PRINT_OFF("aerogpu_alloc_table_header", struct aerogpu_alloc_table_header, entry_stride_bytes);

  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, gpa);
  PRINT_OFF("aerogpu_alloc_entry", struct aerogpu_alloc_entry, size_bytes);

  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, cmd_gpa);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, alloc_table_gpa);
  PRINT_OFF("aerogpu_submit_desc", struct aerogpu_submit_desc, signal_fence);

  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, head);
  PRINT_OFF("aerogpu_ring_header", struct aerogpu_ring_header, tail);

  PRINT_OFF("aerogpu_fence_page", struct aerogpu_fence_page, completed_fence);

  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, size_bytes);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, struct_version);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_mmio_magic);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_abi_version_u32);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, device_features);
  PRINT_OFF("aerogpu_umd_private_v1", aerogpu_umd_private_v1, flags);

  /* ------------------------------ Constants ------------------------------- */
  PRINT_CONST(AEROGPU_ABI_MAJOR);
  PRINT_CONST(AEROGPU_ABI_MINOR);
  PRINT_CONST(AEROGPU_ABI_VERSION_U32);
  PRINT_CONST(AEROGPU_PCI_VENDOR_ID);
  PRINT_CONST(AEROGPU_PCI_DEVICE_ID);
  PRINT_CONST(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER);
  PRINT_CONST(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE);
  PRINT_CONST(AEROGPU_PCI_PROG_IF);

  PRINT_CONST(AEROGPU_MMIO_MAGIC);
  PRINT_CONST(AEROGPU_MMIO_REG_DOORBELL);
  PRINT_CONST(AEROGPU_FEATURE_FENCE_PAGE);
  PRINT_CONST(AEROGPU_FEATURE_VBLANK);
  PRINT_CONST(AEROGPU_RING_CONTROL_ENABLE);
  PRINT_CONST(AEROGPU_IRQ_FENCE);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO);
  PRINT_CONST(AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);

  PRINT_CONST(AEROGPU_CMD_STREAM_MAGIC);
  PRINT_CONST(AEROGPU_ALLOC_TABLE_MAGIC);
  PRINT_CONST(AEROGPU_RING_MAGIC);
  PRINT_CONST(AEROGPU_FENCE_PAGE_MAGIC);

  PRINT_CONST(AEROGPU_CMD_NOP);
  PRINT_CONST(AEROGPU_CMD_DEBUG_MARKER);
  PRINT_CONST(AEROGPU_CMD_CREATE_BUFFER);
  PRINT_CONST(AEROGPU_CMD_CREATE_TEXTURE2D);
  PRINT_CONST(AEROGPU_CMD_DESTROY_RESOURCE);
  PRINT_CONST(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  PRINT_CONST(AEROGPU_CMD_UPLOAD_RESOURCE);
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
  PRINT_CONST(AEROGPU_CMD_CLEAR);
  PRINT_CONST(AEROGPU_CMD_DRAW);
  PRINT_CONST(AEROGPU_CMD_DRAW_INDEXED);
  PRINT_CONST(AEROGPU_CMD_PRESENT);
  PRINT_CONST(AEROGPU_CMD_PRESENT_EX);
  PRINT_CONST(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  PRINT_CONST(AEROGPU_CMD_FLUSH);

  PRINT_CONST(AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
  PRINT_CONST(AEROGPU_INPUT_LAYOUT_BLOB_VERSION);

  PRINT_CONST(AEROGPU_TOPOLOGY_POINTLIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_LINESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLELIST);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLESTRIP);
  PRINT_CONST(AEROGPU_TOPOLOGY_TRIANGLEFAN);

  PRINT_CONST(AEROGPU_FORMAT_B8G8R8A8_UNORM);
  PRINT_CONST(AEROGPU_FORMAT_D32_FLOAT);

  PRINT_CONST(AEROGPU_SUBMIT_FLAG_PRESENT);
  PRINT_CONST(AEROGPU_SUBMIT_FLAG_NO_IRQ);

  PRINT_CONST(AEROGPU_UMDPRIV_STRUCT_VERSION_V1);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP);
  PRINT_CONST(AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE);
  PRINT_CONST(AEROGPU_UMDPRIV_FEATURE_VBLANK);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_IS_LEGACY);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_VBLANK);
  PRINT_CONST(AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE);

  return 0;
}
