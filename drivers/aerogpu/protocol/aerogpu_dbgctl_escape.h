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
#define AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION 9u
#define AEROGPU_ESCAPE_OP_QUERY_SCANOUT 10u
#define AEROGPU_ESCAPE_OP_QUERY_CURSOR 11u
/* Query performance/health counters snapshot. */
#define AEROGPU_ESCAPE_OP_QUERY_PERF 12u
/*
 * Debug-only, security-gated guest physical memory read.
 *
 * See `aerogpu_escape_read_gpa_inout`.
 */
#define AEROGPU_ESCAPE_OP_READ_GPA 13u
/* Query most recent device error state (MMIO error registers when available). */
#define AEROGPU_ESCAPE_OP_QUERY_ERROR 14u

#define AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS 32u
#define AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS 32u
/* Maximum payload size for AEROGPU_ESCAPE_OP_READ_GPA (bounded guest physical reads). */
#define AEROGPU_DBGCTL_READ_GPA_MAX_BYTES 4096u

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
  /* Vblank sanity (optional, gated by AEROGPU_FEATURE_VBLANK). */
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE = 6,
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK = 7,
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE = 8,
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED = 9,
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED = 10,
  /* Cursor sanity (optional, gated by AEROGPU_FEATURE_CURSOR). */
  AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE = 11,
  AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH = 12,
  /* IRQ delivery sanity (optional, gated by AEROGPU_FEATURE_VBLANK + scanout enabled). */
  AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED = 13,
  /* Selftest could not complete within timeout_ms (time budget exhausted). */
  AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED = 14,
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
  /*
   * Adapter-global fence counters as tracked by the KMD.
   *
   * NOTE: `last_submitted_fence` is global across all guest processes using the
   * adapter (DWM + apps). UMDs must not use it to infer the fence ID for an
   * individual submission; per-submission fence IDs come from the D3D runtime
   * callbacks (for example `SubmissionFenceId` / `NewFenceValue`).
   * `last_completed_fence` is useful for polling overall GPU forward progress.
   */
  aerogpu_escape_u64 last_submitted_fence;
  aerogpu_escape_u64 last_completed_fence;
  /*
   * Sticky error IRQ diagnostics (best-effort; 0 if not supported by this KMD build).
   *
   * These fields are appended to the original struct (hdr + last_submitted + last_completed) to
   * keep the layout backwards compatible with older bring-up tooling.
   *
   * When the device/emulator signals a submission failure (AEROGPU_IRQ_ERROR), the KMD increments
   * `error_irq_count` and records the most recent fence value associated with an error in
   * `last_error_fence`.
   */
  aerogpu_escape_u64 error_irq_count;
  aerogpu_escape_u64 last_error_fence;
} aerogpu_escape_query_fence_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_fence_out) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_fence_out, last_submitted_fence) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_fence_out, last_completed_fence) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_fence_out, error_irq_count) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_fence_out, last_error_fence) == 40);

/*
 * Query performance/health counters snapshot.
 *
 * This is intended to be a low-friction, stable "first glance" dump that helps
 * diagnose forward progress and interrupt delivery.
 *
 * All counters are best-effort snapshots and may change concurrently while the
 * escape is being processed.
 */
