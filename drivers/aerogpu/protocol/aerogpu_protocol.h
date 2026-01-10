// AeroGPU Protocol - command stream shared by all AeroGPU UMDs.
//
// The intent of this header is to define the stable "wire format" that is
// submitted by the guest Windows UMD into the AeroGPU KMD, and then consumed by
// the host-side translator (e.g. WebGPU backend).
//
// This protocol is intentionally conservative:
// - Little-endian, fixed-size POD structs.
// - No pointers; all references use 32-bit "allocation indices" allocated by
//   the UMD. This avoids heavy patching/relocation work in the KMD.
// - Extensible: new commands can be appended without changing old ones.
//
// NOTE: This protocol is designed to support both D3D9 and D3D10/11 style UMDs.
// D3D10/11 resources are represented using the same allocation index namespace.

#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Command stream header for every packet.
//
// The command stream is a sequence of:
//   [AEROGPU_CMD_HEADER][payload bytes...]
//
// `size_bytes` includes the header itself.
typedef struct AEROGPU_CMD_HEADER {
  uint32_t opcode;
  uint32_t size_bytes;
} AEROGPU_CMD_HEADER;

enum AEROGPU_CMD_OPCODE {
  // Resource lifetime.
  AEROGPU_CMD_CREATE_RESOURCE = 0x0001,
  AEROGPU_CMD_DESTROY_RESOURCE = 0x0002,
  AEROGPU_CMD_UPLOAD_RESOURCE = 0x0003,

  // Shaders and pipeline state.
  AEROGPU_CMD_CREATE_SHADER = 0x0101,
  AEROGPU_CMD_DESTROY_SHADER = 0x0102,
  AEROGPU_CMD_BIND_SHADERS = 0x0103,
  AEROGPU_CMD_SET_INPUT_LAYOUT = 0x0104,

  // Binding.
  AEROGPU_CMD_SET_VERTEX_BUFFER = 0x0201,
  AEROGPU_CMD_SET_INDEX_BUFFER = 0x0202,
  AEROGPU_CMD_SET_RENDER_TARGET = 0x0203,
  AEROGPU_CMD_SET_VIEWPORT = 0x0204,

  // Draw.
  AEROGPU_CMD_CLEAR_RTV = 0x0301,
  AEROGPU_CMD_DRAW = 0x0302,
  AEROGPU_CMD_DRAW_INDEXED = 0x0303,

  // Presentation / synchronization.
  AEROGPU_CMD_PRESENT = 0x0401,
  AEROGPU_CMD_SIGNAL_FENCE = 0x0402,
};

// Resource types understood by the host translator.
enum AEROGPU_RESOURCE_KIND {
  AEROGPU_RESOURCE_KIND_BUFFER = 1,
  AEROGPU_RESOURCE_KIND_TEX2D = 2,
};

// Formats are expressed using DXGI_FORMAT numeric values to avoid yet another
// format enum. This keeps the protocol stable across UMDs.

typedef struct AEROGPU_CMD_CREATE_RESOURCE_PAYLOAD {
  uint32_t alloc_index;
  uint32_t kind; // AEROGPU_RESOURCE_KIND

  // Common fields.
  uint32_t bind_flags; // D3D10/11 bind flags (D3D11_BIND_*)
  uint32_t misc_flags; // driver-defined for now

  // Buffer fields when kind == BUFFER
  uint32_t size_bytes;
  uint32_t stride_bytes;

  // Texture2D fields when kind == TEX2D
  uint32_t width;
  uint32_t height;
  uint32_t mip_levels;
  uint32_t array_size;
  uint32_t dxgi_format; // DXGI_FORMAT numeric value
} AEROGPU_CMD_CREATE_RESOURCE_PAYLOAD;

typedef struct AEROGPU_CMD_DESTROY_RESOURCE_PAYLOAD {
  uint32_t alloc_index;
} AEROGPU_CMD_DESTROY_RESOURCE_PAYLOAD;

typedef struct AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD {
  uint32_t alloc_index;
  uint32_t dst_offset_bytes;
  uint32_t data_size_bytes;
  // Followed by `data_size_bytes` of raw data.
} AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD;

