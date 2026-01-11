#pragma once

#include <ntddk.h>

NTSTATUS
VirtIoSndAcquirePciBusInterface(
    _In_ PDEVICE_OBJECT LowerDevice,
    _Out_ PPCI_BUS_INTERFACE_STANDARD Out,
    _Out_ BOOLEAN* AcquiredOut
    );

VOID
VirtIoSndReleasePciBusInterface(
    _Inout_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _Inout_ BOOLEAN* AcquiredInOut
    );

ULONG
VirtIoSndPciReadConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _Out_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );

ULONG
VirtIoSndPciWriteConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _In_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );
