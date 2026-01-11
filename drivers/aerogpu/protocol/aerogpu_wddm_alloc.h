#ifndef AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_
#define AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_

/*
 * AeroGPU WDDM allocation private data contract (KMD <-> UMD).
 *
 * This structure is written by the WDDM KMD in DxgkDdiCreateAllocation and
 * persisted by dxgkrnl for later DxgkDdiOpenAllocation calls when a shared
 * allocation is opened in another process (e.g. DWM redirected surfaces).
 *
 * Requirements:
 * - Must be usable in WDK 7.1 kernel-mode builds and in user-mode UMD projects.
 * - Fixed-width fields and packed layout.
 * - Versioned so we can extend without breaking older binaries.
 *
 * NOTE: This header intentionally does NOT include `aerogpu_protocol.h` (legacy
 * bring-up ABI) because that header defines conflicting global enum constants
 * (e.g. AEROGPU_CMD_*) with the versioned protocol (`aerogpu_cmd.h`). Keep this
 * file self-contained so both the legacy KMD and the new UMDs can include it.
 */

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>

/* Fixed-width types (WDK 7.1 doesn't guarantee stdint.h in kernel-mode). */
#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT32 aerogpu_wddm_u32;
typedef UINT64 aerogpu_wddm_u64;
#else
#include <stdint.h>
typedef uint32_t aerogpu_wddm_u32;
typedef uint64_t aerogpu_wddm_u64;
#endif

#define AEROGPU_WDDM_ALLOC_PRIV_MAGIC 0x414C4C4Fu /* 'A''L''L''O' */
#define AEROGPU_WDDM_ALLOC_PRIV_VERSION 1u

/* aerogpu_wddm_alloc_priv::flags */
#define AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED 0x00000001u

#pragma pack(push, 1)
typedef struct aerogpu_wddm_alloc_priv {
  aerogpu_wddm_u32 magic;
  aerogpu_wddm_u32 version;

  /* Stable 32-bit allocation ID. 0 is reserved/invalid. */
  aerogpu_wddm_u32 alloc_id;

  /* AEROGPU_WDDM_ALLOC_PRIV_FLAG_* */
  aerogpu_wddm_u32 flags;

  /*
   * Stable share token for cross-process opens. 0 if the allocation is not
   * shared. Recommended scheme: share_token = (u64)alloc_id.
   */
  aerogpu_wddm_u64 share_token;

  /* Allocation size, used to sanity-check OpenAllocation. */
  aerogpu_wddm_u64 size_bytes;

  aerogpu_wddm_u64 reserved0;
} aerogpu_wddm_alloc_priv;
#pragma pack(pop)

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_WDDM_ALLOC_H_ */
