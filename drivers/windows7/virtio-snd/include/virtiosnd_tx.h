/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_dma.h"
#include "virtiosnd_queue.h"

/*
 * virtio-snd TX streaming engine (playback stream 0).
 *
 * This module owns a bounded pool of pre-allocated DMA-able request contexts and
 * provides DISPATCH_LEVEL-safe submission APIs:
 *  - VirtioSndTxSubmitPeriod: copy from up to two caller-provided period buffers
 *  - VirtioSndTxSubmitSg: submit a period as a list of (DMA address, length)
 *    segments without copying
 *
 * The driver is responsible for period pacing and for calling
 * VirtioSndTxDrainCompletions() (or the compatibility wrapper
 * VirtioSndTxProcessCompletions) from the DPC/interrupt path to recycle
 * contexts and update completion statistics.
 */

/*
 * To keep descriptor usage bounded and ensure that the virtqueue implementation
 * can always select indirect descriptors, cap the number of PCM segments per TX
 * submission so the full chain fits within the indirect table size (default:
 * 32 descriptors).
 *
 * Chain layout:
 *   [TX_HDR] + [PCM segments...] + [PCM_STATUS]
 * => sg_count = SegmentCount + 2
 */
#define VIRTIOSND_TX_MAX_SEGMENTS 30u
#define VIRTIOSND_TX_SG_CAP (2u + VIRTIOSND_TX_MAX_SEGMENTS)

/*
 * A single physically contiguous segment of PCM bytes.
 *
 * Note: Address is a device DMA address (guest physical address in the Aero
 * contract environment). The caller must ensure buffers are resident and that
 * any required cache maintenance has been performed.
 */
typedef struct _VIRTIOSND_TX_SEGMENT {
    PHYSICAL_ADDRESS Address;
    ULONG Length;
} VIRTIOSND_TX_SEGMENT, *PVIRTIOSND_TX_SEGMENT;

typedef struct _VIRTIOSND_TX_STATS {
    volatile LONG Submitted;
    volatile LONG Completed;
    volatile LONG InFlight;

    volatile LONG StatusOk;
    volatile LONG StatusBadMsg;
    volatile LONG StatusNotSupp;
    volatile LONG StatusIoErr;
    volatile LONG StatusOther;

    volatile LONG DroppedNoBuffers;
    volatile LONG SubmitErrors;
} VIRTIOSND_TX_STATS, *PVIRTIOSND_TX_STATS;

typedef struct _VIRTIOSND_TX_BUFFER {
    LIST_ENTRY Link;

    /* Base of the DMA common buffer allocation for this buffer. */
    VIRTIOSND_DMA_BUFFER Allocation;

    /* OUT base: [VIRTIO_SND_TX_HDR][pcm_bytes...] */
    PVOID DataVa;
    UINT64 DataDma;

    /* IN: VIRTIO_SND_PCM_STATUS (last descriptor in chain) */
    VIRTIO_SND_PCM_STATUS* StatusVa;
    UINT64 StatusDma;

    ULONG PcmBytes;

    ULONG Sequence;
    BOOLEAN Inflight;

    /* Scratch SG array used for submission (header + segments + status). */
    VIRTIOSND_SG Sg[VIRTIOSND_TX_SG_CAP];
} VIRTIOSND_TX_BUFFER, *PVIRTIOSND_TX_BUFFER;

typedef struct _VIRTIOSND_TX_ENGINE {
    KSPIN_LOCK Lock;

    LIST_ENTRY FreeList;
    LIST_ENTRY InflightList;
    ULONG FreeCount;
    ULONG InflightCount;

    const VIRTIOSND_QUEUE* Queue;
    PVIRTIOSND_DMA_CONTEXT DmaCtx;

    ULONG MaxPeriodBytes;
    ULONG BufferCount;
    VIRTIOSND_TX_BUFFER* Buffers;

    VIRTIOSND_TX_STATS Stats;

    ULONG LastVirtioStatus;
    ULONG LastLatencyBytes;
    BOOLEAN FatalError;

    ULONG NextSequence;
} VIRTIOSND_TX_ENGINE, *PVIRTIOSND_TX_ENGINE;

#ifdef __cplusplus
extern "C" {
#endif

ULONG VirtioSndTxFrameSizeBytes(VOID);

/*
 * Initialize the TX engine.
 *
 * If BufferCount is 0, the engine selects a reasonable default.
 *
 * If SuppressInterrupts is TRUE, the engine requests that the device suppress
 * interrupts for the TX queue (VRING_AVAIL_F_NO_INTERRUPT). The engine still
 * functions correctly if interrupts are delivered anyway.
 *
 * IRQL: PASSIVE_LEVEL only (allocates and initializes DMA buffers).
 */
_Must_inspect_result_ NTSTATUS VirtioSndTxInit(
    _Out_ VIRTIOSND_TX_ENGINE* Tx,
    _In_ PVIRTIOSND_DMA_CONTEXT DmaCtx,
    _In_ const VIRTIOSND_QUEUE* Queue,
    _In_ ULONG MaxPeriodBytes,
    _In_ ULONG BufferCount,
    _In_ BOOLEAN SuppressInterrupts);

/*
 * Tear down the TX engine and free resources.
 *
 * IRQL: PASSIVE_LEVEL only.
 */
VOID VirtioSndTxUninit(_Inout_ VIRTIOSND_TX_ENGINE* Tx);

/*
 * Submit a TX period by copying PCM bytes from up to two source ranges.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_Must_inspect_result_ NTSTATUS VirtioSndTxSubmitPeriod(
    _Inout_ VIRTIOSND_TX_ENGINE* Tx,
    _In_opt_ const VOID* Pcm1,
    _In_ ULONG Pcm1Bytes,
    _In_opt_ const VOID* Pcm2,
    _In_ ULONG Pcm2Bytes,
    _In_ BOOLEAN AllowSilenceFill);

/*
 * Submit a TX period as a list of DMA segments (no copy).
 *
 * Returns STATUS_INSUFFICIENT_RESOURCES if no buffers are available or if the
 * virtqueue is full.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_Must_inspect_result_ NTSTATUS VirtioSndTxSubmitSg(
    _Inout_ VIRTIOSND_TX_ENGINE* Tx,
    _In_reads_(SegmentCount) const VIRTIOSND_TX_SEGMENT* Segments,
    _In_ ULONG SegmentCount);

/*
 * Drain used completions from the TX virtqueue and recycle contexts.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
ULONG VirtioSndTxDrainCompletions(_Inout_ VIRTIOSND_TX_ENGINE* Tx);

/* Backwards-compatible name used by the INTx DPC path. */
VOID VirtioSndTxProcessCompletions(_Inout_ VIRTIOSND_TX_ENGINE* Tx);

/*
 * Complete a single used entry from the TX virtqueue.
 *
 * This is intended for generic virtqueue drain loops that pop used entries and
 * then dispatch completions to the queue owner (TX engine).
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
VOID VirtioSndTxOnUsed(_Inout_ VIRTIOSND_TX_ENGINE* Tx, _In_opt_ void* Cookie, _In_ UINT32 UsedLen);

#ifdef __cplusplus
} /* extern "C" */
#endif

