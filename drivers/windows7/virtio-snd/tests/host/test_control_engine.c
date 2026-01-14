/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_common.h"

#include <ntddk.h>

#include "virtio_snd_proto.h"
#include "virtiosnd_control.h"
#include "virtiosnd_host_queue.h"

static void host_queue_complete_last_ok_on_kick(_Inout_ VIRTIOSND_HOST_QUEUE* Q)
{
    if (Q == NULL || Q->LastCookie == NULL || Q->LastSgCount < 2) {
        return;
    }

    /* SG[1] is the device-writable response buffer and begins with virtio status. */
    TEST_ASSERT(Q->LastSg[1].addr != 0);
    TEST_ASSERT(Q->LastSg[1].len >= sizeof(UINT32));

    *(UINT32*)(uintptr_t)Q->LastSg[1].addr = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(Q, Q->LastCookie, Q->LastSg[1].len);
}

static VIRTIO_SND_PCM_INFO g_pcm_info_playback;
static VIRTIO_SND_PCM_INFO g_pcm_info_capture;

static void host_queue_complete_last_pcm_info_on_kick(_Inout_ VIRTIOSND_HOST_QUEUE* Q)
{
    UCHAR* resp;
    UINT32 code;
    ULONG needed;

    if (Q == NULL || Q->LastCookie == NULL || Q->LastSgCount < 2) {
        return;
    }

    TEST_ASSERT(Q->LastSg[0].addr != 0);
    TEST_ASSERT(Q->LastSg[0].len >= sizeof(UINT32));

    code = *(const UINT32*)(uintptr_t)Q->LastSg[0].addr;
    TEST_ASSERT(code == VIRTIO_SND_R_PCM_INFO);

    /* SG[1] is the device-writable response buffer and begins with virtio status. */
    TEST_ASSERT(Q->LastSg[1].addr != 0);
    needed = sizeof(UINT32) + (ULONG)sizeof(VIRTIO_SND_PCM_INFO) * 2u;
    TEST_ASSERT(Q->LastSg[1].len >= needed);

    resp = (UCHAR*)(uintptr_t)Q->LastSg[1].addr;
    *(UINT32*)resp = VIRTIO_SND_S_OK;

    RtlCopyMemory(resp + sizeof(UINT32), &g_pcm_info_playback, sizeof(g_pcm_info_playback));
    RtlCopyMemory(resp + sizeof(UINT32) + sizeof(VIRTIO_SND_PCM_INFO), &g_pcm_info_capture, sizeof(g_pcm_info_capture));

    VirtioSndHostQueuePushUsed(Q, Q->LastCookie, Q->LastSg[1].len);
}

static VIRTIOSND_CONTROL* g_reqidle_hook_ctrl = NULL;
static KEVENT* g_reqidle_hook_event = NULL;

static void reqidle_ke_set_event_hook(_Inout_ KEVENT* Event)
{
    if (g_reqidle_hook_ctrl != NULL && g_reqidle_hook_event != NULL && Event == g_reqidle_hook_event) {
        /*
         * Simulate STOP/REMOVE teardown proceeding as soon as ReqIdleEvent is
         * signaled. If the control engine signals ReqIdleEvent before freeing
         * request DMA buffers, subsequent frees will see a NULL DmaCtx and
         * trip assertions in the DMA stub.
         */
        g_reqidle_hook_ctrl->DmaCtx = NULL;
    }
}

