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

/*
 * Driver-defined handle IDs used by the command stream.
 *
 * The host treats these handles as living in a single global namespace (across
 * all submission contexts). Guest drivers must therefore allocate handles that
 * are unique across the entire guest (multi-process), not just within one
 * process.
 */
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
  uint32_t size_bytes; /* Total bytes including this header (<= cmd_size_bytes; 4-byte aligned; trailing bytes ignored) */
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
  AEROGPU_CMD_UPLOAD_RESOURCE = 0x104,
  /* Requires AEROGPU_FEATURE_TRANSFER (introduced in ABI 1.1). */
  AEROGPU_CMD_COPY_BUFFER = 0x105,
  /* Requires AEROGPU_FEATURE_TRANSFER (introduced in ABI 1.1). */
  AEROGPU_CMD_COPY_TEXTURE2D = 0x106,
  /*
   * Creates a texture view (subresource selection / format reinterpretation).
   *
   * This is optional and may not be supported by all hosts. When unsupported,
   * guest drivers should bind base texture handles directly (legacy behavior).
   */
  AEROGPU_CMD_CREATE_TEXTURE_VIEW = 0x107,
  AEROGPU_CMD_DESTROY_TEXTURE_VIEW = 0x108,

  /* Shaders */
  AEROGPU_CMD_CREATE_SHADER_DXBC = 0x200,
  AEROGPU_CMD_DESTROY_SHADER = 0x201,
  AEROGPU_CMD_BIND_SHADERS = 0x202,

  /* D3D9-style shader constant updates (float4 registers). */
  AEROGPU_CMD_SET_SHADER_CONSTANTS_F = 0x203,

  /* D3D9 vertex declaration / D3D10+ input layout blob (opaque to protocol). */
  AEROGPU_CMD_CREATE_INPUT_LAYOUT = 0x204,
  AEROGPU_CMD_DESTROY_INPUT_LAYOUT = 0x205,
  AEROGPU_CMD_SET_INPUT_LAYOUT = 0x206,

  /* D3D9-style shader constant updates (int4 registers). */
  AEROGPU_CMD_SET_SHADER_CONSTANTS_I = 0x207,
  /* D3D9-style shader constant updates (bool registers). */
  AEROGPU_CMD_SET_SHADER_CONSTANTS_B = 0x208,

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
  AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY = 0x502,

  /* Resource binding / state (initially D3D9-centric; can be generalized). */
  AEROGPU_CMD_SET_TEXTURE = 0x510,
  AEROGPU_CMD_SET_SAMPLER_STATE = 0x511,
  AEROGPU_CMD_SET_RENDER_STATE = 0x512,

  /* D3D10/11-style binding tables (FL10_0 baseline). */
  AEROGPU_CMD_CREATE_SAMPLER = 0x520,
  AEROGPU_CMD_DESTROY_SAMPLER = 0x521,
  AEROGPU_CMD_SET_SAMPLERS = 0x522,
  AEROGPU_CMD_SET_CONSTANT_BUFFERS = 0x523,
  /* D3D11-style buffer SRV table binding (t# where SRV is a buffer view). */
  AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS = 0x524,
  /* D3D11-style UAV table binding for buffers (u#). */
  AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS = 0x525,

  /* Drawing */
  AEROGPU_CMD_CLEAR = 0x600,
  AEROGPU_CMD_DRAW = 0x601,
  AEROGPU_CMD_DRAW_INDEXED = 0x602,
  /* Compute dispatch. */
  AEROGPU_CMD_DISPATCH = 0x603,

  /* Presentation */
  AEROGPU_CMD_PRESENT = 0x700,
  /* D3D9Ex-style presentation (PresentEx flags, etc). */
  AEROGPU_CMD_PRESENT_EX = 0x701,

  /* D3D9Ex/DWM shared surface interop. */
  AEROGPU_CMD_EXPORT_SHARED_SURFACE = 0x710,
  AEROGPU_CMD_IMPORT_SHARED_SURFACE = 0x711,
  /*
   * End-of-life signal for a shared surface token (emitted by the Win7 KMD once
   * the final per-process allocation wrapper is released).
   */
  AEROGPU_CMD_RELEASE_SHARED_SURFACE = 0x712,

  /* Explicit flush point (may be a no-op on some hosts). */
  AEROGPU_CMD_FLUSH = 0x720,
};

enum aerogpu_shader_stage {
  AEROGPU_SHADER_STAGE_VERTEX = 0,
  AEROGPU_SHADER_STAGE_PIXEL = 1,
  AEROGPU_SHADER_STAGE_COMPUTE = 2,
  /* D3D10+ geometry shader stage. */
  AEROGPU_SHADER_STAGE_GEOMETRY = 3,
};

/*
 * Minimum command-stream ABI minor version that enables the `stage_ex` encoding.
 *
 * Hosts must ignore `reserved0` as `stage_ex` when decoding a command stream whose header reports
 * `abi_minor < AEROGPU_STAGE_EX_MIN_ABI_MINOR` to avoid misinterpreting legacy reserved data.
 *
 * Introduced in ABI 1.3.
 */
#define AEROGPU_STAGE_EX_MIN_ABI_MINOR 3u

