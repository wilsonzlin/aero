#pragma once

#include <ntddk.h>
#include <d3dkmddi.h>

#include "aerogpu_log.h"
#include "aerogpu_pci.h"
#include "aerogpu_ring.h"
#include "aerogpu_wddm_alloc.h"
#include "aerogpu_legacy_abi.h"

/* Compatibility alias used by some KMD code paths. */
#define AEROGPU_PCI_MMIO_MAGIC AEROGPU_MMIO_MAGIC /* "AGPU" */

/* Driver pool tag: 'A','G','P','U' */
#define AEROGPU_POOL_TAG 'UPGA'

#define AEROGPU_CHILD_UID 1
#define AEROGPU_VIDPN_SOURCE_ID 0
#define AEROGPU_VIDPN_TARGET_ID 0
#define AEROGPU_NODE_ORDINAL 0
#define AEROGPU_ENGINE_ORDINAL 0

#define AEROGPU_SEGMENT_ID_SYSTEM 1

#define AEROGPU_RING_ENTRY_COUNT_DEFAULT 256u

#define AEROGPU_SUBMISSION_LOG_SIZE 64u

/*
 * Driver-private submission types.
 *
 * Legacy ABI: encoded into `aerogpu_legacy_submission_desc_header::type`.
 * New ABI: used to derive `AEROGPU_SUBMIT_FLAG_PRESENT` via DMA private data.
 */
#define AEROGPU_SUBMIT_RENDER 1u
#define AEROGPU_SUBMIT_PRESENT 2u
#define AEROGPU_SUBMIT_PAGING 3u

typedef enum _AEROGPU_ABI_KIND {
    AEROGPU_ABI_KIND_UNKNOWN = 0,
    AEROGPU_ABI_KIND_LEGACY = 1,
    AEROGPU_ABI_KIND_V1 = 2,
} AEROGPU_ABI_KIND;

typedef struct _AEROGPU_SUBMISSION_LOG_ENTRY {
    ULONG Fence;
    ULONG Type;
    ULONG DmaSize;
    LARGE_INTEGER Qpc;
} AEROGPU_SUBMISSION_LOG_ENTRY;

typedef struct _AEROGPU_SUBMISSION_LOG {
    ULONG WriteIndex;
    AEROGPU_SUBMISSION_LOG_ENTRY Entries[AEROGPU_SUBMISSION_LOG_SIZE];
} AEROGPU_SUBMISSION_LOG;

typedef struct _AEROGPU_SUBMISSION_META {
    PVOID AllocTableVa;
    PHYSICAL_ADDRESS AllocTablePa;
    UINT AllocTableSizeBytes;

    UINT AllocationCount;
    aerogpu_legacy_submission_desc_allocation Allocations[1]; /* variable length */
} AEROGPU_SUBMISSION_META;

typedef struct _AEROGPU_SUBMISSION {
    LIST_ENTRY ListEntry;
    ULONGLONG Fence;

    PVOID DmaCopyVa;
    SIZE_T DmaCopySize;
    PHYSICAL_ADDRESS DmaCopyPa;

    PVOID DescVa;
    SIZE_T DescSize;
    PHYSICAL_ADDRESS DescPa;

    PVOID AllocTableVa;
    PHYSICAL_ADDRESS AllocTablePa;
    UINT AllocTableSizeBytes;
} AEROGPU_SUBMISSION;

typedef struct _AEROGPU_ALLOCATION {
    LIST_ENTRY ListEntry;
    ULONG AllocationId; /* aerogpu_wddm_alloc_priv.alloc_id */
    ULONGLONG ShareToken; /* aerogpu_wddm_alloc_priv.share_token */
    SIZE_T SizeBytes;
    ULONG Flags;
    PHYSICAL_ADDRESS LastKnownPa; /* updated from allocation lists */
} AEROGPU_ALLOCATION;

typedef struct _AEROGPU_DEVICE {
    struct _AEROGPU_ADAPTER* Adapter;
} AEROGPU_DEVICE;

typedef struct _AEROGPU_CONTEXT {
    AEROGPU_DEVICE* Device;
} AEROGPU_CONTEXT;

typedef struct _AEROGPU_ADAPTER {
    PDEVICE_OBJECT PhysicalDeviceObject;

    DXGK_START_INFO StartInfo;
    DXGKRNL_INTERFACE DxgkInterface;

    PUCHAR Bar0;
    ULONG Bar0Length;

    /* Device feature bits (AEROGPU_FEATURE_* from aerogpu_pci.h). */
    ULONGLONG DeviceFeatures;
    BOOLEAN SupportsVblank;
    DXGK_INTERRUPT_TYPE VblankInterruptType;
    BOOLEAN VblankInterruptTypeValid;

    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    ULONG RingSizeBytes;
    ULONG RingEntryCount;
    ULONG RingTail;
    struct aerogpu_ring_header* RingHeader; /* Only when AbiKind == AEROGPU_ABI_KIND_V1 */
    struct aerogpu_fence_page* FencePageVa; /* Only when AbiKind == AEROGPU_ABI_KIND_V1 */
    PHYSICAL_ADDRESS FencePagePa;
    KSPIN_LOCK RingLock;

    KSPIN_LOCK IrqEnableLock;
    ULONG IrqEnableMask; /* Cached AEROGPU_MMIO_REG_IRQ_ENABLE value (AbiKind == AEROGPU_ABI_KIND_V1). */

    LIST_ENTRY PendingSubmissions;
    KSPIN_LOCK PendingLock;
    ULONGLONG LastSubmittedFence;
    ULONGLONG LastCompletedFence;

    LIST_ENTRY Allocations;
    KSPIN_LOCK AllocationsLock;
    /*
     * Atomic alloc_id generator for non-AeroGPU (kernel/runtime) allocations
     * that do not carry an AeroGPU private-data blob.
     *
     * The counter is initialised so that the first generated ID is
     * AEROGPU_WDDM_ALLOC_ID_KMD_MIN, keeping the namespace split described in
     * `aerogpu_wddm_alloc.h`.
     */
    volatile LONG NextKmdAllocId;

    AEROGPU_ABI_KIND AbiKind;

    /* Current mode (programmed via CommitVidPn / SetVidPnSourceAddress). */
    ULONG CurrentWidth;
    ULONG CurrentHeight;
    ULONG CurrentPitch;
    ULONG CurrentFormat; /* enum aerogpu_format */
    BOOLEAN SourceVisible;
    BOOLEAN UsingNewAbi;

    /* VBlank / scanline estimation state (see DxgkDdiGetScanLine). */
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastVblankSeq;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastVblankInterruptTime100ns;
    ULONG VblankPeriodNs;

    AEROGPU_SUBMISSION_LOG SubmissionLog;
} AEROGPU_ADAPTER;

static __forceinline ULONG AeroGpuReadRegU32(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG Offset)
{
    return READ_REGISTER_ULONG((volatile ULONG*)(Adapter->Bar0 + Offset));
}

static __forceinline VOID AeroGpuWriteRegU32(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG Offset, _In_ ULONG Value)
{
    WRITE_REGISTER_ULONG((volatile ULONG*)(Adapter->Bar0 + Offset), Value);
}
