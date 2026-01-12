#include "virtio_input.h"

typedef struct _VIRTIO_INPUT_WRITE_REQUEST_CONTEXT {
    PHID_XFER_PACKET XferPacket;
    PMDL XferPacketMdl;

    PUCHAR ReportBufferUser;
    PUCHAR ReportBuffer;
    PMDL ReportBufferMdl;
    ULONG ReportBufferLen;
} VIRTIO_INPUT_WRITE_REQUEST_CONTEXT, *PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_INPUT_WRITE_REQUEST_CONTEXT, VirtioInputGetWriteRequestContext);

static EVT_WDF_OBJECT_CONTEXT_CLEANUP VirtioInputEvtWriteRequestContextCleanup;

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

static NTSTATUS VirtioInputPrepareWriteRequest(
    _In_ WDFREQUEST Request,
    _In_ HID_XFER_PACKET *Packet,
    _Outptr_ const HID_XFER_PACKET **MappedPacketOut,
    _Outptr_opt_ const UCHAR **MappedReportBufferOut
)
{
    NTSTATUS status;
    PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT ctx;
    WDF_OBJECT_ATTRIBUTES attributes;

    KPROCESSOR_MODE requestorMode;
    const HID_XFER_PACKET *xfer;

    requestorMode = WdfRequestGetRequestorMode(Request);
    if (requestorMode != UserMode) {
        *MappedPacketOut = Packet;
        *MappedReportBufferOut = Packet->reportBuffer;
        return STATUS_SUCCESS;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, VIRTIO_INPUT_WRITE_REQUEST_CONTEXT);
    attributes.EvtCleanupCallback = VirtioInputEvtWriteRequestContextCleanup;

    status = WdfObjectAllocateContext(Request, &attributes, (PVOID *)&ctx);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(ctx, sizeof(*ctx));

    status = VirtioInputMapUserAddress(Packet, sizeof(HID_XFER_PACKET), IoReadAccess, &ctx->XferPacketMdl, (PVOID *)&ctx->XferPacket);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    xfer = ctx->XferPacket;
    ctx->ReportBufferUser = xfer->reportBuffer;
    ctx->ReportBufferLen = xfer->reportBufferLen;

    *MappedPacketOut = ctx->XferPacket;
    *MappedReportBufferOut = NULL;
    return STATUS_SUCCESS;
}

