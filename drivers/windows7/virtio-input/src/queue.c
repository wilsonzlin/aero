#include "virtio_input.h"

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
#ifdef IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST
        case IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST:
            // No dedicated counter; keep it out of IoctlUnknown.
            break;
#endif
        default:
            VioInputCounterInc(&Counters->IoctlUnknown);
            break;
    }
}

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device)
{
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDFQUEUE queue;
    NTSTATUS status;

    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoInternalDeviceControl = VirtioInputEvtIoInternalDeviceControl;
    queueConfig.EvtIoDeviceControl = VirtioInputEvtIoDeviceControl;

    status = WdfIoQueueCreate(Device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

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
        "IOCTL %s (0x%08X) in=%Iu out=%Iu txRing=%ld pendingRing=%ld readQ=%ld\n",
        name,
        IoControlCode,
        InputBufferLength,
        OutputBufferLength,
        devCtx->Counters.ReportRingDepth,
        devCtx->Counters.PendingRingDepth,
        devCtx->Counters.ReadReportQueueDepth);

    switch (IoControlCode) {
    case IOCTL_HID_READ_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (read report handler)\n", name);
        (VOID)VirtioInputHandleHidReadReport(Queue, Request, OutputBufferLength);
        return;
#ifdef IOCTL_HID_GET_INPUT_REPORT
    case IOCTL_HID_GET_INPUT_REPORT:
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> (get input report handler)\n", name);
        (VOID)VirtioInputHandleHidGetInputReport(Queue, Request, OutputBufferLength);
        return;
#endif
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
        /*
         * Win7 HID idle / selective suspend.
         *
         * This IOCTL is METHOD_NEITHER and may contain user pointers. We don't
         * touch any buffers here; completing the request with STATUS_SUCCESS is
         * sufficient to tell HIDCLASS that the device may idle.
         */
        VIOINPUT_LOG(VIOINPUT_LOG_IOCTL, "IOCTL %s -> %!STATUS! bytes=0\n", name, STATUS_SUCCESS);
        WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, 0);
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

        if (InputBufferLength >= sizeof(ULONG) && NT_SUCCESS(VioInputReadRequestInputUlong(Request, &numBuffers))) {
            UNREFERENCED_PARAMETER(numBuffers);
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

    switch (IoControlCode) {
    case IOCTL_VIOINPUT_QUERY_COUNTERS: {
        PUCHAR outBuf;
        size_t outBytes;
        size_t availBytes;
        size_t copyBytes;
        VIOINPUT_COUNTERS snapshot;

        outBuf = NULL;
        outBytes = 0;
        status = WdfRequestRetrieveOutputBuffer(Request, 0, (PVOID *)&outBuf, &outBytes);
        if (!NT_SUCCESS(status)) {
            break;
        }

        VioInputCountersSnapshot(&devCtx->Counters, &snapshot);

        availBytes = outBytes;
        if (OutputBufferLength < availBytes) {
            availBytes = OutputBufferLength;
        }

        copyBytes = availBytes;
        if (copyBytes > sizeof(snapshot)) {
            copyBytes = sizeof(snapshot);
        }

        /*
         * Allow version negotiation: if the caller's buffer is too small for the
         * current counters struct, return STATUS_BUFFER_TOO_SMALL but still copy
         * as much of the snapshot as fits (starting with Size+Version).
         *
         * This keeps METHOD_BUFFERED semantics and preserves compatibility with
         * older tools that pass an older struct size: they still get the fields
         * they know, and can read Size/Version to allocate a larger buffer.
         */
        if (copyBytes != 0) {
            RtlCopyMemory(outBuf, &snapshot, copyBytes);
            info = copyBytes;
        }

        status = (copyBytes < sizeof(snapshot)) ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
        break;
    }
    case IOCTL_VIOINPUT_QUERY_STATE: {
        PVIOINPUT_STATE outState;
        size_t outBytes;
        VIOINPUT_STATE snapshot;
        LONG virtioStarted;
        LONG64 negotiatedFeatures;

        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(*outState), (PVOID *)&outState, &outBytes);
        if (!NT_SUCCESS(status)) {
            break;
        }

        if (OutputBufferLength < sizeof(*outState) || outBytes < sizeof(*outState)) {
            status = STATUS_BUFFER_TOO_SMALL;
            break;
        }

        RtlZeroMemory(&snapshot, sizeof(snapshot));
        snapshot.Size = sizeof(snapshot);
        snapshot.Version = VIOINPUT_STATE_VERSION;
        snapshot.DeviceKind = (ULONG)devCtx->DeviceKind;
        snapshot.PciRevisionId = (ULONG)devCtx->PciRevisionId;
        snapshot.PciSubsystemDeviceId = (ULONG)devCtx->PciSubsystemDeviceId;
        snapshot.HardwareReady = devCtx->HardwareReady ? 1u : 0u;
        snapshot.InD0 = devCtx->InD0 ? 1u : 0u;
        snapshot.HidActivated = devCtx->HidActivated ? 1u : 0u;

        virtioStarted = InterlockedCompareExchange(&devCtx->VirtioStarted, 0, 0);
        snapshot.VirtioStarted = (virtioStarted != 0) ? 1u : 0u;

        negotiatedFeatures = InterlockedCompareExchange64(&devCtx->NegotiatedFeatures, 0, 0);
        snapshot.NegotiatedFeatures = (UINT64)negotiatedFeatures;

        RtlCopyMemory(outState, &snapshot, sizeof(snapshot));
        status = STATUS_SUCCESS;
        info = sizeof(snapshot);
        break;
    }
    case IOCTL_VIOINPUT_RESET_COUNTERS: {
        VioInputCountersReset(&devCtx->Counters);
        status = STATUS_SUCCESS;
        info = 0;
        break;
    }
#if VIOINPUT_DIAGNOSTICS
    case IOCTL_VIOINPUT_GET_LOG_MASK: {
        ULONG* outMask;
        size_t outBytes;

        status = WdfRequestRetrieveOutputBuffer(Request, sizeof(*outMask), (PVOID *)&outMask, &outBytes);
        if (!NT_SUCCESS(status)) {
            break;
        }

        if (OutputBufferLength < sizeof(*outMask) || outBytes < sizeof(*outMask)) {
            status = STATUS_BUFFER_TOO_SMALL;
            break;
        }

        *outMask = VioInputLogGetMask();
        status = STATUS_SUCCESS;
        info = sizeof(*outMask);
        break;
    }
    case IOCTL_VIOINPUT_SET_LOG_MASK: {
        ULONG* inMask;
        size_t inBytes;

        status = WdfRequestRetrieveInputBuffer(Request, sizeof(*inMask), (PVOID *)&inMask, &inBytes);
        if (!NT_SUCCESS(status)) {
            break;
        }

        if (InputBufferLength < sizeof(*inMask) || inBytes < sizeof(*inMask)) {
            status = STATUS_BUFFER_TOO_SMALL;
            break;
        }

        (VOID)VioInputLogSetMask(*inMask);
        status = STATUS_SUCCESS;
        info = 0;
        break;
    }
#endif
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
