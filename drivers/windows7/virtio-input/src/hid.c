#include "virtio_input.h"

#include "descriptor.h"

static NTSTATUS VirtioInputMapUserAddress(
    _In_ PVOID UserAddress,
    _In_ SIZE_T Length,
    _In_ LOCK_OPERATION Operation,
    _Outptr_ PMDL *MdlOut,
    _Outptr_result_bytebuffer_(Length) PVOID *SystemAddressOut
)
{
    PMDL mdl;
    PVOID systemAddress;

    if (UserAddress == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Length > (SIZE_T)MAXULONG) {
        return STATUS_INVALID_PARAMETER;
    }

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

static NTSTATUS VirtioInputReadRequestInputUlong(_In_ WDFREQUEST Request, _Out_ ULONG *ValueOut)
{
    NTSTATUS status;
    ULONG *userPtr;
    size_t len;
    KPROCESSOR_MODE requestorMode;

    if (ValueOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *ValueOut = 0;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG), (PVOID *)&userPtr, &len);
    if (!NT_SUCCESS(status) || len < sizeof(ULONG)) {
        return STATUS_INVALID_PARAMETER;
    }

    requestorMode = WdfRequestGetRequestorMode(Request);
    if (requestorMode == UserMode) {
        PMDL mdl;
        ULONG *systemPtr;

        mdl = NULL;
        systemPtr = NULL;
        status = VirtioInputMapUserAddress(userPtr, sizeof(ULONG), IoReadAccess, &mdl, (PVOID *)&systemPtr);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        *ValueOut = *systemPtr;

        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
        return STATUS_SUCCESS;
    }

    *ValueOut = *userPtr;
    return STATUS_SUCCESS;
}

static NTSTATUS VirtioInputWriteRequestOutputBuffer(
    _In_ WDFREQUEST Request, _In_reads_bytes_(SourceLength) const void *Source, _In_ size_t SourceLength, _Out_ size_t *BytesWritten)
{
    void *outputBuffer;
    size_t outputLength;

    *BytesWritten = 0;

    NTSTATUS status = WdfRequestRetrieveOutputBuffer(Request, SourceLength, &outputBuffer, &outputLength);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (outputLength < SourceLength) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    if (WdfRequestGetRequestorMode(Request) == UserMode) {
        PMDL mdl;
        PVOID systemAddress;

        mdl = NULL;
        systemAddress = NULL;
        status = VirtioInputMapUserAddress(outputBuffer, SourceLength, IoWriteAccess, &mdl, &systemAddress);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        RtlCopyMemory(systemAddress, Source, SourceLength);

        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
    } else {
        RtlCopyMemory(outputBuffer, Source, SourceLength);
    }
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

    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    NTSTATUS status = STATUS_SUCCESS;
    size_t bytesReturned = 0;

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
        if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            status = VirtioInputWriteRequestOutputBuffer(
                Request, &VirtioInputKeyboardHidDescriptor, sizeof(VirtioInputKeyboardHidDescriptor), &bytesReturned);
        } else if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            status = VirtioInputWriteRequestOutputBuffer(
                Request, &VirtioInputMouseHidDescriptor, sizeof(VirtioInputMouseHidDescriptor), &bytesReturned);
        } else {
            status = STATUS_DEVICE_NOT_READY;
        }
        break;

    case IOCTL_HID_GET_REPORT_DESCRIPTOR:
        if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            status = VirtioInputWriteRequestOutputBuffer(
                Request, VirtioInputKeyboardReportDescriptor, VirtioInputKeyboardReportDescriptorLength, &bytesReturned);
        } else if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            status = VirtioInputWriteRequestOutputBuffer(
                Request, VirtioInputMouseReportDescriptor, VirtioInputMouseReportDescriptorLength, &bytesReturned);
        } else {
            status = STATUS_DEVICE_NOT_READY;
        }
        break;

    case IOCTL_HID_GET_DEVICE_ATTRIBUTES: {
        HID_DEVICE_ATTRIBUTES attributes;
        RtlZeroMemory(&attributes, sizeof(attributes));
        attributes.Size = sizeof(attributes);
        attributes.VendorID = VIRTIO_INPUT_VID;
        attributes.ProductID =
            (devCtx->DeviceKind == VioInputDeviceKindMouse) ? VIRTIO_INPUT_PID_MOUSE : VIRTIO_INPUT_PID_KEYBOARD;
        attributes.VersionNumber = VIRTIO_INPUT_VERSION;

        status = VirtioInputWriteRequestOutputBuffer(Request, &attributes, sizeof(attributes), &bytesReturned);
        break;
    }

    case IOCTL_HID_GET_COLLECTION_INFORMATION: {
        HID_COLLECTION_INFORMATION info;
        RtlZeroMemory(&info, sizeof(info));

        info.DescriptorSize =
            (devCtx->DeviceKind == VioInputDeviceKindMouse) ? VirtioInputMouseReportDescriptorLength : VirtioInputKeyboardReportDescriptorLength;
        info.Polled = FALSE;
        info.VendorID = VIRTIO_INPUT_VID;
        info.ProductID =
            (devCtx->DeviceKind == VioInputDeviceKindMouse) ? VIRTIO_INPUT_PID_MOUSE : VIRTIO_INPUT_PID_KEYBOARD;
        info.VersionNumber = VIRTIO_INPUT_VERSION;

        status = VirtioInputWriteRequestOutputBuffer(Request, &info, sizeof(info), &bytesReturned);
        break;
    }

    case IOCTL_HID_GET_STRING: {
        ULONG stringId;
        status = VirtioInputReadRequestInputUlong(Request, &stringId);
        if (!NT_SUCCESS(status)) {
            break;
        }

        switch (stringId) {
        case HID_STRING_ID_IMANUFACTURER:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetManufacturerString(), &bytesReturned);
            break;
        case HID_STRING_ID_IPRODUCT:
            status = VirtioInputWriteRequestOutputString(
                Request,
                (devCtx->DeviceKind == VioInputDeviceKindMouse) ? VirtioInputGetMouseProductString() : VirtioInputGetKeyboardProductString(),
                &bytesReturned);
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
        ULONG stringIndex;
        status = VirtioInputReadRequestInputUlong(Request, &stringIndex);
        if (!NT_SUCCESS(status)) {
            break;
        }

        switch (stringIndex) {
        case 1:
            status = VirtioInputWriteRequestOutputString(Request, VirtioInputGetManufacturerString(), &bytesReturned);
            break;
        case 2:
            status = VirtioInputWriteRequestOutputString(
                Request,
                (devCtx->DeviceKind == VioInputDeviceKindMouse) ? VirtioInputGetMouseProductString() : VirtioInputGetKeyboardProductString(),
                &bytesReturned);
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
