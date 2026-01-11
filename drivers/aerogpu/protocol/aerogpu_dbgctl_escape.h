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

/*
 * This header intentionally does NOT include `aerogpu_protocol.h` (legacy,
 * monolithic ABI) because it macro-conflicts with the versioned ABI headers
 * (`aerogpu_pci.h` + `aerogpu_ring.h`) used by the KMD.
 *
 * This escape ABI is stable across x86/x64 and is shared between:
 * - the Win7 AeroGPU KMD (`DxgkDdiEscape`), and
 * - bring-up tooling (`drivers/aerogpu/tools/win7_dbgctl`).
 */

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Escape ops specific to dbgctl. */
#define AEROGPU_ESCAPE_VERSION 1u
#define AEROGPU_ESCAPE_OP_QUERY_DEVICE 1u
#define AEROGPU_ESCAPE_OP_QUERY_FENCE 2u
#define AEROGPU_ESCAPE_OP_DUMP_RING 3u
#define AEROGPU_ESCAPE_OP_SELFTEST 4u
#define AEROGPU_ESCAPE_OP_QUERY_VBLANK 5u
#define AEROGPU_ESCAPE_OP_DUMP_VBLANK AEROGPU_ESCAPE_OP_QUERY_VBLANK

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

typedef struct aerogpu_escape_header {
  uint32_t version; /* AEROGPU_ESCAPE_VERSION */
  uint32_t op;      /* AEROGPU_ESCAPE_OP_* */
  uint32_t size;    /* total size including this header */
  uint32_t reserved0;
} aerogpu_escape_header;

typedef struct aerogpu_escape_query_device_out {
  aerogpu_escape_header hdr;
  uint32_t mmio_version; /* legacy MMIO version or versioned ABI version */
  uint32_t reserved0;
} aerogpu_escape_query_device_out;

typedef struct aerogpu_escape_query_fence_out {
  aerogpu_escape_header hdr;
  uint64_t last_submitted_fence;
  uint64_t last_completed_fence;
} aerogpu_escape_query_fence_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_fence_out) == 32);

typedef struct aerogpu_dbgctl_ring_desc {
  uint64_t fence;
  uint64_t desc_gpa;
  uint32_t desc_size_bytes;
  uint32_t flags;
} aerogpu_dbgctl_ring_desc;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc) == 24);

typedef struct aerogpu_escape_dump_ring_inout {
  aerogpu_escape_header hdr;
  uint32_t ring_id;
  uint32_t ring_size_bytes;
  uint32_t head;
  uint32_t tail;
  uint32_t desc_count;
  uint32_t desc_capacity;
  aerogpu_dbgctl_ring_desc desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_inout) == (40 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 24)));

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
  uint32_t vidpn_source_id; /* input (0 for MVP), echoed back */
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
  uint32_t reserved1;
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
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, reserved1) == 52);

typedef aerogpu_escape_query_vblank_out aerogpu_escape_dump_vblank_inout;

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
