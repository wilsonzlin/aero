#include "virtio_input.h"

#ifdef IOCTL_HID_SET_NUM_DEVICE_INPUT_BUFFERS

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

#endif

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
    case IOCTL_HID_READ_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (read report handler)\n", name);
        (VOID)VirtioInputHandleHidReadReport(Queue, Request, OutputBufferLength);
        return;
    case IOCTL_HID_WRITE_REPORT:
    case IOCTL_HID_SET_OUTPUT_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (write report handler)\n", name);
        (VOID)VirtioInputHandleHidWriteReport(Queue, Request, InputBufferLength);
        return;
    case IOCTL_HID_ACTIVATE_DEVICE:
        status = VirtioInputHidActivateDevice(device);
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, status);
        WdfRequestComplete(Request, status);
        return;
    case IOCTL_HID_DEACTIVATE_DEVICE:
        status = VirtioInputHidDeactivateDevice(device);
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, status);
        WdfRequestComplete(Request, status);
        return;
#ifdef IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST
    case IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_NOT_SUPPORTED);
        WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
        return;
#endif
#ifdef IOCTL_HID_FLUSH_QUEUE
    case IOCTL_HID_FLUSH_QUEUE:
        VirtioInputHidFlushQueue(device);
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_SUCCESS);
        WdfRequestComplete(Request, STATUS_SUCCESS);
        return;
#endif
#ifdef IOCTL_HID_SET_NUM_DEVICE_INPUT_BUFFERS
    case IOCTL_HID_SET_NUM_DEVICE_INPUT_BUFFERS: {
        ULONG numBuffers;

        if (InputBufferLength >= sizeof(ULONG) && NT_SUCCESS(VirtioInputReadRequestInputUlong(Request, &numBuffers))) {
            devCtx->NumDeviceInputBuffers = numBuffers;
        }

        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_SUCCESS);
        WdfRequestComplete(Request, STATUS_SUCCESS);
        return;
    }
#endif
    default:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (generic handler)\n", name);
        (VOID)VirtioInputHandleHidIoctl(Queue, Request, OutputBufferLength, InputBufferLength, IoControlCode);
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
