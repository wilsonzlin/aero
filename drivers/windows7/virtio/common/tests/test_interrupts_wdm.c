/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_interrupts_wdm.h"

/*
 * This test harness relies on assert() for side-effectful setup calls. Ensure
 * it remains active even in Release builds (which typically define NDEBUG).
 */
#undef assert
#define assert(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                  \
            abort();                                                                                                     \
        }                                                                                                                \
    } while (0)

typedef struct interrupts_test_ctx {
    PVIRTIO_PCI_WDM_INTERRUPTS expected;
    int config_calls;
    int queue_calls;
    int dpc_calls;
    int dpc_config_calls;
    int dpc_queue_calls;
    ULONG last_message_id;
    USHORT last_queue_index;
    BOOLEAN last_is_config;
    int trigger_once;
} interrupts_test_ctx_t;

typedef struct ke_insert_queue_dpc_hook_ctx {
    int call_count;
    LONG inflight_at_call[4];
    BOOLEAN inserted_at_call[4];
    ULONG message_id_at_call[4];
} ke_insert_queue_dpc_hook_ctx_t;

static VOID ke_insert_queue_dpc_hook(_Inout_ PKDPC Dpc,
                                     _In_opt_ PVOID SystemArgument1,
                                     _In_opt_ PVOID SystemArgument2,
                                     _In_opt_ PVOID Context)
{
    PVIRTIO_PCI_WDM_INTERRUPTS intr;
    ke_insert_queue_dpc_hook_ctx_t* ctx = (ke_insert_queue_dpc_hook_ctx_t*)Context;

    (void)SystemArgument1;
    (void)SystemArgument2;

    assert(ctx != NULL);
    assert(Dpc != NULL);

    intr = (PVIRTIO_PCI_WDM_INTERRUPTS)Dpc->DeferredContext;
    assert(intr != NULL);
    assert(intr->Mode == VirtioPciWdmInterruptModeMessage);
    assert(intr->u.Message.MessageDpcs != NULL);

    assert(ctx->call_count >= 0);
    assert(ctx->call_count < (int)(sizeof(ctx->inflight_at_call) / sizeof(ctx->inflight_at_call[0])));

    ctx->inserted_at_call[ctx->call_count] = Dpc->Inserted;
    ctx->inflight_at_call[ctx->call_count] = InterlockedCompareExchange(&intr->u.Message.DpcInFlight, 0, 0);
    ctx->message_id_at_call[ctx->call_count] = (ULONG)(Dpc - intr->u.Message.MessageDpcs);
    ctx->call_count++;
}

typedef struct io_connect_interrupt_ex_hook_ctx {
    ULONG message_id_to_trigger;
    int call_count;
} io_connect_interrupt_ex_hook_ctx_t;

static VOID io_connect_interrupt_ex_hook_trigger_message(_Inout_ PIO_CONNECT_INTERRUPT_PARAMETERS Parameters, _In_opt_ PVOID Context)
{
    io_connect_interrupt_ex_hook_ctx_t* ctx = (io_connect_interrupt_ex_hook_ctx_t*)Context;
    assert(ctx != NULL);
    assert(Parameters != NULL);
    assert(Parameters->Version == CONNECT_MESSAGE_BASED);
    assert(Parameters->MessageBased.MessageInfo != NULL);
    assert(ctx->message_id_to_trigger < Parameters->MessageBased.MessageInfo->MessageCount);
    ctx->call_count++;

    /*
     * Simulate an interrupt arriving immediately after IoConnectInterruptEx
     * establishes the connection, but before the driver's connect helper returns.
     */
    assert(WdkTestTriggerMessageInterrupt(Parameters->MessageBased.MessageInfo, ctx->message_id_to_trigger) != FALSE);
}

static VOID evt_config(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts, _In_opt_ PVOID Cookie)
{
    interrupts_test_ctx_t* ctx = (interrupts_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Interrupts == ctx->expected);
    ctx->config_calls++;
}

