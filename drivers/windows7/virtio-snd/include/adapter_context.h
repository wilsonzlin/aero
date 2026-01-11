/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "portcls_compat.h"

#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
#include "aero_virtio_snd_ioport.h"
typedef PAEROVIOSND_DEVICE_EXTENSION VIRTIOSND_PORTCLS_DX;
#else
#include "virtiosnd.h"
typedef PVIRTIOSND_DEVICE_EXTENSION VIRTIOSND_PORTCLS_DX;
#endif

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
VirtIoSndAdapterContext_Register(_In_ PUNKNOWN UnknownAdapter, _In_ VIRTIOSND_PORTCLS_DX Dx, _In_ BOOLEAN ForceNullBackend);

VOID
VirtIoSndAdapterContext_Unregister(_In_ PUNKNOWN UnknownAdapter);

_IRQL_requires_max_(DISPATCH_LEVEL)
_Ret_maybenull_ VIRTIOSND_PORTCLS_DX
VirtIoSndAdapterContext_Lookup(_In_opt_ PUNKNOWN UnknownAdapter, _Out_opt_ BOOLEAN* ForceNullBackendOut);

/*
 * Best-effort teardown hook for device stop/remove paths where a PortCls callback
 * is not available. Intended to be called by miniports when they are destroyed.
 *
 * If MarkRemoved is TRUE and the build uses the modern device extension, sets
 * Dx->Removed before stopping hardware so protocol engines observe
 * STATUS_DEVICE_REMOVED.
 */
VOID
VirtIoSndAdapterContext_UnregisterAndStop(_In_ PUNKNOWN UnknownAdapter, _In_ BOOLEAN MarkRemoved);

#ifdef __cplusplus
} /* extern "C" */
#endif
