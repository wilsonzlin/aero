/*
 * AeroGPU Guest↔Emulator ABI (Ring + submissions + fences)
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_RING_H_
#define AEROGPU_PROTOCOL_AEROGPU_RING_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>

#include "aerogpu_pci.h"

/* ---------------------------- Submission descriptor ---------------------- */

/*
 * Submission flow:
 * - The KMD writes an `aerogpu_submit_desc` into the ring.
 * - It updates ring->tail.
 * - It writes to MMIO `AEROGPU_MMIO_REG_DOORBELL`.
 *
 * The device processes descriptors in order, updating ring->head.
 */

enum aerogpu_submit_flags {
  AEROGPU_SUBMIT_FLAG_NONE = 0,
  AEROGPU_SUBMIT_FLAG_PRESENT = (1u << 0), /* Submission contains a PRESENT */
  AEROGPU_SUBMIT_FLAG_NO_IRQ = (1u << 1), /* Do not raise IRQ on completion */
};

enum aerogpu_engine_id {
  AEROGPU_ENGINE_0 = 0, /* Only engine currently defined */
};

/*
 * Optional sideband allocation table:
 * - The submit descriptor can reference a table mapping small allocation IDs
 *   (alloc_id) to guest physical addresses and sizes.
 * - Commands may reference allocations by alloc_id.
 */

#define AEROGPU_ALLOC_TABLE_MAGIC 0x434F4C41u /* "ALOC" little-endian */

struct aerogpu_alloc_table_header {
  uint32_t magic; /* AEROGPU_ALLOC_TABLE_MAGIC */
  uint32_t abi_version; /* AEROGPU_ABI_VERSION_U32 */
  uint32_t size_bytes; /* Total size including header + entries */
  uint32_t entry_count;
  uint32_t entry_stride_bytes; /* sizeof(struct aerogpu_alloc_entry) */
  uint32_t reserved0;
};

enum aerogpu_alloc_flags {
  AEROGPU_ALLOC_FLAG_NONE = 0,
  AEROGPU_ALLOC_FLAG_READONLY = (1u << 0), /* Host must not write to this alloc */
};

struct aerogpu_alloc_entry {
  uint32_t alloc_id; /* 0 is reserved (invalid) */
  uint32_t flags; /* aerogpu_alloc_flags */
  uint64_t gpa; /* Guest physical address */
  uint64_t size_bytes;
  uint64_t reserved0;
};

/*
 * Fixed-size submission descriptor (64 bytes).
 * All fields are little-endian.
 */
#pragma pack(push, 1)
struct aerogpu_submit_desc {
  uint32_t desc_size_bytes; /* Must be sizeof(struct aerogpu_submit_desc) */
  uint32_t flags; /* aerogpu_submit_flags */
  uint32_t context_id; /* Driver-defined (0 for now) */
  uint32_t engine_id; /* aerogpu_engine_id */

  uint64_t cmd_gpa; /* Command buffer guest physical address */
  uint32_t cmd_size_bytes; /* Command buffer size in bytes */
  uint32_t cmd_reserved0;

  uint64_t alloc_table_gpa; /* 0 if not present */
  uint32_t alloc_table_size_bytes; /* 0 if not present */
  uint32_t alloc_table_reserved0;

  uint64_t signal_fence; /* Fence value to signal on completion */
  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_submit_desc) == 64);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_submit_desc, cmd_gpa) == 16);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_submit_desc, alloc_table_gpa) == 32);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_submit_desc, signal_fence) == 48);

/* ------------------------------- Ring layout ----------------------------- */

#define AEROGPU_RING_MAGIC 0x474E5241u /* "ARNG" little-endian */

/*
 * The ring is a contiguous guest memory region starting at RING_GPA.
 *
 * Layout:
 *   [aerogpu_ring_header]
 *   [entry 0: aerogpu_submit_desc]
 *   [entry 1: aerogpu_submit_desc]
 *   ...
 *
 * `head` and `tail` are monotonically increasing indices (not masked).
 * The actual slot is `(index % entry_count)`.
 */
#pragma pack(push, 1)
struct aerogpu_ring_header {
  uint32_t magic; /* AEROGPU_RING_MAGIC */
  uint32_t abi_version; /* AEROGPU_ABI_VERSION_U32 */
  uint32_t size_bytes; /* Total bytes of the ring mapping */
  uint32_t entry_count; /* Number of slots; must be power-of-two */
  uint32_t entry_stride_bytes; /* sizeof(struct aerogpu_submit_desc) */
  uint32_t flags;
  volatile uint32_t head; /* device-owned */
  volatile uint32_t tail; /* driver-owned */
  uint32_t reserved0;
  uint32_t reserved1;
  uint64_t reserved2[3];
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_ring_header) == 64);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_ring_header, head) == 24);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_ring_header, tail) == 28);

/* ------------------------------ Fence page ------------------------------- */

#define AEROGPU_FENCE_PAGE_MAGIC 0x434E4546u /* "FENC" little-endian */

/*
 * Optional shared fence page. If `AEROGPU_MMIO_REG_FENCE_GPA_*` is programmed
 * and the device reports AEROGPU_FEATURE_FENCE_PAGE, the device writes the
 * completed fence value here (in addition to MMIO COMPLETED_FENCE_*).
 *
 * The page should be a single 4 KiB guest page.
 */
#pragma pack(push, 1)
struct aerogpu_fence_page {
  uint32_t magic; /* AEROGPU_FENCE_PAGE_MAGIC */
  uint32_t abi_version; /* AEROGPU_ABI_VERSION_U32 */
  volatile uint64_t completed_fence;
  uint64_t reserved0[5];
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_fence_page) == 56);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_fence_page, completed_fence) == 8);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_RING_H_ */
