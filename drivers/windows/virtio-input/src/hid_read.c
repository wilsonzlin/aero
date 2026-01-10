#include "virtio_input.h"

static VOID VirtioInputEvtIoCanceledOnReadQueue(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request)
{
    UNREFERENCED_PARAMETER(Queue);
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

    if (NT_SUCCESS(WdfRequestRetrieveInputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        VirtioInputIsValidReportId(xfer->reportId)) {
        return xfer->reportId;
    }

    if (OutputBufferLength == sizeof(HID_XFER_PACKET) &&
        NT_SUCCESS(WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_XFER_PACKET), (PVOID *)&xfer, &len)) &&
        len >= sizeof(HID_XFER_PACKET) &&
        VirtioInputIsValidReportId(xfer->reportId)) {
        return xfer->reportId;
    }

    return VIRTIO_INPUT_REPORT_ID_ANY;
}

static UCHAR VirtioInputDetermineReadQueueReportId(_In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    UCHAR reportId = VirtioInputGetReadReportIdFromXferPacket(Request, OutputBufferLength);

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
        if (OutputBufferLength == VIRTIO_INPUT_KBD_INPUT_REPORT_SIZE) {
            return VIRTIO_INPUT_REPORT_ID_KEYBOARD;
        }
        if (OutputBufferLength == VIRTIO_INPUT_MOUSE_INPUT_REPORT_SIZE) {
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
            size_t bytesWritten = 0;
            status = VirtioInputCopyReportToReadRequest(request, ReportId, Report, ReportSize, &bytesWritten);
            WdfRequestCompleteWithInformation(request, status, bytesWritten);

            if (NT_SUCCESS(status)) {
                return STATUS_SUCCESS;
            }
        }

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        devCtx->PendingReport[ReportId].Valid = TRUE;
        devCtx->PendingReport[ReportId].Size = ReportSize;
        RtlCopyMemory(devCtx->PendingReport[ReportId].Data, Report, ReportSize);
        WdfSpinLockRelease(devCtx->ReadReportLock);

        return STATUS_SUCCESS;
    }

    size_t bytesWritten = 0;
    status = VirtioInputCopyReportToReadRequest(request, ReportId, Report, ReportSize, &bytesWritten);
    WdfRequestCompleteWithInformation(request, status, bytesWritten);

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
            return STATUS_SUCCESS;
        }

        NTSTATUS status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[VIRTIO_INPUT_REPORT_ID_ANY]);
        if (!NT_SUCCESS(status)) {
            WdfRequestComplete(Request, status);
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
        return STATUS_SUCCESS;
    }

    NTSTATUS status = WdfRequestForwardToIoQueue(Request, devCtx->ReadReportQueue[reportId]);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

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
