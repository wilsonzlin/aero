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

#include <stddef.h>
#include <stdint.h>

/* -------------------------- Compile-time utilities ----------------------- */

#define AEROGPU_ESCAPE_CONCAT2_(a, b) a##b
#define AEROGPU_ESCAPE_CONCAT_(a, b) AEROGPU_ESCAPE_CONCAT2_(a, b)
#define AEROGPU_ESCAPE_STATIC_ASSERT(expr) \
  typedef char AEROGPU_ESCAPE_CONCAT_(aerogpu_escape_static_assert_, __LINE__)[(expr) ? 1 : -1]

/* ------------------------------- Header ---------------------------------- */

#define AEROGPU_ESCAPE_VERSION 1u

/* Base Escape ops (shared between tooling and the KMD). */
#define AEROGPU_ESCAPE_OP_QUERY_DEVICE 1u

#pragma pack(push, 1)

typedef struct aerogpu_escape_header {
  uint32_t version; /* AEROGPU_ESCAPE_VERSION */
  uint32_t op;      /* AEROGPU_ESCAPE_OP_* */
  uint32_t size;    /* total size including this header */
  uint32_t reserved0;
} aerogpu_escape_header;

AEROGPU_ESCAPE_STATIC_ASSERT(sizeof(aerogpu_escape_header) == 16);
AEROGPU_ESCAPE_STATIC_ASSERT(offsetof(aerogpu_escape_header, version) == 0);
AEROGPU_ESCAPE_STATIC_ASSERT(offsetof(aerogpu_escape_header, op) == 4);
AEROGPU_ESCAPE_STATIC_ASSERT(offsetof(aerogpu_escape_header, size) == 8);
AEROGPU_ESCAPE_STATIC_ASSERT(offsetof(aerogpu_escape_header, reserved0) == 12);

/* ---------------------------- Query device -------------------------------- */

/*
 * Output for AEROGPU_ESCAPE_OP_QUERY_DEVICE.
 *
 * `mmio_version` is the device's canonical MMIO ABI version, i.e. the 32-bit
 * value read from MMIO register `AEROGPU_MMIO_REG_ABI_VERSION` on AGPU devices.
 *
 * It uses a major.minor encoding:
 *   major = (mmio_version >> 16)
 *   minor = (mmio_version & 0xFFFF)
 *
 * The field name is kept as `mmio_version` for backwards compatibility with
 * existing dbgctl tooling.
 */
typedef struct aerogpu_escape_query_device_out {
  aerogpu_escape_header hdr;
  uint32_t mmio_version;
  uint32_t reserved0;
} aerogpu_escape_query_device_out;

AEROGPU_ESCAPE_STATIC_ASSERT(sizeof(aerogpu_escape_query_device_out) == 24);
AEROGPU_ESCAPE_STATIC_ASSERT(offsetof(aerogpu_escape_query_device_out, mmio_version) == 16);
#pragma pack(pop)

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_ESCAPE_H_ */
