/*
 * AeroGPU Windows 7 WDDM (driver-private user↔kernel ABI).
 *
 * These structs are copied verbatim between user-mode (32-bit or 64-bit) and the
 * kernel-mode miniport. On Windows 7 x64, WOW64 allows 32-bit processes to talk
 * to a 64-bit KMD, so any driver-defined ABI must:
 *   - Have a fixed layout across x86/x64.
 *   - Contain no pointers / arch-sized types (void*, size_t, HANDLE, etc.).
 *   - Use fixed-width integers and explicit packing, with size assertions.
 *
 * NOTE: This header intentionally does NOT include `aerogpu_protocol.h` (legacy
 * bring-up ABI) because it defines conflicting global constants with the
 * versioned protocol (`aerogpu_cmd.h` / `aerogpu_pci.h`). Keep this file
 * self-contained.
 */
#ifndef AEROGPU_PROTOCOL_AEROGPU_WIN7_ABI_H_
#define AEROGPU_PROTOCOL_AEROGPU_WIN7_ABI_H_

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h> /* offsetof */

/* Fixed-width types (kernel-mode builds don't reliably provide stdint.h). */
#if defined(_NTDDK_) || defined(_NTIFS_) || defined(_WDMDDK_) || defined(_KERNEL_MODE)
#include <ntdef.h>
typedef UINT32 aerogpu_win7_u32;
typedef UINT64 aerogpu_win7_u64;
#else
#include <stdint.h>
typedef uint32_t aerogpu_win7_u32;
typedef uint64_t aerogpu_win7_u64;
#endif

/* -------------------------- Compile-time utilities ------------------------ */

#define AEROGPU_WIN7_ABI_CONCAT2_(a, b) a##b
#define AEROGPU_WIN7_ABI_CONCAT_(a, b) AEROGPU_WIN7_ABI_CONCAT2_(a, b)
#define AEROGPU_WIN7_ABI_STATIC_ASSERT(expr) \
  typedef char AEROGPU_WIN7_ABI_CONCAT_(aerogpu_win7_abi_static_assert_, __LINE__)[(expr) ? 1 : -1]

/* DXGK_DRIVERCAPS::DmaBufferPrivateDataSize for AeroGPU. Must be stable across x86/x64. */
#define AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES 16u

/*
 * Per-DMA-buffer private data.
 *
 * NOTE: This is a driver-private user→kernel ABI blob (UMD -> dxgkrnl -> KMD).
 * It must not embed pointers. KMD-internal pointers must be represented as
 * opaque IDs and resolved through kernel-owned tables.
 */
#pragma pack(push, 1)
typedef struct AEROGPU_DMA_PRIV {
  aerogpu_win7_u32 Type; /* AEROGPU_SUBMIT_* */
  aerogpu_win7_u32 Reserved0;
  aerogpu_win7_u64 MetaHandle; /* 0 == none */
} AEROGPU_DMA_PRIV;
#pragma pack(pop)

AEROGPU_WIN7_ABI_STATIC_ASSERT(sizeof(AEROGPU_DMA_PRIV) == AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);
AEROGPU_WIN7_ABI_STATIC_ASSERT(offsetof(AEROGPU_DMA_PRIV, MetaHandle) == 8);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AEROGPU_PROTOCOL_AEROGPU_WIN7_ABI_H_ */
