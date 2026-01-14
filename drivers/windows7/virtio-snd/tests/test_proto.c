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

/* Shared by the ntddk.h shim so tests can simulate DISPATCH_LEVEL code paths. */
volatile KIRQL g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
VIRTIOSND_TEST_KE_SET_EVENT_HOOK g_virtiosnd_test_ke_set_event_hook = NULL;

static VIRTIOSND_CONTROL* g_virtiosnd_test_reqidle_hook_ctrl = NULL;
static KEVENT* g_virtiosnd_test_reqidle_hook_event = NULL;

static void virtiosnd_test_reqidle_ke_set_event_hook(KEVENT* event)
{
    if (g_virtiosnd_test_reqidle_hook_ctrl != NULL && event == g_virtiosnd_test_reqidle_hook_event) {
        g_virtiosnd_test_reqidle_hook_ctrl->DmaCtx = NULL;
    }
}

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

static void test_pcm_format_bytes_per_sample_mapping(void)
{
    USHORT bytes;

    bytes = 0;
    assert(VirtioSndPcmFormatToBytesPerSample(VIRTIO_SND_PCM_FMT_S16, &bytes));
    assert(bytes == 2u);

    /*
     * virtio-snd PCM format codes are based on ALSA `snd_pcm_format_t`.
     *
     * In ALSA, S24/U24 correspond to 24-bit samples stored in a 32-bit container
     * (not packed 3-byte samples), so bytes-per-sample must be 4.
     */
    bytes = 0;
    assert(VirtioSndPcmFormatToBytesPerSample(VIRTIO_SND_PCM_FMT_S24, &bytes));
    assert(bytes == 4u);

    bytes = 0;
    assert(VirtioSndPcmFormatToBytesPerSample(VIRTIO_SND_PCM_FMT_U24, &bytes));
    assert(bytes == 4u);

    bytes = 123;
    assert(!VirtioSndPcmFormatToBytesPerSample(0xFFu, &bytes));
    assert(bytes == 0u);
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

static void test_control_set_params_uses_selected_format(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    /*
     * Playback: select a non-contract format/rate and verify SET_PARAMS uses it.
     * S24 is interpreted as 24-bit samples in a 32-bit container, so bytes/sample=4.
     */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    assert(status == STATUS_SUCCESS);

    status = VirtioSndCtrlSetParams(&ctrl, 1920u, 192u); /* divisible by 8 bytes/frame */
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SET_PARAMS_REQ));
    {
        const VIRTIO_SND_PCM_SET_PARAMS_REQ *req = (const VIRTIO_SND_PCM_SET_PARAMS_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_SET_PARAMS);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
        assert(req->channels == 2u);
        assert(req->format == VIRTIO_SND_PCM_FMT_S24);
        assert(req->rate == VIRTIO_SND_PCM_RATE_44100);
    }

    /* Capture: mono S24 @ 44.1k. */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_CAPTURE_STREAM_ID,
        1u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    assert(status == STATUS_SUCCESS);

    status = VirtioSndCtrlSetParams1(&ctrl, 960u, 96u); /* divisible by 4 bytes/frame */
    assert(status == STATUS_SUCCESS);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SET_PARAMS_REQ));
    {
        const VIRTIO_SND_PCM_SET_PARAMS_REQ *req = (const VIRTIO_SND_PCM_SET_PARAMS_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_SET_PARAMS);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
        assert(req->channels == 1u);
        assert(req->format == VIRTIO_SND_PCM_FMT_S24);
        assert(req->rate == VIRTIO_SND_PCM_RATE_44100);
    }

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_select_format_respects_caps_when_valid(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    /*
     * When CapsValid is set, VirtioSndCtrlSelectFormat should reject selections
     * that are not present in the cached PCM_INFO masks/ranges.
     */
    ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].Formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].Rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMin = 2;
    ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMax = 2;

    ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].Formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].Rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMin = 1;
    ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMax = 1;

    InterlockedExchange(&ctrl.CapsValid, 1);

    /* Unsupported format */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S24,
        (UCHAR)VIRTIO_SND_PCM_RATE_48000);
    assert(status == STATUS_NOT_SUPPORTED);

    /* Unsupported rate */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S16,
        (UCHAR)VIRTIO_SND_PCM_RATE_44100);
    assert(status == STATUS_NOT_SUPPORTED);

    /* Unsupported channels */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        3u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S16,
        (UCHAR)VIRTIO_SND_PCM_RATE_48000);
    assert(status == STATUS_NOT_SUPPORTED);

    /* Valid selection */
    status = VirtioSndCtrlSelectFormat(
        &ctrl,
        VIRTIO_SND_PLAYBACK_STREAM_ID,
        2u,
        (UCHAR)VIRTIO_SND_PCM_FMT_S16,
        (UCHAR)VIRTIO_SND_PCM_RATE_48000);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Channels == 2u);
    assert(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Format == VIRTIO_SND_PCM_FMT_S16);
    assert(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Rate == VIRTIO_SND_PCM_RATE_48000);

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_timeout_then_late_completion_runs_at_dpc_level(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG virtioStatus;
    ULONG respLen;
    const VIRTIOSND_SG* sg;
    UINT32 usedLen;
    KIRQL oldIrql;
    KEVENT *idleEvent;
    VIRTIOSND_TEST_KE_SET_EVENT_HOOK prevHook;

    virtio_test_queue_init(&q, FALSE /* auto_complete */);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    /*
     * Install a KeSetEvent hook that clears Ctrl->DmaCtx when ReqIdleEvent is
     * signaled. This simulates STOP/REMOVE teardown proceeding as soon as the
     * idle event is set, and catches regressions where ReqIdleEvent is signaled
     * before request DMA buffers are freed.
     */
    g_virtiosnd_test_reqidle_hook_ctrl = &ctrl;
    g_virtiosnd_test_reqidle_hook_event = &ctrl.ReqIdleEvent;
    prevHook = g_virtiosnd_test_ke_set_event_hook;
    g_virtiosnd_test_ke_set_event_hook = virtiosnd_test_reqidle_ke_set_event_hook;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    virtioStatus = 0;
    respLen = 0;

    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, &virtioStatus, &respLen);
    assert(status == STATUS_IO_TIMEOUT);

    /* A timed out request should still be tracked as active until completion/cancel. */
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);
    assert(q.pending_count == 1);
    assert(q.pending[0].sg_count == 2);

    sg = q.pending[0].sg;
    assert(sg[1].write == TRUE);
    assert(sg[1].addr != 0);

    /* Simulate device writing a successful response and placing the chain on the used ring. */
    *(uint32_t*)(uintptr_t)sg[1].addr = VIRTIO_SND_S_OK;
    usedLen = sg[1].len;

    q.used[q.used_tail].cookie = q.pending[0].cookie;
    q.used[q.used_tail].used_len = usedLen;
    q.used_tail = (q.used_tail + 1u) % VIRTIO_TEST_QUEUE_MAX_PENDING;
    q.used_count++;

    /* Process the used entry at DISPATCH_LEVEL to exercise the DPC completion path. */
    oldIrql = KeRaiseIrqlToDpcLevel();
    VirtioSndCtrlProcessUsed(&ctrl);
    KeLowerIrql(oldIrql);

    /* The request should be freed and removed from the active list (idle signaled). */
    idleEvent = &ctrl.ReqIdleEvent;
    assert(KeReadStateEvent(idleEvent) != 0);

    g_virtiosnd_test_ke_set_event_hook = prevHook;
    g_virtiosnd_test_reqidle_hook_ctrl = NULL;
    g_virtiosnd_test_reqidle_hook_event = NULL;

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_uninit_cancels_timed_out_request(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG respLen;

    virtio_test_queue_init(&q, FALSE /* auto_complete */);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    respLen = 0;

    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, NULL, &respLen);
    assert(status == STATUS_IO_TIMEOUT);

    /* Request is still outstanding until completion/cancel. */
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);
    assert(q.pending_count == 1);

    /* Uninit should cancel and free the request context (idle signaled). */
    VirtioSndCtrlUninit(&ctrl);
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);
    assert(ctrl.DmaCtx == NULL);
    assert(ctrl.ControlQ == NULL);

    virtio_test_queue_destroy(&q);
}

