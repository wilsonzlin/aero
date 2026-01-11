/*
 * AeroGPU WDDM allocation private driver data (KMD → UMD).
 *
 * This header defines the stable, pointer-free payload returned by the AeroGPU
 * Windows 7 KMD in the allocation private driver data for shareable allocations
 * (DxgkDdiCreateAllocation / DxgkDdiOpenAllocation).
 *
 * Primary use: expose the KMD-generated per-allocation ShareToken to the UMD so
 * the UMD can drive cross-process shared surface interop via the AeroGPU command
 * stream (`EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`).
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_ALLOC_PRIVDATA_H_
#define AEROGPU_PROTOCOL_AEROGPU_ALLOC_PRIVDATA_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>

#include "aerogpu_pci.h"

#define AEROGPU_ALLOC_PRIVDATA_MAGIC 0x44504C41u /* "ALPD" little-endian */
#define AEROGPU_ALLOC_PRIVDATA_VERSION 1u

/*
 * NOTE: This struct must remain stable across x86/x64.
 * - No pointers.
 * - Packed layout.
 */
#pragma pack(push, 1)
struct aerogpu_alloc_privdata {
  uint32_t magic; /* AEROGPU_ALLOC_PRIVDATA_MAGIC */
  uint32_t version; /* AEROGPU_ALLOC_PRIVDATA_VERSION */

  /*
   * KMD-generated per-allocation ShareToken.
   *
   * This is the recommended source for `aerogpu_cmd_export_shared_surface::share_token`
   * and `aerogpu_cmd_import_shared_surface::share_token`.
   *
   * 0 means "not shareable / not exported".
   */
  uint64_t share_token;

  uint64_t reserved0;
};
#pragma pack(pop)

AEROGPU_STATIC_ASSERT(sizeof(struct aerogpu_alloc_privdata) == 24);
AEROGPU_STATIC_ASSERT(offsetof(struct aerogpu_alloc_privdata, share_token) == 8);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_ALLOC_PRIVDATA_H_ */
