/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"

DRIVER_INITIALIZE DriverEntry;
DRIVER_UNLOAD VirtIoSndUnload;
DRIVER_ADD_DEVICE VirtIoSndAddDevice;

static DRIVER_DISPATCH VirtIoSndDispatchPnp;
static DRIVER_DISPATCH VirtIoSndDispatchPower;
static DRIVER_DISPATCH VirtIoSndDispatchSystemControl;
static DRIVER_DISPATCH VirtIoSndDispatchCreateClose;
static DRIVER_DISPATCH VirtIoSndDispatchDeviceControl;
static DRIVER_DISPATCH VirtIoSndDispatchUnsupported;

static NTSTATUS
VirtIoSndCompleteIrp(
    _In_ PIRP Irp,
    _In_ NTSTATUS Status,
    _In_ ULONG_PTR Information
    )
{
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = Information;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
}

static NTSTATUS
VirtIoSndSyncCompletionRoutine(
    _In_ PDEVICE_OBJECT DeviceObject,
    _In_ PIRP Irp,
    _In_ PVOID Context
    )
{
    PKEVENT event = (PKEVENT)Context;
    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Irp);

    KeSetEvent(event, IO_NO_INCREMENT, FALSE);
    return STATUS_MORE_PROCESSING_REQUIRED;
}

static NTSTATUS
VirtIoSndForwardIrpSynchronously(
    _In_ PDEVICE_OBJECT LowerDeviceObject,
    _Inout_ PIRP Irp
    )
{
    KEVENT event;
    NTSTATUS status;

    KeInitializeEvent(&event, NotificationEvent, FALSE);

    IoCopyCurrentIrpStackLocationToNext(Irp);
    IoSetCompletionRoutine(Irp, VirtIoSndSyncCompletionRoutine, &event, TRUE, TRUE, TRUE);

    status = IoCallDriver(LowerDeviceObject, Irp);
    if (status == STATUS_PENDING) {
        KeWaitForSingleObject(&event, Executive, KernelMode, FALSE, NULL);
        status = Irp->IoStatus.Status;
    }

    return status;
}

static NTSTATUS
VirtIoSndRemoveLockCompletionRoutine(
    _In_ PDEVICE_OBJECT DeviceObject,
    _In_ PIRP Irp,
    _In_ PVOID Context
    )
{
    PIO_REMOVE_LOCK lock = (PIO_REMOVE_LOCK)Context;
    UNREFERENCED_PARAMETER(DeviceObject);

    if (Irp->PendingReturned) {
        IoMarkIrpPending(Irp);
    }

    IoReleaseRemoveLock(lock, Irp);
    return STATUS_CONTINUE_COMPLETION;
}

static NTSTATUS
VirtIoSndForwardIrpWithRemoveLock(
    _In_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _Inout_ PIRP Irp
    )
{
    IoCopyCurrentIrpStackLocationToNext(Irp);
    IoSetCompletionRoutine(Irp, VirtIoSndRemoveLockCompletionRoutine, &Dx->RemoveLock, TRUE, TRUE, TRUE);
    return IoCallDriver(Dx->LowerDeviceObject, Irp);
}

static NTSTATUS
VirtIoSndForwardPowerIrpWithRemoveLock(
    _In_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _Inout_ PIRP Irp
    )
{
    PoStartNextPowerIrp(Irp);
    IoCopyCurrentIrpStackLocationToNext(Irp);
    IoSetCompletionRoutine(Irp, VirtIoSndRemoveLockCompletionRoutine, &Dx->RemoveLock, TRUE, TRUE, TRUE);
    return PoCallDriver(Dx->LowerDeviceObject, Irp);
}

