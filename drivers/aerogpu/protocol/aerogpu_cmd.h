/*
 * AeroGPU Guest↔Emulator ABI (Command stream)
 *
 * Command buffers are byte streams in guest memory (GPA) referenced by
 * `aerogpu_submit_desc::cmd_gpa/cmd_size_bytes`.
 *
 * A command buffer begins with `aerogpu_cmd_stream_header`, followed by a
 * sequence of packets each beginning with `aerogpu_cmd_hdr`.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_CMD_H_
#define AEROGPU_PROTOCOL_AEROGPU_CMD_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>

#include "aerogpu_pci.h"

typedef uint32_t aerogpu_handle_t;

/* ----------------------------- Stream header ----------------------------- */

#define AEROGPU_CMD_STREAM_MAGIC 0x444D4341u /* "ACMD" little-endian */

enum aerogpu_cmd_stream_flags {
  AEROGPU_CMD_STREAM_FLAG_NONE = 0,
};

/*
 * Command stream header. Must be present at the start of every command buffer.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_stream_header {
  uint32_t magic; /* AEROGPU_CMD_STREAM_MAGIC */
  uint32_t abi_version; /* AEROGPU_ABI_VERSION_U32 */
  uint32_t size_bytes; /* Total bytes including this header */
  uint32_t flags; /* aerogpu_cmd_stream_flags */
  uint32_t reserved0;
  uint32_t reserved1;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_stream_header) == 24);

/* ------------------------------ Packet header ---------------------------- */

/*
 * Packet header used by all commands. Forward-compat rules:
 * - `size_bytes` includes this header.
 * - `size_bytes` must be >= sizeof(aerogpu_cmd_hdr) and 4-byte aligned.
 * - Unknown opcodes must be skipped using `size_bytes`.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_hdr {
  uint32_t opcode; /* enum aerogpu_cmd_opcode */
  uint32_t size_bytes;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_hdr) == 8);

/* ----------------------------- Common enums ------------------------------ */

enum aerogpu_cmd_opcode {
  AEROGPU_CMD_NOP = 0,
  AEROGPU_CMD_DEBUG_MARKER = 1, /* UTF-8 bytes follow */

  /* Resource / memory */
  AEROGPU_CMD_CREATE_BUFFER = 0x100,
  AEROGPU_CMD_CREATE_TEXTURE2D = 0x101,
  AEROGPU_CMD_DESTROY_RESOURCE = 0x102,
  AEROGPU_CMD_RESOURCE_DIRTY_RANGE = 0x103,

  /* Shaders */
  AEROGPU_CMD_CREATE_SHADER_DXBC = 0x200,
  AEROGPU_CMD_DESTROY_SHADER = 0x201,
  AEROGPU_CMD_BIND_SHADERS = 0x202,

  /* Pipeline state */
  AEROGPU_CMD_SET_BLEND_STATE = 0x300,
  AEROGPU_CMD_SET_DEPTH_STENCIL_STATE = 0x301,
  AEROGPU_CMD_SET_RASTERIZER_STATE = 0x302,

  /* Render targets + dynamic state */
  AEROGPU_CMD_SET_RENDER_TARGETS = 0x400,
  AEROGPU_CMD_SET_VIEWPORT = 0x401,
  AEROGPU_CMD_SET_SCISSOR = 0x402,

  /* Input assembler */
  AEROGPU_CMD_SET_VERTEX_BUFFERS = 0x500,
  AEROGPU_CMD_SET_INDEX_BUFFER = 0x501,

  /* Drawing */
  AEROGPU_CMD_CLEAR = 0x600,
  AEROGPU_CMD_DRAW = 0x601,
  AEROGPU_CMD_DRAW_INDEXED = 0x602,

  /* Presentation */
  AEROGPU_CMD_PRESENT = 0x700,
};

enum aerogpu_shader_stage {
  AEROGPU_SHADER_STAGE_VERTEX = 0,
  AEROGPU_SHADER_STAGE_PIXEL = 1,
  AEROGPU_SHADER_STAGE_COMPUTE = 2,
};

enum aerogpu_index_format {
  AEROGPU_INDEX_FORMAT_UINT16 = 0,
  AEROGPU_INDEX_FORMAT_UINT32 = 1,
};

/* --------------------------- Resource management ------------------------- */

enum aerogpu_resource_usage_flags {
  AEROGPU_RESOURCE_USAGE_NONE = 0,
  AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER = (1u << 0),
  AEROGPU_RESOURCE_USAGE_INDEX_BUFFER = (1u << 1),
  AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER = (1u << 2),
  AEROGPU_RESOURCE_USAGE_TEXTURE = (1u << 3),
  AEROGPU_RESOURCE_USAGE_RENDER_TARGET = (1u << 4),
  AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL = (1u << 5),
  AEROGPU_RESOURCE_USAGE_SCANOUT = (1u << 6),
};