/*
 * Extended shader stage encoding (`stage_ex`).
 *
 * Some packets contain a `shader_stage` (or `stage`) field whose base enum supports VS/PS/CS (+ GS).
 * To represent additional D3D10+ stages (HS/DS) without changing packet layouts, when
 * `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` the packet's `reserved0` field is repurposed as a
 * `stage_ex` override. If `shader_stage != COMPUTE`, `reserved0` MUST be 0 and is ignored.
 *
 * This extension is only valid for command streams with ABI minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR.
 * For older command streams, `reserved0` must be treated as reserved and ignored even if non-zero.
 *
 * Canonical rules:
 * - `reserved0 == 0` means "no stage_ex override" and MUST be interpreted as the legacy Compute stage
 *   (older guests always wrote 0 into reserved fields).
 * - Non-zero `reserved0` values are interpreted as `enum aerogpu_shader_stage_ex`.
 *
 * Note: GS is representable either via the legacy stage enum (`shader_stage = AEROGPU_SHADER_STAGE_GEOMETRY`,
 * `reserved0 = 0`) or via `stage_ex` (`shader_stage = COMPUTE`, `reserved0 = GEOMETRY`). The `stage_ex`
 * mechanism is primarily required for non-legacy stages like HS/DS.
 *
 * Numeric values intentionally match the D3D DXBC "program type" numbers used in
 * the shader version token:
 *   Pixel=0, Vertex=1, Geometry=2, Hull=3, Domain=4, Compute=5.
 *
 * `stage_ex` can only represent the non-legacy stages because:
 * - `reserved0 == 0` is reserved for "no override" (legacy Compute), so `stage_ex`
 *   cannot encode Pixel (0), and
 * - Vertex (1) must be encoded via the legacy `shader_stage = VERTEX` for clarity;
 *   `reserved0 == 1` is intentionally invalid and must be rejected by decoders.
 *
 * `AEROGPU_SHADER_STAGE_EX_COMPUTE` (5) is accepted by decoders and treated the
 * same as "no override" (Compute). Writers should emit 0 for Compute to preserve
 * legacy packet semantics.
 */
enum aerogpu_shader_stage_ex {
  /* 0 = no stage_ex override (legacy Compute). */
  AEROGPU_SHADER_STAGE_EX_NONE = 0,
  AEROGPU_SHADER_STAGE_EX_GEOMETRY = 2,
  AEROGPU_SHADER_STAGE_EX_HULL = 3,
  AEROGPU_SHADER_STAGE_EX_DOMAIN = 4,
  /* Optional alias for Compute (see above). */
  AEROGPU_SHADER_STAGE_EX_COMPUTE = 5,
};

enum aerogpu_index_format {
  AEROGPU_INDEX_FORMAT_UINT16 = 0,
  AEROGPU_INDEX_FORMAT_UINT32 = 1,
};

enum aerogpu_sampler_filter {
  AEROGPU_SAMPLER_FILTER_NEAREST = 0,
  AEROGPU_SAMPLER_FILTER_LINEAR = 1,
};

enum aerogpu_sampler_address_mode {
  AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE = 0,
  AEROGPU_SAMPLER_ADDRESS_REPEAT = 1,
  AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT = 2,
};

enum aerogpu_primitive_topology {
  AEROGPU_TOPOLOGY_POINTLIST = 1,
  AEROGPU_TOPOLOGY_LINELIST = 2,
  AEROGPU_TOPOLOGY_LINESTRIP = 3,
  AEROGPU_TOPOLOGY_TRIANGLELIST = 4,
  AEROGPU_TOPOLOGY_TRIANGLESTRIP = 5,
  AEROGPU_TOPOLOGY_TRIANGLEFAN = 6,

  /* D3D10/11 adjacency topologies (used by geometry shaders; require expansion/emulation). */
  AEROGPU_TOPOLOGY_LINELIST_ADJ = 10,
  AEROGPU_TOPOLOGY_LINESTRIP_ADJ = 11,
  AEROGPU_TOPOLOGY_TRIANGLELIST_ADJ = 12,
  AEROGPU_TOPOLOGY_TRIANGLESTRIP_ADJ = 13,

