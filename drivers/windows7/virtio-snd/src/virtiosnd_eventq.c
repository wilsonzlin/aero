/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd_eventq.h"

/*
 * topology.c includes PortCls/KS headers that are not available in the host-test
 * environment. Declare the small surface area we need here and provide host-side
 * stubs in the unit tests.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID VirtIoSndTopology_UpdateJackStateEx(_In_ ULONG JackId, _In_ BOOLEAN IsConnected, _In_ BOOLEAN NotifyEvenIfUnchanged);

static __forceinline BOOLEAN VirtIoSndEventqShouldRateLimitLog(_Inout_ volatile LONG* Counter)
{
    /*
     * eventq contents are device-controlled. Even in free builds, avoid spamming
     * DbgPrintEx under malformed/stress scenarios.
     *
     * Log the 1st occurrence and then every 256th.
     */
    LONG n;
    if (Counter == NULL) {
        return TRUE;
    }
    n = InterlockedIncrement(Counter);
    return ((n & 0xFF) == 1) ? TRUE : FALSE;
}

static __forceinline BOOLEAN VirtIoSndEventqShouldLogRareCounter(_In_ LONG Count)
{
    ULONG u;

    /*
     * Log the first few occurrences, then exponentially back off (powers of two).
     *
     * This is used to keep eventq debug logging from spamming (e.g. if a future
     * device model emits high-rate PCM_PERIOD_ELAPSED notifications), while still
     * providing enough visibility for debugging.
     */
    if (Count <= 4) {
        return TRUE;
    }

    /* Handle negative/overflowed counters defensively. */
    if (Count < 0) {
        return TRUE;
    }

    u = (ULONG)Count;
    return ((u & (u - 1u)) == 0u) ? TRUE : FALSE;
}

