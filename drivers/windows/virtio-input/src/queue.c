#include "virtio_input.h"

NTSTATUS VirtioInputQueueInitialize(_In_ WDFDEVICE Device)
{
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDFQUEUE queue;
    PDEVICE_CONTEXT deviceContext;
    NTSTATUS status;

    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoInternalDeviceControl = VirtioInputEvtIoInternalDeviceControl;

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
    UNREFERENCED_PARAMETER(Queue);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);
    UNREFERENCED_PARAMETER(IoControlCode);

    WdfRequestComplete(Request, STATUS_NOT_SUPPORTED);
}

