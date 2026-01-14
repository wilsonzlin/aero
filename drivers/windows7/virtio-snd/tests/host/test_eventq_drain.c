/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtiosnd_eventq.h"
#include "virtiosnd_host_queue.h"

/*
 * Host shim for topology.c integration.
 *
 * The production driver updates PortCls topology jack state from eventq JACK
 * notifications. topology.c depends on PortCls/KS headers, so host tests provide
 * a minimal stub to validate that eventq handling calls into the topology layer
 * without pulling in the full PortCls stack.
 */
static ULONG g_topology_update_calls;
static ULONG g_topology_last_jack_id;
static BOOLEAN g_topology_last_connected;
static BOOLEAN g_topology_last_notify_even_if_unchanged;

VOID VirtIoSndTopology_UpdateJackStateEx(_In_ ULONG JackId, _In_ BOOLEAN IsConnected, _In_ BOOLEAN NotifyEvenIfUnchanged)
{
    g_topology_update_calls++;
    g_topology_last_jack_id = JackId;
    g_topology_last_connected = IsConnected;
    g_topology_last_notify_even_if_unchanged = NotifyEvenIfUnchanged;
}

typedef struct _TEST_EVENTQ_CB_REC {
    ULONG Calls;
    ULONG LastType;
    ULONG LastData;
} TEST_EVENTQ_CB_REC;

typedef struct _TEST_EVENTQ_CB_INFLIGHT_REC {
    TEST_EVENTQ_CB_REC Base;
    volatile LONG* InFlight;
    LONG InFlightExpectedAtCall;
} TEST_EVENTQ_CB_INFLIGHT_REC;

typedef struct _TEST_EVENTQ_SIGNAL_REC {
    ULONG Calls;
    ULONG LastStreamId;
} TEST_EVENTQ_SIGNAL_REC;

static VOID TestEventqCallback(_In_opt_ void* Context, _In_ ULONG Type, _In_ ULONG Data)
{
    TEST_EVENTQ_CB_REC* rec;
    rec = (TEST_EVENTQ_CB_REC*)Context;
    if (rec == NULL) {
        return;
    }
    rec->Calls++;
    rec->LastType = Type;
    rec->LastData = Data;
}

static VOID TestEventqCallbackCheckInFlight(_In_opt_ void* Context, _In_ ULONG Type, _In_ ULONG Data)
{
    TEST_EVENTQ_CB_INFLIGHT_REC* rec;

    rec = (TEST_EVENTQ_CB_INFLIGHT_REC*)Context;
    TEST_ASSERT(rec != NULL);

    if (rec->InFlight != NULL) {
        TEST_ASSERT(InterlockedCompareExchange(rec->InFlight, 0, 0) == rec->InFlightExpectedAtCall);
    }

    rec->Base.Calls++;
    rec->Base.LastType = Type;
    rec->Base.LastData = Data;
}

static BOOLEAN TestSignalStreamNotification(_In_opt_ void* Context, _In_ ULONG StreamId)
{
    TEST_EVENTQ_SIGNAL_REC* rec;
    rec = (TEST_EVENTQ_SIGNAL_REC*)Context;
    if (rec == NULL) {
        return FALSE;
    }
    rec->Calls++;
    rec->LastStreamId = StreamId;
    return TRUE;
}

static void TestInitPool(_Out_ VIRTIOSND_DMA_BUFFER* Pool, _In_ ULONG BufferCount)
{
    SIZE_T bytes;
    void* mem;

    RtlZeroMemory(Pool, sizeof(*Pool));

    if (BufferCount == 0) {
        BufferCount = 1;
    }

    bytes = (SIZE_T)BufferCount * (SIZE_T)VIRTIOSND_EVENTQ_BUFFER_SIZE;
    mem = calloc(1, bytes);
    TEST_ASSERT(mem != NULL);

    Pool->Va = mem;
    Pool->Size = bytes;
    Pool->DmaAddr = (UINT64)(uintptr_t)mem;
    Pool->IsCommonBuffer = TRUE;
    Pool->CacheEnabled = FALSE;
}

