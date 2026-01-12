#include "virtio_input.h"

typedef struct _VIRTIO_INPUT_READ_REQUEST_CONTEXT {
    PHID_XFER_PACKET XferPacket;
    PMDL XferPacketMdl;

    PUCHAR ReportBuffer;
    PMDL ReportBufferMdl;
    ULONG ReportBufferLen;
} VIRTIO_INPUT_READ_REQUEST_CONTEXT, *PVIRTIO_INPUT_READ_REQUEST_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_INPUT_READ_REQUEST_CONTEXT, VirtioInputGetReadRequestContext);

static EVT_WDF_OBJECT_CONTEXT_CLEANUP VirtioInputEvtReadRequestContextCleanup;

static VOID VirtioInputEvtIoCanceledOnReadQueue(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
    VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

    VIOINPUT_LOG(
        VIOINPUT_LOG_QUEUE,
        "READ_REPORT cancelled: status=%!STATUS! bytes=0 ring=%ld pending=%ld\n",
        STATUS_CANCELLED,
        devCtx->Counters.ReportRingDepth,
        devCtx->Counters.ReadReportQueueDepth);

    WdfRequestComplete(Request, STATUS_CANCELLED);
}

static VOID VirtioInputPendingRingInit(_Inout_ struct virtio_input_report_ring *ring)
{
    ring->head = 0;
    ring->tail = 0;
    ring->count = 0;
}

