/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_msix_wdm.h"

/*
 * Keep assert() active in all build configs (Release may define NDEBUG).
 */
#undef assert
#define assert(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                  \
            abort();                                                                                                     \
        }                                                                                                                \
    } while (0)

typedef struct msix_test_ctx {
    PVIRTIO_MSIX_WDM expected_msix;
    int config_calls;
    int drain_calls;
    int drain_calls_by_queue[8];
} msix_test_ctx_t;

typedef struct ke_insert_queue_dpc_hook_ctx {
    int call_count;
    LONG inflight_at_call[4];
    BOOLEAN inserted_at_call[4];
    USHORT vector_index_at_call[4];
} ke_insert_queue_dpc_hook_ctx_t;

static VOID ke_insert_queue_dpc_hook(_Inout_ PKDPC Dpc,
                                     _In_opt_ PVOID SystemArgument1,
                                     _In_opt_ PVOID SystemArgument2,
                                     _In_opt_ PVOID Context)
{
    PVIRTIO_MSIX_WDM_VECTOR vec;
    PVIRTIO_MSIX_WDM msix;
    ke_insert_queue_dpc_hook_ctx_t* ctx = (ke_insert_queue_dpc_hook_ctx_t*)Context;

    (void)SystemArgument1;
    (void)SystemArgument2;

    assert(ctx != NULL);
    assert(Dpc != NULL);

    vec = (PVIRTIO_MSIX_WDM_VECTOR)Dpc->DeferredContext;
    assert(vec != NULL);
    msix = vec->Msix;
    assert(msix != NULL);

    assert(ctx->call_count >= 0);
    assert(ctx->call_count < (int)(sizeof(ctx->inflight_at_call) / sizeof(ctx->inflight_at_call[0])));

    ctx->inserted_at_call[ctx->call_count] = Dpc->Inserted;
    ctx->inflight_at_call[ctx->call_count] = InterlockedCompareExchange(&msix->DpcInFlight, 0, 0);
    ctx->vector_index_at_call[ctx->call_count] = vec->VectorIndex;
    ctx->call_count++;
}

static VOID evt_config(_In_ PDEVICE_OBJECT DeviceObject, _In_opt_ PVOID Cookie)
{
    msix_test_ctx_t* ctx = (msix_test_ctx_t*)Cookie;
    (void)DeviceObject;
    assert(ctx != NULL);
    assert(ctx->expected_msix != NULL);
    ctx->config_calls++;
}

static VOID evt_drain(_In_ PDEVICE_OBJECT DeviceObject, _In_ ULONG QueueIndex, _In_opt_ PVOID Cookie)
{
    msix_test_ctx_t* ctx = (msix_test_ctx_t*)Cookie;
    (void)DeviceObject;
    assert(ctx != NULL);
    assert(ctx->expected_msix != NULL);
    assert(QueueIndex < (ULONG)(sizeof(ctx->drain_calls_by_queue) / sizeof(ctx->drain_calls_by_queue[0])));
    ctx->drain_calls++;
    ctx->drain_calls_by_queue[QueueIndex]++;
}

static CM_PARTIAL_RESOURCE_DESCRIPTOR make_msg_desc(_In_ USHORT messageCount)
{
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    RtlZeroMemory(&desc, sizeof(desc));
    desc.Type = CmResourceTypeInterrupt;
    desc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    desc.u.MessageInterrupt.Vector = 0x20;
    desc.u.MessageInterrupt.Level = 0x5;
    desc.u.MessageInterrupt.Affinity = 0x1;
    desc.u.MessageInterrupt.MessageCount = messageCount;
    return desc;
}

