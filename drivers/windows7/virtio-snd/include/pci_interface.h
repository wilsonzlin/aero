/* SPDX-License-Identifier: MIT OR Apache-2.0 */

/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndAcquirePciBusInterface(
    _In_ PDEVICE_OBJECT LowerDevice,
    _Out_ PPCI_BUS_INTERFACE_STANDARD Out,
    _Out_ BOOLEAN* AcquiredOut
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtIoSndReleasePciBusInterface(
    _Inout_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _Inout_ BOOLEAN* AcquiredInOut
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VirtIoSndPciReadConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _Out_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VirtIoSndPciWriteConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD Iface,
    _In_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );
