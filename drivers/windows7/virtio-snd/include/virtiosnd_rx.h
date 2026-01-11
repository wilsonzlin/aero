/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_dma.h"
#include "virtiosnd_queue.h"

/*
 * virtio-snd RX streaming engine (capture stream 1).
 *
 * This module owns a pool of per-request DMA buffers used for the virtio-snd RX
 * header (OUT) and response status (IN). The PCM payload destination buffers
 * are provided by the caller as a scatter/gather list of (DMA address, length)
 * pairs.
 *
 * IRQL requirements:
 *  - VirtIoSndRxInit / VirtIoSndRxUninit: PASSIVE_LEVEL
 *  - VirtIoSndRxSubmitSg / VirtIoSndRxDrainCompletions / VirtIoSndRxOnUsed /
 *    VirtIoSndRxSetCompletionCallback: <= DISPATCH_LEVEL
 *
 * Cache coherency contract for device-written payload buffers:
 *  - The payload buffers described by VirtIoSndRxSubmitSg are written by the
 *    device (VRING_DESC_F_WRITE). Callers must ensure the provided buffers are
 *    DMA-accessible and resident (nonpaged) for the duration of the request.
 *  - On Windows 7 x86/x64, DMA is cache coherent, so no explicit cache
 *    maintenance is required for normal MDL-backed allocations.
 *  - If this code is ported to a non-coherent DMA architecture, the caller must
 *    ensure coherency before reading captured samples (e.g. by using
 *    KeFlushIoBuffers on the associated MDL for a read operation).
 */

/*
 * To ensure RX submissions use indirect descriptors (required by the Aero
 * contract), the virtqueue implementation constrains the maximum SG elements
 * per request. The chain consists of:
 *  - 1 OUT header descriptor
 *  - N IN payload descriptors
 *  - 1 IN status descriptor
 */
#define VIRTIOSND_RX_MAX_PAYLOAD_SG 30u

typedef struct _VIRTIOSND_RX_SEGMENT {
    UINT64 addr;
    UINT32 len;
} VIRTIOSND_RX_SEGMENT, *PVIRTIOSND_RX_SEGMENT;

typedef VOID EVT_VIRTIOSND_RX_COMPLETION(
    _In_opt_ void* Cookie,
    _In_ NTSTATUS CompletionStatus,
    _In_ ULONG VirtioStatus,
    _In_ ULONG LatencyBytes,
    _In_ ULONG PayloadBytes,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context);

typedef struct _VIRTIOSND_RX_REQUEST {
    LIST_ENTRY Link;

    /* DMA common buffer for [VIRTIO_SND_TX_HDR][VIRTIO_SND_PCM_STATUS]. */
    VIRTIOSND_DMA_BUFFER Allocation;

    VIRTIO_SND_TX_HDR* HdrVa;
    UINT64 HdrDma;

    VIRTIO_SND_PCM_STATUS* StatusVa;
    UINT64 StatusDma;

    ULONG PayloadBytes;
    ULONG Sequence;
    void* Cookie;
    BOOLEAN Inflight;
} VIRTIOSND_RX_REQUEST, *PVIRTIOSND_RX_REQUEST;

typedef struct _VIRTIOSND_RX_ENGINE {
    KSPIN_LOCK Lock;

    LIST_ENTRY FreeList;
    LIST_ENTRY InflightList;
    ULONG FreeCount;
    ULONG InflightCount;

    const VIRTIOSND_QUEUE* Queue;
    PVIRTIOSND_DMA_CONTEXT DmaCtx;

    ULONG RequestCount;
    VIRTIOSND_RX_REQUEST* Requests;

    /* Completion callback invoked from VirtIoSndRxOnUsed (DPC context). */
    EVT_VIRTIOSND_RX_COMPLETION* CompletionCallback;
    void* CompletionCallbackContext;

    /* Stats */
    ULONG SubmittedBuffers;
    ULONG CompletedBuffers;
    ULONG CompletedByStatus[4]; /* indexed by VIRTIO_SND_S_* */
    ULONG CompletedUnknownStatus;
    ULONG DroppedDueToNoRequests;
    ULONG LastVirtioStatus;
    ULONG LastLatencyBytes;
    BOOLEAN FatalError;

    ULONG NextSequence;
} VIRTIOSND_RX_ENGINE, *PVIRTIOSND_RX_ENGINE;

#ifdef __cplusplus
extern "C" {
#endif

_Must_inspect_result_ NTSTATUS VirtIoSndRxInit(
    _Out_ VIRTIOSND_RX_ENGINE* Rx,
    _In_ PVIRTIOSND_DMA_CONTEXT DmaCtx,
    _In_ const VIRTIOSND_QUEUE* Queue,
    _In_ ULONG RequestCount);

VOID VirtIoSndRxUninit(_Inout_ VIRTIOSND_RX_ENGINE* Rx);

/*
 * Set the completion callback that is invoked from VirtIoSndRxOnUsed.
 *
 * The callback may be called at DISPATCH_LEVEL and must be non-blocking.
 */
VOID VirtIoSndRxSetCompletionCallback(
    _Inout_ VIRTIOSND_RX_ENGINE* Rx,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION* Callback,
    _In_opt_ void* Context);

_Must_inspect_result_ NTSTATUS VirtIoSndRxSubmitSg(
    _Inout_ VIRTIOSND_RX_ENGINE* Rx,
    _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT* Segments,
    _In_ USHORT SegmentCount,
    _In_opt_ void* Cookie);

/*
 * Drain all currently used entries from the RX virtqueue using Queue->PopUsed()
 * and deliver each completion via the provided callback.
 *
 * If Callback is NULL, the callback registered via VirtIoSndRxSetCompletionCallback()
 * is used instead.
 *
 * Returns the number of used entries drained.
 */
ULONG VirtIoSndRxDrainCompletions(
    _Inout_ VIRTIOSND_RX_ENGINE* Rx,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION* Callback,
    _In_opt_ void* Context);

/*
 * Handle a single used entry completion (typically called from the driver's
 * interrupt DPC via the virtqueue drain callback).
 */
VOID VirtIoSndRxOnUsed(_Inout_ VIRTIOSND_RX_ENGINE* Rx, _In_opt_ void* Cookie, _In_ UINT32 UsedLen);

#ifdef __cplusplus
} /* extern "C" */
#endif
