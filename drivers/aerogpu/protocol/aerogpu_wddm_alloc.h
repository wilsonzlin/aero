/*
 * AeroGPU WDDM allocation private-driver-data contract (Win7 / WDDM 1.1).
 *
 * This header defines the byte layout stored in WDDM "allocation private driver
 * data" buffers.
 *
 * Key semantics (Win7 WDDM 1.1):
 * - The UMD provides a private-data buffer per allocation at creation time
 *   (UMD -> dxgkrnl -> KMD).
 * - The KMD fills this blob during `DxgkDdiCreateAllocation` and again during
 *   `DxgkDdiOpenAllocation`.
 * - For shared allocations, dxgkrnl preserves and replays the blob verbatim
 *   across processes, so the opening UMD instance observes the same
 *   `alloc_id`/`share_token` values.
 *
 * IMPORTANT:
 * - The numeric value of the UMD-visible `hAllocation` handle is not the same
 *   identity the KMD later sees in DXGK_ALLOCATIONLIST. AeroGPU therefore uses
 *   a driver-defined 32-bit `alloc_id` (stored in this private-data blob) as
 *   the stable cross-layer key for a backing allocation.
 *
 * Requirements:
 * - Must be usable in kernel-mode builds and in user-mode UMD projects.
 * - Fixed-width fields and packed layout.
 * - Versioned so we can extend without breaking older binaries.
 *
 * NOTE: This header intentionally does NOT include
 * `legacy/aerogpu_protocol_legacy.h` (legacy bring-up ABI) because that header
 * defines conflicting global enum constants with the versioned protocol
 * (`aerogpu_cmd.h`). Keep this file self-contained.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_
#define AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h> /* offsetof */

#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT32 aerogpu_wddm_u32;
typedef UINT64 aerogpu_wddm_u64;
#else
#include <stdint.h>
typedef uint32_t aerogpu_wddm_u32;
typedef uint64_t aerogpu_wddm_u64;
#endif

#define AEROGPU_WDDM_ALLOC_CONCAT2_(a, b) a##b
#define AEROGPU_WDDM_ALLOC_CONCAT_(a, b) AEROGPU_WDDM_ALLOC_CONCAT2_(a, b)
#define AEROGPU_WDDM_ALLOC_STATIC_ASSERT(expr) \
  typedef char AEROGPU_WDDM_ALLOC_CONCAT_(aerogpu_wddm_alloc_static_assert_, __LINE__)[(expr) ? 1 : -1]

#define AEROGPU_WDDM_ALLOC_PRIV_MAGIC 0x414C4C4Fu /* 'A''L''L''O' */
#define AEROGPU_WDDM_ALLOC_PRIV_VERSION 1u
#define AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 2u

/* Backwards-compat aliases (older code used *_PRIVATE_DATA_* names). */
#define AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC AEROGPU_WDDM_ALLOC_PRIV_MAGIC
#define AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION AEROGPU_WDDM_ALLOC_PRIV_VERSION

/*
 * alloc_id namespace split:
 * - IDs with the high bit clear (1..0x7fffffff) are reserved for UMD-generated
 *   values.
 * - IDs with the high bit set (0x80000000..0xffffffff) are reserved for KMD
 *   internal/standard allocations where the runtime does not provide an
 *   AeroGPU private-data blob.
 *
 * This avoids collisions in the host's allocation-ID map.
 */
#define AEROGPU_WDDM_ALLOC_ID_UMD_MAX 0x7FFFFFFFu
#define AEROGPU_WDDM_ALLOC_ID_KMD_MIN 0x80000000u

enum aerogpu_wddm_alloc_private_flags {
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE = 0,
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED = (1u << 0),
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE = (1u << 1),
  AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING = (1u << 2),
};

#define AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED

/*
 * Optional resource description encoding (reserved0).
 *
 * Win7/WDDM 1.1 does not guarantee that the D3D9 UMD OpenResource DDI provides
 * enough information to reconstruct a shareable surface's format/width/height
 * in a different process. However, dxgkrnl preserves the per-allocation private
 * driver data blob for shared allocations and returns it verbatim when another
 * process opens the resource.
 *
 * AeroGPU uses the `reserved0` field to optionally encode a minimal, portable
 * surface description so the UMD can reconstruct the resource at OpenResource
 * time without relying on header-specific DDI fields.
 *
 * Layout (little-endian bit numbering):
 *   bit 63: marker (1 == description present)
 *   bits 0..31:  D3D9 format (u32, numeric D3DFORMAT value)
 *   bits 32..47: width  (u16)
 *   bits 48..62: height (u15, max 32767; sufficient for Win7-era surfaces)
 */
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER 0x8000000000000000ull
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH 0xFFFFu
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT 0x7FFFu

#define AEROGPU_WDDM_ALLOC_PRIV_DESC_PACK(format_u32, width_u32, height_u32)                                      \
  (AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER |                                                                          \
   ((aerogpu_wddm_u64)((aerogpu_wddm_u32)(format_u32)) & 0xFFFFFFFFull) |                                         \
   (((aerogpu_wddm_u64)((aerogpu_wddm_u32)(width_u32)) & 0xFFFFull) << 32) |                                      \
   (((aerogpu_wddm_u64)((aerogpu_wddm_u32)(height_u32)) & 0x7FFFull) << 48))

