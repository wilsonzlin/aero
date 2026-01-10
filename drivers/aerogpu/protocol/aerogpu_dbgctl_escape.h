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

#include "aerogpu_protocol.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Escape ops specific to dbgctl. */
#define AEROGPU_ESCAPE_OP_QUERY_FENCE 2u
#define AEROGPU_ESCAPE_OP_DUMP_RING 3u
#define AEROGPU_ESCAPE_OP_SELFTEST 4u

#define AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS 32u

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

typedef struct aerogpu_dbgctl_ring_desc {
  aerogpu_u64 fence;
  aerogpu_u64 desc_gpa;
  aerogpu_u32 desc_size_bytes;
  aerogpu_u32 flags;
} aerogpu_dbgctl_ring_desc;

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

typedef struct aerogpu_escape_selftest_inout {
  aerogpu_escape_header hdr;
  aerogpu_u32 timeout_ms;
  aerogpu_u32 passed;
  aerogpu_u32 error_code;
  aerogpu_u32 reserved0;
} aerogpu_escape_selftest_inout;

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
