#pragma once

#include <ntddk.h>
#include <d3dkmddi.h>

#include "aerogpu_log.h"
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
    ULONG Type;
    ULONG AllocationCount;
    aerogpu_submission_desc_allocation Allocations[1]; /* variable length */
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

    AEROGPU_SUBMISSION_META* Meta;
} AEROGPU_SUBMISSION;

typedef struct _AEROGPU_ALLOCATION {
    ULONG AllocationId;
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

    ULONG NextAllocationId;

    /* Current mode (programmed via CommitVidPn / SetVidPnSourceAddress). */
    ULONG CurrentWidth;
    ULONG CurrentHeight;
    ULONG CurrentPitch;
    ULONG CurrentFormat; /* aerogpu_scanout_format */
    BOOLEAN SourceVisible;

    /* VBlank / scanline estimation state (see DxgkDdiGetScanLine). */
    volatile ULONGLONG LastVblankSeq;
    volatile ULONGLONG LastVblankInterruptTime100ns;
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
