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
 * Optional sideband allocation table (per-submit):
 *
 * Motivation:
 * - AeroGPU command packets can reference guest-backed memory via `alloc_id`
 *   (`backing_alloc_id` in CREATE_BUFFER/CREATE_TEXTURE2D).
 * - The host must be able to resolve `alloc_id -> (GPA, size, flags)` for the
 *   *current submission*, because WDDM may remap allocations between submits.
 *
 * alloc_id namespaces and stability (see `aerogpu_wddm_alloc.h`):
 * - alloc_id 0 is reserved/invalid.
 * - 1..0x7fffffff: UMD-owned namespace. IDs MUST be stable for the lifetime of
 *   the underlying WDDM allocation and collision-resistant across guest
 *   processes (DWM may reference allocations from many processes in one
 *   submission).
 * - 0x80000000..0xffffffff: reserved for KMD-synthesised IDs when the runtime
 *   creates allocations without an AeroGPU private-data blob.
 * - Multiple WDDM handles may alias the same underlying allocation (e.g.
 *   CreateAllocation vs OpenAllocation). Aliases MUST share the same alloc_id.
 *   The per-submit allocation table is keyed by alloc_id: the KMD must
 *   deduplicate identical aliases and fail the submission if the same alloc_id
 *   maps to different GPAs.
 *
 * Table format:
 * - The submit descriptor points to `alloc_table_gpa/alloc_table_size_bytes`.
 * - The table is:
 *     [aerogpu_alloc_table_header]
 *     [entry 0: aerogpu_alloc_entry]
 *     [entry 1: aerogpu_alloc_entry]
 *     ...
 * - `aerogpu_alloc_table_header::size_bytes` is the total size including header
 *   + entries and MUST be <= `alloc_table_size_bytes` from the descriptor.
 *
 * Host validation rules (when alloc_table is present):
 * - `alloc_table_gpa` and `alloc_table_size_bytes` must be both zero (absent) or
 *   both non-zero (present).
 * - header.magic must equal AEROGPU_ALLOC_TABLE_MAGIC.
 * - ABI major version must match. Minor may be newer.
 * - header.entry_stride_bytes must be >= sizeof(struct aerogpu_alloc_entry).
 *   - Newer ABI minor versions may extend `aerogpu_alloc_entry` by increasing the stride and
 *     appending fields. Hosts must ignore unknown trailing bytes.
 * - header.entry_count * header.entry_stride_bytes must fit within
 *   header.size_bytes.
 * - Each entry must have alloc_id != 0, size_bytes != 0, and gpa+size_bytes
 *   must not overflow.
 *   - Note: gpa itself may be 0 (backing beginning at physical address 0 is valid).
 * - alloc_id values must be unique within a table (duplicates are a validation
 *   error).
 * - The host must reject (validation error) any command that requires `alloc_id`
 *   resolution if the table is absent or does not contain that alloc_id. This
 *   includes:
 *   - Packets that carry `backing_alloc_id` fields directly (`CREATE_BUFFER`,
 *     `CREATE_TEXTURE2D`).
 *   - Packets that operate on a *guest-backed resource* and require host access
 *     to guest memory, such as `RESOURCE_DIRTY_RANGE` and `COPY_* WRITEBACK_DST`.
 *
 * Backing layout (see `aerogpu_cmd.h`):
 * - backing_offset_bytes is relative to the alloc table entry's base GPA.
 * - For buffers: the backing range is
 *     [backing_offset_bytes, backing_offset_bytes + size_bytes).
 * - For textures: backing memory is linear with `row_pitch_bytes` bytes per row
 *   and `height` rows starting at backing_offset_bytes.
 *
 * READONLY:
 * - The host must not write to guest backing memory for allocations marked
 *   AEROGPU_ALLOC_FLAG_READONLY. Any command that would cause guest-memory
 *   writeback to a READONLY allocation must be rejected.
 * - On Windows 7 (WDDM 1.1), the KMD derives READONLY per submission from the
 *   allocation list entry's write-access metadata (`WriteOperation` bit;
 *   `DXGK_ALLOCATIONLIST::Flags.Value & 0x1`).
 *
 * Fence ordering:
 * - The host must only advance `completed_fence` for a submission after all
 *   requested guest-memory writebacks are complete and visible to the guest.
 */

#define AEROGPU_ALLOC_TABLE_MAGIC 0x434F4C41u /* "ALOC" little-endian */

struct aerogpu_alloc_table_header {
  uint32_t magic; /* AEROGPU_ALLOC_TABLE_MAGIC */
  uint32_t abi_version; /* AEROGPU_ABI_VERSION_U32 */
  uint32_t size_bytes; /* Total size including header + entries */
  uint32_t entry_count;
  uint32_t entry_stride_bytes; /* >= sizeof(struct aerogpu_alloc_entry) */
  uint32_t reserved0;
};

enum aerogpu_alloc_flags {
  AEROGPU_ALLOC_FLAG_NONE = 0,
  /*
   * Host must not write to this allocation's guest backing memory.
   * The host should reject any command that requests a guest-memory writeback
   * to an allocation marked READONLY.
   */
  AEROGPU_ALLOC_FLAG_READONLY = (1u << 0),
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
 *
 * Descriptor validation:
 * - `cmd_gpa` and `cmd_size_bytes` must be both zero (empty submission) or both non-zero.
 * - When `cmd_gpa/cmd_size_bytes` are non-zero, `cmd_gpa + cmd_size_bytes` must not overflow.
 * - `alloc_table_gpa` and `alloc_table_size_bytes` must be both zero (absent) or both non-zero
 *   (present).
 * - When `alloc_table_gpa/alloc_table_size_bytes` are non-zero, the range must be valid:
 *   `alloc_table_gpa + alloc_table_size_bytes` must not overflow.
 */
#pragma pack(push, 1)
struct aerogpu_submit_desc {
  /*
   * Forward-compat: treat as a minimum size so newer ABI minor versions can append fields.
   * The ring header's `entry_stride_bytes` must be >= desc_size_bytes.
   */
  uint32_t desc_size_bytes; /* >= sizeof(struct aerogpu_submit_desc) */
  uint32_t flags; /* aerogpu_submit_flags */
  uint32_t context_id; /* Driver-defined (0 == default/unknown) */
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
  /*
   * Total bytes used by the ring layout.
   *
   * Forward-compat: treat as a minimum so the MMIO-programmed ring mapping
   * (`AEROGPU_MMIO_REG_RING_SIZE_BYTES`) may be larger (page rounding, future
   * extension space). The device validates `size_bytes <= RING_SIZE_BYTES`.
   */
  uint32_t size_bytes;
  uint32_t entry_count; /* Number of slots; must be power-of-two */
  uint32_t entry_stride_bytes; /* >= sizeof(struct aerogpu_submit_desc) */
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
