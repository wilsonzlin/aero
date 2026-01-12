/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_intx_wdm.h"

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

typedef struct intx_test_ctx {
    PVIRTIO_INTX expected_intx;
    int config_calls;
    int queue_calls;
    int dpc_calls;
    UCHAR last_isr_status;
    int trigger_once;
} intx_test_ctx_t;

static VOID evt_config(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    intx_test_ctx_t* ctx = (intx_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Intx == ctx->expected_intx);
    ctx->config_calls++;
}

static VOID evt_queue(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    intx_test_ctx_t* ctx = (intx_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Intx == ctx->expected_intx);
    ctx->queue_calls++;
}

static VOID evt_queue_trigger_interrupt_once(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    intx_test_ctx_t* ctx = (intx_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Intx == ctx->expected_intx);

    ctx->queue_calls++;

    /*
     * Simulate another interrupt arriving while the DPC is executing. This
     * exercises DpcInFlight tracking across the "ISR queues DPC while DPC is
     * running" case.
     */
    if (ctx->trigger_once == 0) {
        ctx->trigger_once = 1;

        /* Trigger a config interrupt. */
        *Intx->IsrStatusRegister = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
        assert(WdkTestTriggerInterrupt(Intx->InterruptObject) != FALSE);
        assert(*Intx->IsrStatusRegister == 0);
    }
}

static VOID evt_dpc(_Inout_ PVIRTIO_INTX Intx, _In_ UCHAR IsrStatus, _In_opt_ PVOID Cookie)
{
    intx_test_ctx_t* ctx = (intx_test_ctx_t*)Cookie;
    assert(ctx != NULL);
    assert(Intx == ctx->expected_intx);
    ctx->dpc_calls++;
    ctx->last_isr_status = IsrStatus;
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
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, NULL, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, NULL);
    assert(status == STATUS_INVALID_PARAMETER);

    status = VirtioIntxConnect(NULL, &desc, NULL, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_INVALID_DEVICE_STATE);

    desc.Type = 0;
    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_INVALID_PARAMETER);

    desc = make_int_desc();
    desc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_NOT_SUPPORTED);

    /* Parameter validation failures must not call through to WDK interrupt routines. */
    assert(WdkTestGetIoConnectInterruptCount() == 0);
    assert(WdkTestGetIoDisconnectInterruptCount() == 0);
}

static void test_connect_descriptor_translation(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    /* Latched + shared -> Latched + ShareVector=TRUE */
    desc = make_int_desc();
    desc.Flags = CM_RESOURCE_INTERRUPT_LATCHED;

    RtlZeroMemory(&ctx, sizeof(ctx));
    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    assert(intx.InterruptObject != NULL);
    assert(intx.InterruptObject->InterruptMode == Latched);
    assert(intx.InterruptObject->ShareVector == TRUE);
    assert(intx.InterruptObject->Vector == desc.u.Interrupt.Vector);
    assert(intx.InterruptObject->Irql == (KIRQL)desc.u.Interrupt.Level);
    assert(intx.InterruptObject->SynchronizeIrql == (KIRQL)desc.u.Interrupt.Level);
    assert(intx.InterruptObject->ProcessorEnableMask == (KAFFINITY)desc.u.Interrupt.Affinity);

    VirtioIntxDisconnect(&intx);

    /* Level-sensitive + non-shared -> LevelSensitive + ShareVector=FALSE */
    desc = make_int_desc();
    desc.ShareDisposition = 0;
    desc.Flags = 0;

    RtlZeroMemory(&ctx, sizeof(ctx));
    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    assert(intx.InterruptObject != NULL);
    assert(intx.InterruptObject->InterruptMode == LevelSensitive);
    assert(intx.InterruptObject->ShareVector == FALSE);

    VirtioIntxDisconnect(&intx);
}

static void test_connect_failure_zeroes_state(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    /*
     * Ensure VirtioIntxConnect zeroes the output object on failure so teardown
     * paths can safely call VirtioIntxDisconnect unconditionally.
     */
    memset(&intx, 0xA5, sizeof(intx));

    WdkTestSetIoConnectInterruptStatus(STATUS_INSUFFICIENT_RESOURCES);
    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_INSUFFICIENT_RESOURCES);

    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
    assert(intx.Dpc.DeferredRoutine == NULL);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);

    /*
     * Even on failure, VirtioIntxConnect should have attempted IoConnectInterrupt
     * exactly once, and should not call IoDisconnectInterrupt because the
     * interrupt object was never created.
     */
    assert(WdkTestGetIoConnectInterruptCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptCount() == 0);

    /* Restore default for other tests. */
    WdkTestSetIoConnectInterruptStatus(STATUS_SUCCESS);
}

static void test_connect_disconnect_calls_wdk_routines(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    WdkTestResetIoConnectInterruptCount();
    WdkTestResetIoDisconnectInterruptCount();

    desc = make_int_desc();

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intx);
    assert(status == STATUS_SUCCESS);
    assert(WdkTestGetIoConnectInterruptCount() == 1);
    assert(WdkTestGetIoDisconnectInterruptCount() == 0);

    VirtioIntxDisconnect(&intx);
    assert(WdkTestGetIoDisconnectInterruptCount() == 1);

    /* Disconnect again should not call IoDisconnectInterrupt again. */
    VirtioIntxDisconnect(&intx);
    assert(WdkTestGetIoDisconnectInterruptCount() == 1);
}

static void test_disconnect_uninitialized_is_safe(void)
{
    VIRTIO_INTX intx;
    RtlZeroMemory(&intx, sizeof(intx));

    /* Should be safe to call even if VirtioIntxConnect never succeeded/ran. */
    assert(intx.Initialized == FALSE);
    VirtioIntxDisconnect(&intx);

    /* Disconnect should leave it zeroed. */
    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
    assert(intx.DpcInFlight == 0);
    assert(intx.PendingIsrStatus == 0);
}

static void test_disconnect_is_idempotent(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_int_desc();

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intx);
    assert(status == STATUS_SUCCESS);

    VirtioIntxDisconnect(&intx);

    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
    assert(intx.DpcInFlight == 0);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.Dpc.Inserted == FALSE);

    /* Allow drivers to call VirtioIntxDisconnect multiple times during teardown. */
    VirtioIntxDisconnect(&intx);

    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
    assert(intx.DpcInFlight == 0);
    assert(intx.PendingIsrStatus == 0);
}

static void test_spurious_interrupt(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;
    BOOLEAN claimed;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    ctx.expected_intx = &intx;

    /* Spurious interrupt: status byte contains 0. */
    isr_reg = 0;
    claimed = WdkTestTriggerInterrupt(intx.InterruptObject);
    assert(claimed == FALSE);
    assert(intx.SpuriousCount == 1);
    assert(intx.IsrCount == 0);
    assert(intx.DpcCount == 0);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.Dpc.Inserted == FALSE);

    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioIntxDisconnect(&intx);
    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
}

static void test_isr_defensive_null_service_context(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_int_desc();

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intx);
    assert(status == STATUS_SUCCESS);

    /* Corrupt the stored service context to NULL: ISR should just return FALSE. */
    assert(intx.InterruptObject != NULL);
    intx.InterruptObject->ServiceContext = NULL;

    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) == FALSE);

    /* Without a service context, the ISR can't ACK (it doesn't know the register). */
    assert(isr_reg == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);

    VirtioIntxDisconnect(&intx);
}

static void test_isr_defensive_null_isr_register(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_int_desc();

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intx);
    assert(status == STATUS_SUCCESS);

    /* Corrupt IsrStatusRegister: ISR should return FALSE without touching memory. */
    intx.IsrStatusRegister = NULL;

    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) == FALSE);
    assert(isr_reg == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);
    assert(intx.SpuriousCount == 0);
    assert(intx.IsrCount == 0);
    assert(intx.DpcInFlight == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_null_callbacks_safe(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    NTSTATUS status;

    desc = make_int_desc();

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, NULL, NULL, NULL, NULL, &intx);
    assert(status == STATUS_SUCCESS);

    /* Interrupt with both bits set should still be ACKed and drained safely. */
    isr_reg = 0x3;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.PendingIsrStatus == 0x3);
    assert(intx.DpcInFlight == 1);

    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    VirtioIntxDisconnect(&intx);
}

