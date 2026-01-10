#include "virtio_input.h"

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
    PCSTR name = VioInputHidIoctlToString(IOCTL_HID_WRITE_REPORT);

    HID_XFER_PACKET *packet = NULL;
    size_t packetBytes = 0;
    NTSTATUS status = WdfRequestRetrieveInputBuffer(Request, sizeof(*packet), (PVOID *)&packet, &packetBytes);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s input buffer retrieve failed: %!STATUS!\n", name, status);
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

    UCHAR reportId = VirtioInputDetermineWriteReportId(Request, packet);
    if (reportId != VIRTIO_INPUT_REPORT_ID_KEYBOARD) {
        VIOINPUT_LOG(
            VIOINPUT_LOG_IOCTL,
            "%s ignored: reportId=%u bytes=%lu\n",
            name,
            (ULONG)reportId,
            packet->reportBufferLen);
        WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, packet->reportBufferLen);
        return STATUS_SUCCESS;
    }

    UCHAR ledBitfield = 0;
    status = VirtioInputParseKeyboardLedReport(packet, reportId, &ledBitfield);
    if (!NT_SUCCESS(status)) {
        VIOINPUT_LOG(VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL, "%s parse failed: %!STATUS!\n", name, status);
        WdfRequestComplete(Request, status);
        return STATUS_SUCCESS;
    }

    if (ctx->StatusQ != NULL) {
        status = VirtioStatusQWriteKeyboardLedReport(ctx->StatusQ, ledBitfield);
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

    VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "%s -> %!STATUS! bytes=%lu\n", name, STATUS_SUCCESS, packet->reportBufferLen);
    WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, packet->reportBufferLen);
    return STATUS_SUCCESS;
}
