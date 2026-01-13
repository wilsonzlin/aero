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
} interrupts_test_ctx_t;

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
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();
    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    desc = make_msg_desc(2);

    status = VirtioPciWdmInterruptConnect(&dev, NULL, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioPciWdmInterruptConnect(NULL, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, NULL);
    assert(status == STATUS_INVALID_PARAMETER);

    desc.Type = 0;
    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    desc = make_msg_desc(0);
    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_INVALID_PARAMETER);

    /* Parameter validation failures must not call through to WDK interrupt routines. */
    assert(WdkTestGetIoConnectInterruptCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptCount() == 0);
    assert(WdkTestGetIoConnectInterruptExCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);
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

    status = VirtioPciWdmInterruptConnect(&dev, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeIntx);
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
}

static void test_message_connect_disconnect_calls_wdk_routines(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    desc = make_msg_desc(4);
    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);
    assert(intr.Mode == VirtioPciWdmInterruptModeMessage);
    assert(intr.u.Message.MessageCount == 4);
    assert(intr.u.Message.MessageInfo != NULL);
    assert(intr.u.Message.MessageInfo->MessageCount == 4);
    assert(WdkTestGetIoConnectInterruptExCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptExCount() == 0);

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
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    volatile UCHAR isr_reg = 0xAA;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(2);
    RtlZeroMemory(&ctx, sizeof(ctx));

    g_mmio_read_count = 0;
    WdkSetMmioHandlers(mmio_read_handler, NULL);

    status = VirtioPciWdmInterruptConnect(&dev, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intr);
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
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    interrupts_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_msg_desc(3);
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intr);
    assert(status == STATUS_SUCCESS);
    ctx.expected = &intr;

    /* Override routes: msg0=config, msg1=queue2, msg2=queue3. */
    assert(VirtioPciWdmInterruptSetMessageRoute(&intr, 0, TRUE, VIRTIO_PCI_WDM_QUEUE_INDEX_UNKNOWN) == STATUS_SUCCESS);
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
    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, evt_config, evt_queue, evt_dpc, &ctx, &intr);
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

static void test_connect_failure_zeroes_state(void)
{
    VIRTIO_PCI_WDM_INTERRUPTS intr;
    DEVICE_OBJECT dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(2);

    WdkTestResetIoConnectInterruptExCount();
    WdkTestResetIoDisconnectInterruptExCount();

    memset(&intr, 0xA5, sizeof(intr));

    WdkTestSetIoConnectInterruptExStatus(STATUS_INSUFFICIENT_RESOURCES);
    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
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

    status = VirtioPciWdmInterruptConnect(&dev, &desc, &isr_reg, evt_config, evt_queue, evt_dpc, &ctx, &intr);
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
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(1);

    WdkTestResetKeDelayExecutionThreadCount();

    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
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
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_msg_desc(2);

    status = VirtioPciWdmInterruptConnect(&dev, &desc, NULL, NULL, NULL, NULL, NULL, &intr);
    assert(status == STATUS_SUCCESS);

    assert(WdkTestTriggerMessageInterrupt(intr.u.Message.MessageInfo, 1) != FALSE);
    assert(intr.u.Message.MessageDpcs[1].Inserted != FALSE);
    assert(intr.u.Message.DpcInFlight == 1);

    VirtioPciWdmInterruptDisconnect(&intr);
    assert(intr.Initialized == FALSE);
}

int main(void)
{
    test_connect_validation();
    test_intx_connect_and_dispatch();
    test_message_connect_disconnect_calls_wdk_routines();
    test_message_isr_does_not_read_isr_status_byte();
    test_message_isr_dpc_routing_and_evt_dpc_override();
    test_connect_failure_zeroes_state();
    test_intx_evt_dpc_override();
    test_disconnect_waits_for_inflight_dpc();
    test_disconnect_cancels_queued_dpc();

    printf("virtio_interrupts_wdm_tests: PASS\n");
    return 0;
}