static void TestFreePool(_Inout_ VIRTIOSND_DMA_BUFFER* Pool)
{
    if (Pool == NULL) {
        return;
    }
    free(Pool->Va);
    RtlZeroMemory(Pool, sizeof(*Pool));
}

static void test_eventq_null_cookie_does_not_repost(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    VIRTIOSND_JACK_STATE jack;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    VirtIoSndJackStateInit(&jack);
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    g_topology_update_calls = 0;

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        &jack,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/NULL,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == FALSE);
    TEST_ASSERT(q.SubmitCalls == 0);
    TEST_ASSERT(stats.Completions == 0);
    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(g_topology_update_calls == 0);

    TestFreePool(&pool);
}

static void test_eventq_cookie_out_of_range_is_rejected(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    BOOLEAN reposted;
    void* cookie;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    g_topology_update_calls = 0;

    cookie = (uint8_t*)pool.Va + pool.Size; /* one-past-end */

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        cookie,
        /*UsedLen=*/0u,
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == FALSE);
    TEST_ASSERT(q.SubmitCalls == 0);
    TEST_ASSERT(stats.Completions == 0);
    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(g_topology_update_calls == 0);

    TestFreePool(&pool);
}

static void test_eventq_cookie_misaligned_is_rejected(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    BOOLEAN reposted;
    void* cookie;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    g_topology_update_calls = 0;

    cookie = (uint8_t*)pool.Va + 1; /* misaligned within range */

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        cookie,
        /*UsedLen=*/0u,
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == FALSE);
    TEST_ASSERT(q.SubmitCalls == 0);
    TEST_ASSERT(stats.Completions == 0);
    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(g_topology_update_calls == 0);

    TestFreePool(&pool);
}

static void test_eventq_used_len_overflow_is_ignored_but_buffer_reposted(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    BOOLEAN reposted;
    void* cookie;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    g_topology_update_calls = 0;

    cookie = pool.Va;

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        cookie,
        /*UsedLen=*/(UINT32)(VIRTIOSND_EVENTQ_BUFFER_SIZE + 1u),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(q.SubmitCalls == 1);
    TEST_ASSERT(q.LastCookie == cookie);
    TEST_ASSERT(q.LastSgCount == 1u);
    TEST_ASSERT(q.LastSg[0].addr == pool.DmaAddr);
    TEST_ASSERT(q.LastSg[0].len == VIRTIOSND_EVENTQ_BUFFER_SIZE);
    TEST_ASSERT(q.LastSg[0].write == TRUE);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 0);
    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(g_topology_update_calls == 0);

    TestFreePool(&pool);
}