static VOID evt_queue(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts, _In_ USHORT QueueIndex, _In_opt_ PVOID Cookie)
{
    interrupts_test_ctx_t* ctx = (interrupts_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Interrupts == ctx->expected);
    ctx->queue_calls++;
    ctx->last_queue_index = QueueIndex;
}

static VOID evt_queue_trigger_message_interrupt_once(_Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts, _In_ USHORT QueueIndex, _In_opt_ PVOID Cookie)
{
    interrupts_test_ctx_t* ctx = (interrupts_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Interrupts == ctx->expected);
    assert(QueueIndex == 0);

    ctx->queue_calls++;
    ctx->last_queue_index = QueueIndex;

    /*
     * Simulate another interrupt arriving while the DPC is executing. This
     * exercises DpcInFlight tracking across the "ISR queues DPC while DPC is
     * running" case (common on SMP systems).
     */
    if (ctx->trigger_once == 0) {
        ctx->trigger_once = 1;

        assert(Interrupts->Mode == VirtioPciWdmInterruptModeMessage);
        assert(Interrupts->u.Message.MessageInfo != NULL);

        /* Trigger another interrupt for the same message (queue 0). */
        assert(WdkTestTriggerMessageInterrupt(Interrupts->u.Message.MessageInfo, 1) != FALSE);
    }
}

static VOID evt_dpc(
    _Inout_ PVIRTIO_PCI_WDM_INTERRUPTS Interrupts,
    _In_ ULONG MessageId,
    _In_ BOOLEAN IsConfig,
    _In_ USHORT QueueIndex,
    _In_opt_ PVOID Cookie)
{
    interrupts_test_ctx_t* ctx = (interrupts_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Interrupts == ctx->expected);
    ctx->dpc_calls++;
    if (IsConfig) {
        ctx->dpc_config_calls++;
    } else {
        ctx->dpc_queue_calls++;
    }
    ctx->last_message_id = MessageId;
    ctx->last_is_config = IsConfig;
    ctx->last_queue_index = QueueIndex;
}

static CM_PARTIAL_RESOURCE_DESCRIPTOR make_msg_desc(_In_ USHORT MessageCount)
{
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    RtlZeroMemory(&desc, sizeof(desc));
    desc.Type = CmResourceTypeInterrupt;
    desc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    desc.u.MessageInterrupt.Vector = 0x20;
    desc.u.MessageInterrupt.Level = 0x5;
    desc.u.MessageInterrupt.Affinity = 0x1;
    desc.u.MessageInterrupt.MessageCount = MessageCount;
    return desc;
}

static CM_PARTIAL_RESOURCE_DESCRIPTOR make_int_desc(void)
{
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    RtlZeroMemory(&desc, sizeof(desc));
    desc.Type = CmResourceTypeInterrupt;
    desc.ShareDisposition = 3; /* shared */
    desc.Flags = 0;
    desc.u.Interrupt.Vector = 0x10;
    desc.u.Interrupt.Level = 0x5;
    desc.u.Interrupt.Affinity = 0x1;
    return desc;
}