  /* D3D11 patchlist topologies (1..32 control points; used by tessellation HS/DS). */
  AEROGPU_TOPOLOGY_PATCHLIST_1 = 33,
  AEROGPU_TOPOLOGY_PATCHLIST_2 = 34,
  AEROGPU_TOPOLOGY_PATCHLIST_3 = 35,
  AEROGPU_TOPOLOGY_PATCHLIST_4 = 36,
  AEROGPU_TOPOLOGY_PATCHLIST_5 = 37,
  AEROGPU_TOPOLOGY_PATCHLIST_6 = 38,
  AEROGPU_TOPOLOGY_PATCHLIST_7 = 39,
  AEROGPU_TOPOLOGY_PATCHLIST_8 = 40,
  AEROGPU_TOPOLOGY_PATCHLIST_9 = 41,
  AEROGPU_TOPOLOGY_PATCHLIST_10 = 42,
  AEROGPU_TOPOLOGY_PATCHLIST_11 = 43,
  AEROGPU_TOPOLOGY_PATCHLIST_12 = 44,
  AEROGPU_TOPOLOGY_PATCHLIST_13 = 45,
  AEROGPU_TOPOLOGY_PATCHLIST_14 = 46,
  AEROGPU_TOPOLOGY_PATCHLIST_15 = 47,
  AEROGPU_TOPOLOGY_PATCHLIST_16 = 48,
  AEROGPU_TOPOLOGY_PATCHLIST_17 = 49,
  AEROGPU_TOPOLOGY_PATCHLIST_18 = 50,
  AEROGPU_TOPOLOGY_PATCHLIST_19 = 51,
  AEROGPU_TOPOLOGY_PATCHLIST_20 = 52,
  AEROGPU_TOPOLOGY_PATCHLIST_21 = 53,
  AEROGPU_TOPOLOGY_PATCHLIST_22 = 54,
  AEROGPU_TOPOLOGY_PATCHLIST_23 = 55,
  AEROGPU_TOPOLOGY_PATCHLIST_24 = 56,
  AEROGPU_TOPOLOGY_PATCHLIST_25 = 57,
  AEROGPU_TOPOLOGY_PATCHLIST_26 = 58,
  AEROGPU_TOPOLOGY_PATCHLIST_27 = 59,
  AEROGPU_TOPOLOGY_PATCHLIST_28 = 60,
  AEROGPU_TOPOLOGY_PATCHLIST_29 = 61,
  AEROGPU_TOPOLOGY_PATCHLIST_30 = 62,
  AEROGPU_TOPOLOGY_PATCHLIST_31 = 63,
  AEROGPU_TOPOLOGY_PATCHLIST_32 = 64,
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
  /* Storage binding usage (WebGPU STORAGE / STORAGE_BINDING; SRV/UAV buffers). */
  AEROGPU_RESOURCE_USAGE_STORAGE = (1u << 7),
};

/*
 * Copy / transfer command flags.
 *
 * If AEROGPU_COPY_FLAG_WRITEBACK_DST is set, and the destination resource is
 * backed by a guest allocation, the host MUST write the resulting bytes into
 * the guest backing memory before signaling the submission fence.
 *
 * This requires the submission to provide an alloc-table entry for the
 * destination resource's `backing_alloc_id` (as specified by its CREATE_* packet)
 * so the host can resolve the guest physical address range to write.
 *
 * If the destination resource has no guest backing allocation, the host should
 * treat this as a validation error (recommended) so drivers don't get silent
 * failures.
 */
enum aerogpu_copy_flags {
  AEROGPU_COPY_FLAG_NONE = 0,
  AEROGPU_COPY_FLAG_WRITEBACK_DST = (1u << 0),
};

/*
 * CREATE_BUFFER
 * - `backing_alloc_id` identifies the guest memory backing for this resource.
 *   If non-zero, this is a stable per-allocation ID (`alloc_id`) key into the
 *   submission's allocation table (see `struct aerogpu_alloc_table_header` /
 *   `aerogpu_ring.h`).
 *   - It is **not** an array index; allocation tables may be re-ordered between
 *     submissions.
 *
 *   - `backing_alloc_id == 0` means the resource is host-allocated (no guest
 *     backing memory and therefore no alloc-table entry).
 *   - `backing_alloc_id != 0` requires the submission to provide an allocation
 *     table entry for that alloc_id so the host can resolve the guest physical
 *     pages.
 *
 *   Win7/WDDM UMDs typically source `alloc_id` from the per-allocation private
 *   driver data blob (`aerogpu_wddm_alloc_priv` in `aerogpu_wddm_alloc.h`), which
 *   the KMD copies into `DXGK_ALLOCATION::AllocationId` and then uses to build
 *   the alloc table sideband for each submission.
 * - The host must validate that `backing_offset_bytes + size_bytes` is within
 *   the allocation's size.
 * - `size_bytes` must be a multiple of 4 (WebGPU `COPY_BUFFER_ALIGNMENT`).
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
 * - For block-compressed (BC*) formats, `row_pitch_bytes` is measured in bytes
 *   per row of blocks (not per row of pixels). I.e. it is the stride between
 *   consecutive rows of 4x4 blocks in the backing allocation.
 * - Unknown `format` values MUST be treated as invalid.
 * - `backing_alloc_id` follows the same `alloc_id` resolution rules as
 *   CREATE_BUFFER.
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

/*
 * CREATE_TEXTURE_VIEW
 * - Creates a view `view_handle` into an existing `texture_handle`.
 * - Views select a subresource range:
 *   - mip range: [base_mip_level, base_mip_level + mip_level_count)
 *   - array range: [base_array_layer, base_array_layer + array_layer_count)
 * - `format` allows format reinterpretation (must be compatible with the base texture).
 * - The view handle lives in the same global handle namespace as other resources.
 * - The host may treat the view as usable for both sampling and render-target binding.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_create_texture_view {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_TEXTURE_VIEW */
  aerogpu_handle_t view_handle;
  aerogpu_handle_t texture_handle;
  uint32_t format; /* enum aerogpu_format */
  uint32_t base_mip_level;
  uint32_t mip_level_count;
  uint32_t base_array_layer;
  uint32_t array_layer_count;
  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_texture_view) == 44);

#pragma pack(push, 1)
struct aerogpu_cmd_destroy_resource {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_RESOURCE */
  aerogpu_handle_t resource_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_resource) == 16);