static void test_spurious_interrupt_does_not_affect_pending(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;
    BOOLEAN claimed;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    /* First interrupt queues a DPC and sets PendingIsrStatus. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    claimed = WdkTestTriggerInterrupt(intx.InterruptObject);
    assert(claimed != FALSE);
    assert(isr_reg == 0);
    assert(intx.IsrCount == 1);
    assert(intx.SpuriousCount == 0);
    assert(intx.PendingIsrStatus == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Spurious interrupt while DPC is still queued should not disturb pending state. */
    isr_reg = 0;
    claimed = WdkTestTriggerInterrupt(intx.InterruptObject);
    assert(claimed == FALSE);
    assert(intx.IsrCount == 1);
    assert(intx.SpuriousCount == 1);
    assert(intx.PendingIsrStatus == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Run the DPC and ensure the original pending bit is processed. */
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);
    assert(ctx.queue_calls == 1);
    assert(ctx.config_calls == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_unknown_isr_bits_no_callbacks_without_evt_dpc(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    /* Unknown bit: should still be ACKed and drained, but no callbacks. */
    isr_reg = 0x80;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_queue_config_dispatch(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;
    BOOLEAN claimed;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    ctx.expected_intx = &intx;

    /* Interrupt with both queue + config bits set. */
    isr_reg = 0x3;
    claimed = WdkTestTriggerInterrupt(intx.InterruptObject);
    assert(claimed != FALSE);

    /* READ_REGISTER_UCHAR is a read-to-clear ACK. */
    assert(isr_reg == 0);

    assert(intx.IsrCount == 1);
    assert(intx.SpuriousCount == 0);
    assert(intx.PendingIsrStatus == 0x3);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Now run the queued DPC. */
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(intx.Dpc.Inserted == FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);

    VirtioIntxDisconnect(&intx);
}

static void test_queue_only_dispatch(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    assert(ctx.queue_calls == 1);
    assert(ctx.config_calls == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_config_only_dispatch(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    isr_reg = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    assert(ctx.queue_calls == 0);
    assert(ctx.config_calls == 1);

    VirtioIntxDisconnect(&intx);
}

static void test_bit_accumulation_single_dpc(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    ctx.expected_intx = &intx;

    /* Queue interrupt -> queues a DPC. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.PendingIsrStatus == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    /* Config interrupt arrives before the DPC runs -> bits accumulate, no second DPC. */
    isr_reg = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.PendingIsrStatus == (VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT));
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Only one DPC should be queued/runnable. */
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) == FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    assert(ctx.config_calls == 1);
    assert(ctx.queue_calls == 1);

    VirtioIntxDisconnect(&intx);
}

static void test_evt_dpc_accumulation_single_dpc(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, evt_dpc, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    /* Queue interrupt -> queues a DPC. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.PendingIsrStatus == VIRTIO_PCI_ISR_QUEUE_INTERRUPT);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Config interrupt arrives before the DPC runs -> bits accumulate, no second DPC. */
    isr_reg = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.PendingIsrStatus == (VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT));
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) == FALSE);

    assert(intx.PendingIsrStatus == 0);
    assert(intx.DpcInFlight == 0);
    assert(intx.DpcCount == 1);

    /* With EvtDpc installed, the helper should not call the per-bit callbacks. */
    assert(ctx.dpc_calls == 1);
    assert(ctx.last_isr_status == 0x3);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_disconnect_cancels_queued_dpc(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);

    ctx.expected_intx = &intx;

    /* Queue a DPC but do not run it. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(intx.Dpc.Inserted != FALSE);
    assert(intx.DpcInFlight == 1);

    /* Disconnect should cancel safely and zero the state. */
    VirtioIntxDisconnect(&intx);
    assert(intx.Initialized == FALSE);
    assert(intx.InterruptObject == NULL);
    assert(intx.IsrStatusRegister == NULL);
    assert(intx.DpcInFlight == 0);
    assert(intx.PendingIsrStatus == 0);
    assert(intx.Dpc.Inserted == FALSE);
}