enum AEROGPU_SHADER_STAGE {
  AEROGPU_SHADER_STAGE_VS = 1,
  AEROGPU_SHADER_STAGE_PS = 2,
};

typedef struct AEROGPU_CMD_CREATE_SHADER_PAYLOAD {
  uint32_t shader_id;
  uint32_t stage; // AEROGPU_SHADER_STAGE
  uint32_t dxbc_size_bytes;
  // Followed by `dxbc_size_bytes` of DXBC.
} AEROGPU_CMD_CREATE_SHADER_PAYLOAD;

typedef struct AEROGPU_CMD_DESTROY_SHADER_PAYLOAD {
  uint32_t shader_id;
} AEROGPU_CMD_DESTROY_SHADER_PAYLOAD;

typedef struct AEROGPU_CMD_BIND_SHADERS_PAYLOAD {
  uint32_t vs_shader_id; // 0 == unbind
  uint32_t ps_shader_id; // 0 == unbind
} AEROGPU_CMD_BIND_SHADERS_PAYLOAD;

// The input layout is emitted as a variable-length command because D3D input
// layouts are small and immutable (set once, reused across draws).
//
// The payload is:
//   [AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD]
//   [AEROGPU_INPUT_ELEMENT element[element_count]]
typedef struct AEROGPU_INPUT_ELEMENT {
  uint32_t semantic_name_hash; // FNV-1a hash of ASCII semantic name
  uint32_t semantic_index;
  uint32_t format_dxgi; // DXGI_FORMAT numeric value
  uint32_t input_slot;
  uint32_t aligned_byte_offset;
  uint32_t input_slot_class; // 0: per-vertex, 1: per-instance
  uint32_t instance_data_step_rate;
} AEROGPU_INPUT_ELEMENT;

typedef struct AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD {
  uint32_t element_count;
} AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD;

typedef struct AEROGPU_CMD_SET_VERTEX_BUFFER_PAYLOAD {
  uint32_t alloc_index;
  uint32_t stride_bytes;
  uint32_t offset_bytes;
} AEROGPU_CMD_SET_VERTEX_BUFFER_PAYLOAD;

typedef struct AEROGPU_CMD_SET_INDEX_BUFFER_PAYLOAD {
  uint32_t alloc_index;
  uint32_t index_format_dxgi; // DXGI_FORMAT_R16_UINT / DXGI_FORMAT_R32_UINT numeric
  uint32_t offset_bytes;
} AEROGPU_CMD_SET_INDEX_BUFFER_PAYLOAD;

typedef struct AEROGPU_CMD_SET_RENDER_TARGET_PAYLOAD {
  uint32_t rtv_alloc_index; // allocation index of render target texture
} AEROGPU_CMD_SET_RENDER_TARGET_PAYLOAD;

typedef struct AEROGPU_CMD_SET_VIEWPORT_PAYLOAD {
  float x;
  float y;
  float width;
  float height;
  float min_depth;
  float max_depth;
} AEROGPU_CMD_SET_VIEWPORT_PAYLOAD;

typedef struct AEROGPU_CMD_CLEAR_RTV_PAYLOAD {
  float rgba[4];
} AEROGPU_CMD_CLEAR_RTV_PAYLOAD;

typedef struct AEROGPU_CMD_DRAW_PAYLOAD {
  uint32_t vertex_count;
  uint32_t start_vertex_location;
} AEROGPU_CMD_DRAW_PAYLOAD;

typedef struct AEROGPU_CMD_DRAW_INDEXED_PAYLOAD {
  uint32_t index_count;
  uint32_t start_index_location;
  int32_t base_vertex_location;
} AEROGPU_CMD_DRAW_INDEXED_PAYLOAD;

typedef struct AEROGPU_CMD_PRESENT_PAYLOAD {
  uint32_t backbuffer_alloc_index;
  uint32_t sync_interval; // 0 or 1 (initially)
} AEROGPU_CMD_PRESENT_PAYLOAD;

typedef struct AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD {
  uint64_t fence_value;
} AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD;

#ifdef __cplusplus
} // extern "C"
#endif