/*
 * DESTROY_TEXTURE_VIEW
 * - Destroys a previously created texture view.
 * - Must be idempotent: destroying an already-destroyed/unknown handle is a no-op.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_destroy_texture_view {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_TEXTURE_VIEW */
  aerogpu_handle_t view_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_texture_view) == 16);

/*
 * RESOURCE_DIRTY_RANGE:
 * Notifies the host that a CPU write has modified the guest backing memory for
 * a resource. The host should re-upload the dirty range from guest memory
 * before the resource is consumed by subsequent commands.
 *
 * This is only meaningful for guest-backed resources (`backing_alloc_id != 0`).
 * Host-owned resources (`backing_alloc_id == 0`) should be updated via
 * `UPLOAD_RESOURCE` instead.
 *
 * If the resource is guest-backed, the submission must provide an alloc-table
 * entry for that allocation ID so the host can resolve the guest physical
 * address range for the dirty bytes.
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

/*
 * UPLOAD_RESOURCE:
 * Copies raw bytes into a resource.
 *
 * Notes:
 * - For buffers, `offset_bytes` and `size_bytes` must be multiples of 4 (WebGPU
 *   `COPY_BUFFER_ALIGNMENT`).
 *
 * Payload format:
 *   struct aerogpu_cmd_upload_resource
 *   uint8_t data[size_bytes]
 *   padding to 4-byte alignment
 *
 * This is primarily intended for bring-up / system-memory-backed resources
 * where the emulator/host does not have direct access to the guest allocation.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_upload_resource {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_UPLOAD_RESOURCE */
  aerogpu_handle_t resource_handle;
  uint32_t reserved0;
  uint64_t offset_bytes;
  uint64_t size_bytes;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_upload_resource) == 32);

/*
 * COPY_BUFFER
 * - Source and destination resources must be buffers.
 * - Ranges must be in-bounds:
 *     dst_offset_bytes + size_bytes <= dst_buffer.size_bytes
 *     src_offset_bytes + size_bytes <= src_buffer.size_bytes
 * - Offsets and size must be multiples of 4 (WebGPU `COPY_BUFFER_ALIGNMENT`).
 * - If AEROGPU_COPY_FLAG_WRITEBACK_DST is set:
 *   - dst_buffer MUST be backed by a guest allocation.
 *   - The host MUST write back the resulting bytes into the guest backing
 *     memory before signaling the submission fence.
 *   - The submission must provide an alloc-table entry for that allocation ID.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_copy_buffer {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_COPY_BUFFER */
  aerogpu_handle_t dst_buffer;
  aerogpu_handle_t src_buffer;
  uint64_t dst_offset_bytes;
  uint64_t src_offset_bytes;
  uint64_t size_bytes;
  uint32_t flags; /* aerogpu_copy_flags */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_copy_buffer) == 48);

/*
 * COPY_TEXTURE2D
 * - Source and destination resources must be texture2d.
 * - Formats must match.
 * - Subresource indices must be valid:
 *     dst_mip_level < dst_texture.mip_levels
 *     dst_array_layer < dst_texture.array_layers
 *     src_mip_level < src_texture.mip_levels
 *     src_array_layer < src_texture.array_layers
 * - Copy rectangle must be in-bounds of both subresources.
 * - If AEROGPU_COPY_FLAG_WRITEBACK_DST is set:
 *   - dst_texture MUST be backed by a guest allocation.
 *   - The host MUST write back the resulting bytes into the guest backing
 *     memory before signaling the submission fence.
 *   - The submission must provide an alloc-table entry for that allocation ID.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_copy_texture2d {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_COPY_TEXTURE2D */
  aerogpu_handle_t dst_texture;
  aerogpu_handle_t src_texture;
  uint32_t dst_mip_level;
  uint32_t dst_array_layer;
  uint32_t src_mip_level;
  uint32_t src_array_layer;
  uint32_t dst_x;
  uint32_t dst_y;
  uint32_t src_x;
  uint32_t src_y;
  uint32_t width;
  uint32_t height;
  uint32_t flags; /* aerogpu_copy_flags */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_copy_texture2d) == 64);

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
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is treated
   *   as `enum aerogpu_shader_stage_ex` (DXBC program type numbering), allowing the guest to create
   *   a GS/HS/DS shader without adding new fields.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t stage; /* enum aerogpu_shader_stage */
  uint32_t dxbc_size_bytes;
  /*
   * stage_ex: enum aerogpu_shader_stage_ex
   *
   * Used by the "stage_ex" ABI extension to represent additional DXBC program types (GS/HS/DS)
   * without extending the legacy `stage` enum.
   *
   * Encoding:
   * - Legacy: `stage = VERTEX/PIXEL/GEOMETRY/COMPUTE` and `stage_ex = 0`.
   * - Stage-ex: set `stage = COMPUTE` and set `stage_ex` to a non-zero DXBC program type:
   *   - GS: stage_ex = GEOMETRY (2) (alternative to legacy `stage = GEOMETRY` where supported)
   *   - HS: stage_ex = HULL     (3)
   *   - DS: stage_ex = DOMAIN   (4)
   *
   * Note: `stage_ex == 0` is reserved for legacy/default (old guests always write 0 into reserved
   * fields). As a result, DXBC `stage_ex == 0` (Pixel) is not encodable here; pixel shaders must
   * use the legacy `stage = PIXEL` encoding.
   */
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