typedef struct aerogpu_escape_query_perf_out {
  aerogpu_escape_header hdr;

  aerogpu_escape_u64 last_submitted_fence;
  aerogpu_escape_u64 last_completed_fence;

  /* Ring 0 snapshot (AGPU ring when supported; 0 if unknown). */
  aerogpu_escape_u32 ring0_head;
  aerogpu_escape_u32 ring0_tail;
  aerogpu_escape_u32 ring0_size_bytes;
  aerogpu_escape_u32 ring0_entry_count;

  /* Submission counters. */
  aerogpu_escape_u64 total_submissions;
  aerogpu_escape_u64 total_presents;
  aerogpu_escape_u64 total_render_submits;
  aerogpu_escape_u64 total_internal_submits;

  /* Interrupt counters. */
  aerogpu_escape_u64 irq_fence_delivered;
  aerogpu_escape_u64 irq_vblank_delivered;
  aerogpu_escape_u64 irq_spurious;

  /* Reset/TDR counters. */
  aerogpu_escape_u64 reset_from_timeout_count;
  aerogpu_escape_u64 last_reset_time_100ns;

  /* VBlank snapshot. */
  aerogpu_escape_u64 vblank_seq;
  aerogpu_escape_u64 last_vblank_time_ns;
  aerogpu_escape_u32 vblank_period_ns;
  /*
   * Packed error state (best-effort):
   * - Bit 31: KMD device error latched (AEROGPU_IRQ_ERROR observed).
   * - Bits 0..30: last error time in 10ms units since boot (clamped).
   */
  aerogpu_escape_u32 reserved0;

  /*
   * Sticky error IRQ diagnostics (mirrors QUERY_FENCE).
   *
   * These fields are appended to keep the layout backwards compatible with
   * older bring-up tooling.
   */
  aerogpu_escape_u64 error_irq_count;
  aerogpu_escape_u64 last_error_fence;

  /*
   * Additional perf counters (appended).
   *
   * These fields are appended to keep the layout backwards compatible with
   * older bring-up tooling. Callers must check `hdr.size` before reading them.
   */
  aerogpu_escape_u64 ring_push_failures;
  aerogpu_escape_u64 selftest_count;
  aerogpu_escape_u32 selftest_last_error_code; /* enum aerogpu_dbgctl_selftest_error */
  /*
   * Flags (appended):
   * - Bit 31: flags are valid (newer KMDs). If clear, tooling should treat any
   *   other flag bits as unavailable.
   * - Bit 0: ring0_head/tail are valid (0 when unavailable, e.g. legacy device
   *   while powered down).
   * - Bit 1: vblank snapshot fields are valid (device supports vblank).
   */
  aerogpu_escape_u32 flags;

  /*
   * Pending Render/Present meta handle bookkeeping (appended).
   *
   * These counters reflect the current size of the KMD's PendingMetaHandles list
   * (meta handles produced by DxgkDdiRender/DxgkDdiPresent and consumed by
   * DxgkDdiSubmitCommand).
   *
   * The KMD enforces hard caps (count + bytes) on this backlog to avoid unbounded
   * nonpaged memory growth under pathological call patterns or failures.
   */
  aerogpu_escape_u32 pending_meta_handle_count;
  aerogpu_escape_u32 pending_meta_handle_reserved0;
  aerogpu_escape_u64 pending_meta_handle_bytes;
  
  /*
   * DxgkDdiGetScanLine (GetRasterStatus) telemetry (appended).
   *
   * When supported (DBG builds), these counters allow measuring how often the KMD
   * served scanline queries from the cached vblank anchor vs falling back to MMIO
   * polling of vblank timing registers.
   *
   * Callers must check `hdr.size` before reading them.
   */
  aerogpu_escape_u64 get_scanline_cache_hits;
  aerogpu_escape_u64 get_scanline_mmio_polls;

  /*
   * Submission-path contiguous allocation pool counters (appended).
   *
   * These fields are appended to keep the layout backwards compatible with
   * older bring-up tooling. Callers must check `hdr.size` before reading them.
   */
  aerogpu_escape_u64 contig_pool_hit;
  aerogpu_escape_u64 contig_pool_miss;
  aerogpu_escape_u64 contig_pool_bytes_saved;
} aerogpu_escape_query_perf_out;

#define AEROGPU_DBGCTL_QUERY_PERF_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_PERF_FLAG_RING_VALID (1u << 0)
#define AEROGPU_DBGCTL_QUERY_PERF_FLAG_VBLANK_VALID (1u << 1)
#define AEROGPU_DBGCTL_QUERY_PERF_FLAG_GETSCANLINE_COUNTERS_VALID (1u << 2)

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_perf_out) == 240);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, last_submitted_fence) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, last_completed_fence) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, ring0_head) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, ring0_tail) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, ring0_size_bytes) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, ring0_entry_count) == 44);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, total_submissions) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, total_presents) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, total_render_submits) == 64);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, total_internal_submits) == 72);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, irq_fence_delivered) == 80);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, irq_vblank_delivered) == 88);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, irq_spurious) == 96);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, reset_from_timeout_count) == 104);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, last_reset_time_100ns) == 112);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, vblank_seq) == 120);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, last_vblank_time_ns) == 128);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, vblank_period_ns) == 136);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, reserved0) == 140);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, error_irq_count) == 144);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, last_error_fence) == 152);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, ring_push_failures) == 160);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, selftest_count) == 168);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, selftest_last_error_code) == 176);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, flags) == 180);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_count) == 184);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_reserved0) == 188);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_bytes) == 192);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, get_scanline_cache_hits) == 200);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, get_scanline_mmio_polls) == 208);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, contig_pool_hit) == 216);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, contig_pool_miss) == 224);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_perf_out, contig_pool_bytes_saved) == 232);

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
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc, signal_fence) == 0);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc, cmd_gpa) == 8);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc, cmd_size_bytes) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc, flags) == 20);

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
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, ring_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, ring_size_bytes) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, head) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, tail) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, desc_count) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, desc_capacity) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_inout, desc) == 40);

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
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, fence) == 0);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, cmd_gpa) == 8);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, cmd_size_bytes) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, flags) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, alloc_table_gpa) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, alloc_table_size_bytes) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_ring_desc_v2, reserved0) == 36);

