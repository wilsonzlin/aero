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

enum aerogpu_dbgctl_vblank_flags {
  /* KMD observed AEROGPU_FEATURE_VBLANK and populated vblank fields. */
  AEROGPU_DBGCTL_VBLANK_SUPPORTED = (1u << 0),
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
 *   Legacy devices must return 0 for both.
 */
typedef struct aerogpu_escape_query_device_v2_out {
  aerogpu_escape_header hdr;
  uint32_t detected_mmio_magic;
  uint32_t abi_version_u32;
  uint64_t features_lo;
  uint64_t features_hi;
  uint64_t reserved0;
} aerogpu_escape_query_device_v2_out;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_device_v2_out) == 48);

typedef struct aerogpu_escape_query_fence_out {
  aerogpu_escape_header hdr;
  uint64_t last_submitted_fence;
  uint64_t last_completed_fence;
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
  uint64_t signal_fence;
  uint64_t cmd_gpa;
  uint32_t cmd_size_bytes;
  uint32_t flags;
} aerogpu_dbgctl_ring_desc;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc) == 24);

typedef struct aerogpu_escape_dump_ring_inout {
  aerogpu_escape_header hdr;
  uint32_t ring_id;
  uint32_t ring_size_bytes;
  /*
   * Ring indices.
   *
   * `head` and `tail` are monotonically increasing indices (not masked).
   * The slot is `(index % entry_count)`.
   */
  uint32_t head;
  uint32_t tail;
  uint32_t desc_count;
  uint32_t desc_capacity;
  aerogpu_dbgctl_ring_desc desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_inout) == (40 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 24)));

enum aerogpu_dbgctl_ring_format {
  AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN = 0,
  AEROGPU_DBGCTL_RING_FORMAT_LEGACY = 1,
  AEROGPU_DBGCTL_RING_FORMAT_AGPU = 2,
};

typedef struct aerogpu_dbgctl_ring_desc_v2 {
  uint64_t fence; /* signal_fence */
  uint64_t cmd_gpa;
  uint32_t cmd_size_bytes;
  uint32_t flags;
  uint64_t alloc_table_gpa;
  uint32_t alloc_table_size_bytes;
  uint32_t reserved0;
} aerogpu_dbgctl_ring_desc_v2;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc_v2) == 40);

typedef struct aerogpu_escape_dump_ring_v2_inout {
  aerogpu_escape_header hdr;
  uint32_t ring_id;
  uint32_t ring_format; /* enum aerogpu_dbgctl_ring_format */
  uint32_t ring_size_bytes;
  uint32_t head;
  uint32_t tail;
  uint32_t desc_count;
  uint32_t desc_capacity;
  uint32_t reserved0;
  uint32_t reserved1;
  aerogpu_dbgctl_ring_desc_v2 desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_v2_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_v2_inout) == (52 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 40)));

typedef struct aerogpu_escape_selftest_inout {
  aerogpu_escape_header hdr;
  uint32_t timeout_ms;
  uint32_t passed;
  uint32_t error_code;
  uint32_t reserved0;
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
  uint32_t vidpn_source_id;
  uint32_t irq_enable;
  uint32_t irq_status;
  /*
   * Flags:
   * - Bit 31: flags are valid (newer KMDs). If clear, tooling should assume
   *   vblank is supported because older KMDs only returned success when
   *   `AEROGPU_FEATURE_VBLANK` was present.
   * - Bit 0: vblank registers are supported/valid.
   */
  uint32_t flags;
  uint64_t vblank_seq;
  uint64_t last_vblank_time_ns;
  uint32_t vblank_period_ns;
  uint32_t reserved0;
} aerogpu_escape_query_vblank_out;

#define AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED (1u << 0)

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_vblank_out) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vidpn_source_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_enable) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_status) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, flags) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_seq) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, last_vblank_time_ns) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_period_ns) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, reserved0) == 52);

typedef aerogpu_escape_query_vblank_out aerogpu_escape_dump_vblank_inout;

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