static void test_control_cancel_all_frees_timed_out_request(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG respLen;

    virtio_test_queue_init(&q, FALSE /* auto_complete */);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    respLen = 0;

    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, NULL, &respLen);
    assert(status == STATUS_IO_TIMEOUT);

    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);
    assert(q.pending_count == 1);

    VirtioSndCtrlCancelAll(&ctrl, STATUS_CANCELLED);
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_cancel_all_drains_used_entries(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    ULONG respStatus;
    ULONG respLen;

    virtio_test_queue_init(&q, FALSE /* auto_complete */);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    respLen = 0;

    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, NULL, &respLen);
    assert(status == STATUS_IO_TIMEOUT);

    /* Still outstanding until completion/cancel. */
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);
    assert(q.pending_count == 1);
    assert(q.used_count == 0);

    /*
     * Simulate the device completing the request after the send thread timed out:
     * move the pending chain to the used ring without running CtrlProcessUsed yet.
     */
    q.auto_complete = TRUE;
    VirtioSndQueueKick(&q.queue);
    assert(q.pending_count == 0);
    assert(q.used_count == 1);

    /*
     * CancelAll should drain used entries before releasing request contexts so
     * there are no stale cookies left in the used ring.
     */
    VirtioSndCtrlCancelAll(&ctrl, STATUS_CANCELLED);
    assert(q.used_count == 0);
    assert(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_playback_state_machine(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    /* Invalid transitions from Idle. */
    status = VirtioSndCtrlPrepare(&ctrl);
    assert(status == STATUS_INVALID_DEVICE_STATE);
    status = VirtioSndCtrlStart(&ctrl);
    assert(status == STATUS_INVALID_DEVICE_STATE);
    status = VirtioSndCtrlStop(&ctrl);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    status = VirtioSndCtrlSetParams(&ctrl, 1920u, 192u);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] == VirtioSndStreamStateParamsSet);

    status = VirtioSndCtrlPrepare(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] == VirtioSndStreamStatePrepared);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_PREPARE);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    }

    status = VirtioSndCtrlStart(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] == VirtioSndStreamStateRunning);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_START);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    }

    /* Can't change params while running. */
    status = VirtioSndCtrlSetParams(&ctrl, 1920u, 192u);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    status = VirtioSndCtrlStop(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] == VirtioSndStreamStatePrepared);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_STOP);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    }

    /* Still can't change params in the Prepared state. */
    status = VirtioSndCtrlSetParams(&ctrl, 1920u, 192u);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    status = VirtioSndCtrlRelease(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_PLAYBACK_STREAM_ID] == VirtioSndStreamStateIdle);
    assert(ctrl.Params[VIRTIO_SND_PLAYBACK_STREAM_ID].BufferBytes == 0);
    assert(ctrl.Params[VIRTIO_SND_PLAYBACK_STREAM_ID].PeriodBytes == 0);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_RELEASE);
        assert(req->stream_id == VIRTIO_SND_PLAYBACK_STREAM_ID);
    }

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