static void test_eventq_well_formed_events_update_stats_and_repost(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    VIRTIOSND_JACK_STATE jack;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    BOOLEAN reposted;
    uint8_t* buf0;
    uint8_t* buf1;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    VirtIoSndJackStateInit(&jack);
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    g_topology_update_calls = 0;
    g_topology_last_jack_id = 0;
    g_topology_last_connected = TRUE;

    buf0 = (uint8_t*)pool.Va;
    buf1 = buf0 + VIRTIOSND_EVENTQ_BUFFER_SIZE;

    /* JACK_DISCONNECTED (jack_id=1) */
    {
        const uint8_t evt[] = {
            0x01, 0x10, 0x00, 0x00, /* type = JACK_DISCONNECTED */
            0x01, 0x00, 0x00, 0x00, /* data = jack_id (1) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        &jack,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(q.SubmitCalls == 1);
    TEST_ASSERT(q.LastCookie == buf0);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 1);
    TEST_ASSERT(stats.JackDisconnected == 1);
    TEST_ASSERT(cbRec.Calls == 1);
    TEST_ASSERT(cbRec.LastType == VIRTIO_SND_EVT_JACK_DISCONNECTED);
    TEST_ASSERT(cbRec.LastData == 1u);
    TEST_ASSERT(g_topology_update_calls == 1);
    TEST_ASSERT(g_topology_last_jack_id == 1u);
    TEST_ASSERT(g_topology_last_connected == FALSE);
    TEST_ASSERT(g_topology_last_notify_even_if_unchanged == TRUE);
    TEST_ASSERT(VirtIoSndJackStateIsConnected(&jack, 1u) == FALSE);

    /* PCM_PERIOD_ELAPSED (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf1, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        &jack,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf1,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(q.SubmitCalls == 2);
    TEST_ASSERT(q.LastCookie == buf1);
    TEST_ASSERT(stats.Completions == 2);
    TEST_ASSERT(stats.Parsed == 2);
    TEST_ASSERT(stats.PcmPeriodElapsed == 1);
    TEST_ASSERT(cbRec.Calls == 2);
    TEST_ASSERT(cbRec.LastType == VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED);
    TEST_ASSERT(cbRec.LastData == 0u);
    TEST_ASSERT(g_topology_update_calls == 1); /* unchanged by PCM event */

    TestFreePool(&pool);
}

static void test_eventq_period_elapsed_signals_when_callback_missing(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;
    TEST_EVENTQ_SIGNAL_REC signalRec;
    LONG seq[2];
    LONGLONG lastTime[2];
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = NULL;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    RtlZeroMemory(&signalRec, sizeof(signalRec));
    RtlZeroMemory(seq, sizeof(seq));
    RtlZeroMemory(lastTime, sizeof(lastTime));

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = TestSignalStreamNotification;
    period.SignalStreamNotificationContext = &signalRec;
    period.PcmPeriodSeq = seq;
    period.PcmLastPeriodEventTime100ns = lastTime;
    period.StreamCount = 2u;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_PERIOD_ELAPSED (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        &period,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(q.SubmitCalls == 1);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 1);
    TEST_ASSERT(stats.PcmPeriodElapsed == 1);

    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(signalRec.Calls == 1);
    TEST_ASSERT(signalRec.LastStreamId == 0u);
    TEST_ASSERT(seq[0] == 1);
    TEST_ASSERT(lastTime[0] != 0);

    TestFreePool(&pool);
}

static void test_eventq_period_elapsed_does_not_signal_when_callback_present(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;
    TEST_EVENTQ_SIGNAL_REC signalRec;
    LONG seq[2];
    LONGLONG lastTime[2];
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    RtlZeroMemory(&signalRec, sizeof(signalRec));
    RtlZeroMemory(seq, sizeof(seq));
    RtlZeroMemory(lastTime, sizeof(lastTime));

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = TestSignalStreamNotification;
    period.SignalStreamNotificationContext = &signalRec;
    period.PcmPeriodSeq = seq;
    period.PcmLastPeriodEventTime100ns = lastTime;
    period.StreamCount = 2u;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_PERIOD_ELAPSED (stream_id=1) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x01, 0x00, 0x00, 0x00, /* data = stream_id (1) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        &period,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(q.SubmitCalls == 1);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 1);
    TEST_ASSERT(stats.PcmPeriodElapsed == 1);

    TEST_ASSERT(cbRec.Calls == 1);
    TEST_ASSERT(cbRec.LastType == VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED);
    TEST_ASSERT(cbRec.LastData == 1u);
    TEST_ASSERT(signalRec.Calls == 0);
    TEST_ASSERT(seq[1] == 1);
    TEST_ASSERT(lastTime[1] != 0);

    TestFreePool(&pool);
}

static void test_eventq_period_elapsed_signals_without_callback_state(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;
    TEST_EVENTQ_SIGNAL_REC signalRec;
    LONG seq[2];
    LONGLONG lastTime[2];
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));

    RtlZeroMemory(&signalRec, sizeof(signalRec));
    RtlZeroMemory(seq, sizeof(seq));
    RtlZeroMemory(lastTime, sizeof(lastTime));

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = TestSignalStreamNotification;
    period.SignalStreamNotificationContext = &signalRec;
    period.PcmPeriodSeq = seq;
    period.PcmLastPeriodEventTime100ns = lastTime;
    period.StreamCount = 2u;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_PERIOD_ELAPSED (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        /*CallbackState=*/NULL,
        &period,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(signalRec.Calls == 1);
    TEST_ASSERT(signalRec.LastStreamId == 0u);
    TEST_ASSERT(seq[0] == 1);
    TEST_ASSERT(lastTime[0] != 0);

    TestFreePool(&pool);
}

static void test_eventq_period_elapsed_out_of_range_stream_is_ignored(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;
    TEST_EVENTQ_SIGNAL_REC signalRec;
    LONG seq[2];
    LONGLONG lastTime[2];
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));

    RtlZeroMemory(&signalRec, sizeof(signalRec));
    RtlZeroMemory(seq, sizeof(seq));
    RtlZeroMemory(lastTime, sizeof(lastTime));

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = TestSignalStreamNotification;
    period.SignalStreamNotificationContext = &signalRec;
    period.PcmPeriodSeq = seq;
    period.PcmLastPeriodEventTime100ns = lastTime;
    period.StreamCount = 2u;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_PERIOD_ELAPSED (stream_id=99) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x63, 0x00, 0x00, 0x00, /* data = stream_id (99) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        /*CallbackState=*/NULL,
        &period,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(stats.PcmPeriodElapsed == 1);
    TEST_ASSERT(signalRec.Calls == 0);
    TEST_ASSERT(seq[0] == 0);
    TEST_ASSERT(seq[1] == 0);
    TEST_ASSERT(lastTime[0] == 0);
    TEST_ASSERT(lastTime[1] == 0);

    TestFreePool(&pool);
}

