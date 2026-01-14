/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include "virtio_snd_proto.h"
#include "virtiosnd_tx.h"
#include "virtiosnd_host_queue.h"
#include "virtiosnd_limits.h"

static void test_tx_init_sets_fixed_stream_id_and_can_suppress_interrupts(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    ULONG i;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 32u, 4u, TRUE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(q.DisableInterruptCalls == 1);

    TEST_ASSERT(tx.BufferCount == 4u);
    TEST_ASSERT(tx.FreeCount == 4u);
    TEST_ASSERT(tx.InflightCount == 0u);

    for (i = 0; i < tx.BufferCount; i++) {
        const VIRTIO_SND_TX_HDR* hdr = (const VIRTIO_SND_TX_HDR*)tx.Buffers[i].DataVa;
        TEST_ASSERT(hdr != NULL);
        TEST_ASSERT(hdr->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
        TEST_ASSERT(hdr->reserved == 0u);
    }

    VirtioSndTxUninit(&tx);
}

static void test_tx_init_default_and_clamped_buffer_count(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 32u, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(tx.BufferCount == 16u);
    TEST_ASSERT(tx.FreeCount == 16u);
    VirtioSndTxUninit(&tx);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 32u, 100u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(tx.BufferCount == 64u);
    TEST_ASSERT(tx.FreeCount == 64u);
    VirtioSndTxUninit(&tx);
}

static void test_tx_init_rejects_unaligned_max_period_bytes(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 6u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);
}

static void test_tx_init_rejects_max_period_bytes_over_contract_limit(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, VIRTIOSND_MAX_PCM_PAYLOAD_BYTES + 4u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);
}

static void test_tx_submit_sg_allows_payload_at_contract_limit(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_TX_SEGMENT seg;
    VIRTIOSND_TX_BUFFER* buf;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);

    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, VIRTIOSND_MAX_PCM_PAYLOAD_BYTES, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    seg.Address.QuadPart = 0x1000;
    seg.Length = VIRTIOSND_MAX_PCM_PAYLOAD_BYTES;
    status = VirtioSndTxSubmitSg(&tx, &seg, 1u);
    TEST_ASSERT(status == STATUS_SUCCESS);

    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    /* Complete it to recycle the buffer before uninit. */
    buf->StatusVa->status = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, buf, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(VirtioSndTxDrainCompletions(&tx) == 1u);

    VirtioSndTxUninit(&tx);
}

static void test_tx_submit_period_wrap_copies_both_segments_and_builds_sg(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm1[8];
    uint8_t pcm2[4];
    ULONG i;

    for (i = 0; i < sizeof(pcm1); i++) {
        pcm1[i] = (uint8_t)(0xA0u + i);
    }
    for (i = 0; i < sizeof(pcm2); i++) {
        pcm2[i] = (uint8_t)(0xB0u + i);
    }

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 16u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm1, (ULONG)sizeof(pcm1), pcm2, (ULONG)sizeof(pcm2), FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(q.LastCookie != NULL);
    TEST_ASSERT(q.LastSgCount == 2u);

    {
        const VIRTIOSND_TX_BUFFER* buf = (const VIRTIOSND_TX_BUFFER*)q.LastCookie;
        const uint8_t* payload = (const uint8_t*)buf->DataVa + sizeof(VIRTIO_SND_TX_HDR);

        TEST_ASSERT(buf->PcmBytes == 12u);

        TEST_ASSERT_MEMEQ(payload, pcm1, sizeof(pcm1));
        TEST_ASSERT_MEMEQ(payload + sizeof(pcm1), pcm2, sizeof(pcm2));

        /* SG[0] = header+payload (device-readable), SG[1] = status (device-writable). */
        TEST_ASSERT(q.LastSg[0].addr == buf->DataDma);
        TEST_ASSERT(q.LastSg[0].len == (UINT32)(sizeof(VIRTIO_SND_TX_HDR) + sizeof(pcm1) + sizeof(pcm2)));
        TEST_ASSERT(q.LastSg[0].write == FALSE);

        TEST_ASSERT(q.LastSg[1].addr == buf->StatusDma);
        TEST_ASSERT(q.LastSg[1].len == (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
        TEST_ASSERT(q.LastSg[1].write == TRUE);
    }

    TEST_ASSERT(tx.FreeCount == 0u);
    TEST_ASSERT(tx.InflightCount == 1u);

    VirtioSndTxUninit(&tx);
}

static void test_tx_no_free_buffers_drops_period(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[4] = {1, 2, 3, 4};

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_INSUFFICIENT_RESOURCES);
    TEST_ASSERT(tx.Stats.DroppedNoBuffers == 1);

    VirtioSndTxUninit(&tx);
}

static void test_tx_queue_full_returns_buffer_to_pool(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[4] = {1, 2, 3, 4};

    RtlZeroMemory(&dma, sizeof(dma));

    /* Queue capacity 1, buffer pool size 2 => second submit fails due to queue full, not pool exhaustion. */
    VirtioSndHostQueueInit(&q, 1);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 2u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(tx.FreeCount == 1u);
    TEST_ASSERT(tx.InflightCount == 1u);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_INSUFFICIENT_RESOURCES);
    TEST_ASSERT(tx.Stats.SubmitErrors == 1);
    TEST_ASSERT(tx.FreeCount == 1u);
    TEST_ASSERT(tx.InflightCount == 1u);

    VirtioSndTxUninit(&tx);
}