#define AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(desc_u64) (((aerogpu_wddm_u64)(desc_u64) & AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER) != 0)
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_FORMAT(desc_u64) ((aerogpu_wddm_u32)((aerogpu_wddm_u64)(desc_u64) & 0xFFFFFFFFull))
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_WIDTH(desc_u64) ((aerogpu_wddm_u32)(((aerogpu_wddm_u64)(desc_u64) >> 32) & 0xFFFFull))
#define AEROGPU_WDDM_ALLOC_PRIV_DESC_HEIGHT(desc_u64) ((aerogpu_wddm_u32)(((aerogpu_wddm_u64)(desc_u64) >> 48) & 0x7FFFull))

enum aerogpu_wddm_alloc_kind {
  AEROGPU_WDDM_ALLOC_KIND_UNKNOWN = 0,
  AEROGPU_WDDM_ALLOC_KIND_BUFFER = 1,
  AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D = 2,
};

#pragma pack(push, 1)
typedef struct aerogpu_wddm_alloc_priv {
  aerogpu_wddm_u32 magic;   /* AEROGPU_WDDM_ALLOC_PRIV_MAGIC */
  aerogpu_wddm_u32 version; /* AEROGPU_WDDM_ALLOC_PRIV_VERSION */

  /*
   * Allocation ID used by the guest↔host allocation table.
   * 0 is reserved/invalid.
   *
   * For shared allocations, alloc_id should be unique across guest processes
   * because DWM may reference many redirected surfaces from different processes
   * in a single submission. alloc_id collisions must be treated as fatal
   * validation errors (never silently alias distinct allocations).
   */
  aerogpu_wddm_u32 alloc_id;

  aerogpu_wddm_u32 flags; /* aerogpu_wddm_alloc_private_flags */

  /*
   * Stable cross-process token used by `EXPORT_SHARED_SURFACE` /
   * `IMPORT_SHARED_SURFACE`.
   *
   * For shared allocations, the KMD generates a stable non-zero token and
   * writes it here during `DxgkDdiCreateAllocation`. dxgkrnl preserves the
   * allocation private driver data bytes and returns them verbatim when another
   * process opens the shared resource, allowing the opening UMD instance to
   * recover the same token.
   *
   * Collision policy:
   * - share_token is treated as a globally unique identifier on the host.
   * - share_token == 0 is reserved/invalid.
   *
   * Must be 0 for non-shared allocations (KMD rejects non-zero tokens when the
   * shared flag is not set).
   *
   * Do NOT derive this from the numeric value of the user-mode shared `HANDLE`:
   * for real NT handles it is process-local (commonly different after
   * `DuplicateHandle`), and even token-style shared handles must not be treated
   * as stable protocol keys.
   */
  aerogpu_wddm_u64 share_token;

  /*
   * Allocation size (bytes).
   *
   * This is required for KMD OpenAllocation wrappers to be able to reconstruct
   * per-allocation bookkeeping without querying the original process.
   */
  aerogpu_wddm_u64 size_bytes;

  /*
   * Reserved for UMD/KMD extensions.
   *
   * Current uses:
   * - D3D9 shared-surface description encoding:
   *     bit63 == 1 (see AEROGPU_WDDM_ALLOC_PRIV_DESC_* macros above).
   * - Optional pitch metadata for linear surface allocations:
   *     bit63 == 0 and bits[31:0] = row pitch in bytes, or 0 if unknown.
   *
   * Keep this field backward-compatible: older binaries may leave it as 0.
   */
  aerogpu_wddm_u64 reserved0;
} aerogpu_wddm_alloc_priv;

typedef aerogpu_wddm_alloc_priv aerogpu_wddm_alloc_private_data;
#pragma pack(pop)

AEROGPU_WDDM_ALLOC_STATIC_ASSERT(sizeof(aerogpu_wddm_alloc_priv) == 40);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, alloc_id) == 8);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, share_token) == 16);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, reserved0) == 32);

#pragma pack(push, 1)
typedef struct aerogpu_wddm_alloc_priv_v2 {
  aerogpu_wddm_u32 magic;   /* AEROGPU_WDDM_ALLOC_PRIV_MAGIC */
  aerogpu_wddm_u32 version; /* AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 */

  aerogpu_wddm_u32 alloc_id; /* driver-defined; 0 is invalid */
  aerogpu_wddm_u32 flags;    /* enum aerogpu_wddm_alloc_private_flags */

  aerogpu_wddm_u64 share_token; /* stable cross-process token (shared resources) */
  aerogpu_wddm_u64 size_bytes;  /* allocation size (bytes) */

  /* See `aerogpu_wddm_alloc_priv.reserved0` (UMD/KMD extension field). */
  aerogpu_wddm_u64 reserved0;

  aerogpu_wddm_u32 kind; /* enum aerogpu_wddm_alloc_kind */

  /* Texture metadata (0 for buffers). */
  aerogpu_wddm_u32 width;
  aerogpu_wddm_u32 height;
  aerogpu_wddm_u32 format;         /* DXGI_FORMAT numeric value */
  aerogpu_wddm_u32 row_pitch_bytes; /* linear row pitch; 0 if unknown */

  aerogpu_wddm_u32 reserved1; /* must be zero */
} aerogpu_wddm_alloc_priv_v2;
#pragma pack(pop)

AEROGPU_WDDM_ALLOC_STATIC_ASSERT(sizeof(aerogpu_wddm_alloc_priv_v2) == 64);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv_v2, alloc_id) == 8);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv_v2, share_token) == 16);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv_v2, reserved0) == 32);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv_v2, kind) == 40);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv_v2, row_pitch_bytes) == 56);

#ifdef __cplusplus
} /* extern "C" */
#endif
#endif /* AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_ */
