/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_host_queue.h"
#include "virtiosnd_rx.h"
#include "virtiosnd_limits.h"

typedef struct _RX_COMPLETION_CAPTURE {
    int Called;
    void* Cookie;
    NTSTATUS CompletionStatus;
    ULONG VirtioStatus;
    ULONG LatencyBytes;
    ULONG PayloadBytes;
    UINT32 UsedLen;
} RX_COMPLETION_CAPTURE;

static VOID RxCompletionCb(
    _In_opt_ void* Cookie,
    _In_ NTSTATUS CompletionStatus,
    _In_ ULONG VirtioStatus,
    _In_ ULONG LatencyBytes,
    _In_ ULONG PayloadBytes,
    _In_ UINT32 UsedLen,
    _In_opt_ void* Context)
{
    RX_COMPLETION_CAPTURE* cap = (RX_COMPLETION_CAPTURE*)Context;
    TEST_ASSERT(cap != NULL);

    cap->Called++;
    cap->Cookie = Cookie;
    cap->CompletionStatus = CompletionStatus;
    cap->VirtioStatus = VirtioStatus;
    cap->LatencyBytes = LatencyBytes;
    cap->PayloadBytes = PayloadBytes;
    cap->UsedLen = UsedLen;
}

static void test_rx_init_sets_fixed_stream_id(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    ULONG i;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 2u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    TEST_ASSERT(rx.RequestCount == 2u);
    TEST_ASSERT(rx.FreeCount == 2u);
    TEST_ASSERT(rx.InflightCount == 0u);

    for (i = 0; i < rx.RequestCount; i++) {
        const VIRTIO_SND_TX_HDR* hdr = rx.Requests[i].HdrVa;
        TEST_ASSERT(hdr != NULL);
        TEST_ASSERT(hdr->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
        TEST_ASSERT(hdr->reserved == 0u);
    }

    VirtIoSndRxUninit(&rx);
}

static void test_rx_init_default_and_clamped_request_count(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 0u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(rx.RequestCount == 16u);
    TEST_ASSERT(rx.FreeCount == 16u);
    VirtIoSndRxUninit(&rx);

    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1000u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(rx.RequestCount == (ULONG)VIRTIOSND_QUEUE_SIZE_RXQ);
    TEST_ASSERT(rx.FreeCount == (ULONG)VIRTIOSND_QUEUE_SIZE_RXQ);
    VirtIoSndRxUninit(&rx);
}

static void test_rx_submit_sg_validates_segments(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT segs[16];

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtIoSndRxSubmitSg(&rx, NULL, 0, NULL);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    RtlZeroMemory(segs, sizeof(segs));
    status = VirtIoSndRxSubmitSg(&rx, segs, 0, NULL);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtIoSndRxSubmitSg(&rx, segs, (USHORT)(VIRTIOSND_RX_MAX_PAYLOAD_SG + 1u), NULL);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    segs[0].addr = 0x1000;
    segs[0].len = 0;
    status = VirtIoSndRxSubmitSg(&rx, segs, 1, NULL);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Must be 2-byte aligned (mono S16). */
    segs[0].len = 1;
    status = VirtIoSndRxSubmitSg(&rx, segs, 1, NULL);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_submit_sg_rejects_payload_overflow(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT segs[2];

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /* payloadBytes + len overflow should be caught before alignment checks. */
    segs[0].addr = 0x1000;
    segs[0].len = 0xFFFFFFFFu;
    segs[1].addr = 0x2000;
    segs[1].len = 2u;

    status = VirtIoSndRxSubmitSg(&rx, segs, 2, NULL);
    TEST_ASSERT(status == STATUS_INTEGER_OVERFLOW);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_submit_sg_rejects_payload_over_contract_limit(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = VIRTIOSND_MAX_PCM_PAYLOAD_BYTES + 2u; /* mono S16 frame size */
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, NULL);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_submit_sg_allows_payload_at_contract_limit(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    VIRTIOSND_RX_REQUEST* req;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = VIRTIOSND_MAX_PCM_PAYLOAD_BYTES; /* mono S16 frame size */
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /* Complete it to keep teardown deterministic. */
    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);
    req->StatusVa->status = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    (VOID)VirtIoSndRxDrainCompletions(&rx, NULL, NULL);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_submit_sg_builds_descriptor_chain(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT segs[2];
    void* userCookie;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    segs[0].addr = 0x1000;
    segs[0].len = 8;
    segs[1].addr = 0x2000;
    segs[1].len = 4;
    userCookie = (void*)0xDEADBEEFul;

    status = VirtIoSndRxSubmitSg(&rx, segs, 2, userCookie);
    TEST_ASSERT(status == STATUS_SUCCESS);

    TEST_ASSERT(q.LastCookie != NULL);
    TEST_ASSERT(q.LastSgCount == 4u);

    {
        const VIRTIOSND_RX_REQUEST* req = (const VIRTIOSND_RX_REQUEST*)q.LastCookie;

        TEST_ASSERT(req->Cookie == userCookie);
        TEST_ASSERT(req->PayloadBytes == 12u);

        TEST_ASSERT(q.LastSg[0].addr == req->HdrDma);
        TEST_ASSERT(q.LastSg[0].len == (UINT32)sizeof(VIRTIO_SND_TX_HDR));
        TEST_ASSERT(q.LastSg[0].write == FALSE);

        TEST_ASSERT(q.LastSg[1].addr == segs[0].addr);
        TEST_ASSERT(q.LastSg[1].len == segs[0].len);
        TEST_ASSERT(q.LastSg[1].write == TRUE);

        TEST_ASSERT(q.LastSg[2].addr == segs[1].addr);
        TEST_ASSERT(q.LastSg[2].len == segs[1].len);
        TEST_ASSERT(q.LastSg[2].write == TRUE);

        TEST_ASSERT(q.LastSg[3].addr == req->StatusDma);
        TEST_ASSERT(q.LastSg[3].len == (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
        TEST_ASSERT(q.LastSg[3].write == TRUE);
    }

    TEST_ASSERT(rx.FreeCount == 0u);
    TEST_ASSERT(rx.InflightCount == 1u);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_on_used_uses_registered_callback(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    RX_COMPLETION_CAPTURE cap;
    VIRTIOSND_RX_REQUEST* req;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    RtlZeroMemory(&cap, sizeof(cap));
    VirtIoSndRxSetCompletionCallback(&rx, RxCompletionCb, &cap);

    seg.addr = 0x1000;
    seg.len = 4;
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0xABCDu);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);

    req->StatusVa->status = VIRTIO_SND_S_OK;
    req->StatusVa->latency_bytes = 77u;

    VirtIoSndRxOnUsed(&rx, req, (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 4u));

    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0xABCDu);
    TEST_ASSERT(cap.CompletionStatus == STATUS_SUCCESS);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(cap.LatencyBytes == 77u);
    TEST_ASSERT(cap.PayloadBytes == 4u);
    TEST_ASSERT(cap.UsedLen == (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 4u));
    TEST_ASSERT(rx.FreeCount == 1u);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_ok_with_no_payload_is_success_and_payload_zero(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    RX_COMPLETION_CAPTURE cap;
    VIRTIOSND_RX_REQUEST* req;
    ULONG drained;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = 4;
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0xABCDu);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);

    req->StatusVa->status = VIRTIO_SND_S_OK;
    req->StatusVa->latency_bytes = 0u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));

    drained = VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(drained == 1u);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0xABCDu);
    TEST_ASSERT(cap.CompletionStatus == STATUS_SUCCESS);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(cap.PayloadBytes == 0u);
    TEST_ASSERT(cap.UsedLen == (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(rx.FatalError == FALSE);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_no_free_requests_drops_submission(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    VIRTIOSND_RX_REQUEST* req;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = 4;

    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x1u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(rx.FreeCount == 0u);

    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x2u);
    TEST_ASSERT(status == STATUS_INSUFFICIENT_RESOURCES);
    TEST_ASSERT(rx.DroppedDueToNoRequests == 1u);

    /* Complete the first request so uninit runs with lists in a clean state. */
    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);
    req->StatusVa->status = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 4u));
    (VOID)VirtIoSndRxDrainCompletions(&rx, NULL, NULL);
    TEST_ASSERT(rx.FreeCount == 1u);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_not_supp_sets_fatal(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    RX_COMPLETION_CAPTURE cap;
    VIRTIOSND_RX_REQUEST* req;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = 4;
    RtlZeroMemory(&cap, sizeof(cap));

    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x123u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);
    req->StatusVa->status = VIRTIO_SND_S_NOT_SUPP;
    req->StatusVa->latency_bytes = 0u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    (VOID)VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);

    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0x123u);
    TEST_ASSERT(cap.CompletionStatus == STATUS_NOT_SUPPORTED);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_NOT_SUPP);
    TEST_ASSERT(rx.FatalError == TRUE);
    TEST_ASSERT(rx.CompletedByStatus[VIRTIO_SND_S_NOT_SUPP] == 1u);

    /* Once fatal, submissions fail fast. */
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x456u);
    TEST_ASSERT(status == STATUS_INVALID_DEVICE_STATE);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_used_len_clamps_payload_and_io_err_is_not_fatal(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    RX_COMPLETION_CAPTURE cap;
    VIRTIOSND_RX_REQUEST* req;
    ULONG drained;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /* Submit a 12-byte capture buffer. */
    seg.addr = 0x1000;
    seg.len = 12;
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x1111u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);

    /* Device returns fewer bytes than requested. */
    req->StatusVa->status = VIRTIO_SND_S_OK;
    req->StatusVa->latency_bytes = 12u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 4u));
    drained = VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(drained == 1u);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0x1111u);
    TEST_ASSERT(cap.CompletionStatus == STATUS_SUCCESS);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(cap.LatencyBytes == 12u);
    TEST_ASSERT(cap.PayloadBytes == 4u);
    TEST_ASSERT(rx.FreeCount == 1u);
    TEST_ASSERT(rx.InflightCount == 0u);

    /* Re-submit a 12-byte capture buffer for the remaining cases. */
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x1111u);
    TEST_ASSERT(status == STATUS_SUCCESS);
    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);

    /* Device returns more bytes than requested -> clamp to requested payload size. */
    req->StatusVa->status = VIRTIO_SND_S_OK;
    req->StatusVa->latency_bytes = 55u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 20u));
    drained = VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(drained == 1u);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0x1111u);
    TEST_ASSERT(cap.CompletionStatus == STATUS_SUCCESS);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(cap.LatencyBytes == 55u);
    TEST_ASSERT(cap.PayloadBytes == 12u);
    TEST_ASSERT(cap.UsedLen == (UINT32)(sizeof(VIRTIO_SND_PCM_STATUS) + 20u));
    TEST_ASSERT(rx.FreeCount == 1u);
    TEST_ASSERT(rx.InflightCount == 0u);

    /* IO_ERR should surface as INVALID_DEVICE_STATE but not set FatalError. */
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x2222u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);
    req->StatusVa->status = VIRTIO_SND_S_IO_ERR;
    req->StatusVa->latency_bytes = 0u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    drained = VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(drained == 1u);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0x2222u);
    TEST_ASSERT(cap.CompletionStatus == STATUS_INVALID_DEVICE_STATE);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_IO_ERR);
    TEST_ASSERT(cap.PayloadBytes == 0u);
    TEST_ASSERT(rx.FatalError == FALSE);
    TEST_ASSERT(rx.CompletedByStatus[VIRTIO_SND_S_IO_ERR] == 1u);

    /* Unknown status should not set FatalError. */
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x3333u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);
    req->StatusVa->status = 0x1234u;
    req->StatusVa->latency_bytes = 0u;
    VirtioSndHostQueuePushUsed(&q, req, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    drained = VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(drained == 1u);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.Cookie == (void*)0x3333u);
    TEST_ASSERT(cap.CompletionStatus == STATUS_DEVICE_PROTOCOL_ERROR);
    TEST_ASSERT(cap.VirtioStatus == 0x1234u);
    TEST_ASSERT(rx.FatalError == FALSE);
    TEST_ASSERT(rx.CompletedUnknownStatus == 1u);

    VirtIoSndRxUninit(&rx);
}

