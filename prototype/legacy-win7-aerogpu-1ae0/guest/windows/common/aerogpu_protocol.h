#pragma once

// AeroGPU guest↔host ABI (minimal v1)
//
// This header is shared by the Windows kernel-mode miniport (KMD) and the
// Direct3D 9 user-mode driver (UMD). The goal of v1 is to be "just enough" for
// a Win7 D3D9Ex stack to submit work to the host via a paravirtual command ring.
//
// NOTE: The actual PCI IDs / MMIO layout must match the host device model
// (Task 51). The values below are the project defaults used by the guest stack;
// update them in lockstep with the host.

#include <stdint.h>

// -----------------------------------------------------------------------------
// PCI identification (must match host device model)
// -----------------------------------------------------------------------------

#define AEROGPU_PCI_VENDOR_ID 0x1AE0u
#define AEROGPU_PCI_DEVICE_ID 0x0001u

// -----------------------------------------------------------------------------
// MMIO register layout (BAR0)
// -----------------------------------------------------------------------------

// Common
#define AEROGPU_REG_DEVICE_ID 0x0000u // RO: 'AERO' (0x4F524541) for sanity
#define AEROGPU_REG_VERSION 0x0004u   // RO: protocol version (AEROGPU_PROTOCOL_VERSION)

// Command ring configuration
#define AEROGPU_REG_RING_GPA_LO 0x0100u // RW
#define AEROGPU_REG_RING_GPA_HI 0x0104u // RW
#define AEROGPU_REG_RING_SIZE 0x0108u   // RW (bytes, power-of-two)
#define AEROGPU_REG_RING_TAIL 0x010Cu   // WO: guest writes new tail (bytes)
#define AEROGPU_REG_RING_HEAD 0x0110u   // RO: host updates head (bytes)

// Fence completion (host -> guest)
#define AEROGPU_REG_FENCE_COMPLETED_LO 0x0200u // RO
#define AEROGPU_REG_FENCE_COMPLETED_HI 0x0204u // RO

// Interrupt status/ack (optional in v1; polling is allowed)
#define AEROGPU_REG_IRQ_STATUS 0x0300u // RO
#define AEROGPU_REG_IRQ_ACK 0x0304u    // WO

// -----------------------------------------------------------------------------
// Guest→host command stream
// -----------------------------------------------------------------------------

#define AEROGPU_PROTOCOL_VERSION 1u

// Commands are written into the ring as:
//   [aerogpu_cmd_header][payload...]
// The command size is in bytes and includes the header itself. Commands are
// naturally aligned to 8 bytes; the writer should pad as needed.

typedef struct aerogpu_cmd_header {
  uint32_t opcode;
  uint32_t size_bytes;
} aerogpu_cmd_header_t;

typedef enum aerogpu_opcode {
  // No-op; size may be used for padding.
  AEROGPU_CMD_NOP = 0x0000,

  // Payload: aerogpu_cmd_fence_signal
  AEROGPU_CMD_FENCE_SIGNAL = 0x0001,

  // Payload: aerogpu_cmd_d3d9_stream (opaque byte stream for host translator)
  AEROGPU_CMD_D3D9_STREAM = 0x0100,
} aerogpu_opcode_t;

typedef struct aerogpu_cmd_fence_signal {
  uint64_t fence_value;
} aerogpu_cmd_fence_signal_t;

// D3D9 stream is intentionally opaque at the ABI level in v1. The host-side
// translator owns this encoding.
typedef struct aerogpu_cmd_d3d9_stream {
  // Followed by `byte_count` bytes of payload.
  uint32_t byte_count;
  uint32_t reserved;
} aerogpu_cmd_d3d9_stream_t;

// -----------------------------------------------------------------------------
// Escape (UMD -> KMD) payloads
// -----------------------------------------------------------------------------

// The UMD submits work through DxgkDdiEscape using a single escape packet:
//   aerogpu_escape_packet { header + payload }
// The KMD validates and copies the payload into the AeroGPU ring.

#define AEROGPU_ESCAPE_MAGIC 0x304F5245u // 'ERO0' little-endian
#define AEROGPU_ESCAPE_VERSION 1u

typedef enum aerogpu_escape_op {
  AEROGPU_ESCAPE_SUBMIT = 1u,
  AEROGPU_ESCAPE_QUERY_CAPS = 2u,
} aerogpu_escape_op_t;

typedef struct aerogpu_escape_packet {
  uint32_t magic;   // AEROGPU_ESCAPE_MAGIC
  uint32_t version; // AEROGPU_ESCAPE_VERSION
  uint32_t op;      // aerogpu_escape_op_t
  uint32_t size_bytes;
  // Followed by op-specific payload.
} aerogpu_escape_packet_t;

typedef struct aerogpu_escape_submit {
  // Input: opaque command stream to be placed into the ring.
  // Output: written by KMD to indicate the fence value associated with this
  // submission (0 if no fence was inserted by UMD).
  uint64_t fence_value;
  uint32_t stream_bytes;
  uint32_t reserved;
  // Followed by `stream_bytes` bytes to be copied verbatim into the device ring
  // (typically a sequence of aerogpu_cmd_*).
} aerogpu_escape_submit_t;
