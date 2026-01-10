/*
 * AeroGPU Protocol
 *
 * This header defines the guest<->emulator ABI for the AeroGPU virtual device.
 *
 * It intentionally contains two layers:
 *   1) A command stream "wire format" used by Windows user-mode drivers (UMDs).
 *   2) A BAR0 MMIO + ring submission ABI used by the Windows WDDM KMD.
 *
 * The command stream is intentionally conservative:
 *   - Little-endian, fixed-size POD structs.
 *   - No pointers; most references use 32-bit "allocation indices".
 *   - Extensible: new commands can be appended without changing old ones.
 *
 * The KMD-level ABI is designed for Windows 7 WDDM 1.1 bring-up:
 *   - One shared ring in guest memory (physically contiguous) for submissions.
 *   - A single scanout head programmed via MMIO.
 *   - A fence register + interrupt bit for reliable completion signaling.
 */

#pragma once

#ifdef __cplusplus
extern "C" {
#endif

/* Fixed-width protocol types (WDK 7.1 doesn't guarantee stdint.h in kernel-mode). */
#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT8 aerogpu_u8;
typedef UINT16 aerogpu_u16;
typedef UINT32 aerogpu_u32;
typedef UINT64 aerogpu_u64;
typedef LONG aerogpu_i32;
#else
#include <stdint.h>
typedef uint8_t aerogpu_u8;
typedef uint16_t aerogpu_u16;
typedef uint32_t aerogpu_u32;
typedef uint64_t aerogpu_u64;
typedef int32_t aerogpu_i32;
#endif

/* ------------------------------------------------------------------------- */
/* 1) UMD command stream                                                     */
/* ------------------------------------------------------------------------- */

/*
 * Command stream header for every packet.
 *
 * The command stream is a sequence of:
 *   [AEROGPU_CMD_HEADER][payload bytes...]
 *
 * `size_bytes` includes the header itself.
 */
typedef struct AEROGPU_CMD_HEADER {
  aerogpu_u32 opcode;
  aerogpu_u32 size_bytes;
} AEROGPU_CMD_HEADER;

enum AEROGPU_CMD_OPCODE {
  /* Resource lifetime. */
  AEROGPU_CMD_CREATE_RESOURCE = 0x0001,
  AEROGPU_CMD_DESTROY_RESOURCE = 0x0002,
  AEROGPU_CMD_UPLOAD_RESOURCE = 0x0003,

  /* Shaders and pipeline state. */
  AEROGPU_CMD_CREATE_SHADER = 0x0101,
  AEROGPU_CMD_DESTROY_SHADER = 0x0102,
  AEROGPU_CMD_BIND_SHADERS = 0x0103,
  AEROGPU_CMD_SET_INPUT_LAYOUT = 0x0104,

  /* Binding. */
  AEROGPU_CMD_SET_VERTEX_BUFFER = 0x0201,
  AEROGPU_CMD_SET_INDEX_BUFFER = 0x0202,
  AEROGPU_CMD_SET_RENDER_TARGET = 0x0203,
  AEROGPU_CMD_SET_VIEWPORT = 0x0204,

  /* Draw. */
  AEROGPU_CMD_CLEAR_RTV = 0x0301,
  AEROGPU_CMD_DRAW = 0x0302,
  AEROGPU_CMD_DRAW_INDEXED = 0x0303,

  /* Presentation / synchronization. */
  AEROGPU_CMD_PRESENT = 0x0401,
  AEROGPU_CMD_SIGNAL_FENCE = 0x0402,
};

/* Resource types understood by the host translator. */
enum AEROGPU_RESOURCE_KIND {
  AEROGPU_RESOURCE_KIND_BUFFER = 1,
  AEROGPU_RESOURCE_KIND_TEX2D = 2,
};

/*
 * Formats are expressed using DXGI_FORMAT numeric values to avoid yet another
 * format enum. This keeps the protocol stable across UMDs.
 */

typedef struct AEROGPU_CMD_CREATE_RESOURCE_PAYLOAD {
  aerogpu_u32 alloc_index;
  aerogpu_u32 kind; /* AEROGPU_RESOURCE_KIND */

  /* Common fields. */
  aerogpu_u32 bind_flags; /* D3D10/11 bind flags (D3D11_BIND_*) */
  aerogpu_u32 misc_flags; /* driver-defined for now */

  /* Buffer fields when kind == BUFFER */
  aerogpu_u32 size_bytes;
  aerogpu_u32 stride_bytes;

  /* Texture2D fields when kind == TEX2D */
  aerogpu_u32 width;
  aerogpu_u32 height;
  aerogpu_u32 mip_levels;
  aerogpu_u32 array_size;
  aerogpu_u32 dxgi_format; /* DXGI_FORMAT numeric value */
} AEROGPU_CMD_CREATE_RESOURCE_PAYLOAD;

typedef struct AEROGPU_CMD_DESTROY_RESOURCE_PAYLOAD {
  aerogpu_u32 alloc_index;
} AEROGPU_CMD_DESTROY_RESOURCE_PAYLOAD;

typedef struct AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD {
  aerogpu_u32 alloc_index;
  aerogpu_u32 dst_offset_bytes;
  aerogpu_u32 data_size_bytes;
  /* Followed by `data_size_bytes` of raw data. */
} AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD;

enum AEROGPU_SHADER_STAGE {
  AEROGPU_SHADER_STAGE_VS = 1,
  AEROGPU_SHADER_STAGE_PS = 2,
};

typedef struct AEROGPU_CMD_CREATE_SHADER_PAYLOAD {
  aerogpu_u32 shader_id;
  aerogpu_u32 stage; /* AEROGPU_SHADER_STAGE */
  aerogpu_u32 dxbc_size_bytes;
  /* Followed by `dxbc_size_bytes` of DXBC. */
} AEROGPU_CMD_CREATE_SHADER_PAYLOAD;

typedef struct AEROGPU_CMD_DESTROY_SHADER_PAYLOAD {
  aerogpu_u32 shader_id;
} AEROGPU_CMD_DESTROY_SHADER_PAYLOAD;

typedef struct AEROGPU_CMD_BIND_SHADERS_PAYLOAD {
  aerogpu_u32 vs_shader_id; /* 0 == unbind */
  aerogpu_u32 ps_shader_id; /* 0 == unbind */
} AEROGPU_CMD_BIND_SHADERS_PAYLOAD;

/*
 * The input layout is emitted as a variable-length command because D3D input
 * layouts are small and immutable (set once, reused across draws).
 *
 * The payload is:
 *   [AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD]
 *   [AEROGPU_INPUT_ELEMENT element[element_count]]
 */
typedef struct AEROGPU_INPUT_ELEMENT {
  aerogpu_u32 semantic_name_hash; /* FNV-1a hash of ASCII semantic name */
  aerogpu_u32 semantic_index;
  aerogpu_u32 format_dxgi; /* DXGI_FORMAT numeric value */
  aerogpu_u32 input_slot;
  aerogpu_u32 aligned_byte_offset;
  aerogpu_u32 input_slot_class; /* 0: per-vertex, 1: per-instance */
  aerogpu_u32 instance_data_step_rate;
} AEROGPU_INPUT_ELEMENT;

typedef struct AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD {
  aerogpu_u32 element_count;
} AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD;

typedef struct AEROGPU_CMD_SET_VERTEX_BUFFER_PAYLOAD {
  aerogpu_u32 alloc_index;
  aerogpu_u32 stride_bytes;
  aerogpu_u32 offset_bytes;
} AEROGPU_CMD_SET_VERTEX_BUFFER_PAYLOAD;

typedef struct AEROGPU_CMD_SET_INDEX_BUFFER_PAYLOAD {
  aerogpu_u32 alloc_index;
  aerogpu_u32 index_format_dxgi; /* DXGI_FORMAT_R16_UINT / DXGI_FORMAT_R32_UINT numeric */
  aerogpu_u32 offset_bytes;
} AEROGPU_CMD_SET_INDEX_BUFFER_PAYLOAD;

typedef struct AEROGPU_CMD_SET_RENDER_TARGET_PAYLOAD {
  aerogpu_u32 rtv_alloc_index; /* allocation index of render target texture */
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
  aerogpu_u32 vertex_count;
  aerogpu_u32 start_vertex_location;
} AEROGPU_CMD_DRAW_PAYLOAD;

typedef struct AEROGPU_CMD_DRAW_INDEXED_PAYLOAD {
  aerogpu_u32 index_count;
  aerogpu_u32 start_index_location;
  aerogpu_i32 base_vertex_location;
} AEROGPU_CMD_DRAW_INDEXED_PAYLOAD;

typedef struct AEROGPU_CMD_PRESENT_PAYLOAD {
  aerogpu_u32 backbuffer_alloc_index;
  aerogpu_u32 sync_interval; /* 0 or 1 (initially) */
} AEROGPU_CMD_PRESENT_PAYLOAD;

typedef struct AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD {
  aerogpu_u64 fence_value;
} AEROGPU_CMD_SIGNAL_FENCE_PAYLOAD;

/* ------------------------------------------------------------------------- */
/* 2) KMD BAR0 MMIO + ring submission ABI                                    */
/* ------------------------------------------------------------------------- */

/*
 * PCI identification.
 *
 * The actual VEN/DEV values are part of the virtual machine's PCI device model.
 * If your device model uses different IDs, update the INF accordingly.
 */
#define AEROGPU_PCI_VENDOR_ID 0x1AED
#define AEROGPU_PCI_DEVICE_ID 0x0001

/*
 * MMIO register space (BAR0) layout.
 *
 * All registers are little-endian.
 */
#define AEROGPU_MMIO_MAGIC 0x41524750u /* 'A''R''G''P' */
#define AEROGPU_MMIO_VERSION 0x00010000u

enum aerogpu_mmio_reg {
  /* Identification */
  AEROGPU_REG_MAGIC = 0x0000,   /* u32 */
  AEROGPU_REG_VERSION = 0x0004, /* u32 */

  /* Ring setup (written by guest driver during start) */
  AEROGPU_REG_RING_BASE_LO = 0x0010,       /* u32 */
  AEROGPU_REG_RING_BASE_HI = 0x0014,       /* u32 */
  AEROGPU_REG_RING_ENTRY_COUNT = 0x0018,   /* u32: number of entries */
  AEROGPU_REG_RING_HEAD = 0x001C,          /* u32: emulator-owned */
  AEROGPU_REG_RING_TAIL = 0x0020,          /* u32: guest-owned */
  AEROGPU_REG_RING_DOORBELL = 0x0024,      /* u32: write-any to notify */

  /* Interrupt + fence completion */
  AEROGPU_REG_INT_STATUS = 0x0030,         /* u32 */
  AEROGPU_REG_INT_ACK = 0x0034,            /* u32 */
  AEROGPU_REG_FENCE_COMPLETED = 0x0038,    /* u32: last completed fence */

  /* Scanout (single head) */
  AEROGPU_REG_SCANOUT_FB_LO = 0x0100,      /* u32 */
  AEROGPU_REG_SCANOUT_FB_HI = 0x0104,      /* u32 */
  AEROGPU_REG_SCANOUT_PITCH = 0x0108,      /* u32: bytes/row */
  AEROGPU_REG_SCANOUT_WIDTH = 0x010C,      /* u32 */
  AEROGPU_REG_SCANOUT_HEIGHT = 0x0110,     /* u32 */
  AEROGPU_REG_SCANOUT_FORMAT = 0x0114,     /* u32: see aerogpu_scanout_format */
  AEROGPU_REG_SCANOUT_ENABLE = 0x0118,     /* u32: 0/1 */
};

enum aerogpu_interrupt_bits {
  AEROGPU_INT_FENCE = 0x00000001u,
};

enum aerogpu_scanout_format {
  AEROGPU_SCANOUT_X8R8G8B8 = 1,
};

/*
 * Ring entries.
 *
 * The guest pushes entries into a shared ring and rings the doorbell. Each
 * entry points at a physically contiguous submission descriptor in guest
 * memory.
 */
enum aerogpu_ring_entry_type {
  AEROGPU_RING_ENTRY_SUBMIT = 1,
};

typedef struct aerogpu_ring_entry_submit {
  aerogpu_u32 type;      /* AEROGPU_RING_ENTRY_SUBMIT */
  aerogpu_u32 flags;     /* reserved */
  aerogpu_u32 fence;     /* monotonically increasing fence id */
  aerogpu_u32 desc_size; /* bytes */
  aerogpu_u64 desc_gpa;  /* guest physical address of submission descriptor */
} aerogpu_ring_entry_submit;

typedef union aerogpu_ring_entry {
  aerogpu_u32 type;
  aerogpu_ring_entry_submit submit;
} aerogpu_ring_entry;

/*
 * Submission descriptor (pointed to by a ring entry).
 *
 * The emulator reads this structure from guest physical memory and then reads
 * the DMA buffer copy referenced by it. The allocation snapshot is optional
 * but enables the emulator to resolve allocation handles to physical memory.
 */
#define AEROGPU_SUBMISSION_DESC_VERSION 1u

enum aerogpu_submission_type {
  AEROGPU_SUBMIT_RENDER = 1,
  AEROGPU_SUBMIT_PRESENT = 2,
  AEROGPU_SUBMIT_PAGING = 3,
};

typedef struct aerogpu_submission_desc_header {
  aerogpu_u32 version; /* AEROGPU_SUBMISSION_DESC_VERSION */
  aerogpu_u32 type;    /* aerogpu_submission_type */
  aerogpu_u32 fence;   /* same as ring entry fence */
  aerogpu_u32 reserved0;

  aerogpu_u64 dma_buffer_gpa; /* guest physical address, contiguous */
  aerogpu_u32 dma_buffer_size;
  aerogpu_u32 allocation_count;
} aerogpu_submission_desc_header;

typedef struct aerogpu_submission_desc_allocation {
  aerogpu_u64 allocation_handle; /* driver-private token (for debugging) */
  aerogpu_u64 gpa;               /* base guest physical address */
  aerogpu_u32 size_bytes;
  aerogpu_u32 reserved0;
} aerogpu_submission_desc_allocation;

/*
 * Escape channel ABI (DxgkDdiEscape).
 *
 * Input and output share the same header; operations define additional payload.
 */
#define AEROGPU_ESCAPE_VERSION 1u
#define AEROGPU_ESCAPE_OP_QUERY_DEVICE 1u

typedef struct aerogpu_escape_header {
  aerogpu_u32 version; /* AEROGPU_ESCAPE_VERSION */
  aerogpu_u32 op;      /* AEROGPU_ESCAPE_OP_* */
  aerogpu_u32 size;    /* total size including this header */
  aerogpu_u32 reserved0;
} aerogpu_escape_header;

typedef struct aerogpu_escape_query_device_out {
  aerogpu_escape_header hdr;
  aerogpu_u32 mmio_version;
  aerogpu_u32 reserved0;
} aerogpu_escape_query_device_out;

#ifdef __cplusplus
} // extern "C"
#endif