_Use_decl_annotations_
NTSTATUS
DriverEntry(
    PDRIVER_OBJECT DriverObject,
    PUNICODE_STRING RegistryPath
    )
{
    ULONG i;

    UNREFERENCED_PARAMETER(RegistryPath);

    for (i = 0; i <= IRP_MJ_MAXIMUM_FUNCTION; ++i) {
        DriverObject->MajorFunction[i] = VirtIoSndDispatchUnsupported;
    }

    DriverObject->MajorFunction[IRP_MJ_PNP] = VirtIoSndDispatchPnp;
    DriverObject->MajorFunction[IRP_MJ_POWER] = VirtIoSndDispatchPower;
    DriverObject->MajorFunction[IRP_MJ_SYSTEM_CONTROL] = VirtIoSndDispatchSystemControl;
    DriverObject->MajorFunction[IRP_MJ_CREATE] = VirtIoSndDispatchCreateClose;
    DriverObject->MajorFunction[IRP_MJ_CLOSE] = VirtIoSndDispatchCreateClose;
    DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL] = VirtIoSndDispatchDeviceControl;

    DriverObject->DriverUnload = VirtIoSndUnload;
    DriverObject->DriverExtension->AddDevice = VirtIoSndAddDevice;

    VIRTIOSND_TRACE("DriverEntry\n");
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
VirtIoSndUnload(PDRIVER_OBJECT DriverObject)
{
    UNREFERENCED_PARAMETER(DriverObject);
    VIRTIOSND_TRACE("Unload\n");
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndAddDevice(PDRIVER_OBJECT DriverObject, PDEVICE_OBJECT PhysicalDeviceObject)
{
    NTSTATUS status;
    PDEVICE_OBJECT deviceObject = NULL;
    PVIRTIOSND_DEVICE_EXTENSION dx;

    VIRTIOSND_TRACE("AddDevice\n");

    status = IoCreateDevice(
        DriverObject,
        sizeof(VIRTIOSND_DEVICE_EXTENSION),
        NULL,
        FILE_DEVICE_UNKNOWN,
        0,
        FALSE,
        &deviceObject);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IoCreateDevice failed: 0x%08X\n", (ULONG)status);
        return status;
    }

    dx = VIRTIOSND_GET_DX(deviceObject);
    RtlZeroMemory(dx, sizeof(*dx));
    dx->Self = deviceObject;
    dx->Pdo = PhysicalDeviceObject;
    dx->LowerDeviceObject = IoAttachDeviceToDeviceStack(deviceObject, PhysicalDeviceObject);

    if (dx->LowerDeviceObject == NULL) {
        VIRTIOSND_TRACE_ERROR("IoAttachDeviceToDeviceStack failed\n");
        IoDeleteDevice(deviceObject);
        return STATUS_NO_SUCH_DEVICE;
    }

    IoInitializeRemoveLock(&dx->RemoveLock, VIRTIOSND_POOL_TAG, 0, 0);

    VirtIoSndIntxInitialize(dx);

    deviceObject->Flags |= dx->LowerDeviceObject->Flags & (DO_BUFFERED_IO | DO_DIRECT_IO | DO_POWER_PAGABLE);
    deviceObject->Flags &= ~DO_DEVICE_INITIALIZING;

    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndDispatchUnsupported(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    NTSTATUS status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    IoReleaseRemoveLock(&dx->RemoveLock, Irp);
    return VirtIoSndCompleteIrp(Irp, STATUS_NOT_SUPPORTED, 0);
}

static NTSTATUS
VirtIoSndDispatchCreateClose(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    NTSTATUS status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    IoReleaseRemoveLock(&dx->RemoveLock, Irp);
    return VirtIoSndCompleteIrp(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS
VirtIoSndDispatchDeviceControl(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    NTSTATUS status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    IoReleaseRemoveLock(&dx->RemoveLock, Irp);
    return VirtIoSndCompleteIrp(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
}

static NTSTATUS
VirtIoSndDispatchSystemControl(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    NTSTATUS status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    return VirtIoSndForwardIrpWithRemoveLock(dx, Irp);
}

static NTSTATUS
VirtIoSndDispatchPower(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    NTSTATUS status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        PoStartNextPowerIrp(Irp);
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    return VirtIoSndForwardPowerIrpWithRemoveLock(dx, Irp);
}

static NTSTATUS
VirtIoSndDispatchPnp(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DEVICE_EXTENSION dx = VIRTIOSND_GET_DX(DeviceObject);
    PIO_STACK_LOCATION stack = IoGetCurrentIrpStackLocation(Irp);
    NTSTATUS status;

    status = IoAcquireRemoveLock(&dx->RemoveLock, Irp);
    if (!NT_SUCCESS(status)) {
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    switch (stack->MinorFunction) {
    case IRP_MN_START_DEVICE: {
        PCM_RESOURCE_LIST raw = stack->Parameters.StartDevice.AllocatedResources;
        PCM_RESOURCE_LIST translated = stack->Parameters.StartDevice.AllocatedResourcesTranslated;

        status = VirtIoSndForwardIrpSynchronously(dx->LowerDeviceObject, Irp);
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("Lower driver failed START_DEVICE: 0x%08X\n", (ULONG)status);
            IoReleaseRemoveLock(&dx->RemoveLock, Irp);
            return VirtIoSndCompleteIrp(Irp, status, 0);
        }

        status = VirtIoSndStartHardware(dx, raw, translated);
        IoReleaseRemoveLock(&dx->RemoveLock, Irp);
        return VirtIoSndCompleteIrp(Irp, status, 0);
    }

    case IRP_MN_STOP_DEVICE:
        VirtIoSndStopHardware(dx);
        return VirtIoSndForwardIrpWithRemoveLock(dx, Irp);

    case IRP_MN_SURPRISE_REMOVAL:
        dx->Removed = TRUE;
        VirtIoSndStopHardware(dx);
        return VirtIoSndForwardIrpWithRemoveLock(dx, Irp);

    case IRP_MN_REMOVE_DEVICE: {
        dx->Removed = TRUE;
        VirtIoSndStopHardware(dx);

        IoSkipCurrentIrpStackLocation(Irp);
        status = IoCallDriver(dx->LowerDeviceObject, Irp);

        IoReleaseRemoveLockAndWait(&dx->RemoveLock, Irp);
        IoDetachDevice(dx->LowerDeviceObject);
        IoDeleteDevice(DeviceObject);
        return status;
    }

    default:
        return VirtIoSndForwardIrpWithRemoveLock(dx, Irp);
    }
}
