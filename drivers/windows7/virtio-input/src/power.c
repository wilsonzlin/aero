#include "virtio_input.h"

static VOID VirtioInputDrainReportRing(_In_ PDEVICE_CONTEXT Ctx)
{
    struct virtio_input_report report;

    while (virtio_input_try_pop_report(&Ctx->InputDevice, &report)) {
    }
}

static VOID VirtioInputFlushPendingReportRings(_In_ PDEVICE_CONTEXT Ctx)
{
    UCHAR i;

    if (Ctx->ReadReportLock == NULL) {
        return;
    }

    WdfSpinLockAcquire(Ctx->ReadReportLock);
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        Ctx->PendingReportRing[i].head = 0;
        Ctx->PendingReportRing[i].tail = 0;
        Ctx->PendingReportRing[i].count = 0;
    }
    WdfSpinLockRelease(Ctx->ReadReportLock);
}

NTSTATUS VirtioInputHidActivateDevice(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(Device);

    if (!ctx->HardwareReady) {
        return STATUS_DEVICE_NOT_READY;
    }

    ctx->HidActivated = TRUE;

    if (ctx->InD0) {
        VirtioInputDrainReportRing(ctx);
        VirtioInputReadReportQueuesStart(Device);
        virtio_input_device_reset_state(&ctx->InputDevice, true);
    }

    VirtioInputUpdateStatusQActiveState(ctx);
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputHidDeactivateDevice(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(Device);
    BOOLEAN emitResetReports;

    /*
     * If HID is currently active, emit an all-zero report before disabling the
     * read queues so Windows releases any latched key state ("stuck keys").
     *
     * If the read queues are already stopping, the reset report will be safely
     * dropped by VirtioInputReportArrived() once ReadReportsEnabled is cleared.
     */
    emitResetReports = ctx->HidActivated ? TRUE : FALSE;
    if (emitResetReports && ctx->InD0) {
        virtio_input_device_reset_state(&ctx->InputDevice, true);
    }

    ctx->HidActivated = FALSE;
    VirtioInputUpdateStatusQActiveState(ctx);
    VirtioInputReadReportQueuesStopAndFlush(Device, STATUS_DEVICE_NOT_READY);
    VirtioInputDrainReportRing(ctx);
    virtio_input_device_reset_state(&ctx->InputDevice, false);
    return STATUS_SUCCESS;
}

VOID VirtioInputHidFlushQueue(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(Device);

    if (ctx->ReadReportWaitLock != NULL) {
        WdfWaitLockAcquire(ctx->ReadReportWaitLock, NULL);
    }

    VirtioInputFlushPendingReportRings(ctx);

    if (ctx->ReadReportWaitLock != NULL) {
        WdfWaitLockRelease(ctx->ReadReportWaitLock);
    }

    VirtioInputDrainReportRing(ctx);
}