static void test_control_send_sync_success_path(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    UINT32 respStatus;
    ULONG virtioStatus;
    ULONG respLen;
    NTSTATUS status;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = host_queue_complete_last_ok_on_kick;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    virtioStatus = 0;
    respLen = 0;

    status =
        VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 100u, &virtioStatus, &respLen);
    TEST_ASSERT(status == STATUS_SUCCESS);
    TEST_ASSERT(respStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(virtioStatus == VIRTIO_SND_S_OK);
    TEST_ASSERT(respLen == sizeof(respStatus));

    TEST_ASSERT(q.SubmitCalls == 1u);
    TEST_ASSERT(q.KickCalls == 1u);

    TEST_ASSERT(ctrl.Stats.RequestsSent == 1);
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 1);
    TEST_ASSERT(ctrl.Stats.RequestsTimedOut == 0);

    TEST_ASSERT(IsListEmpty(&ctrl.ReqList));
    TEST_ASSERT(IsListEmpty(&ctrl.InflightList));
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);

    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_send_sync_timeout_path(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    UINT32 respStatus;
    ULONG virtioStatus;
    ULONG respLen;
    NTSTATUS status;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    /* No completion injected => should time out. */
    q.OnKick = NULL;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    virtioStatus = 0xDEADBEEFu;
    respLen = 0xDEADBEEFu;

    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, &virtioStatus, &respLen);
    TEST_ASSERT(status == STATUS_IO_TIMEOUT);
    TEST_ASSERT(virtioStatus == 0u);
    TEST_ASSERT(respLen == 0u);

    TEST_ASSERT(q.SubmitCalls == 1u);
    TEST_ASSERT(q.KickCalls == 1u);

    TEST_ASSERT(ctrl.Stats.RequestsSent == 1);
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 0);
    TEST_ASSERT(ctrl.Stats.RequestsTimedOut == 1);

    /* A timed-out request should remain tracked as active until completion/cancel. */
    TEST_ASSERT(IsListEmpty(&ctrl.ReqList) == FALSE);
    TEST_ASSERT(IsListEmpty(&ctrl.InflightList) == FALSE);
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);

    /* Cleanup (cancels and frees the timed-out request). */
    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_timeout_then_late_completion_runs_at_dpc_level(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    UINT32 respStatus;
    ULONG virtioStatus;
    ULONG respLen;
    NTSTATUS status;
    void* cookie;
    UINT64 respAddr;
    UINT32 usedLen;
    KIRQL oldIrql;
    VIRTIOSND_TEST_KE_SET_EVENT_HOOK prevHook;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = NULL;

    /*
     * Install a KeSetEvent hook that clears Ctrl->DmaCtx when ReqIdleEvent is
     * signaled. This simulates STOP/REMOVE teardown proceeding as soon as the
     * idle event is set, and catches regressions where ReqIdleEvent is signaled
     * before request DMA buffers are freed.
     */
    g_reqidle_hook_ctrl = &ctrl;
    g_reqidle_hook_event = &ctrl.ReqIdleEvent;
    prevHook = g_virtiosnd_test_ke_set_event_hook;
    g_virtiosnd_test_ke_set_event_hook = reqidle_ke_set_event_hook;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    virtioStatus = 0;
    respLen = 0;

    status =
        VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, &virtioStatus, &respLen);
    TEST_ASSERT(status == STATUS_IO_TIMEOUT);

    /* A timed out request should still be tracked as active until completion/cancel. */
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);
    TEST_ASSERT(q.SubmitCalls == 1u);
    TEST_ASSERT(q.LastSgCount == 2u);

    cookie = q.LastCookie;
    TEST_ASSERT(cookie != NULL);

    respAddr = q.LastSg[1].addr;
    usedLen = q.LastSg[1].len;
    TEST_ASSERT(respAddr != 0);
    TEST_ASSERT(usedLen >= sizeof(UINT32));

    /* Simulate device writing a successful response and placing the chain on the used ring. */
    *(UINT32*)(uintptr_t)respAddr = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, cookie, usedLen);

    /* Process the used entry at DISPATCH_LEVEL to exercise the DPC completion path. */
    oldIrql = KeRaiseIrqlToDpcLevel();
    VirtioSndCtrlProcessUsed(&ctrl);
    KeLowerIrql(oldIrql);

    /* The request should be freed and removed from the active list (idle signaled). */
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);
    TEST_ASSERT(IsListEmpty(&ctrl.ReqList));
    TEST_ASSERT(IsListEmpty(&ctrl.InflightList));
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 1);

    /* Hook should have fired (ReqIdleEvent signaled). */
    TEST_ASSERT(ctrl.DmaCtx == NULL);

    g_virtiosnd_test_ke_set_event_hook = prevHook;
    g_reqidle_hook_ctrl = NULL;
    g_reqidle_hook_event = NULL;

    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_cancel_all_drains_used_entries_before_canceling_inflight(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_SIMPLE_REQ req;
    UINT32 respStatus;
    ULONG respLen;
    NTSTATUS status;

    void* cookie0;
    UINT64 respAddr0;
    UINT32 usedLen0;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = NULL;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_RELEASE;
    req.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;

    respStatus = 0xFFFFFFFFu;
    respLen = 0;

    /* Submit request 0 (timeout). */
    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, NULL, &respLen);
    TEST_ASSERT(status == STATUS_IO_TIMEOUT);

    cookie0 = q.LastCookie;
    respAddr0 = q.LastSg[1].addr;
    usedLen0 = q.LastSg[1].len;
    TEST_ASSERT(cookie0 != NULL);
    TEST_ASSERT(respAddr0 != 0);
    TEST_ASSERT(usedLen0 >= sizeof(UINT32));

    /* Submit request 1 (timeout). */
    status = VirtioSndCtrlSendSync(&ctrl, &req, sizeof(req), &respStatus, sizeof(respStatus), 1u, NULL, &respLen);
    TEST_ASSERT(status == STATUS_IO_TIMEOUT);

    TEST_ASSERT(ctrl.Stats.RequestsSent == 2);
    TEST_ASSERT(ctrl.Stats.RequestsTimedOut == 2);
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 0);

    /* Still outstanding until completion/cancel. */
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) == 0);

    /* Simulate request 0 completing after its send thread timed out: push to used ring without running CtrlProcessUsed yet. */
    *(UINT32*)(uintptr_t)respAddr0 = VIRTIO_SND_S_OK;
    VirtioSndHostQueuePushUsed(&q, cookie0, usedLen0);
    TEST_ASSERT(q.UsedHead != q.UsedTail);

    /*
     * CancelAll should drain used entries before releasing request contexts so
     * there are no stale cookies left in the used ring.
     */
    VirtioSndCtrlCancelAll(&ctrl, STATUS_CANCELLED);

    TEST_ASSERT(q.UsedHead == q.UsedTail);
    TEST_ASSERT(KeReadStateEvent(&ctrl.ReqIdleEvent) != 0);
    TEST_ASSERT(IsListEmpty(&ctrl.ReqList));
    TEST_ASSERT(IsListEmpty(&ctrl.InflightList));

    /* Used entry should have been processed (completed) rather than canceled. */
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 1);

    /* Ensure there are no stale cookies left to process. */
    {
        KIRQL oldIrql = KeRaiseIrqlToDpcLevel();
        VirtioSndCtrlProcessUsed(&ctrl);
        KeLowerIrql(oldIrql);
    }
    TEST_ASSERT(ctrl.Stats.RequestsCompleted == 1);

    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_pcm_info_all_sets_caps_and_selected_format(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_INFO playback;
    VIRTIO_SND_PCM_INFO capture;
    NTSTATUS status;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = host_queue_complete_last_pcm_info_on_kick;

    RtlZeroMemory(&g_pcm_info_playback, sizeof(g_pcm_info_playback));
    g_pcm_info_playback.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    g_pcm_info_playback.direction = VIRTIO_SND_D_OUTPUT;
    g_pcm_info_playback.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    g_pcm_info_playback.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    g_pcm_info_playback.channels_min = 2;
    g_pcm_info_playback.channels_max = 2;

    RtlZeroMemory(&g_pcm_info_capture, sizeof(g_pcm_info_capture));
    g_pcm_info_capture.stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    g_pcm_info_capture.direction = VIRTIO_SND_D_INPUT;
    g_pcm_info_capture.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    g_pcm_info_capture.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    g_pcm_info_capture.channels_min = 1;
    g_pcm_info_capture.channels_max = 1;

    status = VirtioSndCtrlPcmInfoAll(&ctrl, &playback, &capture);
    TEST_ASSERT(status == STATUS_SUCCESS);

    TEST_ASSERT(InterlockedCompareExchange(&ctrl.CapsValid, 0, 0) != 0);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].Formats == g_pcm_info_playback.formats);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].Rates == g_pcm_info_playback.rates);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMin == g_pcm_info_playback.channels_min);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMax == g_pcm_info_playback.channels_max);

    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].Formats == g_pcm_info_capture.formats);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].Rates == g_pcm_info_capture.rates);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMin == g_pcm_info_capture.channels_min);
    TEST_ASSERT(ctrl.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMax == g_pcm_info_capture.channels_max);

    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Channels == 2);
    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID].Rate == VIRTIO_SND_PCM_RATE_48000);

    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID].Channels == 1);
    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID].Format == VIRTIO_SND_PCM_FMT_S16);
    TEST_ASSERT(ctrl.SelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID].Rate == VIRTIO_SND_PCM_RATE_48000);

    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_pcm_info_all_rejects_missing_playback_baseline(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_INFO playback;
    VIRTIO_SND_PCM_INFO capture;
    NTSTATUS status;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = host_queue_complete_last_pcm_info_on_kick;

    RtlZeroMemory(&g_pcm_info_playback, sizeof(g_pcm_info_playback));
    g_pcm_info_playback.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    g_pcm_info_playback.direction = VIRTIO_SND_D_OUTPUT;
    g_pcm_info_playback.formats = VIRTIO_SND_PCM_FMT_MASK_S24;
    g_pcm_info_playback.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    g_pcm_info_playback.channels_min = 2;
    g_pcm_info_playback.channels_max = 2;

    RtlZeroMemory(&g_pcm_info_capture, sizeof(g_pcm_info_capture));
    g_pcm_info_capture.stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    g_pcm_info_capture.direction = VIRTIO_SND_D_INPUT;
    g_pcm_info_capture.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    g_pcm_info_capture.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    g_pcm_info_capture.channels_min = 1;
    g_pcm_info_capture.channels_max = 1;

    status = VirtioSndCtrlPcmInfoAll(&ctrl, &playback, &capture);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
    TEST_ASSERT(InterlockedCompareExchange(&ctrl.CapsValid, 0, 0) == 0);

    VirtioSndCtrlUninit(&ctrl);
}