static void test_connect_validation(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();
    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();
    WdkTestResetLastIoConnectInterruptExParams();

    desc = make_msg_desc(2);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, NULL, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioPciWdmInterruptConnect(NULL, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, NULL);
    assert(status == STATUS_INVALID_PARAMETER);

    /* Message interrupts require a PDO for IoConnectInterruptEx. */
    status = VirtioPciWdmInterruptConnect(&dev, NULL, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    desc.Type = 0;
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    desc = make_msg_desc(0);
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    /* INTx requires a mapped ISR status register. */
    desc = make_int_desc();
    status = VirtioPciWdmInterruptConnect(&dev, NULL, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    /* Parameter validation failures must not call through to WDK interrupt routines. */
    assert(WdkTestGetIoConnectInterruptCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptCount() == 0);
    assert(WdkTestGetIoConnectInterruptExCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
    assert(WdkTestGetLastIoConnectInterruptExPhysicalDeviceObject() == NULL);
    assert(WdkTestGetLastIoConnectInterruptExMessageCount() == 0);
    assert(WdkTestGetLastIoConnectInterruptExSynchronizeIrql() == 0);
}

static void test_intx_connect_and_dispatch(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;
    volatile UCHAR isr_reg = 0;
    BOOLEAN claimed;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();
    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    status = VirtioPciWdmInterruptConnect(&dev, NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeIntx);
    assert(WdkTestGetIoConnectInterruptCount() == 1);
    assert(WdkTestGetIoConnectInterruptExCount() == 0);
    ctx.expected = &intr;

    /* Spurious interrupt: status byte contains 0. */
    isr_reg = 0;
    claimed = WdkTestTriggerInterrupt(intr.u.Intx.Intx.InterruptObject);
    assert(claimed == FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Intx.Intx.Dpc) == FALSE);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    /* Queue only. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    claimed = WdkTestTriggerInterrupt(intr.u.Intx.Intx.InterruptObject);
    assert(claimed != FALSE);
    assert(isr_reg == 0); /* ACK via read-to-clear */
    assert(WdkTestRunQueuedDpc(&intr.u.Intx.Intx.Dpc) != FALSE);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN);
    assert(ctx.config_calls == 0);

    /* Config only. */
    isr_reg = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    claimed = WdkTestTriggerInterrupt(intr.u.Intx.Intx.InterruptObject);
    assert(claimed != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intr.u.Intx.Intx.Dpc) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);

    /* Both bits. */
    isr_reg = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    claimed = WdkTestTriggerInterrupt(intr.u.Intx.Intx.InterruptObject);
    assert(claimed != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intr.u.Intx.Intx.Dpc) != FALSE);
    assert(ctx.config_calls == 2);
    assert(ctx.queue_calls == 2);

    VirtioPciWdmInterruptDisconnect(&intr);
    assert(WdkTestGetIoDisconnectInterruptCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
}

static void test_message_connect_disconnect_calls_wdk_routines(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();
    WdkTestResetLastIoConnectInterruptExParams();

    desc = make_msg_desc(4);
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    assert(intr.u.Message.MessageCount == 4);
    assert(intr.u.Message.MessageInfo != NULL);
    assert(intr.u.Message.MessageInfo->MessageCount == 4);
    assert(WdkTestGetIoConnectInterruptExCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
    assert(WdkTestGetLastIoConnectInterruptExPhysicalDeviceObject() == &pdo);
    assert(WdkTestGetLastIoConnectInterruptExMessageCount() == 4);
    assert(WdkTestGetLastIoConnectInterruptExSynchronizeIrql() == desc.u.MessageInterrupt.Level);

    VirtioPciWdmInterruptDisconnect(&intr);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 1);

    /* Disconnect again should be safe and not call IoDisconnectInterruptEx again. */
    VirtioPciWdmInterruptDisconnect(&intr);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 1);
}

static int g_mmio_read_count = 0;
static BOOLEAN mmio_read_handler(_In_ const volatile VOID* Register, _In_ size_t Width, _Out_ ULONGLONG* ValueOut)
{
    (void)Register;
    (void)Width;
    if (ValueOut == NULL) {
        return FALSE;
    }
    g_mmio_read_count++;
    *ValueOut = 0;
    return TRUE;
}

static void test_message_isr_does_not_read_isr_status_byte(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    volatile UCHAR isr_reg = 0xAA;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(2);
    RtlZeroMemory(&ctx, sizeof(ctx));

    g_mmio_read_count = 0;
    WdkSetMmioHandlers(mmio_read_handler, NULL);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    /* Trigger a queue message (message 1). */
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(isr_reg == 0xAA);
    assert(g_mmio_read_count == 0);

    /* Run the queued DPC and observe the default mapping (message 1 -> queue 0). */
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == 0);
    assert(ctx.config_calls == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
    WdkSetMmioHandlers(NULL, NULL);
}

