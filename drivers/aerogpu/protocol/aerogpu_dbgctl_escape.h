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

#include "aerogpu_protocol.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Escape ops specific to dbgctl. */
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

typedef struct aerogpu_escape_query_fence_out {
  aerogpu_escape_header hdr;
  aerogpu_u64 last_submitted_fence;
  aerogpu_u64 last_completed_fence;
} aerogpu_escape_query_fence_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_fence_out) == 32);

typedef struct aerogpu_dbgctl_ring_desc {
  aerogpu_u64 fence;
  aerogpu_u64 desc_gpa;
  aerogpu_u32 desc_size_bytes;
  aerogpu_u32 flags;
} aerogpu_dbgctl_ring_desc;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_ring_desc) == 24);

typedef struct aerogpu_escape_dump_ring_inout {
  aerogpu_escape_header hdr;
  aerogpu_u32 ring_id;
  aerogpu_u32 ring_size_bytes;
  aerogpu_u32 head;
  aerogpu_u32 tail;
  aerogpu_u32 desc_count;
  aerogpu_u32 desc_capacity;
  aerogpu_dbgctl_ring_desc desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_inout) == (40 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 24)));

typedef struct aerogpu_escape_selftest_inout {
  aerogpu_escape_header hdr;
  aerogpu_u32 timeout_ms;
  aerogpu_u32 passed;
  aerogpu_u32 error_code;
  aerogpu_u32 reserved0;
} aerogpu_escape_selftest_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_selftest_inout) == 32);

typedef struct aerogpu_escape_query_vblank_out {
  aerogpu_escape_header hdr;
  aerogpu_u32 vidpn_source_id; /* input (0 for MVP), echoed back */
  aerogpu_u32 irq_enable;
  aerogpu_u32 irq_status;
  aerogpu_u32 reserved0;
  aerogpu_u64 vblank_seq;
  aerogpu_u64 last_vblank_time_ns;
  aerogpu_u32 vblank_period_ns;
  aerogpu_u32 reserved1;
} aerogpu_escape_query_vblank_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_vblank_out) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vidpn_source_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_enable) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, irq_status) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, reserved0) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_seq) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, last_vblank_time_ns) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, vblank_period_ns) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_vblank_out, reserved1) == 52);

typedef aerogpu_escape_query_vblank_out aerogpu_escape_dump_vblank_inout;

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
