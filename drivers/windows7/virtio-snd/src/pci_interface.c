#include <initguid.h>
#include <wdmguid.h>
#undef INITGUID

#include <ntddk.h>

#include "pci_interface.h"

#ifndef PCI_BUS_INTERFACE_STANDARD_VERSION
#define PCI_BUS_INTERFACE_STANDARD_VERSION 1
#endif

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static NTSTATUS
VirtIoSndPciInterfaceSyncCompletionRoutine(
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

_Use_decl_annotations_
NTSTATUS
VirtIoSndAcquirePciBusInterface(
    PDEVICE_OBJECT LowerDevice,
    PPCI_BUS_INTERFACE_STANDARD Out,
    BOOLEAN* AcquiredOut
    )
{
    KEVENT event;
    PIRP irp;
    PIO_STACK_LOCATION stack;
    NTSTATUS status;

    if (AcquiredOut != NULL) {
        *AcquiredOut = FALSE;
    }
    if (Out != NULL) {
        RtlZeroMemory(Out, sizeof(*Out));
    }

    if (LowerDevice == NULL || Out == NULL || AcquiredOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return STATUS_INVALID_DEVICE_STATE;
    }

    KeInitializeEvent(&event, NotificationEvent, FALSE);

    irp = IoAllocateIrp(LowerDevice->StackSize, FALSE);
    if (irp == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    irp->IoStatus.Information = 0;
    irp->RequestorMode = KernelMode;

    stack = IoGetNextIrpStackLocation(irp);
    stack->MajorFunction = IRP_MJ_PNP;
    stack->MinorFunction = IRP_MN_QUERY_INTERFACE;
    stack->Parameters.QueryInterface.InterfaceType = (LPGUID)&GUID_PCI_BUS_INTERFACE_STANDARD;
    stack->Parameters.QueryInterface.Size = sizeof(*Out);
    stack->Parameters.QueryInterface.Version = PCI_BUS_INTERFACE_STANDARD_VERSION;
    stack->Parameters.QueryInterface.Interface = (PINTERFACE)Out;
    stack->Parameters.QueryInterface.InterfaceSpecificData = NULL;

    IoSetCompletionRoutine(irp, VirtIoSndPciInterfaceSyncCompletionRoutine, &event, TRUE, TRUE, TRUE);

    status = IoCallDriver(LowerDevice, irp);
    if (status == STATUS_PENDING) {
        KeWaitForSingleObject(&event, Executive, KernelMode, FALSE, NULL);
        status = irp->IoStatus.Status;
    } else {
        status = irp->IoStatus.Status;
    }

    if (NT_SUCCESS(status)) {
        if (Out->InterfaceReference != NULL) {
            Out->InterfaceReference(Out->Context);
        }
        *AcquiredOut = TRUE;
    } else {
        RtlZeroMemory(Out, sizeof(*Out));
    }

    IoFreeIrp(irp);
    return status;
}

_Use_decl_annotations_
VOID
VirtIoSndReleasePciBusInterface(PPCI_BUS_INTERFACE_STANDARD Iface, BOOLEAN* AcquiredInOut)
{
    if (Iface == NULL || AcquiredInOut == NULL || !*AcquiredInOut) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);
        return;
    }

    if (Iface->InterfaceDereference != NULL) {
        Iface->InterfaceDereference(Iface->Context);
    }

    *AcquiredInOut = FALSE;
    RtlZeroMemory(Iface, sizeof(*Iface));
}

_Use_decl_annotations_
ULONG
VirtIoSndPciReadConfig(PPCI_BUS_INTERFACE_STANDARD Iface, PVOID Buffer, ULONG Offset, ULONG Length)
{
    if (Iface == NULL || Iface->ReadConfig == NULL || Buffer == NULL || Length == 0) {
        return 0;
    }

    return Iface->ReadConfig(Iface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
}

_Use_decl_annotations_
ULONG
VirtIoSndPciWriteConfig(PPCI_BUS_INTERFACE_STANDARD Iface, PVOID Buffer, ULONG Offset, ULONG Length)
{
    if (Iface == NULL || Iface->WriteConfig == NULL || Buffer == NULL || Length == 0) {
        return 0;
    }

    return Iface->WriteConfig(Iface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
}
