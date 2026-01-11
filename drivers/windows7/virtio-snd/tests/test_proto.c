/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "test_queue.h"
#include "virtio_snd_proto.h"
#include "virtiosnd_control.h"
#include "virtiosnd_rx.h"
#include "virtiosnd_tx.h"

/*
 * Keep assertions active in all build configurations.
 *
 * These host tests are typically built as part of a CMake Release configuration
 * in CI, which defines NDEBUG and would normally compile out assert() checks.
 * Override assert() so failures are still caught.
 */
#undef assert
#define assert(expr)                                                                                                      \
    do {                                                                                                                  \
        if (!(expr)) {                                                                                                    \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                   \
            abort();                                                                                                      \
        }                                                                                                                 \
    } while (0)

static void test_tx_rejects_misaligned_pcm_bytes(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_TX_ENGINE tx;
    uint8_t pcm[3] = {0xAA, 0xBB, 0xCC};
    NTSTATUS status;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));

    status = VirtioSndTxInit(&tx, &dma, &q.queue, 64u, 1u, FALSE);
    assert(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0, FALSE);
    assert(status == STATUS_INVALID_BUFFER_SIZE);
    assert(q.submit_count == 0);

    VirtioSndTxUninit(&tx);
    virtio_test_queue_destroy(&q);
}

static void test_tx_builds_hdr_pcm_status_chain(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_TX_ENGINE tx;
    NTSTATUS status;
    uint8_t pcm[8] = {0x10, 0x11, 0x12, 0x13, 0x20, 0x21, 0x22, 0x23};
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;
    VIRTIOSND_TX_BUFFER *buf;
    const uint8_t *payload;
    ULONG drained;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));

    status = VirtioSndTxInit(&tx, &dma, &q.queue, 64u, 1u, FALSE);
    assert(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0, FALSE);
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->sg_count == 2);
    assert(cap->sg[0].write == FALSE);
    assert(cap->sg[1].write == TRUE);
    assert(cap->sg[0].len == sizeof(VIRTIO_SND_TX_HDR) + sizeof(pcm));
    assert(cap->sg[1].len == sizeof(VIRTIO_SND_PCM_STATUS));
    assert(q.kick_count == 1);

    buf = (VIRTIOSND_TX_BUFFER *)cap->cookie;
    assert(buf != NULL);
    assert(cap->sg[0].addr == buf->DataDma);
    assert(cap->sg[1].addr == buf->StatusDma);

    {
        const VIRTIO_SND_TX_HDR *hdr = (const VIRTIO_SND_TX_HDR *)buf->DataVa;
        assert(hdr->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
        assert(hdr->reserved == 0);
    }

    payload = (const uint8_t *)buf->DataVa + sizeof(VIRTIO_SND_TX_HDR);
    assert(memcmp(payload, pcm, sizeof(pcm)) == 0);

    /* Auto-completion via test queue: verify TX drain path consumes a used entry. */
    drained = VirtioSndTxDrainCompletions(&tx);
    assert(drained == 1);

    VirtioSndTxUninit(&tx);
    virtio_test_queue_destroy(&q);
}

static void test_tx_split_payload_and_silence_fill(void)
{
    {
        VIRTIO_TEST_QUEUE q;
        VIRTIOSND_DMA_CONTEXT dma;
        VIRTIOSND_TX_ENGINE tx;
        NTSTATUS status;
        uint8_t pcm1[4] = {0x01, 0x02, 0x03, 0x04};
        uint8_t pcm2[4] = {0xF1, 0xF2, 0xF3, 0xF4};
        const VIRTIO_TEST_QUEUE_CAPTURE *cap;
        VIRTIOSND_TX_BUFFER *buf;
        const uint8_t *payload;

        virtio_test_queue_init(&q, TRUE);
        RtlZeroMemory(&dma, sizeof(dma));

        status = VirtioSndTxInit(&tx, &dma, &q.queue, 64u, 1u, FALSE);
        assert(status == STATUS_SUCCESS);

        status = VirtioSndTxSubmitPeriod(&tx, pcm1, (ULONG)sizeof(pcm1), pcm2, (ULONG)sizeof(pcm2), FALSE);
        assert(status == STATUS_SUCCESS);

        cap = virtio_test_queue_last(&q);
        buf = (VIRTIOSND_TX_BUFFER *)cap->cookie;
        assert(buf != NULL);

        payload = (const uint8_t *)buf->DataVa + sizeof(VIRTIO_SND_TX_HDR);
        assert(memcmp(payload, pcm1, sizeof(pcm1)) == 0);
        assert(memcmp(payload + sizeof(pcm1), pcm2, sizeof(pcm2)) == 0);

        VirtioSndTxUninit(&tx);
        virtio_test_queue_destroy(&q);
    }

    /* Silence fill: NULL PCM pointers are allowed when AllowSilenceFill is TRUE. */
    {
        VIRTIO_TEST_QUEUE q;
        VIRTIOSND_DMA_CONTEXT dma;
        VIRTIOSND_TX_ENGINE tx;
        NTSTATUS status;
        uint8_t pcm2[4] = {0x5A, 0x5B, 0x5C, 0x5D};
        const VIRTIO_TEST_QUEUE_CAPTURE *cap;
        VIRTIOSND_TX_BUFFER *buf;
        const uint8_t *payload;
        uint8_t expected[8];

        virtio_test_queue_init(&q, TRUE);
        RtlZeroMemory(&dma, sizeof(dma));

        status = VirtioSndTxInit(&tx, &dma, &q.queue, 64u, 1u, FALSE);
        assert(status == STATUS_SUCCESS);

        status = VirtioSndTxSubmitPeriod(&tx, NULL, 4u, pcm2, (ULONG)sizeof(pcm2), TRUE);
        assert(status == STATUS_SUCCESS);

        memset(expected, 0, 4);
        memcpy(expected + 4, pcm2, sizeof(pcm2));

        cap = virtio_test_queue_last(&q);
        buf = (VIRTIOSND_TX_BUFFER *)cap->cookie;
        assert(buf != NULL);

        payload = (const uint8_t *)buf->DataVa + sizeof(VIRTIO_SND_TX_HDR);
        assert(memcmp(payload, expected, sizeof(expected)) == 0);

        VirtioSndTxUninit(&tx);
        virtio_test_queue_destroy(&q);
    }
}

