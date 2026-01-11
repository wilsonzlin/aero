/*
 * AeroGPU Escape ABI (DxgkDdiEscape / D3DKMTEscape)
 *
 * This header defines a small, driver-private Escape protocol intended for
 * bring-up/debug tools. It is deliberately decoupled from the device ABI
 * (legacy ARGP vs new AGPU) so tools can remain usable while the stack migrates.
 *
 * Stability requirements:
 * - Escape packets must have a stable layout across x86/x64 because a 32-bit
 *   user-mode tool may send escapes to a 64-bit kernel.
 * - All structs are packed and contain no pointers.
 * - All fields are little-endian.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_ESCAPE_H_
#define AEROGPU_PROTOCOL_AEROGPU_ESCAPE_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>

/* -------------------------- Compile-time utilities ----------------------- */

#define AEROGPU_ESCAPE_CONCAT2_(a, b) a##b
#define AEROGPU_ESCAPE_CONCAT_(a, b) AEROGPU_ESCAPE_CONCAT2_(a, b)
#define AEROGPU_ESCAPE_STATIC_ASSERT(expr) \
  typedef char AEROGPU_ESCAPE_CONCAT_(aerogpu_escape_static_assert_, __LINE__)[(expr) ? 1 : -1]

/* ------------------------------- Header ---------------------------------- */

#define AEROGPU_ESCAPE_VERSION 1u

enum aerogpu_escape_op {
  /*
   * Legacy query-device operation retained for backwards compatibility with
   * early bring-up tools. New tools should prefer QUERY_DEVICE_V2.
   */
  AEROGPU_ESCAPE_OP_QUERY_DEVICE = 1u,

  /* Extended device info (dual-ABI). */
  AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2 = 7u,
};

#pragma pack(push, 1)

typedef struct aerogpu_escape_header {
  uint32_t version; /* AEROGPU_ESCAPE_VERSION */
  uint32_t op;      /* enum aerogpu_escape_op */
  uint32_t size;    /* total size including this header */
  uint32_t reserved0;
} aerogpu_escape_header;

AEROGPU_ESCAPE_STATIC_ASSERT(sizeof(aerogpu_escape_header) == 16);

/* ---------------------------- Query device -------------------------------- */

/* Legacy v1 response. */
typedef struct aerogpu_escape_query_device_out {
  aerogpu_escape_header hdr;
  uint32_t mmio_version;
  uint32_t reserved0;
} aerogpu_escape_query_device_out;

AEROGPU_ESCAPE_STATIC_ASSERT(sizeof(aerogpu_escape_query_device_out) == 24);

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
} aerogpu_escape_query_device_v2_out;

AEROGPU_ESCAPE_STATIC_ASSERT(sizeof(aerogpu_escape_query_device_v2_out) == 48);

#pragma pack(pop)

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_ESCAPE_H_ */