static void test_connect_validation(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();
    WdkTestResetLastIoConnectInterruptExParams();

    desc = make_msg_desc(1);

    status = VirtioMsixConnect(&dev, &pdo, NULL, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioMsixConnect(NULL, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioMsixConnect(&dev, NULL, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    desc.Type = 0;
    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INVALID_PARAMETER);

    desc = make_msg_desc(1);
    desc.Flags = 0; /* not message-based */
    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_NOT_SUPPORTED);

    desc = make_msg_desc(0);
    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_DEVICE_CONFIGURATION_ERROR);

    /* QueueCount > 64 is not supported (helper uses a 64-bit queue mask). */
    desc = make_msg_desc(1);
    status = VirtioMsixConnect(&dev, &pdo, &desc, 65, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_NOT_SUPPORTED);

    /* Parameter validation failures must not call through to WDK interrupt routines. */
    assert(WdkTestGetIoConnectInterruptExCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
}

static void test_connect_failure_zeroes_state(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(1);

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    memset(&msix, 0xA5, sizeof(msix));

    WdkTestSetIoConnectInterruptExStatus(STATUS_INSUFFICIENT_RESOURCES);
    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_INSUFFICIENT_RESOURCES);

    assert(msix.Initialized == FALSE);
    assert(msix.ConnectionContext == NULL);
    assert(msix.MessageInfo == NULL);
    assert(msix.Vectors == NULL);
    assert(msix.QueueLocks == NULL);
    assert(msix.QueueVectors == NULL);
    assert(msix.DpcInFlight == 0);

    /* Connect attempted once, no disconnect because connect failed. */
    assert(WdkTestGetIoConnectInterruptExCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);

    WdkTestSetIoConnectInterruptExStatus(STATUS_SUCCESS);
}

static void test_connect_disconnect_calls_wdk_routines(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(1);

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();
    WdkTestResetLastIoConnectInterruptExParams();

    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_SUCCESS);
    assert(WdkTestGetIoConnectInterruptExCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
    assert(WdkTestGetLastIoConnectInterruptExPhysicalDeviceObject() == &pdo);
    assert(WdkTestGetLastIoConnectInterruptExMessageCount() == 1);
    assert(WdkTestGetLastIoConnectInterruptExSynchronizeIrql() == desc.u.MessageInterrupt.Level);

    VirtioMsixDisconnect(&msix);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 1);

    /* Disconnect again should not call IoDisconnectInterruptEx again. */
    VirtioMsixDisconnect(&msix);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 1);
}

static void test_disconnect_waits_for_inflight_dpc(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(1);

    WdkTestResetKeDelayExecutionThreadCount();

    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_SUCCESS);

    /*
     * Simulate a DPC currently in flight (running but not queued), so
     * KeRemoveQueueDpc won't decrement it and VirtioMsixDisconnect must wait.
     */
    msix.DpcInFlight = 1;
    WdkTestAutoCompleteDpcInFlightAfterDelayCalls(&msix.DpcInFlight, 3);

    VirtioMsixDisconnect(&msix);

    assert(WdkTestGetKeDelayExecutionThreadCount() == 3);
    WdkTestClearAutoCompleteDpcInFlight();
}

static void test_multivector_mapping(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    msix_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(3); /* enough for config + 2 queues */
    RtlZeroMemory(&ctx, sizeof(ctx));

    WdkTestResetLastIoConnectInterruptExParams();
    status = VirtioMsixConnect(&dev, &pdo, &desc, 2, NULL, evt_config, evt_drain, &ctx, &msix);
    assert(status == STATUS_SUCCESS);
    ctx.expected_msix = &msix;

    assert(msix.MessageCount == 3);
    assert(msix.UsedVectorCount == 3);
    assert(WdkTestGetLastIoConnectInterruptExPhysicalDeviceObject() == &pdo);
    assert(WdkTestGetLastIoConnectInterruptExMessageCount() == 3);
    assert(WdkTestGetLastIoConnectInterruptExSynchronizeIrql() == desc.u.MessageInterrupt.Level);
    assert(msix.MessageInfo != NULL);
    assert(msix.MessageInfo->MessageCount == 3);
    /* MessageData is an APIC vector in real systems; ensure it differs from message number indices. */
    assert(msix.MessageInfo->MessageInfo[0].MessageData == 0x50u);
    assert(msix.MessageInfo->MessageInfo[1].MessageData == 0x51u);
    assert(msix.MessageInfo->MessageInfo[2].MessageData == 0x52u);
    assert(msix.ConfigVector == 0);
    assert(msix.QueueVectors != NULL);
    assert(msix.QueueVectors[0] == 1);
    assert(msix.QueueVectors[1] == 2);

    /* Vector 0: config only. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[0].Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 0);

    /* Vector 1: queue 0 only. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 1) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[1].Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 1);
    assert(ctx.drain_calls_by_queue[0] == 1);
    assert(ctx.drain_calls_by_queue[1] == 0);

    /* Vector 2: queue 1 only. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 2) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[2].Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 2);
    assert(ctx.drain_calls_by_queue[0] == 1);
    assert(ctx.drain_calls_by_queue[1] == 1);

    VirtioMsixDisconnect(&msix);
}

static void test_all_on_0_fallback_drains_all_queues(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    msix_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(1); /* only one vector available */
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioMsixConnect(&dev, &pdo, &desc, 2, NULL, evt_config, evt_drain, &ctx, &msix);
    assert(status == STATUS_SUCCESS);
    ctx.expected_msix = &msix;

    assert(msix.UsedVectorCount == 1);
    assert(msix.MessageInfo != NULL);
    assert(msix.MessageInfo->MessageCount == 1);
    assert(msix.MessageInfo->MessageInfo[0].MessageData == 0x50u);
    assert(msix.ConfigVector == 0);
    assert(msix.QueueVectors != NULL);
    assert(msix.QueueVectors[0] == 0);
    assert(msix.QueueVectors[1] == 0);

    /* Vector 0: config + all queues. */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&msix.Vectors[0].Dpc) != FALSE);

    assert(ctx.config_calls == 1);
    assert(ctx.drain_calls == 2);
    assert(ctx.drain_calls_by_queue[0] == 1);
    assert(ctx.drain_calls_by_queue[1] == 1);

    VirtioMsixDisconnect(&msix);
}

