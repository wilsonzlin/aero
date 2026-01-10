#include <ntddk.h>
#include <wdf.h>

#include <hidport.h>

#include "virtio_statusq.h"

typedef struct _VIOINPUT_DEVICE_CONTEXT {
    PVIRTIO_STATUSQ StatusQ;
} VIOINPUT_DEVICE_CONTEXT, *PVIOINPUT_DEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIOINPUT_DEVICE_CONTEXT, VioInputGetDeviceContext);

static NTSTATUS
VioInputParseKeyboardLedReport(_In_ const HID_XFER_PACKET* Packet, _Out_ UCHAR* LedBitfield)
{
    PUCHAR buf;

    if (Packet == NULL || LedBitfield == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Packet->reportBuffer == NULL || Packet->reportBufferLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    buf = Packet->reportBuffer;

    if (Packet->reportId == 1) {
        if (Packet->reportBufferLen >= 2 && buf[0] == 1) {
            *LedBitfield = buf[1];
            return STATUS_SUCCESS;
        }

        *LedBitfield = buf[0];
        return STATUS_SUCCESS;
    }

    if (Packet->reportId == 0 && Packet->reportBufferLen >= 2 && buf[0] == 1) {
        *LedBitfield = buf[1];
        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

_Use_decl_annotations_
VOID VioInputEvtIoInternalDeviceControl(
    WDFQUEUE Queue,
    WDFREQUEST Request,
    size_t OutputBufferLength,
    size_t InputBufferLength,
    ULONG IoControlCode)
{
    WDFDEVICE device;
    PVIOINPUT_DEVICE_CONTEXT ctx;
    HID_XFER_PACKET* packet;
    size_t packetBytes;
    UCHAR ledBitfield;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    device = WdfIoQueueGetDevice(Queue);
    ctx = VioInputGetDeviceContext(device);

    if (IoControlCode != IOCTL_HID_WRITE_REPORT) {
        WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
        return;
    }

    if (WdfDeviceGetDevicePowerState(device) != WdfDevicePowerD0) {
        WdfRequestComplete(Request, STATUS_DEVICE_NOT_READY);
        return;
    }

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(*packet), (PVOID*)&packet, &packetBytes);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return;
    }
    UNREFERENCED_PARAMETER(packetBytes);

    status = VioInputParseKeyboardLedReport(packet, &ledBitfield);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return;
    }

    if (ctx->StatusQ == NULL) {
        WdfRequestComplete(Request, STATUS_DEVICE_NOT_READY);
        return;
    }

    status = VirtioStatusQWriteKeyboardLedReport(ctx->StatusQ, ledBitfield);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return;
    }

    WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, packet->reportBufferLen);
}