/*
 * BIND_SHADERS:
 *
 * Base packet layout is a packed 24-byte prefix (this struct). This prefix is stable and
 * MUST NOT change.
 *
 * Legacy behavior (24-byte packet):
 * - When `hdr.size_bytes == 24` and `reserved0 != 0`, `reserved0` is interpreted as the
 *   geometry shader (`gs`) handle.
 *
 * ABI extension (append-only):
 * - If `hdr.size_bytes >= 36`, the packet appends 3 additional `aerogpu_handle_t` shader
 *   handles in this order:
 *     - `gs` (geometry shader) 0 = unbound
 *     - `hs` (hull shader / tessellation control) 0 = unbound
 *     - `ds` (domain shader / tessellation eval) 0 = unbound
 * - When appended handles are present, they are authoritative; `reserved0` is reserved/ignored
 *   (and emitters SHOULD set it to 0).
 *
 * Forward-compat notes for `reserved0`:
 * - `reserved0` remains reserved and emitters SHOULD set it to 0 for the extended packet (unless
 *   mirroring `gs` for best-effort compatibility with legacy hosts).
 * - Legacy implementations may interpret a non-zero `reserved0` as the geometry shader (`gs`)
 *   handle; for best-effort compatibility an emitter MAY duplicate `gs` into `reserved0`. If it
 *   does so, it SHOULD match the appended `gs` field, but when present, the appended `{gs,hs,ds}`
 *   fields are authoritative.
 *
 * Any bytes beyond the appended `{gs,hs,ds}` handles are reserved for future extension and MUST
 * be ignored by readers.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_bind_shaders {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_BIND_SHADERS */
  aerogpu_handle_t vs; /* 0 = unbound */
  aerogpu_handle_t ps; /* 0 = unbound */
  aerogpu_handle_t cs; /* 0 = unbound */
  /*
   * Reserved for ABI forward-compat.
   *
   * Legacy behavior (24-byte packet):
   * - When `hdr.size_bytes == 24` and `reserved0 != 0`, `reserved0` is interpreted as the
   *   geometry shader (`gs`) handle.
   *
   * ABI extension (append-only):
   * - Decoders MUST treat `hdr.size_bytes` as a minimum size and ignore any trailing bytes they do
   *   not understand.
   * - If `hdr.size_bytes >= sizeof(struct aerogpu_cmd_bind_shaders) + 12` (36 bytes), three
   *   additional u32 shader handles are appended immediately after this struct: `{gs, hs, ds}`.
   * - In the extended form, hosts should prefer the appended handles. Writers may also mirror `gs`
   *   into `reserved0` for best-effort support on hosts that only understand the 24-byte packet.
   *   If mirrored, it SHOULD match the appended `gs` field, but the appended handles are
   *   authoritative.
   */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_bind_shaders) == 24);

/*
 * SET_SHADER_CONSTANTS_F:
 * D3D9-style float4 constants.
 *
 * Payload format:
 *   struct aerogpu_cmd_set_shader_constants_f
 *   float data[vec4_count * 4]
 *   padding to 4-byte alignment
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_shader_constants_f {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SHADER_CONSTANTS_F */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is treated
   *   as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4 correspond to
   *   GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t stage; /* enum aerogpu_shader_stage */
  uint32_t start_register;
  uint32_t vec4_count;
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when stage==AEROGPU_SHADER_STAGE_COMPUTE */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_shader_constants_f) == 24);

/*
 * SET_SHADER_CONSTANTS_I:
 * D3D9-style int4 constants.
 *
 * Payload format:
 *   struct aerogpu_cmd_set_shader_constants_i
 *   int32_t data[vec4_count * 4] (little-endian)
 *   padding to 4-byte alignment
 *
 * Forward-compat: Readers MUST treat `hdr.size_bytes` as a minimum and ignore any trailing bytes
 * they do not understand.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_shader_constants_i {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SHADER_CONSTANTS_I */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is treated
   *   as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4 correspond to
   *   GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t stage; /* enum aerogpu_shader_stage */
  uint32_t start_register;
  uint32_t vec4_count;
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when stage==AEROGPU_SHADER_STAGE_COMPUTE */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_shader_constants_i) == 24);

/*
 * SET_SHADER_CONSTANTS_B:
 * D3D9-style bool constants.
 *
 * Payload format:
 *   struct aerogpu_cmd_set_shader_constants_b
 *   uint32_t data[bool_count * 4] (little-endian), where `bool_count` counts registers
 *
 * Each bool register is encoded as a `vec4<u32>` (16 bytes per register). Writers should
 * replicate the scalar bool value across all 4 lanes (canonical writer behavior).
 *
 * Readers MUST treat any non-zero lane value as "true". Writers SHOULD normalize to
 * 0/1 to preserve canonical encoding.
 *   padding to 4-byte alignment
 *
 * Forward-compat: Readers MUST treat `hdr.size_bytes` as a minimum and ignore any trailing bytes
 * they do not understand.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_shader_constants_b {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SHADER_CONSTANTS_B */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is treated
   *   as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4 correspond to
   *   GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t stage; /* enum aerogpu_shader_stage */
  uint32_t start_register;
  uint32_t bool_count;
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when stage==AEROGPU_SHADER_STAGE_COMPUTE */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_shader_constants_b) == 24);

