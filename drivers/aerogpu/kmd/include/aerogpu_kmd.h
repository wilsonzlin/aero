#pragma once

#include <ntddk.h>
#include <d3dkmddi.h>

#include "aerogpu_log.h"

/*
 * NOTE: The AeroGPU project has two MMIO ABIs:
 * - Legacy: `aerogpu_protocol.h` (used by the current bring-up KMD).
 * - New:    `aerogpu_pci.h` (versioned, feature-gated; required for vblank).
 *
 * Both headers define a small set of overlapping macros (PCI IDs, MMIO magic),
 * so we capture the new ABI's magic value before including the legacy header.
 */
#include "aerogpu_pci.h"
#define AEROGPU_PCI_MMIO_MAGIC AEROGPU_MMIO_MAGIC /* "AGPU" */
#undef AEROGPU_PCI_VENDOR_ID
#undef AEROGPU_PCI_DEVICE_ID
#undef AEROGPU_MMIO_MAGIC
#include "aerogpu_protocol.h"

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
} AEROGPU_SUBMISSION_META;

typedef struct _AEROGPU_SUBMISSION {
    LIST_ENTRY ListEntry;
    ULONG Fence;

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
    ULONG AllocationId;
    ULONGLONG ShareToken;
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

    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    ULONG RingEntryCount;
    ULONG RingTail;
    KSPIN_LOCK RingLock;

    LIST_ENTRY PendingSubmissions;
    KSPIN_LOCK PendingLock;
    ULONG LastSubmittedFence;
    ULONG LastCompletedFence;

    LIST_ENTRY Allocations;
    KSPIN_LOCK AllocationsLock;
    volatile LONG NextAllocationId;

    /* Current mode (programmed via CommitVidPn / SetVidPnSourceAddress). */
    ULONG CurrentWidth;
    ULONG CurrentHeight;
    ULONG CurrentPitch;
    ULONG CurrentFormat; /* aerogpu_scanout_format */
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