/*
 * CREATE_BUFFER
 * - `backing_alloc_id` refers to an entry in the optional allocation table
 *   supplied with the submission.
 * - The host must validate that `backing_offset_bytes + size_bytes` is within
 *   the allocation's size.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_create_buffer {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_BUFFER */
  aerogpu_handle_t buffer_handle;
  uint32_t usage_flags; /* aerogpu_resource_usage_flags */
  uint64_t size_bytes;
  uint32_t backing_alloc_id; /* 0 = none (host allocated) */
  uint32_t backing_offset_bytes;
  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_buffer) == 40);

/*
 * CREATE_TEXTURE2D
 * - Textures are linear in guest memory when backed by an allocation.
 * - `row_pitch_bytes` is required when `backing_alloc_id != 0`.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_create_texture2d {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_TEXTURE2D */
  aerogpu_handle_t texture_handle;
  uint32_t usage_flags; /* aerogpu_resource_usage_flags */
  uint32_t format; /* enum aerogpu_format */
  uint32_t width;
  uint32_t height;
  uint32_t mip_levels; /* >= 1 */
  uint32_t array_layers; /* >= 1 */
  uint32_t row_pitch_bytes;
  uint32_t backing_alloc_id; /* 0 = none (host allocated) */
  uint32_t backing_offset_bytes;
  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_texture2d) == 56);

#pragma pack(push, 1)
struct aerogpu_cmd_destroy_resource {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_RESOURCE */
  aerogpu_handle_t resource_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_resource) == 16);

/*
 * RESOURCE_DIRTY_RANGE:
 * Notifies the host that a CPU write has modified the backing memory for a
 * resource. The host should re-upload the dirty range before the resource is
 * consumed by subsequent commands.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_resource_dirty_range {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_RESOURCE_DIRTY_RANGE */
  aerogpu_handle_t resource_handle;
  uint32_t reserved0;
  uint64_t offset_bytes;
  uint64_t size_bytes;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_resource_dirty_range) == 32);

/* -------------------------------- Shaders -------------------------------- */

/*
 * CREATE_SHADER_DXBC:
 * Payload format:
 *   struct aerogpu_cmd_create_shader_dxbc
 *   uint8_t dxbc_bytes[dxbc_size_bytes]
 *   padding to 4-byte alignment
 */
#pragma pack(push, 1)
struct aerogpu_cmd_create_shader_dxbc {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_SHADER_DXBC */
  aerogpu_handle_t shader_handle;
  uint32_t stage; /* enum aerogpu_shader_stage */
  uint32_t dxbc_size_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_shader_dxbc) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_destroy_shader {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_SHADER */
  aerogpu_handle_t shader_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_shader) == 16);

#pragma pack(push, 1)
struct aerogpu_cmd_bind_shaders {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_BIND_SHADERS */
  aerogpu_handle_t vs; /* 0 = unbound */
  aerogpu_handle_t ps; /* 0 = unbound */
  aerogpu_handle_t cs; /* 0 = unbound */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_bind_shaders) == 24);

/* ------------------------------ Pipeline state --------------------------- */

enum aerogpu_blend_factor {
  AEROGPU_BLEND_ZERO = 0,
  AEROGPU_BLEND_ONE = 1,
  AEROGPU_BLEND_SRC_ALPHA = 2,
  AEROGPU_BLEND_INV_SRC_ALPHA = 3,
  AEROGPU_BLEND_DEST_ALPHA = 4,
  AEROGPU_BLEND_INV_DEST_ALPHA = 5,
};

enum aerogpu_blend_op {
  AEROGPU_BLEND_OP_ADD = 0,
  AEROGPU_BLEND_OP_SUBTRACT = 1,
  AEROGPU_BLEND_OP_REV_SUBTRACT = 2,
  AEROGPU_BLEND_OP_MIN = 3,
  AEROGPU_BLEND_OP_MAX = 4,
};

#pragma pack(push, 1)
struct aerogpu_blend_state {
  uint32_t enable; /* 0/1 */
  uint32_t src_factor; /* aerogpu_blend_factor */
  uint32_t dst_factor; /* aerogpu_blend_factor */
  uint32_t blend_op; /* aerogpu_blend_op */
  uint8_t color_write_mask; /* bit0=R bit1=G bit2=B bit3=A */
  uint8_t reserved0[3];
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_blend_state) == 20);

#pragma pack(push, 1)
struct aerogpu_cmd_set_blend_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_BLEND_STATE */
  struct aerogpu_blend_state state;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_blend_state) == 28);

enum aerogpu_compare_func {
  AEROGPU_COMPARE_NEVER = 0,
  AEROGPU_COMPARE_LESS = 1,
  AEROGPU_COMPARE_EQUAL = 2,
  AEROGPU_COMPARE_LESS_EQUAL = 3,
  AEROGPU_COMPARE_GREATER = 4,
  AEROGPU_COMPARE_NOT_EQUAL = 5,
  AEROGPU_COMPARE_GREATER_EQUAL = 6,
  AEROGPU_COMPARE_ALWAYS = 7,
};

#pragma pack(push, 1)
struct aerogpu_depth_stencil_state {
  uint32_t depth_enable; /* 0/1 */
  uint32_t depth_write_enable; /* 0/1 */
  uint32_t depth_func; /* aerogpu_compare_func */
  uint32_t stencil_enable; /* 0/1 */
  uint8_t stencil_read_mask;
  uint8_t stencil_write_mask;
  uint8_t reserved0[2];
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_depth_stencil_state) == 20);