/*
 * CREATE_INPUT_LAYOUT:
 * Opaque blob that describes the vertex input layout.
 *
 * For D3D10/11 UMDs, the recommended blob format is:
 *   struct aerogpu_input_layout_blob_header
 *   struct aerogpu_input_layout_element_dxgi elements[element_count]
 *
 * D3D9 UMDs may instead upload a raw D3D9 vertex declaration token stream.
 * Consumers should discriminate blob types using the header magic.
 *
 * Payload format:
 *   struct aerogpu_cmd_create_input_layout
 *   uint8_t blob[blob_size_bytes]
 *   padding to 4-byte alignment
 */

#define AEROGPU_INPUT_LAYOUT_BLOB_MAGIC 0x59414C49u /* "ILAY" little-endian */
#define AEROGPU_INPUT_LAYOUT_BLOB_VERSION 1u

#pragma pack(push, 1)
struct aerogpu_input_layout_blob_header {
  uint32_t magic; /* AEROGPU_INPUT_LAYOUT_BLOB_MAGIC */
  uint32_t version; /* AEROGPU_INPUT_LAYOUT_BLOB_VERSION */
  uint32_t element_count;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_input_layout_blob_header) == 16);

/*
 * D3D10/11-style input element. Fields intentionally mirror D3D11_INPUT_ELEMENT_DESC
 * (but with the semantic name represented as a 32-bit FNV-1a hash).
 *
 * `dxgi_format` is the numeric value of DXGI_FORMAT (to avoid duplicating DXGI enums
 * in the protocol).
 */
#pragma pack(push, 1)
struct aerogpu_input_layout_element_dxgi {
  uint32_t semantic_name_hash; /* FNV-1a hash of ASCII uppercase semantic name */
  uint32_t semantic_index;
  uint32_t dxgi_format; /* DXGI_FORMAT numeric */
  uint32_t input_slot;
  uint32_t aligned_byte_offset;
  uint32_t input_slot_class; /* 0: per-vertex, 1: per-instance */
  uint32_t instance_data_step_rate;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_input_layout_element_dxgi) == 28);

#pragma pack(push, 1)
struct aerogpu_cmd_create_input_layout {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_INPUT_LAYOUT */
  aerogpu_handle_t input_layout_handle;
  uint32_t blob_size_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_input_layout) == 20);

#pragma pack(push, 1)
struct aerogpu_cmd_destroy_input_layout {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_INPUT_LAYOUT */
  aerogpu_handle_t input_layout_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_input_layout) == 16);

#pragma pack(push, 1)
struct aerogpu_cmd_set_input_layout {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_INPUT_LAYOUT */
  aerogpu_handle_t input_layout_handle; /* 0 = unbind */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_input_layout) == 16);

/* ------------------------------ Pipeline state --------------------------- */

enum aerogpu_blend_factor {
  AEROGPU_BLEND_ZERO = 0,
  AEROGPU_BLEND_ONE = 1,
  AEROGPU_BLEND_SRC_ALPHA = 2,
  AEROGPU_BLEND_INV_SRC_ALPHA = 3,
  AEROGPU_BLEND_DEST_ALPHA = 4,
  AEROGPU_BLEND_INV_DEST_ALPHA = 5,
  AEROGPU_BLEND_CONSTANT = 6,
  AEROGPU_BLEND_INV_CONSTANT = 7,
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
  uint32_t src_factor_alpha; /* aerogpu_blend_factor */
  uint32_t dst_factor_alpha; /* aerogpu_blend_factor */
  uint32_t blend_op_alpha; /* aerogpu_blend_op */
  uint32_t blend_constant_rgba_f32[4]; /* IEEE-754 float bits */
  uint32_t sample_mask; /* D3D11 OM sample mask (bit0 for single-sample RTs) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_blend_state) == 52);

#pragma pack(push, 1)
struct aerogpu_cmd_set_blend_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_BLEND_STATE */
  struct aerogpu_blend_state state;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_blend_state) == 60);

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

/*
 * Rasterizer state flags (aerogpu_rasterizer_state.flags).
 *
 * Default value 0 corresponds to D3D11 defaults:
 * - DepthClipEnable = TRUE
 */
enum aerogpu_rasterizer_flags {
  AEROGPU_RASTERIZER_FLAG_NONE = 0,
  /* When set: DepthClipEnable = FALSE (i.e. depth clamp). */
  AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE = (1u << 0),
};

#pragma pack(push, 1)
struct aerogpu_rasterizer_state {
  uint32_t fill_mode; /* aerogpu_fill_mode */
  uint32_t cull_mode; /* aerogpu_cull_mode */
  uint32_t front_ccw; /* 0/1 */
  uint32_t scissor_enable; /* 0/1 */
  int32_t depth_bias;
  uint32_t flags; /* aerogpu_rasterizer_flags */
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

#pragma pack(push, 1)
struct aerogpu_cmd_set_primitive_topology {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY */
  uint32_t topology; /* enum aerogpu_primitive_topology */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_primitive_topology) == 16);

