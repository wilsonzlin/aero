/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * WDM helper for acquiring the BUS_INTERFACE_STANDARD interface from the
 * PDO/lower stack via IRP_MN_QUERY_INTERFACE.
 *
 * Notes:
 * - Acquire/Release must be called at PASSIVE_LEVEL.
 * - AcquiredOut/AcquiredInOut tracks whether InterfaceReference was invoked
 *   (and therefore whether InterfaceDereference must be invoked). The queried
 *   interface may still be usable even when the flag is FALSE (if the bus
 *   driver does not provide reference/dereference callbacks).
 * - VirtIoSndReleaseBusInterface() always clears the interface struct and
 *   sets the flag to FALSE; it is safe to call unconditionally during teardown.
 */

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndAcquireBusInterface(
    _In_ PDEVICE_OBJECT LowerDevice,
    _Out_ PBUS_INTERFACE_STANDARD Out,
    _Out_ BOOLEAN* AcquiredOut
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtIoSndReleaseBusInterface(
    _Inout_ PBUS_INTERFACE_STANDARD Iface,
    _Inout_ BOOLEAN* AcquiredInOut
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VirtIoSndBusReadConfig(
    _In_ PBUS_INTERFACE_STANDARD Iface,
    _Out_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );

_IRQL_requires_max_(PASSIVE_LEVEL)
ULONG
VirtIoSndBusWriteConfig(
    _In_ PBUS_INTERFACE_STANDARD Iface,
    _In_ PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length
    );

#ifdef __cplusplus
} /* extern "C" */
#endif
