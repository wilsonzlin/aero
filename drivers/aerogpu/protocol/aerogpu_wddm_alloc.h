/*
 * AeroGPU WDDM allocation private-driver-data contract (Win7 WDDM 1.1).
 *
 * This header defines the byte layout stored in WDDM "allocation private driver
 * data" buffers.
 *
 * Key semantics (Win7 WDDM 1.1):
 * - The UMD provides an opaque private-data blob per allocation at creation
 *   time (UMD -> dxgkrnl -> KMD).
 * - dxgkrnl preserves that blob for shared allocations and returns the exact
 *   bytes back to a different process when it opens the shared resource.
 * - The KMD must treat this blob as INPUT (UMD -> KMD). Do not rely on writing
 *   into the buffer during DxgkDdiCreateAllocation expecting the UMD to observe
 *   a writeback; that is not a stable contract for Win7 WDDM 1.1.
 *
 * Requirements:
 * - Must be usable in WDK 7.1 kernel-mode builds and in user-mode UMD projects.
 * - Fixed-width fields and packed layout.
 * - Versioned so we can extend without breaking older binaries.
 *
 * NOTE: This header intentionally does NOT include `aerogpu_protocol.h` (legacy
 * bring-up ABI) because that header defines conflicting global enum constants
 * with the versioned protocol (`aerogpu_cmd.h`). Keep this file self-contained.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_
#define AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Fixed-width types without depending on C99 stdint in kernel-mode builds.
 *
 * WDK 7.1 kernel builds have UINT32/UINT64 via ntdef.h, but do not reliably
 * provide <stdint.h>.
 */
#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT32 aerogpu_wddm_u32;
typedef UINT64 aerogpu_wddm_u64;
#else
#include <stdint.h>
typedef uint32_t aerogpu_wddm_u32;
typedef uint64_t aerogpu_wddm_u64;
#endif

/* -------------------------- Compile-time utilities ------------------------ */

#define AEROGPU_WDDM_ALLOC_CONCAT2_(a, b) a##b
#define AEROGPU_WDDM_ALLOC_CONCAT_(a, b) AEROGPU_WDDM_ALLOC_CONCAT2_(a, b)
#define AEROGPU_WDDM_ALLOC_STATIC_ASSERT(expr) \
  typedef char AEROGPU_WDDM_ALLOC_CONCAT_(aerogpu_wddm_alloc_static_assert_, __LINE__)[(expr) ? 1 : -1]

#define AEROGPU_WDDM_ALLOC_PRIV_MAGIC 0x414C4C4Fu /* 'A''L''L''O' */
#define AEROGPU_WDDM_ALLOC_PRIV_VERSION 1u

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
};

#define AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED

#pragma pack(push, 1)
typedef struct aerogpu_wddm_alloc_priv {
  aerogpu_wddm_u32 magic;   /* AEROGPU_WDDM_ALLOC_PRIV_MAGIC */
  aerogpu_wddm_u32 version; /* AEROGPU_WDDM_ALLOC_PRIV_VERSION */

  /*
   * Allocation ID used by the guest↔host allocation table.
   * 0 is reserved/invalid.
   */
  aerogpu_wddm_u32 alloc_id;

  aerogpu_wddm_u32 flags; /* aerogpu_wddm_alloc_private_flags */

  /*
   * Stable token for cross-process shared-surface interop.
   *
   * When AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED is set, this must be non-zero and
   * identical for every allocation that is part of the same shared resource.
   *
   * Recommended scheme (simple, collision-resistant if alloc_id is global):
   *   share_token = (u64)alloc_id
   */
  aerogpu_wddm_u64 share_token;

  /*
   * Allocation size (bytes).
   *
   * This is required for KMD OpenAllocation wrappers to be able to reconstruct
   * per-allocation bookkeeping without querying the original process.
   */
  aerogpu_wddm_u64 size_bytes;

  aerogpu_wddm_u64 reserved0;
} aerogpu_wddm_alloc_priv;

typedef aerogpu_wddm_alloc_priv aerogpu_wddm_alloc_private_data;
#pragma pack(pop)

AEROGPU_WDDM_ALLOC_STATIC_ASSERT(sizeof(aerogpu_wddm_alloc_priv) == 40);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, alloc_id) == 8);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, share_token) == 16);
AEROGPU_WDDM_ALLOC_STATIC_ASSERT(offsetof(aerogpu_wddm_alloc_priv, reserved0) == 32);

#ifdef __cplusplus
} /* extern "C" */
#endif
#endif /* AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_ */