static VOID VirtioInputPendingRingPush(
    _Inout_ struct virtio_input_report_ring *ring,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize
)
{
    if (ReportSize == 0 || ReportSize > VIRTIO_INPUT_REPORT_MAX_SIZE) {
        return;
    }

    if (ring->count == VIRTIO_INPUT_REPORT_RING_CAPACITY) {
        ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
        ring->count--;
    }

    {
        struct virtio_input_report *slot = &ring->reports[ring->head];
        slot->len = (uint8_t)ReportSize;
        RtlCopyMemory(slot->data, Report, ReportSize);
    }

    ring->head = (ring->head + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count++;
}

static BOOLEAN VirtioInputPendingRingPop(_Inout_ struct virtio_input_report_ring *ring, _Out_ struct virtio_input_report *out)
{
    if (ring->count == 0) {
        return FALSE;
    }

    *out = ring->reports[ring->tail];
    ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count--;
    return TRUE;
}

static NTSTATUS VirtioInputMapUserAddress(
    _In_ PVOID UserAddress,
    _In_ SIZE_T Length,
    _In_ LOCK_OPERATION Operation,
    _Outptr_ PMDL *MdlOut,
    _Outptr_result_bytebuffer_(Length) PVOID *SystemAddressOut
)
{
    if (UserAddress == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Length > (SIZE_T)MAXULONG) {
        return STATUS_INVALID_PARAMETER;
    }

    PMDL mdl;
    PVOID systemAddress;

    mdl = IoAllocateMdl(UserAddress, (ULONG)Length, FALSE, FALSE, NULL);
    if (mdl == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    __try {
        MmProbeAndLockPages(mdl, UserMode, Operation);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        IoFreeMdl(mdl);
        return (NTSTATUS)GetExceptionCode();
    }

    systemAddress = MmGetSystemAddressForMdlSafe(mdl, NormalPagePriority);
    if (systemAddress == NULL) {
        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    *MdlOut = mdl;
    *SystemAddressOut = systemAddress;
    return STATUS_SUCCESS;
}

static NTSTATUS VirtioInputGetTransferPacket(
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _Outptr_ PHID_XFER_PACKET *XferPacketOut
)
{
    NTSTATUS status;
    PHID_XFER_PACKET xfer;
    size_t len;

    UNREFERENCED_PARAMETER(OutputBufferLength);

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len);
    if (NT_SUCCESS(status) && len >= sizeof(HID_XFER_PACKET)) {
        *XferPacketOut = xfer;
        return STATUS_SUCCESS;
    }

    status = WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len);
    if (NT_SUCCESS(status) && len >= sizeof(HID_XFER_PACKET)) {
        *XferPacketOut = xfer;
        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static NTSTATUS VirtioInputPrepareReadRequest(_In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    NTSTATUS status;
    PHID_XFER_PACKET xfer;
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT ctx;
    WDF_OBJECT_ATTRIBUTES attributes;
    KPROCESSOR_MODE requestorMode;

    status = VirtioInputGetTransferPacket(Request, OutputBufferLength, &xfer);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, VIRTIO_INPUT_READ_REQUEST_CONTEXT);
    attributes.EvtCleanupCallback = VirtioInputEvtReadRequestContextCleanup;

    status = WdfObjectAllocateContext(Request, &attributes, (PVOID *)&ctx);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(ctx, sizeof(*ctx));

    requestorMode = WdfRequestGetRequestorMode(Request);
    if (requestorMode == UserMode) {
        status = VirtioInputMapUserAddress(
            xfer,
            sizeof(HID_XFER_PACKET),
            IoWriteAccess,
            &ctx->XferPacketMdl,
            (PVOID *)&ctx->XferPacket);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        xfer = ctx->XferPacket;
    } else {
        ctx->XferPacket = xfer;
    }

    if (xfer->reportBuffer == NULL || xfer->reportBufferLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    ctx->ReportBufferLen = xfer->reportBufferLen;

    if (requestorMode == UserMode) {
        SIZE_T mapLen;

        /*
         * Only map/lock what we can actually write. The virtio-input HID reports
         * are small (<= VIRTIO_INPUT_REPORT_MAX_SIZE), so avoid pinning large
         * user buffers if a caller passes an excessively-large length.
         */
        mapLen = xfer->reportBufferLen;
        if (mapLen > VIRTIO_INPUT_REPORT_MAX_SIZE) {
            mapLen = VIRTIO_INPUT_REPORT_MAX_SIZE;
        }

        status = VirtioInputMapUserAddress(
            xfer->reportBuffer,
            mapLen,
            IoWriteAccess,
            &ctx->ReportBufferMdl,
            (PVOID *)&ctx->ReportBuffer);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    } else {
        ctx->ReportBuffer = xfer->reportBuffer;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS VirtioInputFillPreparedReadRequest(
    _In_ WDFREQUEST Request,
    _In_ UCHAR ReportId,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize,
    _Out_ size_t *BytesWritten
)
{
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT ctx;

    *BytesWritten = 0;
    ctx = VirtioInputGetReadRequestContext(Request);

    ctx->XferPacket->reportId = ReportId;

    if (ctx->ReportBufferLen < ReportSize) {
        ctx->XferPacket->reportBufferLen = 0;
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlCopyMemory(ctx->ReportBuffer, Report, ReportSize);
    ctx->XferPacket->reportBufferLen = (ULONG)ReportSize;
    *BytesWritten = ReportSize;
    return STATUS_SUCCESS;
}

static BOOLEAN VirtioInputIsValidReportId(_In_ UCHAR ReportId)
{
    return (ReportId == VIRTIO_INPUT_REPORT_ID_KEYBOARD) || (ReportId == VIRTIO_INPUT_REPORT_ID_MOUSE);
}

static UCHAR VirtioInputDetermineReadQueueReportId(
    _In_ WDFREQUEST Request,
    _In_ const HID_XFER_PACKET *XferPacket,
    _In_ size_t OutputBufferLength
)
{
    UCHAR reportId;
    size_t reportLenHint;

    reportId = VIRTIO_INPUT_REPORT_ID_ANY;
    if (VirtioInputIsValidReportId(XferPacket->reportId)) {
        reportId = XferPacket->reportId;
    }

    reportLenHint = OutputBufferLength;
    if (XferPacket->reportBufferLen != 0) {
        reportLenHint = XferPacket->reportBufferLen;
    }

    {
        WDFFILEOBJECT fileObject = WdfRequestGetFileObject(Request);
        if (fileObject == NULL) {
            return reportId;
        }

        PVIRTIO_INPUT_FILE_CONTEXT fileCtx = VirtioInputGetFileContext(fileObject);

        if (reportId != VIRTIO_INPUT_REPORT_ID_ANY) {
            if (fileCtx->DefaultReportId == VIRTIO_INPUT_REPORT_ID_ANY && fileCtx->HasCollectionEa) {
                fileCtx->DefaultReportId = reportId;
            }

            return reportId;
        }

        if (VirtioInputIsValidReportId(fileCtx->DefaultReportId)) {
            return fileCtx->DefaultReportId;
        }

        if (fileCtx->HasCollectionEa) {
            if (reportLenHint == VIRTIO_INPUT_KBD_INPUT_REPORT_SIZE) {
                return VIRTIO_INPUT_REPORT_ID_KEYBOARD;
            }
            if (reportLenHint == VIRTIO_INPUT_MOUSE_INPUT_REPORT_SIZE) {
                return VIRTIO_INPUT_REPORT_ID_MOUSE;
            }
        }
    }

    return VIRTIO_INPUT_REPORT_ID_ANY;
}

static VOID VirtioInputDrainReadRequestsForReportId(_In_ WDFDEVICE Device, _In_ UCHAR ReportId)
{
    PDEVICE_CONTEXT devCtx;

    devCtx = VirtioInputGetDeviceContext(Device);

    for (;;) {
        WDFREQUEST request;
        struct virtio_input_report report;
        NTSTATUS status;
        BOOLEAN haveReport;
        BOOLEAN fromAny;
        size_t bytesWritten;

        request = NULL;
        haveReport = FALSE;
        fromAny = FALSE;
        bytesWritten = 0;

        WdfSpinLockAcquire(devCtx->ReadReportLock);

        if (!devCtx->ReadReportsEnabled || devCtx->PendingReportRing[ReportId].count == 0) {
            WdfSpinLockRelease(devCtx->ReadReportLock);
            break;
        }

        status = WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[ReportId], &request);
        if (!NT_SUCCESS(status)) {
            status = WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[VIRTIO_INPUT_REPORT_ID_ANY], &request);
            if (NT_SUCCESS(status)) {
                fromAny = TRUE;
            }
        }

        if (!NT_SUCCESS(status)) {
            WdfSpinLockRelease(devCtx->ReadReportLock);
            break;
        }

        haveReport = VirtioInputPendingRingPop(&devCtx->PendingReportRing[ReportId], &report);

        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (!haveReport) {
            WdfRequestComplete(request, STATUS_DEVICE_NOT_READY);
            continue;
        }

        VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

        status = VirtioInputFillPreparedReadRequest(request, ReportId, report.data, report.len, &bytesWritten);
        WdfRequestCompleteWithInformation(request, status, bytesWritten);

        VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);

        VIOINPUT_LOG(
            VIOINPUT_LOG_QUEUE,
            "READ_REPORT complete(%s): reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
            fromAny ? "any" : "id",
            (ULONG)ReportId,
            status,
            bytesWritten,
            devCtx->Counters.ReadReportQueueDepth);
    }
}

NTSTATUS VirtioInputReadReportQueuesInitialize(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT devCtx;
    NTSTATUS status;
    WDF_OBJECT_ATTRIBUTES lockAttributes;
    WDF_OBJECT_ATTRIBUTES queueAttributes;
    UCHAR i;

    devCtx = VirtioInputGetDeviceContext(Device);

    RtlZeroMemory(devCtx->ReadReportQueue, sizeof(devCtx->ReadReportQueue));

    WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
    lockAttributes.ParentObject = Device;

    status = WdfSpinLockCreate(&lockAttributes, &devCtx->ReadReportLock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = WdfWaitLockCreate(&lockAttributes, &devCtx->ReadReportWaitLock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    devCtx->ReadReportsEnabled = TRUE;

    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        VirtioInputPendingRingInit(&devCtx->PendingReportRing[i]);
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&queueAttributes);
    queueAttributes.ParentObject = Device;

    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        WDF_IO_QUEUE_CONFIG queueConfig;
        WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);

        queueConfig.PowerManaged = WdfFalse;
        queueConfig.EvtIoCanceledOnQueue = VirtioInputEvtIoCanceledOnReadQueue;

        status = WdfIoQueueCreate(Device, &queueConfig, &queueAttributes, &devCtx->ReadReportQueue[i]);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    return STATUS_SUCCESS;
}

VOID VirtioInputReadReportQueuesStart(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT devCtx;
    UCHAR i;

    devCtx = VirtioInputGetDeviceContext(Device);

    WdfWaitLockAcquire(devCtx->ReadReportWaitLock, NULL);

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    devCtx->ReadReportsEnabled = TRUE;
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        VirtioInputPendingRingInit(&devCtx->PendingReportRing[i]);
    }
    WdfSpinLockRelease(devCtx->ReadReportLock);

    WdfWaitLockRelease(devCtx->ReadReportWaitLock);
}

VOID VirtioInputReadReportQueuesStopAndFlush(_In_ WDFDEVICE Device, _In_ NTSTATUS CompletionStatus)
{
    PDEVICE_CONTEXT devCtx;
    UCHAR i;

    devCtx = VirtioInputGetDeviceContext(Device);

    WdfWaitLockAcquire(devCtx->ReadReportWaitLock, NULL);

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    devCtx->ReadReportsEnabled = FALSE;
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        VirtioInputPendingRingInit(&devCtx->PendingReportRing[i]);
    }
    WdfSpinLockRelease(devCtx->ReadReportLock);

    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        WDFREQUEST request;
        while (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[i], &request))) {
            VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
            VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT cancelled (stop): status=%!STATUS! pending=%ld\n",
                CompletionStatus,
                devCtx->Counters.ReadReportQueueDepth);

            WdfRequestComplete(request, CompletionStatus);
        }
    }

    WdfWaitLockRelease(devCtx->ReadReportWaitLock);
}

