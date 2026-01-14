/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_dma.h"
#include "virtiosnd_jack.h"
#include "virtiosnd_queue.h"

/*
 * Minimal virtio-snd eventq buffer pool.
 *
 * Contract v1 defines no *required* event messages (see
 * docs/windows7-virtio-driver-contract.md §3.4.2.1), so the audio data path must
 * not depend on eventq.
 *
 * However, the virtio-snd specification reserves eventq for asynchronous device
 * notifications. To be robust to future eventq usage (and to device-model bugs
 * that might unexpectedly complete event buffers), we post a small bounded set
 * of writable buffers and recycle them on completion.
 *
 * Buffer sizing:
 *  - Choose a conservative fixed size (64 bytes) that is comfortably larger
 *    than the currently-defined virtio-snd event structures, while keeping the
 *    pool small.
 */
#define VIRTIOSND_EVENTQ_BUFFER_SIZE 64u
#define VIRTIOSND_EVENTQ_BUFFER_COUNT 8u

/*
 * Maximum number of stream IDs for which we keep a referenced WaveRT
 * notification event pointer.
 *
 * The contract v1 device exposes two streams:
 *  - stream 0: playback
 *  - stream 1: capture
 */
#define VIRTIOSND_EVENTQ_MAX_NOTIFY_STREAMS 2u

/* Ensure the fixed pool buffers can hold at least a single virtio-snd event. */
C_ASSERT(VIRTIOSND_EVENTQ_BUFFER_SIZE >= sizeof(VIRTIO_SND_EVENT));

typedef struct _VIRTIOSND_EVENTQ_STATS {
    volatile LONG Completions;
    volatile LONG Parsed;
    volatile LONG ShortBuffers;
    volatile LONG UnknownType;

    volatile LONG JackConnected;
    volatile LONG JackDisconnected;
    volatile LONG PcmPeriodElapsed;
    volatile LONG PcmXrun;
    volatile LONG CtlNotify;
} VIRTIOSND_EVENTQ_STATS, *PVIRTIOSND_EVENTQ_STATS;

/*
 * Optional eventq callback type (WaveRT).
 *
 * The callback is invoked from the interrupt/DPC path after parsing a virtio-snd
 * event (type/data). Higher layers must treat it as best-effort: contract v1
 * devices emit no events and drivers must not depend on them.
 */
typedef VOID EVT_VIRTIOSND_EVENTQ_EVENT(_In_opt_ void* Context, _In_ ULONG Type, _In_ ULONG Data);

/*
 * Event callback storage (device extension wrapper).
 *
 * The callback pointer and context are protected by the spinlock and must be
 * snapshotted before invocation. The callback itself is invoked without holding
 * the lock.
 */
typedef struct _VIRTIOSND_EVENTQ_CALLBACK_STATE {
    _Inout_opt_ KSPIN_LOCK* Lock;
    _Inout_opt_ EVT_VIRTIOSND_EVENTQ_EVENT** Callback;
    _Inout_opt_ void** CallbackContext;
    _Inout_opt_ volatile LONG* CallbackInFlight;
} VIRTIOSND_EVENTQ_CALLBACK_STATE, *PVIRTIOSND_EVENTQ_CALLBACK_STATE;

/*
 * Optional WaveRT-facing signal hook for PCM_PERIOD_ELAPSED.
 *
 * The production driver uses this to signal a per-stream event object registered
 * by the WaveRT miniport. Host tests can pass NULL.
 */
typedef BOOLEAN EVT_VIRTIOSND_EVENTQ_SIGNAL_STREAM_NOTIFICATION(_In_opt_ void* Context, _In_ ULONG StreamId);

typedef struct _VIRTIOSND_EVENTQ_PERIOD_STATE {
    _In_opt_ EVT_VIRTIOSND_EVENTQ_SIGNAL_STREAM_NOTIFICATION* SignalStreamNotification;
    _In_opt_ void* SignalStreamNotificationContext;

    _Inout_opt_ volatile LONG* PcmPeriodSeq;
    _Inout_opt_ volatile LONGLONG* PcmLastPeriodEventTime100ns;
    _In_ ULONG StreamCount;
} VIRTIOSND_EVENTQ_PERIOD_STATE, *PVIRTIOSND_EVENTQ_PERIOD_STATE;

/*
 * Process a used completion from eventq:
 *  - validate the cookie
 *  - record it for reposting (optional)
 *  - best-effort parse + update counters
 *  - update topology/jack state (best-effort)
 *  - signal optional stream notification objects (best-effort)
 *  - dispatch to the optional callback (best-effort)
 *
 * Reposting policy:
 *  - If RepostMask is non-NULL: the buffer is NOT reposted immediately. Instead
 *    the corresponding bit is set in the mask, allowing the caller to repost
 *    after draining the used ring (prevents unbounded drain loops).
 *  - If RepostMask is NULL: the buffer is reposted immediately via Queue->Submit.
 *
 * Returns TRUE if the cookie was accepted and either:
 *  - the buffer was reposted successfully (RepostMask==NULL), or
 *  - the buffer was recorded for reposting (RepostMask!=NULL).
 *
 * IRQL: <= DISPATCH_LEVEL.
 */
_Must_inspect_result_
_IRQL_requires_max_(DISPATCH_LEVEL)
BOOLEAN
VirtIoSndEventqHandleUsed(
    _Inout_ const VIRTIOSND_QUEUE* Queue,
    _In_ const VIRTIOSND_DMA_BUFFER* BufferPool,
    _Inout_ PVIRTIOSND_EVENTQ_STATS Stats,
    _Inout_opt_ PVIRTIOSND_JACK_STATE JackState,
    _In_opt_ const VIRTIOSND_EVENTQ_CALLBACK_STATE* CallbackState,
    _In_opt_ const VIRTIOSND_EVENTQ_PERIOD_STATE* PeriodState,
    _In_ BOOLEAN Started,
    _In_ BOOLEAN Removed,
    _In_opt_ void* Cookie,
    _In_ UINT32 UsedLen,
    _In_ BOOLEAN EnableDebugLogs,
    _Inout_opt_ ULONGLONG* RepostMask);
