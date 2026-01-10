#include "virtio_input.h"

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

NTSTATUS VirtioInputReadReportQueuesInitialize(_In_ WDFDEVICE Device)
{
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(Device);

    RtlZeroMemory(devCtx->ReadReportQueue, sizeof(devCtx->ReadReportQueue));
    RtlZeroMemory(devCtx->PendingReport, sizeof(devCtx->PendingReport));

    NTSTATUS status = WdfSpinLockCreate(WDF_NO_OBJECT_ATTRIBUTES, &devCtx->ReadReportLock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    for (UCHAR i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        WDF_IO_QUEUE_CONFIG queueConfig;
        WDF_IO_QUEUE_CONFIG_INIT(&queueConfig, WdfIoQueueDispatchManual);

        queueConfig.PowerManaged = WdfFalse;
        queueConfig.EvtIoCanceledOnQueue = VirtioInputEvtIoCanceledOnReadQueue;

        status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &devCtx->ReadReportQueue[i]);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    return STATUS_SUCCESS;
}

static BOOLEAN VirtioInputIsValidReportId(_In_ UCHAR ReportId)
{
    return (ReportId == VIRTIO_INPUT_REPORT_ID_KEYBOARD) || (ReportId == VIRTIO_INPUT_REPORT_ID_MOUSE);
}

static UCHAR VirtioInputGetReadReportIdFromXferPacket(_In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    PHID_XFER_PACKET xfer = NULL;
    size_t len = 0;

    UNREFERENCED_PARAMETER(OutputBufferLength);

    if (NT_SUCCESS(WdfRequestRetrieveInputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        VirtioInputIsValidReportId(xfer->reportId)) {
        return xfer->reportId;
    }

    if (NT_SUCCESS(WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        VirtioInputIsValidReportId(xfer->reportId)) {
        return xfer->reportId;
    }

    return VIRTIO_INPUT_REPORT_ID_ANY;
}

static UCHAR VirtioInputDetermineReadQueueReportId(_In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    UCHAR reportId = VirtioInputGetReadReportIdFromXferPacket(Request, OutputBufferLength);
    size_t reportLenHint = OutputBufferLength;

    {
        PHID_XFER_PACKET xfer = NULL;
        size_t len = 0;
        if (NT_SUCCESS(WdfRequestRetrieveInputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
            len >= sizeof(HID_XFER_PACKET) && xfer->reportBufferLen != 0) {
            reportLenHint = xfer->reportBufferLen;
        } else if (NT_SUCCESS(WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
                   len >= sizeof(HID_XFER_PACKET) && xfer->reportBufferLen != 0) {
            reportLenHint = xfer->reportBufferLen;
        }
    }

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

    return VIRTIO_INPUT_REPORT_ID_ANY;
}

static NTSTATUS VirtioInputCopyReportToReadRequest(
    _In_ WDFREQUEST Request,
    _In_ UCHAR ReportId,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize,
    _Out_ size_t *BytesWritten
)
{
    *BytesWritten = 0;

    PHID_XFER_PACKET xfer = NULL;
    size_t len = 0;

    if (NT_SUCCESS(WdfRequestRetrieveInputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        xfer->reportBuffer != NULL &&
        xfer->reportBufferLen >= ReportSize) {

        xfer->reportId = ReportId;
        RtlCopyMemory(xfer->reportBuffer, Report, ReportSize);
        *BytesWritten = ReportSize;
        return STATUS_SUCCESS;
    }

    if (NT_SUCCESS(WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        xfer->reportBuffer != NULL &&
        xfer->reportBufferLen >= ReportSize) {

        xfer->reportId = ReportId;
        RtlCopyMemory(xfer->reportBuffer, Report, ReportSize);
        *BytesWritten = ReportSize;
        return STATUS_SUCCESS;
    }

    PVOID outBuf = NULL;
    NTSTATUS status = WdfRequestRetrieveOutputBuffer(Request, ReportSize, &outBuf, &len);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlCopyMemory(outBuf, Report, ReportSize);
    *BytesWritten = ReportSize;
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputReportArrived(
    _In_ WDFDEVICE Device,
    _In_ UCHAR ReportId,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize
)
{
    if (!VirtioInputIsValidReportId(ReportId)) {
        return STATUS_INVALID_PARAMETER;
    }

    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(Device);

    if (ReportSize > sizeof(devCtx->PendingReport[0].Data)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    WDFREQUEST request = NULL;
    NTSTATUS status = WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[ReportId], &request);
    if (!NT_SUCCESS(status)) {
        while (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[VIRTIO_INPUT_REPORT_ID_ANY], &request))) {
            VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

            size_t bytesWritten = 0;
            status = VirtioInputCopyReportToReadRequest(request, ReportId, Report, ReportSize, &bytesWritten);
            WdfRequestCompleteWithInformation(request, status, bytesWritten);

            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(any): reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
                (ULONG)ReportId,
                status,
                bytesWritten,
                devCtx->Counters.ReadReportQueueDepth);

            if (NT_SUCCESS(status)) {
                return STATUS_SUCCESS;
            }
        }

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        devCtx->PendingReport[ReportId].Valid = TRUE;
        devCtx->PendingReport[ReportId].Size = ReportSize;
        RtlCopyMemory(devCtx->PendingReport[ReportId].Data, Report, ReportSize);
        WdfSpinLockRelease(devCtx->ReadReportLock);

        VIOINPUT_LOG(
            VIOINPUT_LOG_QUEUE,
            "Buffered report (no pending reads): reportId=%u size=%Iu\n",
            (ULONG)ReportId,
            ReportSize);

        return STATUS_SUCCESS;
    }

    VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);
    size_t bytesWritten = 0;
    status = VirtioInputCopyReportToReadRequest(request, ReportId, Report, ReportSize, &bytesWritten);
    WdfRequestCompleteWithInformation(request, status, bytesWritten);

    VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
    VIOINPUT_LOG(
        VIOINPUT_LOG_QUEUE,
        "READ_REPORT complete: reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
        (ULONG)ReportId,
        status,
        bytesWritten,
        devCtx->Counters.ReadReportQueueDepth);

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputHandleHidReadReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    UCHAR reportId = VirtioInputDetermineReadQueueReportId(Request, OutputBufferLength);
    if (!VirtioInputIsValidReportId(reportId)) {
        reportId = VIRTIO_INPUT_REPORT_ID_ANY;
    }

    if (reportId == VIRTIO_INPUT_REPORT_ID_ANY) {
        UCHAR localReportId = VIRTIO_INPUT_REPORT_ID_ANY;
        UCHAR localReport[64] = {0};
        size_t localSize = 0;

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        if (devCtx->PendingReport[VIRTIO_INPUT_REPORT_ID_KEYBOARD].Valid) {
            localReportId = VIRTIO_INPUT_REPORT_ID_KEYBOARD;
        } else if (devCtx->PendingReport[VIRTIO_INPUT_REPORT_ID_MOUSE].Valid) {
            localReportId = VIRTIO_INPUT_REPORT_ID_MOUSE;
        }

        if (localReportId != VIRTIO_INPUT_REPORT_ID_ANY) {
            localSize = devCtx->PendingReport[localReportId].Size;
            RtlCopyMemory(localReport, devCtx->PendingReport[localReportId].Data, localSize);
            devCtx->PendingReport[localReportId].Valid = FALSE;
        }
        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (localReportId != VIRTIO_INPUT_REPORT_ID_ANY) {
            size_t bytesWritten = 0;
            NTSTATUS status = VirtioInputCopyReportToReadRequest(Request, localReportId, localReport, localSize, &bytesWritten);
            WdfRequestCompleteWithInformation(Request, status, bytesWritten);
            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(pending): reportId=%u status=%!STATUS! bytes=%Iu pending=%ld\n",
                (ULONG)localReportId,
                status,
                bytesWritten,
                devCtx->Counters.ReadReportQueueDepth);
            return STATUS_SUCCESS;
        }

        NTSTATUS status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[VIRTIO_INPUT_REPORT_ID_ANY]);
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_QUEUE, "READ_REPORT queue(any) failed: %!STATUS!\n", status);
            WdfRequestComplete(Request, status);
        } else {
            VioInputCounterInc(&devCtx->Counters.ReadReportPended);
            VioInputCounterInc(&devCtx->Counters.ReadReportQueueDepth);
            VioInputCounterMaxUpdate(&devCtx->Counters.ReadReportQueueMaxDepth, devCtx->Counters.ReadReportQueueDepth);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT pended(any): pending=%ld ring=%ld\n",
                devCtx->Counters.ReadReportQueueDepth,
                devCtx->Counters.ReportRingDepth);
        }
        return STATUS_SUCCESS;
    }

    UCHAR localReport[64] = {0};
    size_t localSize = 0;
    BOOLEAN havePending = FALSE;

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    if (devCtx->PendingReport[reportId].Valid) {
        havePending = TRUE;
        localSize = devCtx->PendingReport[reportId].Size;
        RtlCopyMemory(localReport, devCtx->PendingReport[reportId].Data, localSize);
        devCtx->PendingReport[reportId].Valid = FALSE;
    }
    WdfSpinLockRelease(devCtx->ReadReportLock);

    if (havePending) {
        size_t bytesWritten = 0;
        NTSTATUS status = VirtioInputCopyReportToReadRequest(Request, reportId, localReport, localSize, &bytesWritten);
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

    NTSTATUS status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[reportId]);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_QUEUE, "READ_REPORT queue(%u) failed: %!STATUS!\n", (ULONG)reportId, status);
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

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    if (devCtx->PendingReport[reportId].Valid) {
        localSize = devCtx->PendingReport[reportId].Size;
        RtlCopyMemory(localReport, devCtx->PendingReport[reportId].Data, localSize);
        devCtx->PendingReport[reportId].Valid = FALSE;
        havePending = TRUE;
    } else {
        havePending = FALSE;
    }
    WdfSpinLockRelease(devCtx->ReadReportLock);

    if (havePending) {
        (VOID)VirtioInputReportArrived(device, reportId, localReport, localSize);
    }

    return STATUS_SUCCESS;
}
