/*
 * AeroGPU UMD-private discovery blob (DXGKQAITYPE_UMDRIVERPRIVATE)
 *
 * This header defines the payload returned by the AeroGPU WDDM miniport driver
 * for `DXGKQAITYPE_UMDRIVERPRIVATE` (queried from user-mode via
 * `D3DKMTQueryAdapterInfo`).
 *
 * The goal is to provide a stable, versioned, pointer-free blob so UMDs and
 * tooling can discover:
 *  - which AeroGPU MMIO ABI is active (legacy "ARGP" vs new "AGPU"),
 *  - the device-reported ABI version, and
 *  - device feature bits (vblank, fence page, etc.)
 *
 * Requirements:
 *  - Must compile in kernel-mode builds and in user-mode builds.
 *  - Packed, pointer-free POD layout (safe to memcpy across kernel/user).
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_UMD_PRIVATE_H_
#define AEROGPU_PROTOCOL_AEROGPU_UMD_PRIVATE_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>

/* Fixed-width types (kernel-mode builds don't reliably provide stdint.h). */
#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT32 aerogpu_umdpriv_u32;
typedef UINT64 aerogpu_umdpriv_u64;
#else
#include <stdint.h>
typedef uint32_t aerogpu_umdpriv_u32;
typedef uint64_t aerogpu_umdpriv_u64;
#endif

/* -------------------------- Compile-time utilities ------------------------ */

#define AEROGPU_UMDPRIV_CONCAT2_(a, b) a##b
#define AEROGPU_UMDPRIV_CONCAT_(a, b) AEROGPU_UMDPRIV_CONCAT2_(a, b)
#define AEROGPU_UMDPRIV_STATIC_ASSERT(expr) \
  typedef char AEROGPU_UMDPRIV_CONCAT_(aerogpu_umdpriv_static_assert_, __LINE__)[(expr) ? 1 : -1]

/* -------------------------- Legacy vs new ABI detection ------------------- */

/* Raw BAR0[0] values ("MAGIC") for known AeroGPU ABIs. */
#define AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP 0x41524750u /* "ARGP" little-endian */
#define AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU 0x55504741u /* "AGPU" little-endian */

/* These offsets are shared by both ABIs for discovery. */
#define AEROGPU_UMDPRIV_MMIO_REG_MAGIC 0x0000u
#define AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION 0x0004u
#define AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO 0x0008u
#define AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI 0x000Cu

/* Feature bit positions (mirrors `aerogpu_pci.h` for the new "AGPU" ABI). */
#define AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE (1ull << 0)
#define AEROGPU_UMDPRIV_FEATURE_CURSOR (1ull << 1)
#define AEROGPU_UMDPRIV_FEATURE_SCANOUT (1ull << 2)
#define AEROGPU_UMDPRIV_FEATURE_VBLANK (1ull << 3)

/* ------------------------------ Blob layout -------------------------------- */

#define AEROGPU_UMDPRIV_STRUCT_VERSION_V1 1u

/* `flags` bitfield values for `aerogpu_umd_private_v1`. */
#define AEROGPU_UMDPRIV_FLAG_IS_LEGACY (1u << 0)
#define AEROGPU_UMDPRIV_FLAG_HAS_VBLANK (1u << 1)
/* A shared fence page is configured and usable (not just supported). */
#define AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE (1u << 2)

/*
 * Version 1 of the UMDRIVERPRIVATE blob.
 *
 * Forward-compat rules for consumers:
 *  - Require `size_bytes >= sizeof(struct aerogpu_umd_private_v1)` and
 *    `struct_version == 1` to use this layout.
 *  - Ignore any trailing bytes (future expansion).
 */
#pragma pack(push, 1)
typedef struct aerogpu_umd_private_v1 {
  aerogpu_umdpriv_u32 size_bytes; /* sizeof(struct aerogpu_umd_private_v1) */
  aerogpu_umdpriv_u32 struct_version; /* AEROGPU_UMDPRIV_STRUCT_VERSION_V1 */

  aerogpu_umdpriv_u32 device_mmio_magic; /* raw BAR0[0] */
  aerogpu_umdpriv_u32 device_abi_version_u32; /* legacy: MMIO version, new: ABI_VERSION */

  aerogpu_umdpriv_u32 reserved0;

  /* New ABI ("AGPU"): FEATURES_LO/HI. Legacy ("ARGP"): 0. */
  aerogpu_umdpriv_u64 device_features;

  /*
   * Convenience flags derived from the above. Prefer using `device_features`
   * for new ABIs; these flags exist to preserve a stable probe surface across
   * legacy and new devices.
   */
  aerogpu_umdpriv_u32 flags;

  aerogpu_umdpriv_u32 reserved1;
  aerogpu_umdpriv_u32 reserved2;
  aerogpu_umdpriv_u64 reserved3[3];
} aerogpu_umd_private_v1;
#pragma pack(pop)

AEROGPU_UMDPRIV_STATIC_ASSERT(sizeof(aerogpu_umd_private_v1) == 64);
AEROGPU_UMDPRIV_STATIC_ASSERT(offsetof(aerogpu_umd_private_v1, device_features) == 20);
AEROGPU_UMDPRIV_STATIC_ASSERT(offsetof(aerogpu_umd_private_v1, flags) == 28);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_UMD_PRIVATE_H_ */