static void test_message_isr_dpc_routing_and_evt_dpc_override(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(3);
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    /* Override routes: msg0=config, msg1=queue2, msg2=queue3. */
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 0, TRUE, VIRTIO_PCI_WDM_QUEUE_INDEX_NONE) == STATUS_SUCCESS);
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 1, FALSE, 2) == STATUS_SUCCESS);
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 2, FALSE, 3) == STATUS_SUCCESS);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == 2);
    assert(ctx.config_calls == 0);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[0]) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);

    VirtioPciWdmInterruptDisconnect(&intr);

    /* Now verify EvtDpc override suppresses per-type callbacks. */
    RtlZeroMemory(&ctx, sizeof(ctx));
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, evt_dpc, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 1, FALSE, 7) == STATUS_SUCCESS);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);

    assert(ctx.dpc_calls == 1);
    assert(ctx.last_message_id == 1);
    assert(ctx.last_is_config == FALSE);
    assert(ctx.last_queue_index == 7);
    assert(ctx.dpc_queue_calls == 1);
    assert(ctx.dpc_config_calls == 0);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_single_vector_default_mapping_dispatches_queue_work(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(1);
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    ctx.expected = &intr;

    /*
     * With only one message available, the default routing must treat message 0
     * as config + "unknown/all queues" so a virtio device routing all sources to
     * vector 0 continues to deliver queue completions.
     */
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[0]) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_interrupt_during_dpc_requeues(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(2); /* msg0=config, msg1=queue0 */
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, evt_queue_trigger_message_interrupt_once, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    /* Trigger queue message (message 1) and run its DPC. The callback triggers another interrupt mid-DPC. */
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);

    /*
     * A second interrupt occurred during the DPC and should have re-queued the
     * KDPC. DpcInFlight should still be 1 (queued but not yet run).
     */
    assert(ctx.queue_calls == 1);
    assert(intr.u.Message.IsrCount == 2);
    assert(intr.u.Message.DpcCount == 1);
    assert(intr.u.Message.MessageDpcs[1].Inserted != FALSE);
    assert(intr.u.Message.DpcInFlight == 1);

    /* Run the second queued DPC. */
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);
    assert(ctx.queue_calls == 2);
    assert(intr.u.Message.DpcCount == 2);
    assert(intr.u.Message.DpcInFlight == 0);
    assert(intr.u.Message.MessageDpcs[1].Inserted == FALSE);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_connect_failure_zeroes_state(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(2);

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    memset(&intr, 0xA5, sizeof(intr));

    WdkTestSetIoConnectInterruptExStatus(STATUS_INSUFFICIENT_RESOURCES);
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INSUFFICIENT_RESOURCES);
    assert(intr.Initialized == FALSE);
    assert(intr.Mode == VirtioPciWdmInterruptModeUnknown);
    assert(intr.u.Message.MessageDpcs == NULL);
    assert(intr.u.Message.Routes == NULL);
    assert(WdkTestGetIoConnectInterruptExCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);

    WdkTestSetIoConnectInterruptExStatus(STATUS_SUCCESS);
}

static void test_intx_evt_dpc_override(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;
    volatile UCHAR isr_reg = 0;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, NULL, &desc, &isr_reg, evt_config, evt_queue, evt_dpc, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    isr_reg = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    assert(WdkTestTriggerInterrupt(intr.u.Intx.Intx.InterruptObject) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Intx.Intx.Dpc) != FALSE);

    /* INTx adapter splits config + queue into two dispatch calls. */
    assert(ctx.dpc_calls == 2);
    assert(ctx.dpc_config_calls == 1);
    assert(ctx.dpc_queue_calls == 1);
    assert(ctx.last_message_id == VIRTIO_PCI_WDM_MESSAGE_ID_NONE);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_disconnect_waits_for_inflight_dpc(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(1);

    WdkTestResetKeDelayExecutionThreadCount();

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);

    /* Simulate an in-flight DPC (not queued) so disconnect must wait. */
    intr.u.Message.DpcInFlight = 1;
    WdkTestAutoCompleteDpcInFlightAfterDelayCalls(&intr.u.Message.DpcInFlight, 3);

    VirtioPciWdmInterruptDisconnect(&intr);
    assert(WdkTestGetKeDelayExecutionThreadCount() == 3);
    WdkTestClearAutoCompleteDpcInFlight();
}

