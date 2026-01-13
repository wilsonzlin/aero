#include "virtio_input.h"

typedef struct _VIRTIO_INPUT_READ_REQUEST_CONTEXT {
    VIOINPUT_MAPPED_USER_BUFFER XferPacket;
    VIOINPUT_MAPPED_USER_BUFFER ReportBuffer;
    ULONG ReportBufferLen;
} VIRTIO_INPUT_READ_REQUEST_CONTEXT, *PVIRTIO_INPUT_READ_REQUEST_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_INPUT_READ_REQUEST_CONTEXT, VirtioInputGetReadRequestContext);

static EVT_WDF_OBJECT_CONTEXT_CLEANUP VirtioInputEvtReadRequestContextCleanup;

static __forceinline LONG VirtioInputPendingRingTotalDepthLocked(_In_ const DEVICE_CONTEXT* DevCtx)
{
    LONG total;
    UCHAR i;

    total = 0;
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; ++i) {
        total += (LONG)DevCtx->PendingReportRing[i].count;
    }
    return total;
}

static __forceinline VOID VirtioInputPendingRingUpdateCountersLocked(_Inout_ PDEVICE_CONTEXT DevCtx)
{
    LONG depth;

    depth = VirtioInputPendingRingTotalDepthLocked(DevCtx);
    VioInputCounterSet(&DevCtx->Counters.PendingRingDepth, depth);
    VioInputCounterMaxUpdate(&DevCtx->Counters.PendingRingMaxDepth, depth);
}

