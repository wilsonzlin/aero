/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_host_queue.h"

typedef struct _EVENTQ_TEST_CTX {
    ULONG calls;
    ULONG last_type;
    ULONG last_data;

    ULONG period_count[2];
    ULONG xrun_count[2];
    ULONG parse_failures;
} EVENTQ_TEST_CTX;

static VOID EventqTestOnParsedEvent(_Inout_ EVENTQ_TEST_CTX* Ctx, _In_ const VIRTIO_SND_EVENT_PARSED* Event)
{
    TEST_ASSERT(Ctx != NULL);
    TEST_ASSERT(Event != NULL);

    Ctx->calls++;
    Ctx->last_type = Event->Type;
    Ctx->last_data = Event->Data;

    if (Event->Type == VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED && Event->Data < 2u) {
        Ctx->period_count[Event->Data]++;
    } else if (Event->Type == VIRTIO_SND_EVT_PCM_XRUN && Event->Data < 2u) {
        Ctx->xrun_count[Event->Data]++;
    }
}

static ULONG EventqTestDrainUsed(_In_ const VIRTIOSND_QUEUE* Queue, _Inout_ EVENTQ_TEST_CTX* Ctx)
{
    ULONG drained;

    TEST_ASSERT(Queue != NULL);
    TEST_ASSERT(Ctx != NULL);

    drained = 0;
    for (;;) {
        void* cookie;
        UINT32 usedLen;
        VIRTIO_SND_EVENT_PARSED evt;
        NTSTATUS status;

        cookie = NULL;
        usedLen = 0;
        RtlZeroMemory(&evt, sizeof(evt));

        if (!VirtioSndQueuePopUsed(Queue, &cookie, &usedLen)) {
            break;
        }

        drained++;
        status = VirtioSndParseEvent(cookie, usedLen, &evt);
        if (NT_SUCCESS(status)) {
            EventqTestOnParsedEvent(Ctx, &evt);
        } else {
            Ctx->parse_failures++;
        }
    }

    return drained;
}

static void test_eventq_drain_ignores_short_messages(void)
{
    VIRTIOSND_HOST_QUEUE q;
    EVENTQ_TEST_CTX ctx;
    VIRTIO_SND_EVENT evt;
    ULONG drained;

    VirtioSndHostQueueInit(&q, 8);
    RtlZeroMemory(&ctx, sizeof(ctx));
    RtlZeroMemory(&evt, sizeof(evt));

    evt.type = VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED;
    evt.data = VIRTIO_SND_PLAYBACK_STREAM_ID;

    VirtioSndHostQueuePushUsed(&q, &evt, 0u);
    VirtioSndHostQueuePushUsed(&q, &evt, (UINT32)sizeof(evt) - 1u);

    drained = EventqTestDrainUsed(&q.Queue, &ctx);
    TEST_ASSERT(drained == 2u);
    TEST_ASSERT(ctx.calls == 0u);
    TEST_ASSERT(ctx.parse_failures == 2u);
}

static void test_eventq_drain_dispatches_pcm_events(void)
{
    VIRTIOSND_HOST_QUEUE q;
    EVENTQ_TEST_CTX ctx;
    VIRTIO_SND_EVENT evt0;
    VIRTIO_SND_EVENT evt1;
    ULONG drained;

    VirtioSndHostQueueInit(&q, 8);
    RtlZeroMemory(&ctx, sizeof(ctx));

    /* Inject two used entries as if the device completed event buffers. */
    evt0.type = VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED;
    evt0.data = VIRTIO_SND_PLAYBACK_STREAM_ID;
    evt1.type = VIRTIO_SND_EVT_PCM_XRUN;
    evt1.data = VIRTIO_SND_CAPTURE_STREAM_ID;

    VirtioSndHostQueuePushUsed(&q, &evt0, (UINT32)sizeof(evt0));
    VirtioSndHostQueuePushUsed(&q, &evt1, (UINT32)sizeof(evt1));

    drained = EventqTestDrainUsed(&q.Queue, &ctx);
    TEST_ASSERT(drained == 2u);
    TEST_ASSERT(ctx.calls == 2u);
    TEST_ASSERT(ctx.period_count[VIRTIO_SND_PLAYBACK_STREAM_ID] == 1u);
    TEST_ASSERT(ctx.xrun_count[VIRTIO_SND_CAPTURE_STREAM_ID] == 1u);
    TEST_ASSERT(ctx.parse_failures == 0u);
}

int main(void)
{
    test_eventq_drain_ignores_short_messages();
    test_eventq_drain_dispatches_pcm_events();

    printf("virtiosnd_eventq_tests: PASS\n");
    return 0;
}