#pragma pack(push, 1)
struct aerogpu_cmd_set_texture {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_TEXTURE */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is
   *   treated as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4
   *   correspond to GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t slot;
  aerogpu_handle_t texture; /* 0 = unbind */
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when shader_stage==AEROGPU_SHADER_STAGE_COMPUTE; 0=no override (legacy Compute) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_texture) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_set_sampler_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SAMPLER_STATE */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t slot;
  uint32_t state; /* D3D9 sampler state ID */
  uint32_t value; /* D3D9 sampler state value */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_sampler_state) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_create_sampler {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_CREATE_SAMPLER */
  aerogpu_handle_t sampler_handle;
  uint32_t filter; /* enum aerogpu_sampler_filter */
  uint32_t address_u; /* enum aerogpu_sampler_address_mode */
  uint32_t address_v; /* enum aerogpu_sampler_address_mode */
  uint32_t address_w; /* enum aerogpu_sampler_address_mode */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_create_sampler) == 28);

#pragma pack(push, 1)
struct aerogpu_cmd_destroy_sampler {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DESTROY_SAMPLER */
  aerogpu_handle_t sampler_handle;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_destroy_sampler) == 16);

/*
 * SET_SAMPLERS:
 *
 * Payload format:
 *   struct aerogpu_cmd_set_samplers
 *   aerogpu_handle_t samplers[sampler_count]
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_samplers {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SAMPLERS */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is
   *   treated as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4
   *   correspond to GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t start_slot;
  uint32_t sampler_count;
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when shader_stage==AEROGPU_SHADER_STAGE_COMPUTE; 0=no override (legacy Compute) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_samplers) == 24);

/*
 * Constant buffer binding entry for SET_CONSTANT_BUFFERS.
 */
#pragma pack(push, 1)
struct aerogpu_constant_buffer_binding {
  aerogpu_handle_t buffer; /* 0 = unbound */
  uint32_t offset_bytes;
  uint32_t size_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_constant_buffer_binding) == 16);

/*
 * SET_CONSTANT_BUFFERS:
 *
 * Payload format:
 *   struct aerogpu_cmd_set_constant_buffers
 *   struct aerogpu_constant_buffer_binding bindings[buffer_count]
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_constant_buffers {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_CONSTANT_BUFFERS */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is
   *   treated as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4
   *   correspond to GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t start_slot;
  uint32_t buffer_count;
  uint32_t reserved0; /* stage_ex (enum aerogpu_shader_stage_ex) when shader_stage==AEROGPU_SHADER_STAGE_COMPUTE; 0=no override (legacy Compute) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_constant_buffers) == 24);

/*
 * Buffer SRV binding entry for SET_SHADER_RESOURCE_BUFFERS.
 *
 * `size_bytes == 0` means "use the remaining bytes of the buffer starting at
 * offset_bytes" (D3D11-style "whole resource" default).
 */
#pragma pack(push, 1)
struct aerogpu_shader_resource_buffer_binding {
  aerogpu_handle_t buffer; /* 0 = unbound */
  uint32_t offset_bytes;
  uint32_t size_bytes;
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_shader_resource_buffer_binding) == 16);

/*
 * SET_SHADER_RESOURCE_BUFFERS:
 *
 * Binds buffer shader-resource views (SRV buffers; t# where the SRV is a buffer view).
 *
 * Payload format:
 *   struct aerogpu_cmd_set_shader_resource_buffers
 *   struct aerogpu_shader_resource_buffer_binding bindings[buffer_count]
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_shader_resource_buffers {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_SHADER_RESOURCE_BUFFERS */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is
   *   treated as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4
   *   correspond to GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t start_slot;
  uint32_t buffer_count;
  uint32_t reserved0; /* stage_ex: enum aerogpu_shader_stage_ex (0 = legacy/default; see aerogpu_shader_stage_ex docs) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_shader_resource_buffers) == 24);

/*
 * Buffer UAV binding entry for SET_UNORDERED_ACCESS_BUFFERS.
 *
 * `initial_count` follows D3D11 semantics: `0xFFFFFFFF` means "keep current UAV
 * counter value" (do not reset).
 */
#pragma pack(push, 1)
struct aerogpu_unordered_access_buffer_binding {
  aerogpu_handle_t buffer; /* 0 = unbound */
  uint32_t offset_bytes;
  uint32_t size_bytes;
  uint32_t initial_count;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_unordered_access_buffer_binding) == 16);

/*
 * SET_UNORDERED_ACCESS_BUFFERS:
 *
 * Binds unordered-access (storage) buffers (UAV buffers; u#).
 *
 * Payload format:
 *   struct aerogpu_cmd_set_unordered_access_buffers
 *   struct aerogpu_unordered_access_buffer_binding bindings[uav_count]
 */
#pragma pack(push, 1)
struct aerogpu_cmd_set_unordered_access_buffers {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_UNORDERED_ACCESS_BUFFERS */
  /*
   * Shader stage selector (legacy enum).
   *
   * stage_ex extension:
   * - If `shader_stage == AEROGPU_SHADER_STAGE_COMPUTE` and `reserved0 != 0`, then `reserved0` is
   *   treated as `enum aerogpu_shader_stage_ex` (DXBC program type numbering). Values 2/3/4
   *   correspond to GS/HS/DS.
   * - `reserved0 == 0` means legacy compute (no override).
   */
  uint32_t shader_stage; /* enum aerogpu_shader_stage */
  uint32_t start_slot;
  uint32_t uav_count;
  uint32_t reserved0; /* stage_ex: enum aerogpu_shader_stage_ex (0 = legacy/default; see aerogpu_shader_stage_ex docs) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_unordered_access_buffers) == 24);

#pragma pack(push, 1)
struct aerogpu_cmd_set_render_state {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_SET_RENDER_STATE */
  uint32_t state; /* D3D9 render state ID */
  uint32_t value; /* D3D9 render state value */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_set_render_state) == 16);

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