typedef struct aerogpu_escape_dump_ring_v2_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 ring_id;
  aerogpu_escape_u32 ring_format; /* enum aerogpu_dbgctl_ring_format */
  aerogpu_escape_u32 ring_size_bytes;
  /*
   * Ring indices.
   *
   * - For AEROGPU_DBGCTL_RING_FORMAT_AGPU, `head` and `tail` are monotonically increasing
   *   indices (not masked). The returned `desc[]` is a recent tail-window of descriptors
   *   ending at `tail - 1` (newest is `desc[desc_count - 1]`).
   *
   * - For AEROGPU_DBGCTL_RING_FORMAT_LEGACY, head/tail are device-specific indices.
   *   Tooling should treat `desc[]` as a best-effort snapshot and may not assume it
   *   contains completed history beyond the pending region.
   */
  aerogpu_escape_u32 head;
  aerogpu_escape_u32 tail;
  aerogpu_escape_u32 desc_count;
  aerogpu_escape_u32 desc_capacity;
  aerogpu_escape_u32 reserved0;
  aerogpu_escape_u32 reserved1;
  aerogpu_dbgctl_ring_desc_v2 desc[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
} aerogpu_escape_dump_ring_v2_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_ring_v2_inout) == (52 + (AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS * 40)));
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, ring_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, ring_format) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, ring_size_bytes) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, head) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, tail) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, desc_count) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, desc_capacity) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, reserved0) == 44);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, reserved1) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_ring_v2_inout, desc) == 52);

typedef struct aerogpu_escape_selftest_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 timeout_ms;
  aerogpu_escape_u32 passed;
  aerogpu_escape_u32 error_code;
  aerogpu_escape_u32 reserved0;
} aerogpu_escape_selftest_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_selftest_inout) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_selftest_inout, timeout_ms) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_selftest_inout, passed) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_selftest_inout, error_code) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_selftest_inout, reserved0) == 28);

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

typedef struct aerogpu_escape_query_scanout_out {
  aerogpu_escape_header hdr;
  aerogpu_escape_u32 vidpn_source_id;
  /*
   * Flags (newer KMDs):
   * - Bit 31: flags are valid.
   * - Bit 0: cached_fb_gpa is valid (requires QUERY_SCANOUT v2 output).
   *
   * This field was previously reserved; keep its name and offset for ABI stability.
   */
  aerogpu_escape_u32 reserved0;

  /* Cached values tracked by the KMD. */
  aerogpu_escape_u32 cached_enable;
  aerogpu_escape_u32 cached_width;
  aerogpu_escape_u32 cached_height;
  aerogpu_escape_u32 cached_format; /* enum aerogpu_format */
  aerogpu_escape_u32 cached_pitch_bytes;

  /* MMIO scanout registers (best-effort; 0 if not available). */
  aerogpu_escape_u32 mmio_enable;
  aerogpu_escape_u32 mmio_width;
  aerogpu_escape_u32 mmio_height;
  aerogpu_escape_u32 mmio_format;
  aerogpu_escape_u32 mmio_pitch_bytes;
  aerogpu_escape_u64 mmio_fb_gpa;
} aerogpu_escape_query_scanout_out;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_scanout_out) == 72);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, vidpn_source_id) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, reserved0) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, cached_enable) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, cached_width) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, cached_height) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, cached_format) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, cached_pitch_bytes) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_enable) == 44);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_width) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_height) == 52);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_format) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_pitch_bytes) == 60);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out, mmio_fb_gpa) == 64);