static void test_control_capture_state_machine(void)
{
    VIRTIO_TEST_QUEUE q;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_CONTROL ctrl;
    NTSTATUS status;
    const VIRTIO_TEST_QUEUE_CAPTURE *cap;

    virtio_test_queue_init(&q, TRUE);
    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndCtrlInit(&ctrl, &dma, &q.queue);

    status = VirtioSndCtrlSetParams1(&ctrl, 960u, 96u);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] == VirtioSndStreamStateParamsSet);

    status = VirtioSndCtrlPrepare1(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] == VirtioSndStreamStatePrepared);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_PREPARE);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    }

    status = VirtioSndCtrlStart1(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] == VirtioSndStreamStateRunning);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_START);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    }

    status = VirtioSndCtrlStop1(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] == VirtioSndStreamStatePrepared);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_STOP);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    }

    status = VirtioSndCtrlRelease1(&ctrl);
    assert(status == STATUS_SUCCESS);
    assert(ctrl.StreamState[VIRTIO_SND_CAPTURE_STREAM_ID] == VirtioSndStreamStateIdle);
    assert(ctrl.Params[VIRTIO_SND_CAPTURE_STREAM_ID].BufferBytes == 0);
    assert(ctrl.Params[VIRTIO_SND_CAPTURE_STREAM_ID].PeriodBytes == 0);

    cap = virtio_test_queue_last(&q);
    assert(cap->out0_copy_len == sizeof(VIRTIO_SND_PCM_SIMPLE_REQ));
    {
        const VIRTIO_SND_PCM_SIMPLE_REQ *req = (const VIRTIO_SND_PCM_SIMPLE_REQ *)cap->out0_copy;
        assert(req->code == VIRTIO_SND_R_PCM_RELEASE);
        assert(req->stream_id == VIRTIO_SND_CAPTURE_STREAM_ID);
    }

    VirtioSndCtrlUninit(&ctrl);
    virtio_test_queue_destroy(&q);
}

int main(void)
{
    test_pcm_format_bytes_per_sample_mapping();
    test_tx_rejects_misaligned_pcm_bytes();
    test_tx_builds_hdr_pcm_status_chain();
    test_tx_split_payload_and_silence_fill();
    test_rx_rejects_misaligned_payload_bytes();
    test_rx_builds_hdr_payload_status_chain();
    test_control_set_params_formats_channels();
    test_control_set_params_uses_selected_format();
    test_control_select_format_respects_caps_when_valid();
    test_control_timeout_then_late_completion_runs_at_dpc_level();
    test_control_uninit_cancels_timed_out_request();
    test_control_cancel_all_frees_timed_out_request();
    test_control_cancel_all_drains_used_entries();
    test_control_playback_state_machine();
    test_control_capture_state_machine();
    printf("virtiosnd_proto_tests: PASS\n");
    return 0;
}
