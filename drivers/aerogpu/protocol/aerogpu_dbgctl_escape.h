/*
 * AeroGPU debug/control Escape ABI.
 *
 * This is a small, driver-private Escape protocol intended for bring-up tools
 * (e.g. `drivers/aerogpu/tools/win7_dbgctl`).
 *
 * The packets are sent via `D3DKMTEscape` and are handled by the KMD's
 * `DxgkDdiEscape`.
 *
 * NOTE: Escape packets must have a stable layout across x86/x64 because a 32-bit
 * user-mode tool may send escapes to a 64-bit kernel. All structs are packed
 * and contain no pointers.
 */
#pragma once

#include <stddef.h>
#include "aerogpu_escape.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Escape ops specific to dbgctl. */
#define AEROGPU_ESCAPE_OP_QUERY_FENCE 2u
#define AEROGPU_ESCAPE_OP_DUMP_RING 3u
#define AEROGPU_ESCAPE_OP_SELFTEST 4u
#define AEROGPU_ESCAPE_OP_QUERY_VBLANK 5u
#define AEROGPU_ESCAPE_OP_DUMP_VBLANK AEROGPU_ESCAPE_OP_QUERY_VBLANK
#define AEROGPU_ESCAPE_OP_DUMP_RING_V2 6u

/* Extended base Escape ops used by bring-up tooling. */
#define AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2 7u
#define AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE 8u

#define AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS 32u

#define AEROGPU_DBGCTL_CONCAT2_(a, b) a##b
#define AEROGPU_DBGCTL_CONCAT_(a, b) AEROGPU_DBGCTL_CONCAT2_(a, b)
#define AEROGPU_DBGCTL_STATIC_ASSERT(expr) \
  typedef char AEROGPU_DBGCTL_CONCAT_(aerogpu_dbgctl_static_assert_, __LINE__)[(expr) ? 1 : -1]

enum aerogpu_dbgctl_selftest_error {
  AEROGPU_DBGCTL_SELFTEST_OK = 0,
  AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE = 1,
  AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY = 2,
  AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY = 3,
  AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES = 4,
  AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT = 5,
};

#pragma pack(push, 1)

/*
 * Query device response (v2).
 *
 * - `detected_mmio_magic` is the BAR0 magic register value.
 *   - Legacy device: 'A''R''G''P' (0x41524750)
 *   - New device:    "AGPU" little-endian (0x55504741)
 *
 * - `abi_version_u32` is the device's reported ABI version:
 *   - New device: `AEROGPU_MMIO_REG_ABI_VERSION` value.
 *   - Legacy device: legacy MMIO version register value.
 *
 * - `features_lo/hi` is a 128-bit feature bitset. New devices should report
 *   their FEATURES_LO/HI (lower 64 bits) in `features_lo` with `features_hi=0`.
 *   Legacy devices may return 0 for both. If a legacy device model also exposes
 *   the versioned FEATURES_LO/HI registers, drivers may report them here for
 *   tooling/debug purposes.
 */
typedef struct aerogpu_escape_query_device_v2_out {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 detected_mmio_magic;
  aerogpu_escape_u32 abi_version_u32;
  aerogpu_escape_u64 features_lo;
  aerogpu_escape_u64 features_hi;
  aerogpu_escape_u64 reserved0;
} aerogpu_escape_query_device_v2_out;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_device_v2_out) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_v2_out, detected_mmio_magic) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_v2_out, abi_version_u32) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_v2_out, features_lo) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_v2_out, features_hi) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_v2_out, reserved0) == 40);

typedef struct aerogpu_escape_query_fence_out {
  aerogpu_escape_header hdr;
  aerogpu_escape_u64 last_submitted_fence;
  aerogpu_escape_u64 last_completed_fence;
} aerogpu_escape_query_fence_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_fence_out) == 32);

/*
 * Must remain stable across x86/x64.
 *
 * Represents the most interesting fields of a `struct aerogpu_submit_desc`
 * entry (see `aerogpu_ring.h`).
 */
typedef struct aerogpu_dbgctl_ring_desc {
  aerogpu_escape_u64 signal_fence;
  aerogpu_escape_u64 cmd_gpa;
  aerogpu_escape_u32 cmd_size_bytes;
  aerogpu_escape_u32 flags;
} aerogpu_dbgctl_ring_desc;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc) == 24);