static void test_isr_increments_dpc_inflight_before_queueing_dpc(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;
    ke_insert_queue_dpc_hook_ctx_t hook_ctx;

    desc = make_msg_desc(2);
    RtlZeroMemory(&hook_ctx, sizeof(hook_ctx));

    WdkTestSetKeInsertQueueDpcHook(ke_insert_queue_dpc_hook, &hook_ctx);

    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_SUCCESS);
    assert(msix.UsedVectorCount == 1);

    /*
     * Trigger two interrupts before running the DPC.
     *
     * ISR increments DpcInFlight *before* calling KeInsertQueueDpc, and then
     * decrements it on the "already queued" path. This test observes the
     * transient DpcInFlight=2 case on the second interrupt.
     */
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);
    assert(WdkTestTriggerMessageInterrupt(msix.MessageInfo, 0) != FALSE);

    assert(hook_ctx.call_count == 2);

    assert(hook_ctx.vector_index_at_call[0] == 0);
    assert(hook_ctx.inserted_at_call[0] == FALSE);
    assert(hook_ctx.inflight_at_call[0] == 1);

    assert(hook_ctx.vector_index_at_call[1] == 0);
    assert(hook_ctx.inserted_at_call[1] != FALSE);
    assert(hook_ctx.inflight_at_call[1] == 2);

    /* Drain the queued DPC and ensure state returns to idle. */
    assert(WdkTestRunQueuedDpc(&msix.Vectors[0].Dpc) != FALSE);
    assert(msix.DpcInFlight == 0);

    VirtioMsixDisconnect(&msix);
    WdkTestClearKeInsertQueueDpcHook();
}

static void test_isr_returns_false_for_out_of_range_message_id(void)
{
    VIRTIO_MSIX_WDM msix;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;
    PKINTERRUPT intr0;
    PKMESSAGE_SERVICE_ROUTINE sr;
    PVOID ctx;
    BOOLEAN claimed;

    desc = make_msg_desc(1);

    WdkTestResetKeInsertQueueDpcCounts();

    status = VirtioMsixConnect(&dev, &pdo, &desc, 0, NULL, NULL, NULL, NULL, &msix);
    assert(status == STATUS_SUCCESS);
    assert(msix.UsedVectorCount == 1);
    assert(msix.MessageInfo != NULL);
    assert(msix.MessageInfo->MessageCount == 1);

    intr0 = msix.MessageInfo->MessageInfo[0].InterruptObject;
    assert(intr0 != NULL);
    sr = intr0->MessageServiceRoutine;
    ctx = intr0->ServiceContext;
    assert(sr != NULL);

    /* Out-of-range MessageId should be rejected and must not queue a DPC. */
    claimed = sr(intr0, ctx, 99);
    assert(claimed == FALSE);
    assert(msix.DpcInFlight == 0);
    assert(msix.Vectors[0].Dpc.Inserted == FALSE);
    assert(WdkTestGetKeInsertQueueDpcCount() == 0);

    VirtioMsixDisconnect(&msix);
}

int main(void)
{
    test_connect_validation();
    test_connect_failure_zeroes_state();
    test_connect_disconnect_calls_wdk_routines();
    test_disconnect_waits_for_inflight_dpc();
    test_multivector_mapping();
    test_all_on_0_fallback_drains_all_queues();
    test_isr_increments_dpc_inflight_before_queueing_dpc();
    test_isr_returns_false_for_out_of_range_message_id();

    printf("virtio_msix_wdm_tests: PASS\n");
    return 0;
}
