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
    ctx->HidActivated = FALSE;
    VirtioInputUpdateStatusQActiveState(ctx);
    if (emitResetReports && ctx->InD0) {
        virtio_input_device_reset_state(&ctx->InputDevice, true);
    }
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

_Use_decl_annotations_
NTSTATUS VirtioInputHandleVirtioConfigChange(WDFDEVICE Device)
{
    PDEVICE_CONTEXT ctx;
    NTSTATUS quiesceStatus;
    NTSTATUS exitStatus;
    NTSTATUS entryStatus;
    NTSTATUS resumeStatus;
    NTSTATUS status;

    PAGED_CODE();

    ctx = VirtioInputGetDeviceContext(Device);
    if (ctx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!ctx->HardwareReady || ctx->PciDevice.CommonCfg == NULL) {
        return STATUS_DEVICE_NOT_READY;
    }

    //
    // If the framework has already powered the device down (or is powering it
    // down), don't attempt to reinitialize the transport from a queued config
    // change work item.
    //
    if (!ctx->InD0 || InterlockedCompareExchange(&ctx->VirtioStarted, 0, 0) == 0) {
        ctx->LastConfigGeneration = READ_REGISTER_UCHAR(&ctx->PciDevice.CommonCfg->config_generation);
        return STATUS_SUCCESS;
    }

    //
    // The config-change interrupt comes from a DISPATCH_LEVEL DPC. All heavy
    // config reads / reset + reinitialization must happen here at PASSIVE_LEVEL.
    //
    quiesceStatus = VirtioPciInterruptsQuiesce(&ctx->Interrupts, ctx->PciDevice.CommonCfg);
    if (!NT_SUCCESS(quiesceStatus)) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
            "VirtioPciInterruptsQuiesce (config-change) failed: %!STATUS!\n",
            quiesceStatus);
    }

    //
    // Reinitialize the transport similarly to a D0Exit->D0Entry cycle. This
    // re-validates key virtio-input config fields (ID_NAME/DEVIDS/EV_BITS) via
    // VirtioInputEvtDeviceD0Entry and re-programs queues.
    //
    exitStatus = VirtioInputEvtDeviceD0Exit(Device, WdfPowerDeviceD3Final);
    if (!NT_SUCCESS(exitStatus)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "config-change: D0Exit failed: %!STATUS!\n", exitStatus);
    }

    entryStatus = VirtioInputEvtDeviceD0Entry(Device, WdfPowerDeviceD3Final);
    if (!NT_SUCCESS(entryStatus)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ, "config-change: D0Entry failed: %!STATUS!\n", entryStatus);

        //
        // Ensure the device is left in a known-safe reset state if reinit fails.
        //
        VirtioPciResetDevice(&ctx->PciDevice);
    }

    status = entryStatus;

    //
    // Upstream D0Entry re-enables MSI-X delivery via VirtioPciInterruptsResume.
    // Avoid double-enabling (which may fail) by only resuming explicitly for:
    //   - legacy INTx (or unknown) mode, or
    //   - failure paths where D0Entry may not have reached the resume step.
    //
    resumeStatus = STATUS_SUCCESS;
    if (ctx->Interrupts.Mode != VirtioPciInterruptModeMsix || !NT_SUCCESS(entryStatus)) {
        resumeStatus = VirtioPciInterruptsResume(&ctx->Interrupts, ctx->PciDevice.CommonCfg);
        if (!NT_SUCCESS(resumeStatus)) {
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_VIRTQ,
                "VirtioPciInterruptsResume (config-change) failed: %!STATUS!\n",
                resumeStatus);
            if (NT_SUCCESS(status)) {
                status = resumeStatus;
            }
        }
    }

    if (ctx->PciDevice.CommonCfg != NULL) {
        ctx->LastConfigGeneration = READ_REGISTER_UCHAR(&ctx->PciDevice.CommonCfg->config_generation);
    }

    return status;
}
