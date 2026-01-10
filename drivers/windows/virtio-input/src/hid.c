#include "virtio_input.h"

#include "descriptor.h"

static NTSTATUS VirtioInputWriteRequestOutputBuffer(
    _In_ WDFREQUEST Request, _In_reads_bytes_(SourceLength) const void *Source, _In_ size_t SourceLength, _Out_ size_t *BytesWritten)
{
    void *outputBuffer;

    *BytesWritten = 0;

    NTSTATUS status = WdfRequestRetrieveOutputBuffer(Request, SourceLength, &outputBuffer, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlCopyMemory(outputBuffer, Source, SourceLength);
    *BytesWritten = SourceLength;
    return STATUS_SUCCESS;
}

static NTSTATUS VirtioInputWriteRequestOutputString(_In_ WDFREQUEST Request, _In_ PCWSTR SourceString, _Out_ size_t *BytesWritten)
{
    const WCHAR *p;
    size_t cch;
    size_t cb;

    if (SourceString == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    p = SourceString;
    while (*p != L'\0') {
        p++;
    }

    cch = (size_t)(p - SourceString) + 1;
    cb = cch * sizeof(WCHAR);

    return VirtioInputWriteRequestOutputBuffer(Request, SourceString, cb, BytesWritten);
}

NTSTATUS VirtioInputHandleHidIoctl(
    WDFQUEUE Queue, WDFREQUEST Request, size_t OutputBufferLength, size_t InputBufferLength, ULONG IoControlCode)
{
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);

    NTSTATUS status = STATUS_SUCCESS;
    size_t bytesReturned = 0;

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        status =
            VirtioInputWriteRequestOutputBuffer(Request, &VirtioInputHidDescriptor, sizeof(VirtioInputHidDescriptor), &bytesReturned);
        break;

    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        status = VirtioInputWriteRequestOutputBuffer(
            Request, VirtioInputReportDescriptor, VirtioInputReportDescriptorLength, &bytesReturned);
        break;

    case IOCTL_HID_GET_DEVICE_ATTRIBUTES: {
        HID_DEVICE_ATTRIBUTES attributes;
        RtlZeroMemory(&attributes, sizeof(attributes));
        attributes.Size = sizeof(attributes);
        attributes.VendorID = VIRTIO_INPUT_VID;
        attributes.ProductID = VIRTIO_INPUT_PID;
        attributes.VersionNumber = VIRTIO_INPUT_VERSION;

        status = VirtioInputWriteRequestOutputBuffer(Request, &attributes, sizeof(attributes), &bytesReturned);
        break;
    }

    case IOCTL_HID_GET_COLLECTION_INFORMATION: {
        HID_COLLECTION_INFORMATION info;
        RtlZeroMemory(&info, sizeof(info));

        info.DescriptorSize = VirtioInputReportDescriptorLength;
        info.Polled = FALSE;
        info.VendorID = VIRTIO_INPUT_VID;
        info.ProductID = VIRTIO_INPUT_PID;
        info.VersionNumber = VIRTIO_INPUT_VERSION;

        status = VirtioInputWriteRequestOutputBuffer(Request, &info, sizeof(info), &bytesReturned);
        break;
    }

    case IOCTL_HID_GET_STRING: {
        const ULONG *stringId;

        status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG), (void **)&stringId, NULL);
        if (!NT_SUCCESS(status)) {
            break;
        }

        switch (*stringId) {
        case HID_STRING_ID_IMANUFACTURER:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetManufacturerString(), &bytesReturned);
            break;
        case HID_STRING_ID_IPRODUCT:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetProductString(), &bytesReturned);
            break;
        case HID_STRING_ID_ISERIALNUMBER:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetSerialString(), &bytesReturned);
            break;
        default:
            status = STATUS_INVALID_PARAMETER;
            break;
        }

        break;
    }

    case IOCTL_HID_GET_INDEXED_STRING: {
        const ULONG *stringIndex;

        status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG), (void **)&stringIndex, NULL);
        if (!NT_SUCCESS(status)) {
            break;
        }

        switch (*stringIndex) {
        case 1:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetManufacturerString(), &bytesReturned);
            break;
        case 2:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetProductString(), &bytesReturned);
            break;
        case 3:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetSerialString(), &bytesReturned);
            break;
        default:
            status = STATUS_INVALID_PARAMETER;
            break;
        }

        break;
    }

    case IOCTL_HID_GET_POLL_FREQUENCY_MSEC: {
        const ULONG pollFrequencyMsec = 0;
        status = VirtioInputWriteRequestOutputBuffer(Request, &pollFrequencyMsec, sizeof(pollFrequencyMsec), &bytesReturned);
        break;
    }

    case IOCTL_HID_SET_POLL_FREQUENCY_MSEC: {
        bytesReturned = 0;
        status = STATUS_SUCCESS;
        break;
    }

    case IOCTL_HID_ACTIVATE_DEVICE:
    case IOCTL_HID_DEACTIVATE_DEVICE:
        status = STATUS_SUCCESS;
        bytesReturned = 0;
        break;

    default:
        status = STATUS_NOT_SUPPORTED;
        bytesReturned = 0;
        break;
    }

    WdfRequestCompleteWithInformation(Request, status, bytesReturned);
    return STATUS_SUCCESS;
}
