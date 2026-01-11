#pragma once

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
 */

#include "aerogpu_protocol.h"

#ifdef __cplusplus
extern "C" {
#endif

#define AEROGPU_WDDM_ALLOC_PRIV_MAGIC 0x414C4C4Fu /* 'A''L''L''O' */
#define AEROGPU_WDDM_ALLOC_PRIV_VERSION 1u

/* aerogpu_wddm_alloc_priv::flags */
#define AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED 0x00000001u

#pragma pack(push, 1)
typedef struct aerogpu_wddm_alloc_priv {
  aerogpu_u32 magic;
  aerogpu_u32 version;

  /* Stable 32-bit allocation ID. 0 is reserved/invalid. */
  aerogpu_u32 alloc_id;

  /* AEROGPU_WDDM_ALLOC_PRIV_FLAG_* */
  aerogpu_u32 flags;

  /*
   * Stable share token for cross-process opens. 0 if the allocation is not
   * shared. Recommended scheme: share_token = (u64)alloc_id.
   */
  aerogpu_u64 share_token;

  /* Allocation size, used to sanity-check OpenAllocation. */
  aerogpu_u64 size_bytes;

  aerogpu_u64 reserved0;
} aerogpu_wddm_alloc_priv;
#pragma pack(pop)

#ifdef __cplusplus
} // extern "C"
#endif