static void test_eventq_repost_mask_sets_bit_without_submitting(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    VIRTIOSND_JACK_STATE jack;
    uint8_t* buf1;
    ULONGLONG repostMask;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 2);
    RtlZeroMemory(&stats, sizeof(stats));
    VirtIoSndJackStateInit(&jack);

    buf1 = (uint8_t*)pool.Va + VIRTIOSND_EVENTQ_BUFFER_SIZE; /* idx=1 */
    repostMask = 0;

    /* PCM_PERIOD_ELAPSED (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf1, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        &jack,
        /*CallbackState=*/NULL,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf1,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/&repostMask);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(repostMask == (1ull << 1));
    TEST_ASSERT(q.SubmitCalls == 0);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 1);

    TestFreePool(&pool);
}

static void test_eventq_callback_inflight_counter_is_balanced(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_INFLIGHT_REC cbRec;
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbInFlight = 0;

    cbRec.InFlight = &cbInFlight;
    cbRec.InFlightExpectedAtCall = 1;

    cbFn = TestEventqCallbackCheckInFlight;
    cbCtx = &cbRec;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_XRUN (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x01, 0x11, 0x00, 0x00, /* type = PCM_XRUN */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        /*PeriodState=*/NULL,
        /*Started=*/TRUE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(cbRec.Base.Calls == 1);
    TEST_ASSERT(cbRec.Base.LastType == VIRTIO_SND_EVT_PCM_XRUN);
    TEST_ASSERT(cbRec.Base.LastData == 0u);
    TEST_ASSERT(cbInFlight == 0);

    TestFreePool(&pool);
}

static void test_eventq_not_started_skips_callback_and_signal(void)
{
    VIRTIOSND_HOST_QUEUE q;
    VIRTIOSND_DMA_BUFFER pool;
    VIRTIOSND_EVENTQ_STATS stats;
    KSPIN_LOCK lock;
    EVT_VIRTIOSND_EVENTQ_EVENT* cbFn;
    void* cbCtx;
    volatile LONG cbInFlight;
    VIRTIOSND_EVENTQ_CALLBACK_STATE cbState;
    TEST_EVENTQ_CB_REC cbRec;
    VIRTIOSND_EVENTQ_PERIOD_STATE period;
    TEST_EVENTQ_SIGNAL_REC signalRec;
    LONG seq[2];
    LONGLONG lastTime[2];
    uint8_t* buf0;
    BOOLEAN reposted;

    VirtioSndHostQueueInit(&q, 8);
    TestInitPool(&pool, 1);
    RtlZeroMemory(&stats, sizeof(stats));
    KeInitializeSpinLock(&lock);

    RtlZeroMemory(&cbRec, sizeof(cbRec));
    cbFn = TestEventqCallback;
    cbCtx = &cbRec;
    cbInFlight = 0;

    cbState.Lock = &lock;
    cbState.Callback = &cbFn;
    cbState.CallbackContext = &cbCtx;
    cbState.CallbackInFlight = &cbInFlight;

    RtlZeroMemory(&signalRec, sizeof(signalRec));
    RtlZeroMemory(seq, sizeof(seq));
    RtlZeroMemory(lastTime, sizeof(lastTime));

    RtlZeroMemory(&period, sizeof(period));
    period.SignalStreamNotification = TestSignalStreamNotification;
    period.SignalStreamNotificationContext = &signalRec;
    period.PcmPeriodSeq = seq;
    period.PcmLastPeriodEventTime100ns = lastTime;
    period.StreamCount = 2u;

    buf0 = (uint8_t*)pool.Va;

    /* PCM_PERIOD_ELAPSED (stream_id=0) */
    {
        const uint8_t evt[] = {
            0x00, 0x11, 0x00, 0x00, /* type = PCM_PERIOD_ELAPSED */
            0x00, 0x00, 0x00, 0x00, /* data = stream_id (0) */
        };
        RtlCopyMemory(buf0, evt, sizeof(evt));
    }

    reposted = VirtIoSndEventqHandleUsed(
        &q.Queue,
        &pool,
        &stats,
        /*JackState=*/NULL,
        &cbState,
        &period,
        /*Started=*/FALSE,
        /*Removed=*/FALSE,
        /*Cookie=*/buf0,
        /*UsedLen=*/(UINT32)sizeof(VIRTIO_SND_EVENT),
        /*EnableDebugLogs=*/TRUE,
        /*RepostMask=*/NULL);

    TEST_ASSERT(reposted == TRUE);
    TEST_ASSERT(stats.Completions == 1);
    TEST_ASSERT(stats.Parsed == 1);
    TEST_ASSERT(stats.PcmPeriodElapsed == 1);

    TEST_ASSERT(cbRec.Calls == 0);
    TEST_ASSERT(cbInFlight == 0);
    TEST_ASSERT(signalRec.Calls == 0);

    TestFreePool(&pool);
}

int main(void)
{
    test_eventq_null_cookie_does_not_repost();
    test_eventq_cookie_out_of_range_is_rejected();
    test_eventq_cookie_misaligned_is_rejected();
    test_eventq_used_len_overflow_is_ignored_but_buffer_reposted();
    test_eventq_well_formed_events_update_stats_and_repost();
    test_eventq_period_elapsed_signals_when_callback_missing();
    test_eventq_period_elapsed_does_not_signal_when_callback_present();
    test_eventq_period_elapsed_signals_without_callback_state();
    test_eventq_period_elapsed_out_of_range_stream_is_ignored();
    test_eventq_repost_mask_sets_bit_without_submitting();
    test_eventq_callback_inflight_counter_is_balanced();
    test_eventq_not_started_skips_callback_and_signal();

    printf("virtiosnd_eventq_drain_tests: PASS\n");
    return 0;
}