#pragma pack(push, 1)
struct aerogpu_cmd_set_depth_stencil_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_DEPTH_STENCIL_STATE */
  struct aerogpu_depth_stencil_state state;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_depth_stencil_state) == 28);

enum aerogpu_fill_mode {
  AEROGPU_FILL_SOLID = 0,
  AEROGPU_FILL_WIREFRAME = 1,
};

enum aerogpu_cull_mode {
  AEROGPU_CULL_NONE = 0,
  AEROGPU_CULL_FRONT = 1,
  AEROGPU_CULL_BACK = 2,
};

#pragma pack(push, 1)
struct aerogpu_rasterizer_state {
  uint32_t fill_mode; /* aerogpu_fill_mode */
  uint32_t cull_mode; /* aerogpu_cull_mode */
  uint32_t front_ccw; /* 0/1 */
  uint32_t scissor_enable; /* 0/1 */
  int32_t depth_bias;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_rasterizer_state) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_set_rasterizer_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_RASTERIZER_STATE */
  struct aerogpu_rasterizer_state state;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_rasterizer_state) == 32);

/* -------------------------- Render targets / state ----------------------- */

#define AEROGPU_MAX_RENDER_TARGETS 8u

#pragma pack(push, 1)
struct aerogpu_cmd_set_render_targets {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_RENDER_TARGETS */
  uint32_t color_count; /* 0..AEROGPU_MAX_RENDER_TARGETS */
  aerogpu_handle_t depth_stencil; /* 0 = none */
  aerogpu_handle_t colors[AEROGPU_MAX_RENDER_TARGETS]; /* unused entries = 0 */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_render_targets) == 48);

/*
 * Viewport uses IEEE-754 float bits (little-endian).
 * D3D9-style viewport is supported (x/y/width/height/min_depth/max_depth).
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_viewport {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_VIEWPORT */
  uint32_t x_f32;
  uint32_t y_f32;
  uint32_t width_f32;
  uint32_t height_f32;
  uint32_t min_depth_f32;
  uint32_t max_depth_f32;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_viewport) == 32);

#pragma pack(push, 1)
struct aerogpu_cmd_set_scissor {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SCISSOR */
  int32_t x;
  int32_t y;
  int32_t width;
  int32_t height;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_scissor) == 24);

/* ------------------------------ Input assembler -------------------------- */

#pragma pack(push, 1)
struct aerogpu_vertex_buffer_binding {
  aerogpu_handle_t buffer;
  uint32_t stride_bytes;
  uint32_t offset_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_vertex_buffer_binding) == 16);

/*
 * SET_VERTEX_BUFFERS:
 * Payload format:
 *   struct aerogpu_cmd_set_vertex_buffers
 *   struct aerogpu_vertex_buffer_binding bindings[buffer_count]
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_vertex_buffers {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_VERTEX_BUFFERS */
  uint32_t start_slot;
  uint32_t buffer_count;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_vertex_buffers) == 16);

#pragma pack(push, 1)
struct aerogpu_cmd_set_index_buffer {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_INDEX_BUFFER */
  aerogpu_handle_t buffer;
  uint32_t format; /* aerogpu_index_format */
  uint32_t offset_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_index_buffer) == 24);

/* -------------------------------- Drawing -------------------------------- */

enum aerogpu_clear_flags {
  AEROGPU_CLEAR_COLOR = (1u << 0),
  AEROGPU_CLEAR_DEPTH = (1u << 1),
  AEROGPU_CLEAR_STENCIL = (1u << 2),
};

#pragma pack(push, 1)
struct aerogpu_cmd_clear {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CLEAR */
  uint32_t flags; /* aerogpu_clear_flags */
  uint32_t color_rgba_f32[4];
  uint32_t depth_f32;
  uint32_t stencil;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_clear) == 36);

#pragma pack(push, 1)
struct aerogpu_cmd_draw {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DRAW */
  uint32_t vertex_count;
  uint32_t instance_count;
  uint32_t first_vertex;
  uint32_t first_instance;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_draw) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_draw_indexed {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DRAW_INDEXED */
  uint32_t index_count;
  uint32_t instance_count;
  uint32_t first_index;
  int32_t base_vertex;
  uint32_t first_instance;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_draw_indexed) == 28);

/* ------------------------------ Presentation ----------------------------- */

enum aerogpu_present_flags {
  AEROGPU_PRESENT_FLAG_NONE = 0,
  AEROGPU_PRESENT_FLAG_VSYNC = (1u << 0),
};

/*
 * PRESENT:
 * - The device presents Scanout0 using the configuration registers programmed
 *   via MMIO (SCANOUT0_*).
 * - For double-buffering page flips, the driver may update SCANOUT0_FB_GPA_*
 *   before emitting PRESENT.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_present {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_PRESENT */
  uint32_t scanout_id; /* 0 for now */
  uint32_t flags; /* aerogpu_present_flags */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_present) == 16);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_CMD_H_ */