#define AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID (1u << 0)

/*
 * Query scanout response (v2).
 *
 * This extends `aerogpu_escape_query_scanout_out` by appending cached scanout
 * framebuffer GPA. Tooling must check `hdr.size` before reading appended fields.
 */
typedef struct aerogpu_escape_query_scanout_out_v2 {
  aerogpu_escape_query_scanout_out base;
  aerogpu_escape_u64 cached_fb_gpa;
} aerogpu_escape_query_scanout_out_v2;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_scanout_out_v2) == 80);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_scanout_out_v2, cached_fb_gpa) == 72);

typedef struct aerogpu_escape_query_cursor_out {
  aerogpu_escape_header hdr;
  /*
   * Flags:
   * - Bit 31: flags are valid (newer KMDs). If clear, tooling should assume the
   *   cursor MMIO registers are supported because older KMDs would only return
   *   success on devices that implemented the cursor register block.
   * - Bit 0: cursor MMIO registers are supported/valid.
   */
  aerogpu_escape_u32 flags;
  aerogpu_escape_u32 reserved0;

  /* MMIO cursor registers (best-effort; 0 if not available). */
  aerogpu_escape_u32 enable;
  aerogpu_escape_u32 x;     /* signed 32-bit */
  aerogpu_escape_u32 y;     /* signed 32-bit */
  aerogpu_escape_u32 hot_x;
  aerogpu_escape_u32 hot_y;
  aerogpu_escape_u32 width;
  aerogpu_escape_u32 height;
  aerogpu_escape_u32 format; /* enum aerogpu_format */
  aerogpu_escape_u64 fb_gpa;
  aerogpu_escape_u32 pitch_bytes;
  aerogpu_escape_u32 reserved1;
} aerogpu_escape_query_cursor_out;

#define AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED (1u << 0)

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_cursor_out) == 72);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, flags) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, reserved0) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, enable) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, x) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, y) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, hot_x) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, hot_y) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, width) == 44);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, height) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, format) == 52);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, fb_gpa) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, pitch_bytes) == 64);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_cursor_out, reserved1) == 68);

typedef struct aerogpu_escape_query_error_out {
  aerogpu_escape_header hdr;
  /*
   * Flags:
   * - Bit 31: flags are valid (newer KMDs).
   * - Bit 0: error state is supported.
   *   - If the device exposes optional MMIO error registers, fields are sourced from them.
   *   - Otherwise fields are best-effort from the KMD's IRQ_ERROR latch/counters.
   *   - Even when MMIO error registers are present, the KMD may avoid reading them during
   *     power-transition / resume windows; in that case it returns the most recent cached
   *     telemetry (best-effort).
   * - Bit 1: IRQ_ERROR is currently latched (device is in a device-lost state).
   */
  aerogpu_escape_u32 flags;
  aerogpu_escape_u32 error_code; /* enum aerogpu_error_code */
  aerogpu_escape_u64 error_fence;
  aerogpu_escape_u32 error_count;
  aerogpu_escape_u32 reserved0;
} aerogpu_escape_query_error_out;

#define AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID (1u << 31)
#define AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED (1u << 0)
#define AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_LATCHED (1u << 1)

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_query_error_out) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_error_out, flags) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_error_out, error_code) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_error_out, error_fence) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_error_out, error_count) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_query_error_out, reserved0) == 36);

typedef struct aerogpu_escape_read_gpa_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u64 gpa;
  aerogpu_escape_u32 size_bytes;
  aerogpu_escape_u32 reserved0;

  /* Output fields (filled by the KMD). */
  aerogpu_escape_u32 status;       /* NTSTATUS */
  aerogpu_escape_u32 bytes_copied; /* <= AEROGPU_DBGCTL_READ_GPA_MAX_BYTES */
  unsigned char data[AEROGPU_DBGCTL_READ_GPA_MAX_BYTES];
} aerogpu_escape_read_gpa_inout;

/* Must remain stable across x86/x64. */
AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_read_gpa_inout) == (40 + AEROGPU_DBGCTL_READ_GPA_MAX_BYTES));
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, gpa) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, size_bytes) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, reserved0) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, status) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, bytes_copied) == 36);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_read_gpa_inout, data) == 40);