#pragma pack(push, 1)
struct aerogpu_cmd_dispatch {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_DISPATCH */
  uint32_t group_count_x;
  uint32_t group_count_y;
  uint32_t group_count_z;
  uint32_t reserved0; /* stage_ex: enum aerogpu_shader_stage_ex (ABI minor >= AEROGPU_STAGE_EX_MIN_ABI_MINOR; 0 = legacy/default Compute) */
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_dispatch) == 24);

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

/*
 * PRESENT_EX:
 * - Like PRESENT, but additionally carries D3D9Ex PresentEx flags as observed by
 *   the guest UMD.
 * - `d3d9_present_flags` is the raw `dwFlags` passed to IDirect3DDevice9Ex::PresentEx.
 * - The host may ignore unknown/unsupported bits; the primary requirement is
 *   that the command does not fail parsing and is fence-trackable.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_present_ex {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_PRESENT_EX */
  uint32_t scanout_id; /* 0 for now */
  uint32_t flags; /* aerogpu_present_flags */
  uint32_t d3d9_present_flags; /* D3DPRESENT_* (from d3d9.h) */
  uint32_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_present_ex) == 24);

/*
 * EXPORT_SHARED_SURFACE:
 * - Associates an existing `resource_handle` with a driver-chosen `share_token`.
 * - `share_token` is an opaque non-zero 64-bit value that must be stable across
 *   guest processes.
 * - `share_token` values must be treated as globally unique across time:
 *   - Once a token is released (`RELEASE_SHARED_SURFACE`), it is retired and must
 *     not be re-exported for a different resource.
 *   - The host must detect and reject attempts to re-export a retired token.
 * - On Win7/WDDM 1.1, the guest KMD persists `share_token` in the preserved WDDM
 *   allocation private driver data blob (`aerogpu_wddm_alloc_priv.share_token` in
 *   `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`). dxgkrnl preserves this blob
 *   and returns the exact same bytes on cross-process `OpenResource`, so both
 *   processes observe the same token.
 * - Do NOT use the numeric value of the user-mode shared `HANDLE` as `share_token`:
 *   for real NT handles it is process-local (commonly different after
 *   `DuplicateHandle`), and even token-style shared handles must not be treated
 *   as stable protocol keys (and should not be passed to `CloseHandle`).
 * - The host stores a mapping of (share_token -> resource).
 * - MVP limitation: the shared resource must be backed by a single guest
 *   allocation (i.e. one contiguous guest memory range).
 */
#pragma pack(push, 1)
struct aerogpu_cmd_export_shared_surface {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_EXPORT_SHARED_SURFACE */
  aerogpu_handle_t resource_handle;
  uint32_t reserved0;
  uint64_t share_token;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_export_shared_surface) == 24);

/*
 * IMPORT_SHARED_SURFACE:
 * - Creates an alias handle `out_resource_handle` which refers to the same
 *   underlying resource previously exported under `share_token`.
 * - `share_token` must match the value used during export (and recovered from
 *   the preserved allocation private driver data), not the user-mode shared
 *   `HANDLE` value.
 * - If the `share_token` is unknown, the host should treat the command as a
 *   validation error (implementation-defined error reporting).
 */
#pragma pack(push, 1)
struct aerogpu_cmd_import_shared_surface {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_IMPORT_SHARED_SURFACE */
  aerogpu_handle_t out_resource_handle;
  uint32_t reserved0;
  uint64_t share_token;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_import_shared_surface) == 24);

/*
 * RELEASE_SHARED_SURFACE:
 * - Informs the host that `share_token` is no longer valid and should be removed
 *   from shared-surface lookup tables.
 * - Emitted by the Win7 KMD when the final per-process allocation wrapper for a
 *   shared surface is released (to handle Win7's varying
 *   CloseAllocation/DestroyAllocation call patterns).
 * - The host must remove the (share_token -> exported resource) mapping so
 *   future IMPORT_SHARED_SURFACE attempts fail deterministically.
 * - After release, the token must be considered retired and must not be reused
 *   for another export.
 * - Existing imported alias handles remain valid; underlying resource lifetime
 *   is still governed by per-handle DESTROY_RESOURCE refcounting.
 * - MUST be idempotent: unknown or already-released tokens are a no-op.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_release_shared_surface {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_RELEASE_SHARED_SURFACE */
  uint64_t share_token;
  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_release_shared_surface) == 24);

/*
 * FLUSH:
 * - Explicitly requests that the host schedule/submit all prior work for
 *   execution. This is intended to model D3D9Ex-style flush semantics.
 * - For implementations that already submit at every ring submission boundary,
 *   this is typically a no-op.
 */
#pragma pack(push, 1)
struct aerogpu_cmd_flush {
  struct aerogpu_cmd_hdr hdr; /* opcode = AEROGPU_CMD_FLUSH */
  uint32_t reserved0;
  uint32_t reserved1;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_cmd_flush) == 16);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_CMD_H_ */
