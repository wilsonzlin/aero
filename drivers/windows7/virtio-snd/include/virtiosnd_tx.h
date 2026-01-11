#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_queue.h"

/*
 * virtio-snd TX streaming engine (playback stream 0).
 *
 * This module owns a pool of pre-allocated DMA-able packet buffers and
 * provides a DISPATCH_LEVEL-safe submission API for WaveRT-style period pacing.
 *
 * The caller is responsible for period pacing and for calling
 * VirtioSndTxProcessCompletions() from the DPC/interrupt path to recycle
 * buffers and update completion statistics.
 */

typedef struct _VIRTIOSND_TX_BUFFER {
    LIST_ENTRY Link;

    /* Base of the physically contiguous allocation for this buffer. */
    PVOID AllocationVa;
    ULONG AllocationBytes;

    /* OUT: [VIRTIO_SND_TX_HDR][pcm_bytes...] */
    PVOID DataVa;
    PHYSICAL_ADDRESS DataPa;

    /* IN: VIRTIO_SND_PCM_STATUS */
    VIRTIO_SND_PCM_STATUS* StatusVa;
    PHYSICAL_ADDRESS StatusPa;

    ULONG PcmBytes;

    ULONG Sequence;
    BOOLEAN Inflight;
} VIRTIOSND_TX_BUFFER, *PVIRTIOSND_TX_BUFFER;

typedef struct _VIRTIOSND_TX_ENGINE {
    KSPIN_LOCK Lock;

    LIST_ENTRY FreeList;
    LIST_ENTRY InflightList;
    ULONG FreeCount;
    ULONG InflightCount;

    const VIRTIOSND_QUEUE* Queue;

    ULONG MaxPeriodBytes;
    ULONG BufferCount;
    VIRTIOSND_TX_BUFFER* Buffers;

    /* Stats */
    ULONG SubmittedPeriods;
    ULONG CompletedOk;
    ULONG CompletedIoErr;
    ULONG CompletedBadMsgOrNotSupp;
    ULONG DroppedDueToNoBuffers;
    ULONG LastVirtioStatus;
    ULONG LastLatencyBytes;
    BOOLEAN FatalError;

    ULONG NextSequence;
} VIRTIOSND_TX_ENGINE, *PVIRTIOSND_TX_ENGINE;

ULONG VirtioSndTxFrameSizeBytes(VOID);

_Must_inspect_result_ NTSTATUS VirtioSndTxInit(
    _Out_ VIRTIOSND_TX_ENGINE* Tx,
    _In_ const VIRTIOSND_QUEUE* Queue,
    _In_ ULONG MaxPeriodBytes,
    _In_ ULONG BufferCount);

VOID VirtioSndTxUninit(_Inout_ VIRTIOSND_TX_ENGINE* Tx);

_Must_inspect_result_ NTSTATUS VirtioSndTxSubmitPeriod(
    _Inout_ VIRTIOSND_TX_ENGINE* Tx,
    _In_opt_ const VOID* Pcm1,
    _In_ ULONG Pcm1Bytes,
    _In_opt_ const VOID* Pcm2,
    _In_ ULONG Pcm2Bytes,
    _In_ BOOLEAN AllowSilenceFill);

VOID VirtioSndTxProcessCompletions(_Inout_ VIRTIOSND_TX_ENGINE* Tx);