static void test_disconnect_cancels_queued_dpc(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(2);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(intr.u.Message.MessageDpcs[1].Inserted != FALSE);
    assert(intr.u.Message.DpcInFlight == 1);

    VirtioPciWdmInterruptDisconnect(&intr);
    assert(intr.Initialized == FALSE);
}

static void test_set_message_route_validation(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;
    volatile UCHAR isr_reg = 0;

    /* NULL interrupt state pointer. */
    status = VirtioPciWdmInterruptSetMessageRoute(NULL, 0, TRUE, 0);
    assert(status == STATUS_INVALID_PARAMETER);

    /* Uninitialized state object. */
    RtlZeroMemory(&intr, sizeof(intr));
    status = VirtioPciWdmInterruptSetMessageRoute(&intr, 0, TRUE, 0);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    /* INTx mode should reject message route updates. */
    desc = make_int_desc();
    status = VirtioPciWdmInterruptConnect(&dev, NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeIntx);
    status = VirtioPciWdmInterruptSetMessageRoute(&intr, 0, TRUE, 0);
    assert(status == STATUS_INVALID_DEVICE_STATE);
    VirtioPciWdmInterruptDisconnect(&intr);

    /* Out-of-range MessageId. */
    desc = make_msg_desc(2);
    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    status = VirtioPciWdmInterruptSetMessageRoute(&intr, 2, TRUE, 0);
    assert(status == STATUS_INVALID_PARAMETER);
    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_route_can_enable_all_on_vector0_fallback(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    /*
     * Simulate a system where Windows granted >1 message interrupt, but a driver
     * chooses to route all virtio interrupt sources to vector 0 (e.g. because
     * MessageCount < (1 + QueueCount) for a multi-queue device).
     *
     * The helper does not know the device's queue count, so callers must override
     * routing for message 0 to include queue work.
     */
    desc = make_msg_desc(3);
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);

    /* Route message 0 to config + queue(all), and disable other messages. */
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 0, TRUE, VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN) == STATUS_SUCCESS);
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 1, FALSE, VIRTIO_PCI_WDM_QUEUE_INDEX_NONE) == STATUS_SUCCESS);
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 2, FALSE, VIRTIO_PCI_WDM_QUEUE_INDEX_NONE) == STATUS_SUCCESS);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[0]) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_default_mapping_multivector_message0_is_config_only(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(2); /* more than one message available */
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);

    /*
     * Default mapping for MessageCount>1 treats message 0 as config-only to avoid
     * draining queues concurrently with per-queue message DPCs.
     */
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 0) != FALSE);
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[0]) != FALSE);
    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_isr_returns_false_for_out_of_range_message_id(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;
    PKINTERRUPT intr0;
    PKMESSAGE_SERVICE_ROUTINE sr;
    PVOID ctx;
    BOOLEAN claimed;

    desc = make_msg_desc(2);

    WdkTestResetKeInsertQueueDpcCounts();

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    assert(intr.u.Message.MessageInfo != NULL);
    assert(intr.u.Message.MessageInfo->MessageCount == 2);

    intr0 = intr.u.Message.MessageInfo->MessageInfo[0].InterruptObject;
    assert(intr0 != NULL);
    sr = intr0->MessageServiceRoutine;
    ctx = intr0->ServiceContext;
    assert(sr != NULL);

    /* Out-of-range MessageId should be rejected and must not queue a DPC. */
    claimed = sr(intr0, ctx, 99);
    assert(claimed == FALSE);
    assert(intr.u.Message.IsrCount == 0);
    assert(intr.u.Message.DpcInFlight == 0);
    assert(intr.u.Message.MessageDpcs[0].Inserted == FALSE);
    assert(intr.u.Message.MessageDpcs[1].Inserted == FALSE);
    assert(WdkTestGetKeInsertQueueDpcCount() == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_interrupt_during_connect_is_handled(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    io_connect_interrupt_ex_hook_ctx_t hook_ctx;
    NTSTATUS status;

    desc = make_msg_desc(2); /* msg0=config, msg1=queue0 */
    RtlZeroMemory(&ctx, sizeof(ctx));
    RtlZeroMemory(&hook_ctx, sizeof(hook_ctx));

    hook_ctx.message_id_to_trigger = 1;

    WdkTestResetKeInsertQueueDpcCounts();
    WdkTestSetIoConnectInterruptExHook(io_connect_interrupt_ex_hook_trigger_message, &hook_ctx);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);

    /* Hook must have fired exactly once and queued a DPC for message 1. */
    assert(hook_ctx.call_count == 1);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    assert(intr.u.Message.IsrCount == 1);
    assert(intr.u.Message.DpcInFlight == 1);
    assert(intr.u.Message.MessageDpcs[1].Inserted != FALSE);
    assert(WdkTestGetKeInsertQueueDpcCount() == 1);

    WdkTestClearIoConnectInterruptExHook();

    /* Now run the queued DPC and verify dispatch. */
    ctx.expected = &intr;
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);
    assert(ctx.queue_calls == 1);
    assert(ctx.last_queue_index == 0);
    assert(ctx.config_calls == 0);
    assert(intr.u.Message.DpcInFlight == 0);

    VirtioPciWdmInterruptDisconnect(&intr);
}