static NTSTATUS VirtioInputMapWriteReportBuffer(_In_ WDFREQUEST Request, _Outptr_ const UCHAR **MappedReportBufferOut)
{
    PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT ctx;
    NTSTATUS status;
    SIZE_T mapLen;

    ctx = VirtioInputGetWriteRequestContext(Request);

    if (ctx->ReportBufferMdl != NULL) {
        *MappedReportBufferOut = ctx->ReportBuffer;
        return STATUS_SUCCESS;
    }

    if (ctx->ReportBufferUser == NULL || ctx->ReportBufferLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    mapLen = ctx->ReportBufferLen;
    if (mapLen > 2) {
        mapLen = 2;
    }

    status = VirtioInputMapUserAddress(
        ctx->ReportBufferUser,
        mapLen,
        IoReadAccess,
        &ctx->ReportBufferMdl,
        (PVOID *)&ctx->ReportBuffer);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    *MappedReportBufferOut = ctx->ReportBuffer;
    return STATUS_SUCCESS;
}

static BOOLEAN VirtioInputIsValidReportId(_In_ UCHAR ReportId)
{
    return (ReportId == VIRTIO_INPUT_REPORT_ID_KEYBOARD) || (ReportId == VIRTIO_INPUT_REPORT_ID_MOUSE);
}

static UCHAR VirtioInputDetermineWriteReportId(_In_ WDFREQUEST Request, _In_opt_ const HID_XFER_PACKET *Packet)
{
    if (Packet != NULL && VirtioInputIsValidReportId(Packet->reportId)) {
        return Packet->reportId;
    }

    WDFFILEOBJECT fileObject = WdfRequestGetFileObject(Request);
    if (fileObject != NULL) {
        PVIRTIO_INPUT_FILE_CONTEXT fileCtx = VirtioInputGetFileContext(fileObject);
        if (VirtioInputIsValidReportId(fileCtx->DefaultReportId)) {
            return fileCtx->DefaultReportId;
        }
    }

    if (Packet != NULL && Packet->reportBuffer != NULL && Packet->reportBufferLen > 0) {
        const UCHAR *buf = (const UCHAR *)Packet->reportBuffer;
        if (VirtioInputIsValidReportId(buf[0])) {
            return buf[0];
        }
    }

    return VIRTIO_INPUT_REPORT_ID_ANY;
}

static NTSTATUS VirtioInputParseKeyboardLedReport(_In_ const HID_XFER_PACKET *Packet, _In_ UCHAR ReportId, _Out_ UCHAR *LedBitfield)
{
    if (Packet == NULL || LedBitfield == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Packet->reportBuffer == NULL || Packet->reportBufferLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ReportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD) {
        return STATUS_NOT_SUPPORTED;
    }

    const UCHAR *buf = (const UCHAR *)Packet->reportBuffer;

    if (Packet->reportBufferLen >= 2 && buf[0] == ReportId) {
        *LedBitfield = buf[1];
        return STATUS_SUCCESS;
    }

    *LedBitfield = buf[0];
    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputHandleHidWriteReport(_In_ WDFQUEUE Queue, _In_ WDFREQUEST Request, _In_ size_t InputBufferLength)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT ctx = VirtioInputGetDeviceContext(device);
    WDF_REQUEST_PARAMETERS params;
    PCSTR name;

    WDF_REQUEST_PARAMETERS_INIT(&params);
    WdfRequestGetParameters(Request, &params);
    {
        ULONG ioctl;

        ioctl = IOCTL_HID_WRITE_REPORT;
        if ((params.Type == WdfRequestTypeDeviceControlInternal) || (params.Type == WdfRequestTypeDeviceControl)) {
            ioctl = params.Parameters.DeviceIoControl.IoControlCode;
        }

        name = VioInputHidIoctlToString(ioctl);
    }

    HID_XFER_PACKET *packet = NULL;
    size_t packetBytes = 0;
    NTSTATUS status = WdfRequestRetrieveInputBuffer(Request, sizeof(*packet), (PVOID *)&packet, &packetBytes);
    if (!NT_SUCCESS(status)) {
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(*packet), (PVOID *)&packet, &packetBytes);
    }
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s transfer packet retrieve failed: %!STATUS!\n", name, status);
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

    UNREFERENCED_PARAMETER(packetBytes);
    UNREFERENCED_PARAMETER(InputBufferLength);

    if (!VirtioInputIsHidActive(ctx) || WdfDeviceGetDevicePowerState(device) != WdfDevicePowerD0) {
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "%s -> %!STATUS!\n", name, STATUS_DEVICE_NOT_READY);
        WdfRequestComplete(Request, STATUS_DEVICE_NOT_READY);
        return STATUS_SUCCESS;
    }

    const HID_XFER_PACKET *mappedPacket;
    const UCHAR *mappedReportBuffer;
    HID_XFER_PACKET safePacket;

    mappedPacket = NULL;
    mappedReportBuffer = NULL;
    status = VirtioInputPrepareWriteRequest(Request, packet, &mappedPacket, &mappedReportBuffer);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s map user buffers failed: %!STATUS!\n", name, status);
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

    safePacket = *mappedPacket;
    if (WdfRequestGetRequestorMode(Request) == UserMode) {
        PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT reqCtx = VirtioInputGetWriteRequestContext(Request);
        safePacket.reportBufferLen = reqCtx->ReportBufferLen;
        safePacket.reportBuffer = NULL;
    } else {
        safePacket.reportBuffer = (PUCHAR)mappedReportBuffer;
    }

    UCHAR reportId = VirtioInputDetermineWriteReportId(Request, &safePacket);
    if (reportId == VIRTIO_INPUT_REPORT_ID_ANY && safePacket.reportBuffer == NULL &&
        WdfRequestGetRequestorMode(Request) == UserMode) {
        PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT reqCtx = VirtioInputGetWriteRequestContext(Request);
        if (reqCtx->ReportBufferUser != NULL && reqCtx->ReportBufferLen > 0) {
            status = VirtioInputMapWriteReportBuffer(Request, &mappedReportBuffer);
            if (!NT_SUCCESS(status)) {
                VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s map report buffer failed: %!STATUS!\n", name, status);
                WdfRequestComplete(Request, status);
                return STATUS_SUCCESS;
            }
            safePacket.reportBuffer = (PUCHAR)mappedReportBuffer;
            reportId = VirtioInputDetermineWriteReportId(Request, &safePacket);
        }
    }
    if (reportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_IOCTL,
            "%s ignored: reportId=%u bytes=%lu\n",
            name,
            (ULONG)reportId,
            safePacket.reportBufferLen);
        WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, safePacket.reportBufferLen);
        return STATUS_SUCCESS;
    }

    if (safePacket.reportBuffer == NULL && WdfRequestGetRequestorMode(Request) == UserMode) {
        status = VirtioInputMapWriteReportBuffer(Request, &mappedReportBuffer);
        if (!NT_SUCCESS(status)) {
            VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s map report buffer failed: %!STATUS!\n", name, status);
            WdfRequestComplete(Request, status);
            return STATUS_SUCCESS;
        }
        safePacket.reportBuffer = (PUCHAR)mappedReportBuffer;
    }

    UCHAR ledBitfield = 0;
    status = VirtioInputParseKeyboardLedReport(&safePacket, reportId, &ledBitfield);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s parse failed: %!STATUS!\n", name, status);
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

    if (ctx->StatusQ != NULL) {
        if (ctx->Interrupts.QueueLocks != NULL && ctx->Interrupts.QueueCount > 1) {
            WdfSpinLockAcquire(ctx->Interrupts.QueueLocks[1]);
            status = VirtioStatusQWriteKeyboardLedReport(ctx->StatusQ, ledBitfield);
            WdfSpinLockRelease(ctx->Interrupts.QueueLocks[1]);
        } else {
            status = VirtioStatusQWriteKeyboardLedReport(ctx->StatusQ, ledBitfield);
        }
        if (!NT_SUCCESS(status)) {
            /*
             * LED reports are not required for keyboard/mouse input to function.
             * Do not fail the write path if the status queue is not wired up yet
             * or if the device rejects the update.
             */
            VIOINPUT_LOG(
                VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL,
                "%s StatusQ write failed (ignored): %!STATUS!\n",
                name,
                status);
        }
    } else {
        VIOINPUT_LOG(
            VIOINPUT_LOG_VERBOSE | VIOINPUT_LOG_IOCTL,
            "%s dropping LED report (no StatusQ): leds=0x%02X\n",
            name,
            (ULONG)ledBitfield);
    }

    VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "%s -> %!STATUS! bytes=%lu\n", name, STATUS_SUCCESS, safePacket.reportBufferLen);
    WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, safePacket.reportBufferLen);
    return STATUS_SUCCESS;
}

static VOID VirtioInputEvtWriteRequestContextCleanup(_In_ WDFOBJECT Object)
{
    PVIRTIO_INPUT_WRITE_REQUEST_CONTEXT ctx;

    ctx = VirtioInputGetWriteRequestContext(Object);

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
