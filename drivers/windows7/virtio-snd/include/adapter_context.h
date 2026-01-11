/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "portcls_compat.h"
#include "virtiosnd.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Register/lookup mapping between PortCls' UnknownAdapter object and our per-device
 * VIRTIOSND_DEVICE_EXTENSION.
 *
 * Miniports only receive UnknownAdapter during IMiniport::Init, so this is the
 * stable bridge for accessing virtio-snd transport state.
 */

/*
 * Initializes global adapter-context state. Must be called before any
 * Register/Lookup/Unregister calls (DriverEntry does this).
 */
VOID
VirtIoSndAdapterContext_Initialize(VOID);

_Must_inspect_result_ NTSTATUS
VirtIoSndAdapterContext_Register(_In_ PUNKNOWN UnknownAdapter, _In_ PVIRTIOSND_DEVICE_EXTENSION Dx, _In_ BOOLEAN ForceNullBackend);

VOID
VirtIoSndAdapterContext_Unregister(_In_ PUNKNOWN UnknownAdapter);

_IRQL_requires_max_(DISPATCH_LEVEL)
_Ret_maybenull_ PVIRTIOSND_DEVICE_EXTENSION
VirtIoSndAdapterContext_Lookup(_In_opt_ PUNKNOWN UnknownAdapter, _Out_opt_ BOOLEAN* ForceNullBackendOut);

/*
 * Best-effort teardown hook for device stop/remove paths where a PortCls callback
 * is not available. Intended to be called by miniports when they are destroyed.
 *
 * If MarkRemoved is TRUE, sets Dx->Removed before stopping hardware so protocol
 * engines observe STATUS_DEVICE_REMOVED.
 */
VOID
VirtIoSndAdapterContext_UnregisterAndStop(_In_ PUNKNOWN UnknownAdapter, _In_ BOOLEAN MarkRemoved);

#ifdef __cplusplus
} /* extern "C" */
#endif