static void test_tx_submit_sg_builds_descriptor_chain_and_enforces_limits(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    VIRTIOSND_TX_SEGMENT segs[VIRTIOSND_TX_MAX_SEGMENTS];
    ULONG i;
    VIRTIOSND_TX_BUFFER* buf;

    /* Happy path: max segment count builds [hdr][segments...][status]. */
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 64u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    RtlZeroMemory(segs, sizeof(segs));
    for (i = 0; i < VIRTIOSND_TX_MAX_SEGMENTS; i++) {
        segs[i].Address.QuadPart = (LONGLONG)(0x1000u + (i * 0x100u));
        segs[i].Length = 4u; /* 1 frame */
    }

    status = VirtioSndTxSubmitSg(&tx, segs, VIRTIOSND_TX_MAX_SEGMENTS);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(q.LastSgCount == (USHORT)(VIRTIOSND_TX_MAX_SEGMENTS + 2u));

    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    TEST_ASSERT(q.LastSg[0].addr == buf->DataDma);
    TEST_ASSERT(q.LastSg[0].len == (UINT32)sizeof(VIRTIO_SND_TX_HDR));
    TEST_ASSERT(q.LastSg[0].write == FALSE);

    for (i = 0; i < VIRTIOSND_TX_MAX_SEGMENTS; i++) {
        TEST_ASSERT(q.LastSg[1u + i].addr == (UINT64)segs[i].Address.QuadPart);
        TEST_ASSERT(q.LastSg[1u + i].len == segs[i].Length);
        TEST_ASSERT(q.LastSg[1u + i].write == FALSE);
    }

    TEST_ASSERT(q.LastSg[1u + VIRTIOSND_TX_MAX_SEGMENTS].addr == buf->StatusDma);
    TEST_ASSERT(q.LastSg[1u + VIRTIOSND_TX_MAX_SEGMENTS].len == (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(q.LastSg[1u + VIRTIOSND_TX_MAX_SEGMENTS].write == TRUE);

    /* Complete it to recycle the buffer before uninit. */
    buf->StatusVa->status = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, buf, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(VirtioSndTxDrainCompletions(&tx) == 1u);
    TEST_ASSERT(tx.FreeCount == 1u);

    VirtioSndTxUninit(&tx);

    /* SegmentCount > max => invalid parameter. */
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 64u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitSg(&tx, segs, 0u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    status = VirtioSndTxSubmitSg(&tx, segs, (ULONG)(VIRTIOSND_TX_MAX_SEGMENTS + 1u));
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Too large total bytes => invalid buffer size. */
    segs[0].Length = 68u;
    status = VirtioSndTxSubmitSg(&tx, segs, 1u);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    /* Total not frame-aligned => invalid parameter. */
    segs[0].Length = 2u;
    status = VirtioSndTxSubmitSg(&tx, segs, 1u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Zero-length segment => invalid parameter. */
    segs[0].Length = 0u;
    status = VirtioSndTxSubmitSg(&tx, segs, 1u);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    /* Total bytes > UINT32_MAX => invalid buffer size (before MaxPeriodBytes check). */
    segs[0].Length = 0xFFFFFFFFu;
    segs[1].Length = 4u;
    status = VirtioSndTxSubmitSg(&tx, segs, 2u);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    VirtioSndTxUninit(&tx);
}

static void test_tx_max_period_enforcement(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[16] = {0};

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    /* Too many bytes for MaxPeriodBytes. */
    status = VirtioSndTxSubmitPeriod(&tx, pcm, 8u, pcm, 4u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    /* Not aligned to 4-byte frames (stereo S16 => 4 bytes/frame). */
    status = VirtioSndTxSubmitPeriod(&tx, pcm, 6u, NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_BUFFER_SIZE);

    /* NULL PCM pointer is only allowed when silence fill is enabled. */
    status = VirtioSndTxSubmitPeriod(&tx, NULL, 4u, NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_PARAMETER);

    VirtioSndTxUninit(&tx);
}

static void test_tx_status_parsing_sets_fatal_on_bad_msg(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[4] = {1, 2, 3, 4};
    VIRTIOSND_TX_BUFFER* buf;
    ULONG drained;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    buf->StatusVa->status = VIRTIO_SND_S_BAD_MSG;
    buf->StatusVa->latency_bytes = 123u;

    VirtioSndHostQueuePushUsed(&q, buf, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    drained = VirtioSndTxDrainCompletions(&tx);
    TEST_ASSERT(drained == 1u);

    TEST_ASSERT(tx.LastVirtioStatus == VIRTIO_SND_S_BAD_MSG);
    TEST_ASSERT(tx.LastLatencyBytes == 123u);
    TEST_ASSERT(tx.FatalError == TRUE);
    TEST_ASSERT(tx.Stats.Completed == 1);
    TEST_ASSERT(tx.Stats.StatusBadMsg == 1);
    TEST_ASSERT(tx.FreeCount == 1u);
    TEST_ASSERT(tx.InflightCount == 0u);

    /* Once fatal, further submissions fail fast. */
    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_INVALID_DEVICE_STATE);

    VirtioSndTxUninit(&tx);
}

static void test_tx_used_len_too_small_sets_bad_msg_and_fatal(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[4] = {1, 2, 3, 4};
    VIRTIOSND_TX_BUFFER* buf;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    /* UsedLen < sizeof(VIRTIO_SND_PCM_STATUS) => treated as BAD_MSG. */
    VirtioSndHostQueuePushUsed(&q, buf, 4u);
    TEST_ASSERT(VirtioSndTxDrainCompletions(&tx) == 1u);

    TEST_ASSERT(tx.LastVirtioStatus == VIRTIO_SND_S_BAD_MSG);
    TEST_ASSERT(tx.FatalError == TRUE);
    TEST_ASSERT(tx.Stats.StatusBadMsg == 1);

    VirtioSndTxUninit(&tx);
}

static void test_tx_not_supp_sets_fatal_but_io_err_does_not(void)
{
    VIRTIOSND_TX_ENGINE tx;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    NTSTATUS status;
    uint8_t pcm[4] = {1, 2, 3, 4};
    VIRTIOSND_TX_BUFFER* buf;

    /* NOT_SUPP => fatal */
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    buf->StatusVa->status = VIRTIO_SND_S_NOT_SUPP;
    VirtioSndHostQueuePushUsed(&q, buf, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(VirtioSndTxDrainCompletions(&tx) == 1u);
    TEST_ASSERT(tx.FatalError == TRUE);
    TEST_ASSERT(tx.Stats.StatusNotSupp == 1);
    VirtioSndTxUninit(&tx);

    /* IO_ERR => not fatal */
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    status = VirtioSndTxInit(&tx, &dma, &q.Queue, 4u, 8u, 1u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);

    status = VirtioSndTxSubmitPeriod(&tx, pcm, (ULONG)sizeof(pcm), NULL, 0u, FALSE);
    TEST_ASSERT(status == STATUS_SUCCESS);
    buf = (VIRTIOSND_TX_BUFFER*)q.LastCookie;
    TEST_ASSERT(buf != NULL);

    buf->StatusVa->status = VIRTIO_SND_S_IO_ERR;
    VirtioSndHostQueuePushUsed(&q, buf, (UINT32)sizeof(VIRTIO_SND_PCM_STATUS));
    TEST_ASSERT(VirtioSndTxDrainCompletions(&tx) == 1u);
    TEST_ASSERT(tx.FatalError == FALSE);
    TEST_ASSERT(tx.Stats.StatusIoErr == 1);
    VirtioSndTxUninit(&tx);
}

int main(void)
{
    test_tx_init_sets_fixed_stream_id_and_can_suppress_interrupts();
    test_tx_init_default_and_clamped_buffer_count();
    test_tx_init_rejects_unaligned_max_period_bytes();
    test_tx_init_rejects_max_period_bytes_over_contract_limit();
    test_tx_submit_sg_allows_payload_at_contract_limit();
    test_tx_submit_period_wrap_copies_both_segments_and_builds_sg();
    test_tx_no_free_buffers_drops_period();
    test_tx_queue_full_returns_buffer_to_pool();
    test_tx_submit_sg_builds_descriptor_chain_and_enforces_limits();
    test_tx_max_period_enforcement();
    test_tx_status_parsing_sets_fatal_on_bad_msg();
    test_tx_used_len_too_small_sets_bad_msg_and_fatal();
    test_tx_not_supp_sets_fatal_but_io_err_does_not();

    printf("virtiosnd_tx_tests: PASS\n");
    return 0;
}
