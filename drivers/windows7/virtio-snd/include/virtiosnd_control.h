/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_dma.h"
#include "virtiosnd_queue.h"

/*
 * virtio-snd control queue protocol engine.
 *
 * This module builds/parses virtio-snd control messages and tracks two
 * independent PCM stream state machines per the Aero contract v1:
 *
 *   Stream 0 (playback/output): Idle → ParamsSet → Prepared → Running → Prepared → Idle
 *   Stream 1 (capture/input):   Idle → ParamsSet → Prepared → Running → Prepared → Idle
 *
 * Queue integration uses the internal VIRTIOSND_QUEUE abstraction (see
 * virtiosnd_queue.h). The driver is responsible for wiring the queue ops (e.g.
 * split virtqueue + transport notify) and calling VirtioSndCtrlProcessUsed (or
 * dispatching individual used entries via VirtioSndCtrlOnUsed) from its DPC/ISR
 * path to complete in-flight requests.
 */

typedef enum _VIRTIOSND_STREAM_STATE {
    VirtioSndStreamStateIdle = 0,
    VirtioSndStreamStateParamsSet,
    VirtioSndStreamStatePrepared,
    VirtioSndStreamStateRunning,
} VIRTIOSND_STREAM_STATE;

typedef struct _VIRTIOSND_PCM_PARAMS {
    ULONG BufferBytes;
    ULONG PeriodBytes;
    UCHAR Channels;
    UCHAR Format;
    UCHAR Rate;
} VIRTIOSND_PCM_PARAMS;

typedef struct _VIRTIOSND_CONTROL_STATS {
    volatile LONG RequestsSent;
    volatile LONG RequestsCompleted;
    volatile LONG RequestsTimedOut;
} VIRTIOSND_CONTROL_STATS;

typedef struct _VIRTIOSND_CONTROL {
    PVIRTIOSND_DMA_CONTEXT DmaCtx;

    /* Control virtqueue (queue_index=VIRTIO_SND_QUEUE_CONTROL). */
    VIRTIOSND_QUEUE* ControlQ;

    /* Tracks in-flight synchronous requests so stop/remove can cancel waiters. */
    KSPIN_LOCK InflightLock;
    LIST_ENTRY InflightList;

    /* Serializes control operations at PASSIVE_LEVEL (submit + wait + state). */
    FAST_MUTEX Mutex;

    /*
     * Tracks all active control requests so STOP_DEVICE can cancel and drain
     * them before releasing the DMA adapter.
     *
     * Protected by ReqLock and usable at IRQL <= DISPATCH_LEVEL.
     */
    KSPIN_LOCK ReqLock;
    LIST_ENTRY ReqList;
    KEVENT ReqIdleEvent;
    volatile LONG Stopping;

    /*
     * Indexed by stream_id (0 = playback, 1 = capture). Only the two streams in
     * the Aero contract v1 are supported by this driver.
     */
    VIRTIOSND_STREAM_STATE StreamState[2];
    VIRTIOSND_PCM_PARAMS Params[2];

    VIRTIOSND_CONTROL_STATS Stats;
} VIRTIOSND_CONTROL, *PVIRTIOSND_CONTROL;

#ifdef __cplusplus
extern "C" {
#endif

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioSndCtrlInit(_Out_ VIRTIOSND_CONTROL* Ctrl, _In_ PVIRTIOSND_DMA_CONTEXT DmaCtx, _In_ VIRTIOSND_QUEUE* ControlQ);

/*
 * Cancels any in-flight control requests and waits for all request contexts to
 * be freed (PASSIVE_LEVEL only).
 */
_IRQL_requires_(PASSIVE_LEVEL)
VOID VirtioSndCtrlUninit(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/*
 * Cancel all in-flight synchronous control requests and wake any waiters.
 *
 * Called by the device STOP/REMOVE path after interrupts are quiesced and the
 * device has been reset, so no further completions will arrive.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioSndCtrlCancelAll(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ NTSTATUS CancelStatus);

/*
 * Drain used completions from the control virtqueue and complete any in-flight
 * synchronous requests.
 *
 * The driver should call this from its virtqueue interrupt/DPC handler when it
 * observes the control queue has used entries.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioSndCtrlProcessUsed(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/*
 * Complete a single used entry from the control virtqueue.
 *
 * Intended for use by a generic virtqueue drain loop that already popped
 * descriptors and wants to hand completions to the control engine without
 * requiring it to re-pop the used ring.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtioSndCtrlOnUsed(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_opt_ void* Cookie, _In_ UINT32 UsedLen);

/*
 * Submit a single control request and synchronously wait for completion.
 *
 * IRQL: PASSIVE_LEVEL only (waits).
 */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlSendSync(
    _Inout_ VIRTIOSND_CONTROL* Ctrl,
    _In_reads_bytes_(ReqLen) const void* Req,
    _In_ ULONG ReqLen,
    _Out_writes_bytes_(RespCap) void* Resp,
    _In_ ULONG RespCap,
    _In_ ULONG TimeoutMs,
    _Out_opt_ ULONG* OutVirtioStatus,
    _Out_opt_ ULONG* OutRespLen);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlPcmInfo(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlSetParams(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlPrepare(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlStart(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlStop(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlRelease(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlPcmInfo1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlSetParams1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlPrepare1(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlStart1(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlStop1(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/* IRQL: PASSIVE_LEVEL only. */
_IRQL_requires_(PASSIVE_LEVEL)
_Must_inspect_result_ NTSTATUS VirtioSndCtrlRelease1(_Inout_ VIRTIOSND_CONTROL* Ctrl);

/*
 * Contract v1 capture convenience aliases matching the "stream 1" naming used
 * by the contract documentation.
 */
static __inline NTSTATUS VirtIoSndPcmQueryInfo1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info)
{
    return VirtioSndCtrlPcmInfo1(Ctrl, Info);
}

static __inline NTSTATUS VirtIoSndPcmSetParams1(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes)
{
    return VirtioSndCtrlSetParams1(Ctrl, BufferBytes, PeriodBytes);
}

static __inline NTSTATUS VirtIoSndPcmPrepare1(_Inout_ VIRTIOSND_CONTROL* Ctrl) { return VirtioSndCtrlPrepare1(Ctrl); }
static __inline NTSTATUS VirtIoSndPcmStart1(_Inout_ VIRTIOSND_CONTROL* Ctrl) { return VirtioSndCtrlStart1(Ctrl); }
static __inline NTSTATUS VirtIoSndPcmStop1(_Inout_ VIRTIOSND_CONTROL* Ctrl) { return VirtioSndCtrlStop1(Ctrl); }
static __inline NTSTATUS VirtIoSndPcmRelease1(_Inout_ VIRTIOSND_CONTROL* Ctrl) { return VirtioSndCtrlRelease1(Ctrl); }

#ifdef __cplusplus
} /* extern "C" */
#endif
