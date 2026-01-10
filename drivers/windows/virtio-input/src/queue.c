#include "virtio_input.h"

static const UCHAR g_VirtioInputReportDescriptor[] = {
    // Keyboard collection (Report ID 1). Boot keyboard compatible: modifiers, reserved, 6 keys.
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x85, 0x01, //   Report ID (1)
    0x05, 0x07, //   Usage Page (Keyboard)
    0x19, 0xE0, //   Usage Minimum (Left Control)
    0x29, 0xE7, //   Usage Maximum (Right GUI)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data,Var,Abs) - modifiers
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Const) - reserved
    0x05, 0x08, //   Usage Page (LEDs)
    0x19, 0x01, //   Usage Minimum (Num Lock)
    0x29, 0x05, //   Usage Maximum (Kana)
    0x95, 0x05, //   Report Count (5)
    0x75, 0x01, //   Report Size (1)
    0x91, 0x02, //   Output (Data,Var,Abs) - LED bitfield
    0x95, 0x01, //   Report Count (1)
    0x75, 0x03, //   Report Size (3)
    0x91, 0x01, //   Output (Const) - padding
    0x05, 0x07, //   Usage Page (Keyboard)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x06, //   Report Count (6)
    0x81, 0x00, //   Input (Data,Array) - keys
    0xC0,       // End Collection

    // Mouse collection (Report ID 2).
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xA1, 0x01, // Collection (Application)
    0x85, 0x02, //   Report ID (2)
    0x09, 0x01, //   Usage (Pointer)
    0xA1, 0x00, //   Collection (Physical)
    0x05, 0x09, //     Usage Page (Button)
    0x19, 0x01, //     Usage Minimum (Button 1)
    0x29, 0x05, //     Usage Maximum (Button 5)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x75, 0x01, //     Report Size (1)
    0x95, 0x05, //     Report Count (5)
    0x81, 0x02, //     Input (Data,Var,Abs) - buttons
    0x75, 0x03, //     Report Size (3)
    0x95, 0x01, //     Report Count (1)
    0x81, 0x01, //     Input (Const) - padding
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x38, //     Usage (Wheel)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x06, //     Input (Data,Var,Rel) - X/Y/Wheel
    0xC0,       //   End Collection
    0xC0        // End Collection
};

static const HID_DESCRIPTOR g_VirtioInputHidDescriptor = {
    (UCHAR)sizeof(HID_DESCRIPTOR), // bLength
    (UCHAR)HID_HID_DESCRIPTOR_TYPE, // bDescriptorType
    HID_REVISION, // bcdHID
    0x00, // bCountry
    0x01, // bNumDescriptors
    {
        (UCHAR)HID_REPORT_DESCRIPTOR_TYPE,
        (USHORT)sizeof(g_VirtioInputReportDescriptor),
    },
};

static const HID_DEVICE_ATTRIBUTES g_VirtioInputAttributes = {
    (ULONG)sizeof(HID_DEVICE_ATTRIBUTES), // Size
    (USHORT)0x1AF4, // VendorID (virtio)
    (USHORT)0x1052, // ProductID (virtio-input, modern-only PCI ID)
    (USHORT)0x0001, // VersionNumber
};

static VOID VioInputCountHidIoctl(_Inout_ PVIOINPUT_COUNTERS Counters, _In_ ULONG IoControlCode)
{
    VioInputCounterInc(&Counters->IoctlTotal);

    switch (IoControlCode) {
        case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
            VioInputCounterInc(&Counters->IoctlHidGetDeviceDescriptor);
            break;
        case IOCTL_HID_GET_REPORT_DESCRIPTOR:
            VioInputCounterInc(&Counters->IoctlHidGetReportDescriptor);
            break;
        case IOCTL_HID_GET_DEVICE_ATTRIBUTES:
            VioInputCounterInc(&Counters->IoctlHidGetDeviceAttributes);
            break;
#ifdef IOCTL_HID_GET_COLLECTION_INFORMATION
        case IOCTL_HID_GET_COLLECTION_INFORMATION:
            VioInputCounterInc(&Counters->IoctlHidGetCollectionInformation);
            break;
#endif
#ifdef IOCTL_HID_GET_COLLECTION_DESCRIPTOR
        case IOCTL_HID_GET_COLLECTION_DESCRIPTOR:
            VioInputCounterInc(&Counters->IoctlHidGetCollectionDescriptor);
            break;
#endif
#ifdef IOCTL_HID_FLUSH_QUEUE
        case IOCTL_HID_FLUSH_QUEUE:
            VioInputCounterInc(&Counters->IoctlHidFlushQueue);
            break;
#endif
        case IOCTL_HID_GET_STRING:
            VioInputCounterInc(&Counters->IoctlHidGetString);
            break;
        case IOCTL_HID_GET_INDEXED_STRING:
            VioInputCounterInc(&Counters->IoctlHidGetIndexedString);
            break;
        case IOCTL_HID_GET_FEATURE:
            VioInputCounterInc(&Counters->IoctlHidGetFeature);
            break;
        case IOCTL_HID_SET_FEATURE:
            VioInputCounterInc(&Counters->IoctlHidSetFeature);
            break;
#ifdef IOCTL_HID_GET_INPUT_REPORT
        case IOCTL_HID_GET_INPUT_REPORT:
            VioInputCounterInc(&Counters->IoctlHidGetInputReport);
            break;
#endif
#ifdef IOCTL_HID_SET_OUTPUT_REPORT
        case IOCTL_HID_SET_OUTPUT_REPORT:
            VioInputCounterInc(&Counters->IoctlHidSetOutputReport);
            break;
#endif
        case IOCTL_HID_READ_REPORT:
            VioInputCounterInc(&Counters->IoctlHidReadReport);
            break;
        case IOCTL_HID_WRITE_REPORT:
            VioInputCounterInc(&Counters->IoctlHidWriteReport);
            break;
        default:
            VioInputCounterInc(&Counters->IoctlUnknown);
            break;
    }
}

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device)
{
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDFQUEUE queue;
    PDEVICE_CONTEXT deviceContext;
    NTSTATUS status;

    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoInternalDeviceControl = VirtioInputEvtIoInternalDeviceControl;
    queueConfig.EvtIoDeviceControl = VirtioInputEvtIoDeviceControl;

    status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    deviceContext = VirtioInputGetDeviceContext(Device);
    deviceContext->DefaultQueue = queue;

    return STATUS_SUCCESS;
}

VOID VirtioInputEvtIoInternalDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);
    PCSTR name = VioInputHidIoctlToString(IoControlCode);
    NTSTATUS status;
    size_t bytesReturned;

    bytesReturned = 0;
    status = STATUS_NOT_SUPPORTED;

    VioInputCountHidIoctl(&devCtx->Counters, IoControlCode);

    VIOINPUT_LOG(
        VIOINPUT_LOG_IOCTL,
        "IOCTL %s (0x%08X) in=%Iu out=%Iu ring=%ld pending=%ld\n",
        name,
        IoControlCode,
        InputBufferLength,
        OutputBufferLength,
        devCtx->Counters.ReportRingDepth,
        devCtx->Counters.ReadReportQueueDepth);

    switch (IoControlCode) {
    case IOCTL_HID_GET_DEVICE_DESCRIPTOR: {
        PHID_DESCRIPTOR desc;
        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DESCRIPTOR), (PVOID*)&desc, NULL);
        if (NT_SUCCESS(status)) {
            RtlCopyMemory(desc, &g_VirtioInputHidDescriptor, sizeof(HID_DESCRIPTOR));
            bytesReturned = sizeof(HID_DESCRIPTOR);
        }
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=%Iu\n", name, status, bytesReturned);
        WdfRequestCompleteWithInformation(Request, status, bytesReturned);
        return;
    }
    case IOCTL_HID_GET_REPORT_DESCRIPTOR: {
        PUCHAR desc;
        status = WdfRequestRetrieveOutputBuffer(
            Request, sizeof(g_VirtioInputReportDescriptor), (PVOID*)&desc, NULL);
        if (NT_SUCCESS(status)) {
            RtlCopyMemory(desc, g_VirtioInputReportDescriptor, sizeof(g_VirtioInputReportDescriptor));
            bytesReturned = sizeof(g_VirtioInputReportDescriptor);
        }
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=%Iu\n", name, status, bytesReturned);
        WdfRequestCompleteWithInformation(Request, status, bytesReturned);
        return;
    }
    case IOCTL_HID_GET_DEVICE_ATTRIBUTES: {
        PHID_DEVICE_ATTRIBUTES attrs;
        status =
            WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES), (PVOID*)&attrs, NULL);
        if (NT_SUCCESS(status)) {
            RtlCopyMemory(attrs, &g_VirtioInputAttributes, sizeof(HID_DEVICE_ATTRIBUTES));
            bytesReturned = sizeof(HID_DEVICE_ATTRIBUTES);
        }
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=%Iu\n", name, status, bytesReturned);
        WdfRequestCompleteWithInformation(Request, status, bytesReturned);
        return;
    }
    case IOCTL_HID_READ_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (read report handler)\n", name);
        (VOID)VirtioInputHandleHidReadReport(Queue, Request, OutputBufferLength);
        return;
    case IOCTL_HID_WRITE_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (write report handler)\n", name);
        (VOID)VirtioInputHandleHidWriteReport(Queue, Request, InputBufferLength);
        return;
    case IOCTL_HID_ACTIVATE_DEVICE:
    case IOCTL_HID_DEACTIVATE_DEVICE:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_SUCCESS);
        WdfRequestComplete(Request, STATUS_SUCCESS);
        return;
    default:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_NOT_SUPPORTED);
        WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
        return;
    }
}

VOID VirtioInputEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);
    NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
    size_t info = 0;

    UNREFERENCED_PARAMETER(InputBufferLength);

    switch (IoControlCode) {
    case IOCTL_VIOINPUT_QUERY_COUNTERS: {
        PVIOINPUT_COUNTERS outCounters;
        size_t outBytes;
        VIOINPUT_COUNTERS snapshot;

        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(*outCounters), (PVOID *)&outCounters, &outBytes);
        if (!NT_SUCCESS(status)) {
            break;
        }

        if (OutputBufferLength < sizeof(*outCounters) || outBytes < sizeof(*outCounters)) {
            status = STATUS_BUFFER_TOO_SMALL;
            break;
        }

        VioInputCountersSnapshot(&devCtx->Counters, &snapshot);
        RtlCopyMemory(outCounters, &snapshot, sizeof(snapshot));
        status = STATUS_SUCCESS;
        info = sizeof(snapshot);
        break;
    }
    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        info = 0;
        break;
    }

    VIOINPUT_LOG(
        VIOINPUT_LOG_IOCTL,
        "DEVICE_IOCTL 0x%08X -> %!STATUS! bytes=%Iu\n",
        IoControlCode,
        status,
        info);
    WdfRequestCompleteWithInformation(Request, status, info);
}