typedef struct aerogpu_escape_dump_ring_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 ring_id;
  aerogpu_escape_u32 ring_size_bytes;
  /*
   * Ring indices.
   *
   * `head` and `tail` are monotonically increasing indices (not masked).
   * The slot is `(index % entry_count)`.
   */
  aerogpu_escape_u32 head;
  aerogpu_escape_u32 tail;
  aerogpu_escape_u32 desc_count;
  aerogpu_escape_u32 desc_capacity;
  aerogpu_dbgctl_ring_desc desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_inout) == (40 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 24)));

enum aerogpu_dbgctl_ring_format {
  AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN = 0,
  AEROGPU_DBGCTL_RING_FORMAT_LEGACY = 1,
  AEROGPU_DBGCTL_RING_FORMAT_AGPU = 2,
};

typedef struct aerogpu_dbgctl_ring_desc_v2 {
  aerogpu_escape_u64 fence; /* signal_fence */
  aerogpu_escape_u64 cmd_gpa;
  aerogpu_escape_u32 cmd_size_bytes;
  aerogpu_escape_u32 flags;
  aerogpu_escape_u64 alloc_table_gpa;
  aerogpu_escape_u32 alloc_table_size_bytes;
  aerogpu_escape_u32 reserved0;
} aerogpu_dbgctl_ring_desc_v2;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc_v2) == 40);

typedef struct aerogpu_escape_dump_ring_v2_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 ring_id;
  aerogpu_escape_u32 ring_format; /* enum aerogpu_dbgctl_ring_format */
  aerogpu_escape_u32 ring_size_bytes;
  aerogpu_escape_u32 head;
  aerogpu_escape_u32 tail;
  aerogpu_escape_u32 desc_count;
  aerogpu_escape_u32 desc_capacity;
  aerogpu_escape_u32 reserved0;
  aerogpu_escape_u32 reserved1;
  aerogpu_dbgctl_ring_desc_v2 desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_v2_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_v2_inout) == (52 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 40)));

typedef struct aerogpu_escape_selftest_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 timeout_ms;
  aerogpu_escape_u32 passed;
  aerogpu_escape_u32 error_code;
  aerogpu_escape_u32 reserved0;
} aerogpu_escape_selftest_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_selftest_inout) == 32);

typedef struct aerogpu_escape_query_vblank_out {
  aerogpu_escape_header hdr;
  /*
   * Requested VidPn source id.
   *
   * NOTE: Only source 0 is currently implemented. KMDs may ignore non-zero
   * inputs and always return source 0 data.
   */
  aerogpu_escape_u32 vidpn_source_id;
  aerogpu_escape_u32 irq_enable;
  aerogpu_escape_u32 irq_status;
  /*
   * Flags:
   * - Bit 31: flags are valid (newer KMDs). If clear, tooling should assume
   *   vblank is supported because older KMDs only returned success when
   *   `AEROGPU_FEATURE_VBLANK` was present.
   * - Bit 0: vblank registers are supported/valid.
   * - Bit 1: `vblank_interrupt_type` is valid.
   */
  aerogpu_escape_u32 flags;
  aerogpu_escape_u64 vblank_seq;
  aerogpu_escape_u64 last_vblank_time_ns;
  aerogpu_escape_u32 vblank_period_ns;
  /*
   * DXGK_INTERRUPT_TYPE requested via DxgkDdiControlInterrupt.
   *
   * This is only meaningful when `AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID`
   * is set in `flags`.
   */
  aerogpu_escape_u32 vblank_interrupt_type;
} aerogpu_escape_query_vblank_out;

#define AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED (1u << 0)
#define AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID (1u << 1)

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_vblank_out) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vidpn_source_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_enable) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_status) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, flags) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_seq) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, last_vblank_time_ns) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_period_ns) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_interrupt_type) == 52);

typedef aerogpu_escape_query_vblank_out aerogpu_escape_dump_vblank_inout;
typedef struct aerogpu_escape_map_shared_handle_inout {
  aerogpu_escape_header hdr;
  uint64_t shared_handle;
  uint32_t share_token;
  uint32_t reserved0;
} aerogpu_escape_map_shared_handle_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_map_shared_handle_inout) == 32);

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
