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

int main(void)
{
    test_eventq_null_cookie_does_not_repost();
    test_eventq_cookie_out_of_range_is_rejected();
    test_eventq_cookie_misaligned_is_rejected();
    test_eventq_used_len_overflow_is_ignored_but_buffer_reposted();
    test_eventq_well_formed_events_update_stats_and_repost();

    printf("virtiosnd_eventq_drain_tests: PASS\n");
    return 0;
}