static void test_rx_rejects_misaligned_payload_bytes(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_RX_ENGINE rx;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT seg;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));

    status = VirtIoSndRxInit(&rx, &dma, &q.queue, 1u);
    assert(status == STATUS_SUCCESS);

    seg.addr = 0x1000u;
    seg.len = 3u; /* odd => invalid for S16_LE */
    status = VirtIoSndRxSubmitSg(&rx, &seg, 1, (void *)0x1234u);
    assert(status == STATUS_INVALID_BUFFER_SIZE);
    assert(q.submit_count == 0);

    VirtIoSndRxUninit(&rx);
    virtio_test_queue_destroy(&q);
}

static void test_rx_builds_hdr_payload_status_chain(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_RX_ENGINE rx;
    NTSTATUS status;
    VIRTIOSND_RX_SEGMENT segs[2];
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;
    VIRTIOSND_RX_REQUEST *req;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));

    status = VirtIoSndRxInit(&rx, &dma, &q.queue, 1u);
    assert(status == STATUS_SUCCESS);

    segs[0].addr = 0xA000u;
    segs[0].len = 4u;
    segs[1].addr = 0xB000u;
    segs[1].len = 8u;

    status = VirtIoSndRxSubmitSg(&rx, segs, (USHORT)RTL_NUMBER_OF(segs), (void *)0xDEADBEEFu);
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->sg_count == 4);
    assert(cap->sg[0].write == FALSE);
    assert(cap->sg[1].write == TRUE);
    assert(cap->sg[2].write == TRUE);
    assert(cap->sg[3].write == TRUE);
    assert(cap->sg[0].len == sizeof(VIRTIO_SND_TX_HDR));
    assert(cap->sg[1].addr == segs[0].addr);
    assert(cap->sg[1].len == segs[0].len);
    assert(cap->sg[2].addr == segs[1].addr);
    assert(cap->sg[2].len == segs[1].len);
    assert(cap->sg[3].len == sizeof(VIRTIO_SND_PCM_STATUS));

    req = (VIRTIOSND_RX_REQUEST *)cap->cookie;
    assert(req != NULL);
    assert(cap->sg[0].addr == req->HdrDma);
    assert(cap->sg[3].addr == req->StatusDma);

    {
        const VIRTIO_SND_TX_HDR *hdr = req->HdrVa;
        assert(hdr->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
        assert(hdr->reserved == 0);
    }

    VirtIoSndRxUninit(&rx);
    virtio_test_queue_destroy(&q);
}

static void test_control_set_params_formats_channels(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));

    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    status = VirtioSndCtrlSetParams(&ctrl, 1920u, 192u);
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SET_PARAMS_REQ));
    {
        const VIRTIO_SND_PCM_SET_PARAMS_REQ *req = (const VIRTIO_SND_PCM_SET_PARAMS_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_SET_PARAMS);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
        assert(req->channels == 2);
    }

    status = VirtioSndCtrlSetParams1(&ctrl, 960u, 96u);
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SET_PARAMS_REQ));
    {
        const VIRTIO_SND_PCM_SET_PARAMS_REQ *req = (const VIRTIO_SND_PCM_SET_PARAMS_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_SET_PARAMS);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
        assert(req->channels == 1);
    }

    virtio_test_queue_destroy(&q);
}

int main(void)
{
    test_tx_rejects_misaligned_pcm_bytes();
    test_tx_builds_hdr_pcm_status_chain();
    test_tx_split_payload_and_silence_fill();
    test_rx_rejects_misaligned_payload_bytes();
    test_rx_builds_hdr_payload_status_chain();
    test_control_set_params_formats_channels();
    printf("virtiosnd_proto_tests: PASS\n");
    return 0;
}