static void test_control_pcm_info_all_rejects_missing_capture_baseline(void)
{
    VIRTIOSND_CONTROL ctrl;
    VIRTIOSND_DMA_CONTEXT dma;
    VIRTIOSND_HOST_QUEUE q;
    VIRTIO_SND_PCM_INFO playback;
    VIRTIO_SND_PCM_INFO capture;
    NTSTATUS status;

    g_virtiosnd_test_current_irql = PASSIVE_LEVEL;
    g_virtiosnd_test_ke_set_event_hook = NULL;

    RtlZeroMemory(&dma, sizeof(dma));
    VirtioSndHostQueueInit(&q, 8);
    VirtioSndCtrlInit(&ctrl, &dma, &q.Queue);

    q.OnKick = host_queue_complete_last_pcm_info_on_kick;

    RtlZeroMemory(&g_pcm_info_playback, sizeof(g_pcm_info_playback));
    g_pcm_info_playback.stream_id = VIRTIO_SND_PLAYBACK_STREAM_ID;
    g_pcm_info_playback.direction = VIRTIO_SND_D_OUTPUT;
    g_pcm_info_playback.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    g_pcm_info_playback.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    g_pcm_info_playback.channels_min = 2;
    g_pcm_info_playback.channels_max = 2;

    RtlZeroMemory(&g_pcm_info_capture, sizeof(g_pcm_info_capture));
    g_pcm_info_capture.stream_id = VIRTIO_SND_CAPTURE_STREAM_ID;
    g_pcm_info_capture.direction = VIRTIO_SND_D_INPUT;
    g_pcm_info_capture.formats = VIRTIO_SND_PCM_FMT_MASK_S16;
    g_pcm_info_capture.rates = VIRTIO_SND_PCM_RATE_MASK_48000;
    /* Device supports only stereo capture => contract-v1 baseline mono is missing. */
    g_pcm_info_capture.channels_min = 2;
    g_pcm_info_capture.channels_max = 2;

    status = VirtioSndCtrlPcmInfoAll(&ctrl, &playback, &capture);
    TEST_ASSERT(status == STATUS_NOT_SUPPORTED);
    TEST_ASSERT(InterlockedCompareExchange(&ctrl.CapsValid, 0, 0) == 0);

    VirtioSndCtrlUninit(&ctrl);
}

int main(void)
{
    test_control_send_sync_success_path();
    test_control_send_sync_timeout_path();
    test_control_timeout_then_late_completion_runs_at_dpc_level();
    test_control_cancel_all_drains_used_entries_before_canceling_inflight();
    test_control_pcm_info_all_sets_caps_and_selected_format();
    test_control_pcm_info_all_rejects_missing_playback_baseline();
    test_control_pcm_info_all_rejects_missing_capture_baseline();

    printf("virtiosnd_control_engine_tests: PASS\n");
    return 0;
}