static void test_rx_used_len_too_small_sets_bad_msg_and_fatal(void)
{
    VIRTIOSND_RX_ENGINE rx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;
    RX_COMPLETION_CAPTURE cap;
    VIRTIOSND_RX_REQUEST* req;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtIoSndRxInit(&rx, &dma, &q.Queue, 2u, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.addr = 0x1000;
    seg.len = 4;
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x3333u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    req = (VIRTIOSND_RX_REQUEST*)q.LastCookie;
    TEST_ASSERT(req != NULL);

    /* UsedLen < status bytes => treat as BAD_MSG. */
    VirtioSndHostQueuePushUsed(&q, req, 4u);
    (VOID)VirtIoSndRxDrainCompletions(&rx, RxCompletionCb, &cap);
    TEST_ASSERT(cap.Called == 1);
    TEST_ASSERT(cap.VirtioStatus == VIRTIO_SND_S_BAD_MSG);
    TEST_ASSERT(cap.CompletionStatus == STATUS_INVALID_PARAMETER);
    TEST_ASSERT(rx.FatalError == TRUE);

    /* Once fatal, new submissions fail fast. */
    RtlZeroMemory(&cap, sizeof(cap));
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void*)0x4444u);
    TEST_ASSERT(status == STATUS_INVALID_DEVICE_STATE);

    VirtIoSndRxUninit(&rx);
}

int main(void)
{
    test_rx_init_sets_fixed_stream_id();
    test_rx_init_default_and_clamped_request_count();
    test_rx_submit_sg_validates_segments();
    test_rx_submit_sg_rejects_payload_overflow();
    test_rx_submit_sg_rejects_payload_over_contract_limit();
    test_rx_submit_sg_allows_payload_at_contract_limit();
    test_rx_submit_sg_builds_descriptor_chain();
    test_rx_on_used_uses_registered_callback();
    test_rx_ok_with_no_payload_is_success_and_payload_zero();
    test_rx_no_free_requests_drops_submission();
    test_rx_not_supp_sets_fatal();
    test_rx_used_len_clamps_payload_and_io_err_is_not_fatal();
    test_rx_used_len_too_small_sets_bad_msg_and_fatal();

    printf("virtiosnd_rx_tests: PASS\n");
    return 0;
}