/*
 * Recent CreateAllocation trace entry (DxgkDdiCreateAllocation inputs/outputs).
 *
 * This is intended to capture the exact `DXGK_ALLOCATIONINFO::Flags.Value`
 * values the Win7 runtime requests (and the final flags after the KMD applies
 * required bits like CpuVisible/Aperture), without requiring a kernel debugger.
 */
typedef struct aerogpu_dbgctl_createallocation_desc {
  aerogpu_escape_u32 seq;             /* Monotonic entry sequence number (KMD local). */
  aerogpu_escape_u32 call_seq;        /* Monotonic CreateAllocation call sequence number (KMD local). */
  aerogpu_escape_u32 alloc_index;     /* Allocation index within the CreateAllocation call. */
  aerogpu_escape_u32 num_allocations; /* Total allocations in the CreateAllocation call. */
  aerogpu_escape_u32 create_flags;    /* DXGKARG_CREATEALLOCATION::Flags.Value */
  aerogpu_escape_u32 alloc_id;        /* AeroGPU alloc_id (UMD-provided or synthesized). */
  aerogpu_escape_u32 priv_flags;      /* aerogpu_wddm_alloc_private_data.flags (0 if absent). */
  aerogpu_escape_u32 pitch_bytes;     /* Optional pitch for linear surfaces (0 if unknown). */
  aerogpu_escape_u64 share_token;     /* Protocol share_token (0 for non-shared). */
  aerogpu_escape_u64 size_bytes;      /* DXGK_ALLOCATIONINFO::Size */
  aerogpu_escape_u32 flags_in;        /* DXGK_ALLOCATIONINFO::Flags.Value (incoming). */
  aerogpu_escape_u32 flags_out;       /* DXGK_ALLOCATIONINFO::Flags.Value (after KMD edits). */
} aerogpu_dbgctl_createallocation_desc;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_dbgctl_createallocation_desc) == 56);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, seq) == 0);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, call_seq) == 4);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, alloc_index) == 8);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, num_allocations) == 12);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, create_flags) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, alloc_id) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, priv_flags) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, pitch_bytes) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, share_token) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, size_bytes) == 40);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, flags_in) == 48);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_dbgctl_createallocation_desc, flags_out) == 52);

typedef struct aerogpu_escape_dump_createallocation_inout {
  aerogpu_escape_header hdr;
  /*
   * Monotonic KMD write index (total entries written).
   *
   * Tooling can use this to detect whether the log wrapped between dumps.
   */
  aerogpu_escape_u32 write_index;
  aerogpu_escape_u32 entry_count;
  aerogpu_escape_u32 entry_capacity;
  aerogpu_escape_u32 reserved0;
  aerogpu_dbgctl_createallocation_desc entries[AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS];
} aerogpu_escape_dump_createallocation_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_dump_createallocation_inout) ==
                             (32 + (AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS * 56)));
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_createallocation_inout, write_index) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_createallocation_inout, entry_count) == 20);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_createallocation_inout, entry_capacity) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_createallocation_inout, reserved0) == 28);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_dump_createallocation_inout, entries) == 32);

typedef struct aerogpu_escape_map_shared_handle_inout {
  aerogpu_escape_header hdr;
  aerogpu_escape_u64 shared_handle;
  /*
   * Debug-only 32-bit token for mapping a process-local NT handle to a stable
   * value for bring-up tooling. This is NOT the `u64 share_token` used by
   * `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`.
   *
   * Field naming note:
   * - Prefer `debug_token` in new code.
   * - Keep `share_token` as a legacy alias (older code used that field name).
   */
  union {
    aerogpu_escape_u32 debug_token;
    aerogpu_escape_u32 share_token;
  };
  aerogpu_escape_u32 reserved0;
} aerogpu_escape_map_shared_handle_inout;

AEROGPU_DBGCTL_STATIC_ASSERT(sizeof(aerogpu_escape_map_shared_handle_inout) == 32);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_map_shared_handle_inout, shared_handle) == 16);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_map_shared_handle_inout, share_token) == 24);
AEROGPU_DBGCTL_STATIC_ASSERT(offsetof(aerogpu_escape_map_shared_handle_inout, reserved0) == 28);

#pragma pack(pop)

#ifdef __cplusplus
}
#endif
