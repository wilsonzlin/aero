/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

typedef struct _VIRTIOSND_BACKEND VIRTIOSND_BACKEND, *PVIRTIOSND_BACKEND;

#ifdef __cplusplus
extern "C" {
#endif

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_Create(_In_ PDEVICE_OBJECT DeviceObject,
                        _In_ PDEVICE_OBJECT LowerDeviceObject,
                        _In_opt_ PCM_RESOURCE_LIST RawResources,
                        _In_opt_ PCM_RESOURCE_LIST TranslatedResources,
                        _Out_ PVIRTIOSND_BACKEND *BackendOut);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioSndBackend_Destroy(_Inout_ PVIRTIOSND_BACKEND Backend);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_SetParams(_Inout_ PVIRTIOSND_BACKEND Backend, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_Prepare(_Inout_ PVIRTIOSND_BACKEND Backend);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_Start(_Inout_ PVIRTIOSND_BACKEND Backend);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_Stop(_Inout_ PVIRTIOSND_BACKEND Backend);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioSndBackend_Release(_Inout_ PVIRTIOSND_BACKEND Backend);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioSndBackend_Write(_Inout_ PVIRTIOSND_BACKEND Backend,
                       _In_reads_bytes_(Bytes) const VOID *Pcm,
                       _In_ ULONG Bytes,
                       _Out_ PULONG BytesWritten);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioSndBackend_Service(_Inout_ PVIRTIOSND_BACKEND Backend);

#ifdef __cplusplus
} /* extern "C" */
#endif