NTSTATUS VirtioInputReportArrived(
    _In_ WDFDEVICE Device,
    _In_ UCHAR ReportId,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize
)
{
    PDEVICE_CONTEXT devCtx;

    if (!VirtioInputIsValidReportId(ReportId)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ReportSize == 0 || ReportSize > VIRTIO_INPUT_REPORT_MAX_SIZE) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    devCtx = VirtioInputGetDeviceContext(Device);

    if (devCtx->DeviceKind == VioInputDeviceKindKeyboard && ReportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD) {
        return STATUS_NOT_SUPPORTED;
    }
    if (devCtx->DeviceKind == VioInputDeviceKindMouse && ReportId != VIRTIO_INPUT_REPORT_ID_MOUSE) {
        return STATUS_NOT_SUPPORTED;
    }

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    if (!devCtx->ReadReportsEnabled) {
        WdfSpinLockRelease(devCtx->ReadReportLock);
        return STATUS_DEVICE_NOT_READY;
    }

    VirtioInputPendingRingPush(&devCtx->PendingReportRing[ReportId], Report, ReportSize);
    WdfSpinLockRelease(devCtx->ReadReportLock);

    VirtioInputDrainReadRequestsForReportId(Device, ReportId);

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputHandleHidReadReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    WDFDEVICE device;
    PDEVICE_CONTEXT devCtx;
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT reqCtx;
    NTSTATUS status;
    UCHAR reportId;

    device = WdfIoQueueGetDevice(Queue);
    devCtx = VirtioInputGetDeviceContext(device);

    WdfWaitLockAcquire(devCtx->ReadReportWaitLock, NULL);

    if (!devCtx->ReadReportsEnabled || !VirtioInputIsHidActive(devCtx)) {
        WdfWaitLockRelease(devCtx->ReadReportWaitLock);
        WdfRequestComplete(Request, STATUS_DEVICE_NOT_READY);
        return STATUS_SUCCESS;
    }

    status = VirtioInputPrepareReadRequest(Request, OutputBufferLength);
    if (!NT_SUCCESS(status)) {
        WdfWaitLockRelease(devCtx->ReadReportWaitLock);
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

    reqCtx = VirtioInputGetReadRequestContext(Request);
    reportId = VirtioInputDetermineReadQueueReportId(Request, reqCtx->XferPacket, OutputBufferLength);
    if (!VirtioInputIsValidReportId(reportId)) {
        reportId = VIRTIO_INPUT_REPORT_ID_ANY;
    }

    if (reportId == VIRTIO_INPUT_REPORT_ID_ANY) {
        struct virtio_input_report report;
        BOOLEAN haveReport;
        size_t bytesWritten;

        haveReport = FALSE;
        bytesWritten = 0;

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD].count != 0) {
            haveReport = VirtioInputPendingRingPop(&devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD], &report);
        } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE].count != 0) {
            haveReport = VirtioInputPendingRingPop(&devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE], &report);
        }
        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (haveReport) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            status = VirtioInputFillPreparedReadRequest(Request, report.data[0], report.data, report.len, &bytesWritten);
            WdfRequestCompleteWithInformation(Request, status, bytesWritten);

            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(pending): reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
                (ULONG)report.data[0],
                status,
                bytesWritten,
                devCtx->Counters.ReadReportQueueDepth);

            return STATUS_SUCCESS;
        }

        status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[VIRTIO_INPUT_REPORT_ID_ANY]);
        if (!NT_SUCCESS(status)) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_QUEUE, "READ_REPORT queue(any) failed: %!STATUS!\n", status);
            WdfRequestComplete(Request, status);
            return STATUS_SUCCESS;
        }

        VioInputCounterInc(&devCtx->Counters.ReadReportPended);
        VioInputCounterInc(&devCtx->Counters.ReadReportQueueDepth);
        VioInputCounterMaxUpdate(&devCtx->Counters.ReadReportQueueMaxDepth, devCtx->Counters.ReadReportQueueDepth);
        VIOINPUT_LOG(
            VIOINPUT_LOG_QUEUE,
            "READ_REPORT pended(any): pending=%ld ring=%ld\n",
            devCtx->Counters.ReadReportQueueDepth,
            devCtx->Counters.ReportRingDepth);

        WdfWaitLockRelease(devCtx->ReadReportWaitLock);

        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_KEYBOARD);
        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_MOUSE);
        return STATUS_SUCCESS;
    }

    {
        struct virtio_input_report report;
        BOOLEAN haveReport;
        size_t bytesWritten;

        haveReport = FALSE;
        bytesWritten = 0;

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        haveReport = VirtioInputPendingRingPop(&devCtx->PendingReportRing[reportId], &report);
        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (haveReport) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            status = VirtioInputFillPreparedReadRequest(Request, reportId, report.data, report.len, &bytesWritten);
            WdfRequestCompleteWithInformation(Request, status, bytesWritten);

            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(pending): reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
                (ULONG)reportId,
                status,
                bytesWritten,
                devCtx->Counters.ReadReportQueueDepth);

            return STATUS_SUCCESS;
        }

        status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[reportId]);
        if (!NT_SUCCESS(status)) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_QUEUE,
                "READ_REPORT queue(%u) failed: %!STATUS!\n",
                (ULONG)reportId,
                status);
            WdfRequestComplete(Request, status);
            return STATUS_SUCCESS;
        }

        VioInputCounterInc(&devCtx->Counters.ReadReportPended);
        VioInputCounterInc(&devCtx->Counters.ReadReportQueueDepth);
        VioInputCounterMaxUpdate(&devCtx->Counters.ReadReportQueueMaxDepth, devCtx->Counters.ReadReportQueueDepth);
        VIOINPUT_LOG(
            VIOINPUT_LOG_QUEUE,
            "READ_REPORT pended: reportId=%u pending=%ld ring=%ld\n",
            (ULONG)reportId,
            devCtx->Counters.ReadReportQueueDepth,
            devCtx->Counters.ReportRingDepth);

        WdfWaitLockRelease(devCtx->ReadReportWaitLock);

        VirtioInputDrainReadRequestsForReportId(device, reportId);
        return STATUS_SUCCESS;
    }
}

static VOID VirtioInputEvtReadRequestContextCleanup(_In_ WDFOBJECT Object)
{
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT ctx;

    ctx = VirtioInputGetReadRequestContext(Object);

    if (ctx->ReportBufferMdl != NULL) {
        MmUnlockPages(ctx->ReportBufferMdl);
        IoFreeMdl(ctx->ReportBufferMdl);
        ctx->ReportBufferMdl = NULL;
    }

    if (ctx->XferPacketMdl != NULL) {
        MmUnlockPages(ctx->XferPacketMdl);
        IoFreeMdl(ctx->XferPacketMdl);
        ctx->XferPacketMdl = NULL;
    }
}