static void test_message_isr_increments_dpc_inflight_before_queueing_dpc(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    DEVICE_OBJECT pdo;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;
    ke_insert_queue_dpc_hook_ctx_t hook_ctx;

    desc = make_msg_desc(2); /* msg0=config, msg1=queue0 */
    RtlZeroMemory(&hook_ctx, sizeof(hook_ctx));

    WdkTestSetKeInsertQueueDpcHook(ke_insert_queue_dpc_hook, &hook_ctx);

    status = VirtioPciWdmInterruptConnect(&dev, &pdo, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);

    /*
     * Trigger two interrupts for the same message before running its DPC.
     *
     * ISR increments DpcInFlight *before* calling KeInsertQueueDpc, and then
     * decrements it on the "already queued" path. This test observes the
     * transient DpcInFlight=2 case on the second interrupt.
     */
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);

    assert(hook_ctx.call_count == 2);

    /* First insert attempt: DPC not queued yet, DpcInFlight should already be 1. */
    assert(hook_ctx.message_id_at_call[0] == 1);
    assert(hook_ctx.inserted_at_call[0] == FALSE);
    assert(hook_ctx.inflight_at_call[0] == 1);

    /* Second attempt: DPC was already queued, but ISR has incremented DpcInFlight to 2 before attempting to queue. */
    assert(hook_ctx.message_id_at_call[1] == 1);
    assert(hook_ctx.inserted_at_call[1] != FALSE);
    assert(hook_ctx.inflight_at_call[1] == 2);

    /* One DPC instance should still be pending (queued). */
    assert(intr.u.Message.DpcInFlight == 1);
    assert(intr.u.Message.MessageDpcs[1].Inserted != FALSE);

    /* Drain the queued DPC and ensure state returns to idle. */
    assert(WdkTestRunQueuedDpc(&intr.u.Message.MessageDpcs[1]) != FALSE);
    assert(intr.u.Message.DpcInFlight == 0);

    VirtioPciWdmInterruptDisconnect(&intr);

    WdkTestClearKeInsertQueueDpcHook();
}

int main(void)
{
    test_connect_validation();
    test_intx_connect_and_dispatch();
    test_message_connect_disconnect_calls_wdk_routines();
    test_message_single_vector_default_mapping_dispatches_queue_work();
    test_message_isr_does_not_read_isr_status_byte();
    test_message_isr_dpc_routing_and_evt_dpc_override();
    test_message_interrupt_during_dpc_requeues();
    test_connect_failure_zeroes_state();
    test_intx_evt_dpc_override();
    test_disconnect_waits_for_inflight_dpc();
    test_disconnect_cancels_queued_dpc();
    test_set_message_route_validation();
    test_message_route_can_enable_all_on_vector0_fallback();
    test_message_default_mapping_multivector_message0_is_config_only();
    test_message_isr_returns_false_for_out_of_range_message_id();
    test_message_interrupt_during_connect_is_handled();
    test_message_isr_increments_dpc_inflight_before_queueing_dpc();

    printf("virtio_interrupts_wdm_tests: PASS\n");
    return 0;
}
