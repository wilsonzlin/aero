/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_queue.h"

/*
 * virtio-snd control queue protocol engine.
 *
 * This module builds/parses virtio-snd control messages and tracks the playback
 * PCM stream state machine for stream_id=VIRTIO_SND_PLAYBACK_STREAM_ID:
 *
 *   Idle → ParamsSet → Prepared → Running → Prepared → Idle
 *
 * Queue integration uses the internal VIRTIOSND_QUEUE abstraction (see
 * virtiosnd_queue.h). The driver is responsible for wiring the queue ops (e.g.
 * split virtqueue + transport notify) and calling VirtioSndCtrlProcessUsed from
 * its DPC/ISR path to complete in-flight requests.
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

typedef struct _VIRTIOSND_CONTROL {
    /* Control virtqueue (queue_index=VIRTIO_SND_QUEUE_CONTROL). */
    VIRTIOSND_QUEUE* ControlQ;

    /* Tracks in-flight synchronous requests so stop/remove can cancel waiters. */
    KSPIN_LOCK InflightLock;
    LIST_ENTRY InflightList;

    /* Serializes control operations at PASSIVE_LEVEL (submit + wait + state). */
    FAST_MUTEX Mutex;

    VIRTIOSND_STREAM_STATE StreamState;
    VIRTIOSND_PCM_PARAMS Params;
} VIRTIOSND_CONTROL, *PVIRTIOSND_CONTROL;

#ifdef __cplusplus
extern "C" {
#endif

VOID VirtioSndCtrlInit(_Out_ VIRTIOSND_CONTROL* Ctrl, _In_ VIRTIOSND_QUEUE* ControlQ);

/*
 * Cancel all in-flight synchronous control requests and wake any waiters.
 *
 * Called by the device STOP/REMOVE path after interrupts are quiesced and the
 * device has been reset, so no further completions will arrive.
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
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
VOID VirtioSndCtrlOnUsed(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_opt_ void* Cookie, _In_ UINT32 UsedLen);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlSendSync(
    _Inout_ VIRTIOSND_CONTROL* Ctrl,
    _In_reads_bytes_(ReqLen) const void* Req,
    _In_ ULONG ReqLen,
    _Out_writes_bytes_(RespCap) void* Resp,
    _In_ ULONG RespCap,
    _In_ ULONG TimeoutMs,
    _Out_opt_ ULONG* OutVirtioStatus,
    _Out_opt_ ULONG* OutRespLen);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlPcmInfo(_Inout_ VIRTIOSND_CONTROL* Ctrl, _Out_ VIRTIO_SND_PCM_INFO* Info);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlSetParams(_Inout_ VIRTIOSND_CONTROL* Ctrl, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlPrepare(_Inout_ VIRTIOSND_CONTROL* Ctrl);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlStart(_Inout_ VIRTIOSND_CONTROL* Ctrl);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlStop(_Inout_ VIRTIOSND_CONTROL* Ctrl);

_Must_inspect_result_ NTSTATUS VirtioSndCtrlRelease(_Inout_ VIRTIOSND_CONTROL* Ctrl);

#ifdef __cplusplus
} /* extern "C" */
#endif