_Use_decl_annotations_
BOOLEAN
VirtIoSndEventqHandleUsed(
    const VIRTIOSND_QUEUE* Queue,
    const VIRTIOSND_DMA_BUFFER* BufferPool,
    PVIRTIOSND_EVENTQ_STATS Stats,
    PVIRTIOSND_JACK_STATE JackState,
    const VIRTIOSND_EVENTQ_CALLBACK_STATE* CallbackState,
    const VIRTIOSND_EVENTQ_PERIOD_STATE* PeriodState,
    BOOLEAN Started,
    BOOLEAN Removed,
    void* Cookie,
    UINT32 UsedLen,
    BOOLEAN EnableDebugLogs,
    ULONGLONG* RepostMask)
{
    static volatile LONG s_eventqErrorLog;

    ULONG_PTR poolBase;
    ULONG_PTR cookiePtr;
    SIZE_T poolSize;
    SIZE_T off;
    ULONG idx;
    PUCHAR bufVa;
    BOOLEAN haveEvent;
    ULONG evtType;
    ULONG evtData;
    NTSTATUS status;
    VIRTIOSND_SG sg;

    if (Queue == NULL || Queue->Ops == NULL) {
        return FALSE;
    }

    if (BufferPool == NULL || Stats == NULL) {
        return FALSE;
    }

    /*
     * Contract v1 defines no *required* event messages, but the virtio-snd specification
     * reserves eventq for asynchronous notifications. Drain and (best-effort)
     * parse events so that:
     *  - future device models do not break this driver, and
     *  - buggy devices that complete event buffers do not leak ring space.
     *
     * Audio streaming MUST remain correct even if eventq is absent, silent, or
     * emits malformed/unknown events.
     */
    if (Cookie == NULL) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq completion with NULL cookie (len=%lu)\n", (ULONG)UsedLen);
        }
        return FALSE;
    }

    if (Removed) {
        /*
         * On surprise removal avoid MMIO accesses; do not repost/kick.
         * Best-effort draining is still useful to keep queue state consistent.
         */
        return FALSE;
    }

    if (BufferPool->Va == NULL || BufferPool->DmaAddr == 0 || BufferPool->Size == 0) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR(
                "eventq completion but buffer pool is not initialized (cookie=%p len=%lu)\n",
                Cookie,
                (ULONG)UsedLen);
        }
        return FALSE;
    }

    poolBase = (ULONG_PTR)BufferPool->Va;
    poolSize = BufferPool->Size;
    cookiePtr = (ULONG_PTR)Cookie;

    if (cookiePtr < poolBase) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq completion cookie out of range (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        }
        return FALSE;
    }

    off = (SIZE_T)(cookiePtr - poolBase);
    if (off >= poolSize) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq completion cookie out of range (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        }
        return FALSE;
    }

    /* Ensure cookie points at the start of one of our fixed-size buffers. */
    if (((ULONG_PTR)off % (ULONG_PTR)VIRTIOSND_EVENTQ_BUFFER_SIZE) != 0) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq completion cookie misaligned (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        }
        return FALSE;
    }

    if (off + (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE > poolSize) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq completion cookie range overflow (cookie=%p len=%lu)\n", Cookie, (ULONG)UsedLen);
        }
        return FALSE;
    }

    idx = (ULONG)(off / (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE);
    if (RepostMask != NULL) {
        if (idx < 64u) {
            *RepostMask |= (1ull << idx);
        } else {
            if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
                VIRTIOSND_TRACE_ERROR("eventq completion buffer index out of range (idx=%lu cookie=%p)\n", idx, Cookie);
            }
            return FALSE;
        }
    }

    if (UsedLen > (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE) {
        /* Device bug: used length should never exceed posted writable capacity. */
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR(
                "eventq completion length too large: %lu > %u (cookie=%p)\n",
                (ULONG)UsedLen,
                (UINT)VIRTIOSND_EVENTQ_BUFFER_SIZE,
                Cookie);
        }
    }

    InterlockedIncrement(&Stats->Completions);

    haveEvent = FALSE;
    evtType = 0;
    evtData = 0;

    /*
     * Parse the buffer before reposting it.
     *
     * Ensure device writes are visible before reading. The split-ring virtqueue
     * implementation already issues a read barrier after observing used->idx,
     * but keep the eventq path self-contained and robust to alternate queue
     * implementations.
     */
    bufVa = (PUCHAR)BufferPool->Va + off;

    /*
     * Best-effort parse/log. Never let parsing affect reposting; starving eventq
     * would make it impossible for a device to deliver future events.
     */
    if (UsedLen > (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE) {
        /* Malformed completion; ignore payload. */
    } else if (UsedLen >= (UINT32)sizeof(VIRTIO_SND_EVENT)) {
        const UINT32 cappedLen = UsedLen; /* already validated against buffer size */
        VIRTIO_SND_EVENT_PARSED evt;
        BOOLEAN logEvent;
        LONG eventCount;

        /* Ensure device DMA writes are visible before inspecting the buffer. */
        KeMemoryBarrier();

        status = VirtioSndParseEvent(bufVa, cappedLen, &evt);
        if (NT_SUCCESS(status)) {
            haveEvent = TRUE;
            evtType = evt.Type;
            evtData = evt.Data;
            InterlockedIncrement(&Stats->Parsed);

            logEvent = EnableDebugLogs ? TRUE : FALSE;
            eventCount = 0;

            switch (evt.Kind) {
            case VIRTIO_SND_EVENT_KIND_JACK_CONNECTED:
                eventCount = InterlockedIncrement(&Stats->JackConnected);
                {
                    BOOLEAN changed = FALSE;
                    if (JackState != NULL) {
                        changed = VirtIoSndJackStateUpdate(JackState, evt.Data, TRUE);
                    }
                    VirtIoSndTopology_UpdateJackStateEx(evt.Data, TRUE, changed);
                }
                break;
            case VIRTIO_SND_EVENT_KIND_JACK_DISCONNECTED:
                eventCount = InterlockedIncrement(&Stats->JackDisconnected);
                {
                    BOOLEAN changed = FALSE;
                    if (JackState != NULL) {
                        changed = VirtIoSndJackStateUpdate(JackState, evt.Data, FALSE);
                    }
                    VirtIoSndTopology_UpdateJackStateEx(evt.Data, FALSE, changed);
                }
                break;
            case VIRTIO_SND_EVENT_KIND_PCM_PERIOD_ELAPSED:
                eventCount = InterlockedIncrement(&Stats->PcmPeriodElapsed);
                if (PeriodState != NULL) {
                    if (PeriodState->PcmPeriodSeq != NULL &&
                        PeriodState->PcmLastPeriodEventTime100ns != NULL &&
                        evt.Data < PeriodState->StreamCount) {
                        (VOID)InterlockedIncrement(&PeriodState->PcmPeriodSeq[evt.Data]);
                        (VOID)InterlockedExchange64(
                            &PeriodState->PcmLastPeriodEventTime100ns[evt.Data],
                            (LONGLONG)KeQueryInterruptTime());
                    }
                }
                /* PCM period notifications may be high rate; log at a low rate. */
                logEvent = EnableDebugLogs ? VirtIoSndEventqShouldLogRareCounter(eventCount) : FALSE;
                break;
            case VIRTIO_SND_EVENT_KIND_PCM_XRUN:
                eventCount = InterlockedIncrement(&Stats->PcmXrun);
                /* XRUNs can be spammed by misbehaving devices; avoid log spam. */
                logEvent = EnableDebugLogs ? VirtIoSndEventqShouldLogRareCounter(eventCount) : FALSE;
                break;
            case VIRTIO_SND_EVENT_KIND_CTL_NOTIFY:
                eventCount = InterlockedIncrement(&Stats->CtlNotify);
                break;
            default:
                eventCount = InterlockedIncrement(&Stats->UnknownType);
                /* Unknown types are logged at a low rate to avoid log spam. */
                logEvent = EnableDebugLogs ? VirtIoSndEventqShouldLogRareCounter(eventCount) : FALSE;
                break;
            }

            if (logEvent) {
                VIRTIOSND_TRACE(
                    "eventq: %s (0x%08lX) data=0x%08lX len=%lu count=%ld\n",
                    VirtioSndEventTypeToString(evt.Type),
                    evt.Type,
                    evt.Data,
                    (ULONG)UsedLen,
                    eventCount);

                /*
                 * If the device wrote more than the standard header, treat it as
                 * future extension bytes and ignore them.
                 */
                if (cappedLen > (UINT32)sizeof(VIRTIO_SND_EVENT)) {
                    VIRTIOSND_TRACE(
                        "eventq: extra payload bytes (%lu > %Iu) ignored\n",
                        (ULONG)cappedLen,
                        sizeof(VIRTIO_SND_EVENT));
                }
            }
        } else {
            if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
                VIRTIOSND_TRACE_ERROR("eventq: failed to parse event (len=%lu): 0x%08X\n", (ULONG)cappedLen, (UINT)status);
            }
        }
    } else if (UsedLen != 0) {
        InterlockedIncrement(&Stats->ShortBuffers);
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR(
                "eventq: short completion ignored (%lu < %Iu)\n",
                (ULONG)UsedLen,
                sizeof(VIRTIO_SND_EVENT));
        }
    }

    /*
     * Dispatch parsed events to the optional higher-level callback (WaveRT).
     *
     * Contract v1 must remain correct without eventq; treat this as best-effort
     * and skip dispatch during teardown.
     */
    if (haveEvent && Started) {
        EVT_VIRTIOSND_EVENTQ_EVENT* cb;
        void* cbCtx;
        KIRQL oldIrql;

        cb = NULL;
        cbCtx = NULL;
        oldIrql = PASSIVE_LEVEL;

        if (CallbackState != NULL &&
            CallbackState->Lock != NULL &&
            CallbackState->Callback != NULL &&
            CallbackState->CallbackContext != NULL) {
            KeAcquireSpinLock(CallbackState->Lock, &oldIrql);
            cb = *CallbackState->Callback;
            cbCtx = *CallbackState->CallbackContext;
            /*
             * Bump the in-flight counter while still holding the lock so that a
             * concurrent callback teardown (clearing the callback and waiting for
             * CallbackInFlight==0) cannot race with us between releasing the lock
             * and incrementing the counter.
             */
            if (cb != NULL && CallbackState->CallbackInFlight != NULL) {
                InterlockedIncrement(CallbackState->CallbackInFlight);
            }
            KeReleaseSpinLock(CallbackState->Lock, oldIrql);
        }

        if (cb != NULL) {
            cb(cbCtx, evtType, evtData);
            if (CallbackState != NULL && CallbackState->CallbackInFlight != NULL) {
                InterlockedDecrement(CallbackState->CallbackInFlight);
            }
        } else if (evtType == VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED &&
                   PeriodState != NULL &&
                   PeriodState->SignalStreamNotification != NULL &&
                   evtData < PeriodState->StreamCount) {
            /*
             * Optional pacing signal:
             * If WaveRT registered a notification event object for this stream,
             * signal it best-effort. If a higher-level callback is registered,
             * it may queue the WaveRT DPC which signals the event after updating
             * PacketCount; avoid double-signaling by only doing this when no
             * callback is present.
             *
             * Validate stream id against PeriodState->StreamCount to avoid calling
             * into higher layers with device-controlled out-of-range values.
             */
            (VOID)PeriodState->SignalStreamNotification(PeriodState->SignalStreamNotificationContext, evtData);
        }
    }

    if (RepostMask != NULL) {
        /* Caller will repost/kick after draining used ring. */
        return TRUE;
    }

    sg.addr = BufferPool->DmaAddr + (UINT64)off;
    sg.len = (UINT32)VIRTIOSND_EVENTQ_BUFFER_SIZE;
    sg.write = TRUE;

    status = VirtioSndQueueSubmit(Queue, &sg, 1, Cookie);
    if (!NT_SUCCESS(status)) {
        if (EnableDebugLogs && VirtIoSndEventqShouldRateLimitLog(&s_eventqErrorLog)) {
            VIRTIOSND_TRACE_ERROR("eventq repost failed: 0x%08X (cookie=%p)\n", (UINT)status, Cookie);
        }
        return FALSE;
    }

    return TRUE;
}