static void test_interrupt_during_dpc_requeues(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue_trigger_interrupt_once, NULL, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    /* First interrupt: queue bit -> queues a DPC. */
    isr_reg = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(intx.DpcInFlight == 1);
    assert(intx.Dpc.Inserted != FALSE);

    /* Run the DPC. It will trigger another interrupt while executing. */
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    /*
     * A second interrupt occurred during the DPC and should have re-queued the
     * KDPC. DpcInFlight should still be 1 (queued but not yet run).
     */
    assert(intx.IsrCount == 2);
    assert(intx.DpcCount == 1);
    assert(intx.Dpc.Inserted != FALSE);
    assert(intx.DpcInFlight == 1);
    assert(intx.PendingIsrStatus == VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    assert(ctx.queue_calls == 1);
    assert(ctx.config_calls == 0);

    /* Now run the second DPC. */
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);
    assert(intx.Dpc.Inserted == FALSE);
    assert(intx.DpcCount == 2);
    assert(intx.DpcInFlight == 0);
    assert(intx.PendingIsrStatus == 0);
    assert(ctx.queue_calls == 1);
    assert(ctx.config_calls == 1);

    VirtioIntxDisconnect(&intx);
}

static void test_evt_dpc_dispatch_override(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, evt_dpc, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    isr_reg = 0x3;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    /* With EvtDpc installed, the helper should not call the per-bit callbacks. */
    assert(ctx.dpc_calls == 1);
    assert(ctx.last_isr_status == 0x3);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioIntxDisconnect(&intx);
}

static void test_evt_dpc_receives_unknown_bits(void)
{
    VIRTIO_INTX intx;
    volatile UCHAR isr_reg = 0;
    CM_PARTIAL_RESOURCE_DESCRIPTOR desc;
    intx_test_ctx_t ctx;
    NTSTATUS status;

    desc = make_int_desc();
    RtlZeroMemory(&ctx, sizeof(ctx));

    status = VirtioIntxConnect(NULL, &desc, &isr_reg, evt_config, evt_queue, evt_dpc, &ctx, &intx);
    assert(status == STATUS_SUCCESS);
    ctx.expected_intx = &intx;

    isr_reg = 0x80;
    assert(WdkTestTriggerInterrupt(intx.InterruptObject) != FALSE);
    assert(isr_reg == 0);
    assert(WdkTestRunQueuedDpc(&intx.Dpc) != FALSE);

    assert(ctx.dpc_calls == 1);
    assert(ctx.last_isr_status == 0x80);
    assert(ctx.config_calls == 0);
    assert(ctx.queue_calls == 0);

    VirtioIntxDisconnect(&intx);
}

int main(void)
{
    test_connect_validation();
    test_connect_descriptor_translation();
    test_connect_failure_zeroes_state();
    test_connect_disconnect_calls_wdk_routines();
    test_disconnect_uninitialized_is_safe();
    test_disconnect_is_idempotent();
    test_spurious_interrupt();
    test_isr_defensive_null_service_context();
    test_isr_defensive_null_isr_register();
    test_null_callbacks_safe();
    test_spurious_interrupt_does_not_affect_pending();
    test_unknown_isr_bits_no_callbacks_without_evt_dpc();
    test_queue_config_dispatch();
    test_queue_only_dispatch();
    test_config_only_dispatch();
    test_bit_accumulation_single_dpc();
    test_evt_dpc_accumulation_single_dpc();
    test_disconnect_cancels_queued_dpc();
    test_interrupt_during_dpc_requeues();
    test_evt_dpc_dispatch_override();
    test_evt_dpc_receives_unknown_bits();

    printf("virtio_intx_wdm_tests: PASS\n");
    return 0;
}