static VOID VirtioInputEvtIoCanceledOnReadQueue(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
    VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

    VIOINPUT_LOG(
        VIOINPUT_LOG_QUEUE,
        "READ_REPORT cancelled: status=%!STATUS! bytes=0 txRing=%ld pendingRing=%ld readQ=%ld\n",
        STATUS_CANCELLED,
        devCtx->Counters.ReportRingDepth,
        devCtx->Counters.PendingRingDepth,
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
    _Inout_ PDEVICE_CONTEXT DevCtx,
    _Inout_ struct virtio_input_report_ring *ring,
    _In_reads_bytes_(ReportSize) const VOID *Report,
    _In_ size_t ReportSize
)
{
    BOOLEAN dropped;

    if (ReportSize == 0 || ReportSize > VIRTIO_INPUT_REPORT_MAX_SIZE) {
        return;
    }

    dropped = FALSE;
    if (ring->count == VIRTIO_INPUT_REPORT_RING_CAPACITY) {
        ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
        ring->count--;
        dropped = TRUE;
    }

    {
        struct virtio_input_report *slot = &ring->reports[ring->head];
        slot->len = (uint8_t)ReportSize;
        RtlCopyMemory(slot->data, Report, ReportSize);
    }

    ring->head = (ring->head + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count++;

    if (dropped) {
        VioInputCounterInc(&DevCtx->Counters.PendingRingDrops);
    }
    VirtioInputPendingRingUpdateCountersLocked(DevCtx);
}

static BOOLEAN VirtioInputPendingRingPop(
    _Inout_ PDEVICE_CONTEXT DevCtx,
    _Inout_ struct virtio_input_report_ring *ring,
    _Out_ struct virtio_input_report *out
)
{
    if (ring->count == 0) {
        return FALSE;
    }

    *out = ring->reports[ring->tail];
    ring->tail = (ring->tail + 1u) % VIRTIO_INPUT_REPORT_RING_CAPACITY;
    ring->count--;
    VirtioInputPendingRingUpdateCountersLocked(DevCtx);
    return TRUE;
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

    status = VioInputRequestMapUserBuffer(
        Request,
        xfer,
        sizeof(HID_XFER_PACKET),
        sizeof(HID_XFER_PACKET),
        IoWriteAccess,
        &ctx->XferPacket);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    xfer = (PHID_XFER_PACKET)ctx->XferPacket.SystemAddress;

    if (xfer->reportBuffer == NULL || xfer->reportBufferLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    ctx->ReportBufferLen = xfer->reportBufferLen;

    status = VioInputRequestMapUserBuffer(
        Request,
        xfer->reportBuffer,
        (SIZE_T)ctx->ReportBufferLen,
        VIRTIO_INPUT_REPORT_MAX_SIZE,
        IoWriteAccess,
        &ctx->ReportBuffer);
    if (!NT_SUCCESS(status)) {
        return status;
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
    PHID_XFER_PACKET xfer;

    *BytesWritten = 0;
    ctx = VirtioInputGetReadRequestContext(Request);

    xfer = (PHID_XFER_PACKET)ctx->XferPacket.SystemAddress;

    xfer->reportId = ReportId;

    if (ctx->ReportBufferLen < ReportSize) {
        xfer->reportBufferLen = 0;
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlCopyMemory(ctx->ReportBuffer.SystemAddress, Report, ReportSize);
    xfer->reportBufferLen = (ULONG)ReportSize;
    *BytesWritten = ReportSize;
    return STATUS_SUCCESS;
}

static BOOLEAN VirtioInputIsValidReportId(_In_ UCHAR ReportId)
{
    return (ReportId == VIRTIO_INPUT_REPORT_ID_KEYBOARD) || (ReportId == VIRTIO_INPUT_REPORT_ID_MOUSE) ||
           (ReportId == VIRTIO_INPUT_REPORT_ID_CONSUMER) || (ReportId == VIRTIO_INPUT_REPORT_ID_TABLET);
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
            if (reportLenHint == VIRTIO_INPUT_CONSUMER_INPUT_REPORT_SIZE) {
                return VIRTIO_INPUT_REPORT_ID_CONSUMER;
            }
            if (reportLenHint == VIRTIO_INPUT_MOUSE_INPUT_REPORT_SIZE) {
                return VIRTIO_INPUT_REPORT_ID_MOUSE;
            }
            if (reportLenHint == VIRTIO_INPUT_TABLET_INPUT_REPORT_SIZE) {
                return VIRTIO_INPUT_REPORT_ID_TABLET;
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

        haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[ReportId], &report);

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
            "READ_REPORT complete(%s): reportId=%u status=%!STATUS! bytes=%Iu readQ=%ld\n",
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
    RtlZeroMemory(devCtx->LastInputReportValid, sizeof(devCtx->LastInputReportValid));
    RtlZeroMemory(devCtx->LastInputReportLen, sizeof(devCtx->LastInputReportLen));
    RtlZeroMemory(devCtx->LastInputReport, sizeof(devCtx->LastInputReport));
    RtlZeroMemory(devCtx->InputReportSeq, sizeof(devCtx->InputReportSeq));
    RtlZeroMemory(devCtx->LastGetInputReportSeqNoFile, sizeof(devCtx->LastGetInputReportSeqNoFile));

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
        devCtx->LastInputReportValid[i] = FALSE;
        devCtx->LastInputReportLen[i] = 0;
        devCtx->InputReportSeq[i] = 0;
        devCtx->LastGetInputReportSeqNoFile[i] = 0;
    }
    VirtioInputPendingRingUpdateCountersLocked(devCtx);

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
        devCtx->LastInputReportValid[i] = FALSE;
        devCtx->LastInputReportLen[i] = 0;
        devCtx->InputReportSeq[i] = 0;
        devCtx->LastGetInputReportSeqNoFile[i] = 0;
    }
    VirtioInputPendingRingUpdateCountersLocked(devCtx);
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
        devCtx->LastInputReportValid[i] = FALSE;
        devCtx->LastInputReportLen[i] = 0;
        devCtx->InputReportSeq[i] = 0;
        devCtx->LastGetInputReportSeqNoFile[i] = 0;
    }
    VirtioInputPendingRingUpdateCountersLocked(devCtx);
    WdfSpinLockRelease(devCtx->ReadReportLock);

    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        WDFREQUEST request;
        while (NT_SUCCESS(WdfIoQueueRetrieveNextRequest(devCtx->ReadReportQueue[i], &request))) {
            VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
            VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);

            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT cancelled (stop): status=%!STATUS! readQ=%ld\n",
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

    if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
        if (ReportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD && ReportId != VIRTIO_INPUT_REPORT_ID_CONSUMER) {
            return STATUS_NOT_SUPPORTED;
        }
    } else if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
        if (ReportId != VIRTIO_INPUT_REPORT_ID_MOUSE) {
            return STATUS_NOT_SUPPORTED;
        }
    } else if (devCtx->DeviceKind == VioInputDeviceKindTablet) {
        if (ReportId != VIRTIO_INPUT_REPORT_ID_TABLET) {
            return STATUS_NOT_SUPPORTED;
        }
    }

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    if (!devCtx->ReadReportsEnabled) {
        WdfSpinLockRelease(devCtx->ReadReportLock);
        return STATUS_DEVICE_NOT_READY;
    }

    /*
     * Cache the most recent report for IOCTL_HID_GET_INPUT_REPORT polling.
     * Protected by ReadReportLock so we can safely copy the report + sequence
     * together.
     */
    devCtx->InputReportSeq[ReportId]++;
    devCtx->LastInputReportLen[ReportId] = (UCHAR)ReportSize;
    devCtx->LastInputReportValid[ReportId] = TRUE;
    RtlCopyMemory(devCtx->LastInputReport[ReportId], Report, ReportSize);

    VirtioInputPendingRingPush(devCtx, &devCtx->PendingReportRing[ReportId], Report, ReportSize);
    WdfSpinLockRelease(devCtx->ReadReportLock);

    VirtioInputDrainReadRequestsForReportId(Device, ReportId);

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputHandleHidGetInputReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t OutputBufferLength)
{
    WDFDEVICE device;
    PDEVICE_CONTEXT devCtx;
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT reqCtx;
    NTSTATUS status;
    UCHAR reportId;
    WDFFILEOBJECT fileObject;
    PVIRTIO_INPUT_FILE_CONTEXT fileCtx;
    ULONG lastSeq;
    ULONG currentSeq;
    struct virtio_input_report cached;
    BOOLEAN haveReport;
    size_t bytesWritten;

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

    /*
     * Determine the requested report ID.
     *
     * In practice some callers (e.g. HidD_GetInputReport) specify the report ID
     * by placing it in the first byte of the report buffer, so check both the
     * HID_XFER_PACKET and the buffer contents.
     */
    reportId = VIRTIO_INPUT_REPORT_ID_ANY;
    if (VirtioInputIsValidReportId(reqCtx->XferPacket->reportId)) {
        reportId = reqCtx->XferPacket->reportId;
    } else if (reqCtx->ReportBuffer != NULL && reqCtx->ReportBufferLen != 0 && VirtioInputIsValidReportId(reqCtx->ReportBuffer[0])) {
        reportId = reqCtx->ReportBuffer[0];
    } else {
        fileObject = WdfRequestGetFileObject(Request);
        if (fileObject != NULL) {
            fileCtx = VirtioInputGetFileContext(fileObject);
            if (VirtioInputIsValidReportId(fileCtx->DefaultReportId)) {
                reportId = fileCtx->DefaultReportId;
            }
        }
    }

    if (!VirtioInputIsValidReportId(reportId)) {
        if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            reportId = VIRTIO_INPUT_REPORT_ID_KEYBOARD;
        } else if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            reportId = VIRTIO_INPUT_REPORT_ID_MOUSE;
        }
    }

    if (!VirtioInputIsValidReportId(reportId)) {
        WdfWaitLockRelease(devCtx->ReadReportWaitLock);
        WdfRequestComplete(Request, STATUS_INVALID_PARAMETER);
        return STATUS_SUCCESS;
    }

    if (devCtx->DeviceKind == VioInputDeviceKindKeyboard && reportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD) {
        WdfWaitLockRelease(devCtx->ReadReportWaitLock);
        WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
        return STATUS_SUCCESS;
    }
    if (devCtx->DeviceKind == VioInputDeviceKindMouse && reportId != VIRTIO_INPUT_REPORT_ID_MOUSE) {
        WdfWaitLockRelease(devCtx->ReadReportWaitLock);
        WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
        return STATUS_SUCCESS;
    }

    fileObject = WdfRequestGetFileObject(Request);
    fileCtx = (fileObject != NULL) ? VirtioInputGetFileContext(fileObject) : NULL;

    haveReport = FALSE;
    RtlZeroMemory(&cached, sizeof(cached));

    lastSeq = (fileCtx != NULL) ? fileCtx->LastGetInputReportSeq[reportId] : devCtx->LastGetInputReportSeqNoFile[reportId];

    WdfSpinLockAcquire(devCtx->ReadReportLock);
    currentSeq = devCtx->InputReportSeq[reportId];

    if (devCtx->LastInputReportValid[reportId] && currentSeq != lastSeq) {
        cached.len = devCtx->LastInputReportLen[reportId];
        if (cached.len > 0 && cached.len <= VIRTIO_INPUT_REPORT_MAX_SIZE) {
            RtlCopyMemory(cached.data, devCtx->LastInputReport[reportId], cached.len);
            haveReport = TRUE;

            if (fileCtx == NULL) {
                devCtx->LastGetInputReportSeqNoFile[reportId] = currentSeq;
            }
        }
    }
    WdfSpinLockRelease(devCtx->ReadReportLock);

    WdfWaitLockRelease(devCtx->ReadReportWaitLock);

    if (haveReport && fileCtx != NULL) {
        /*
         * Update per-handle cursor outside of ReadReportLock so we don't touch
         * file-object context memory at elevated IRQL.
         */
        fileCtx->LastGetInputReportSeq[reportId] = currentSeq;
    }

    if (!haveReport) {
        /*
         * Do not pend IOCTL_HID_GET_INPUT_REPORT. If there has been no new input
         * report since the last poll, return STATUS_NO_DATA_DETECTED so user-mode
         * callers observe ERROR_NO_DATA rather than hanging.
         */
        reqCtx->XferPacket->reportId = reportId;
        reqCtx->XferPacket->reportBufferLen = 0;
        WdfRequestComplete(Request, STATUS_NO_DATA_DETECTED);
        return STATUS_SUCCESS;
    }

    bytesWritten = 0;
    status = VirtioInputFillPreparedReadRequest(Request, reportId, cached.data, cached.len, &bytesWritten);
    WdfRequestCompleteWithInformation(Request, status, bytesWritten);
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
    reportId = VirtioInputDetermineReadQueueReportId(Request, (HID_XFER_PACKET *)reqCtx->XferPacket.SystemAddress, OutputBufferLength);
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
        if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD].count != 0) {
                haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD], &report);
            } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_CONSUMER].count != 0) {
                haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_CONSUMER], &report);
            }
        } else if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE].count != 0) {
                haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE], &report);
            }
        } else if (devCtx->DeviceKind == VioInputDeviceKindTablet) {
            if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_TABLET].count != 0) {
                haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_TABLET], &report);
            }
        } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD].count != 0) {
            haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_KEYBOARD], &report);
        } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_CONSUMER].count != 0) {
            haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_CONSUMER], &report);
        } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE].count != 0) {
            haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_MOUSE], &report);
        } else if (devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_TABLET].count != 0) {
            haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[VIRTIO_INPUT_REPORT_ID_TABLET], &report);
        }
        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (haveReport) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            status = VirtioInputFillPreparedReadRequest(Request, report.data[0], report.data, report.len, &bytesWritten);
            WdfRequestCompleteWithInformation(Request, status, bytesWritten);

            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(pending): reportId=%u status=%!STATUS! bytes=%Iu readQ=%ld\n",
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
            "READ_REPORT pended(any): readQ=%ld txRing=%ld pendingRing=%ld\n",
            devCtx->Counters.ReadReportQueueDepth,
            devCtx->Counters.ReportRingDepth,
            devCtx->Counters.PendingRingDepth);

        WdfWaitLockRelease(devCtx->ReadReportWaitLock);

        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_KEYBOARD);
        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_CONSUMER);
        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_MOUSE);
        VirtioInputDrainReadRequestsForReportId(device, VIRTIO_INPUT_REPORT_ID_TABLET);
        return STATUS_SUCCESS;
    }

    {
        struct virtio_input_report report;
        BOOLEAN haveReport;
        size_t bytesWritten;

        haveReport = FALSE;
        bytesWritten = 0;

        WdfSpinLockAcquire(devCtx->ReadReportLock);
        haveReport = VirtioInputPendingRingPop(devCtx, &devCtx->PendingReportRing[reportId], &report);
        WdfSpinLockRelease(devCtx->ReadReportLock);

        if (haveReport) {
            WdfWaitLockRelease(devCtx->ReadReportWaitLock);

            status = VirtioInputFillPreparedReadRequest(Request, reportId, report.data, report.len, &bytesWritten);
            WdfRequestCompleteWithInformation(Request, status, bytesWritten);

            VioInputCounterInc(&devCtx->Counters.ReadReportCompleted);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT complete(pending): reportId=%u status=%!STATUS! bytes=%Iu readQ=%ld\n",
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
            "READ_REPORT pended: reportId=%u readQ=%ld txRing=%ld pendingRing=%ld\n",
            (ULONG)reportId,
            devCtx->Counters.ReadReportQueueDepth,
            devCtx->Counters.ReportRingDepth,
            devCtx->Counters.PendingRingDepth);

        WdfWaitLockRelease(devCtx->ReadReportWaitLock);

        VirtioInputDrainReadRequestsForReportId(device, reportId);
        return STATUS_SUCCESS;
    }
}

static VOID VirtioInputEvtReadRequestContextCleanup(_In_ WDFOBJECT Object)
{
    PVIRTIO_INPUT_READ_REQUEST_CONTEXT ctx;

    ctx = VirtioInputGetReadRequestContext(Object);

    VioInputMappedUserBufferCleanup(&ctx->ReportBuffer);
    VioInputMappedUserBufferCleanup(&ctx->XferPacket);
}
