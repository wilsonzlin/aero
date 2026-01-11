/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

typedef struct _VIRTIOSND_BACKEND VIRTIOSND_BACKEND, *PVIRTIOSND_BACKEND;
typedef struct _VIRTIOSND_DEVICE_EXTENSION VIRTIOSND_DEVICE_EXTENSION, *PVIRTIOSND_DEVICE_EXTENSION;

typedef struct _VIRTIOSND_BACKEND_OPS {
    NTSTATUS (*SetParams)(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);
    NTSTATUS (*Prepare)(_In_ PVOID Context);
    NTSTATUS (*Start)(_In_ PVOID Context);
    NTSTATUS (*Stop)(_In_ PVOID Context);
    NTSTATUS (*Release)(_In_ PVOID Context);
    NTSTATUS (*Write)(_In_ PVOID Context, _In_reads_bytes_(Bytes) const VOID *Pcm, _In_ SIZE_T Bytes);
    VOID (*Destroy)(_In_ PVOID Context);
} VIRTIOSND_BACKEND_OPS, *PVIRTIOSND_BACKEND_OPS;

typedef struct _VIRTIOSND_BACKEND {
    const VIRTIOSND_BACKEND_OPS *Ops;
    PVOID Context;
} VIRTIOSND_BACKEND, *PVIRTIOSND_BACKEND;

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_SetParams(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_ ULONG BufferBytes,
    _In_ ULONG PeriodBytes
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->SetParams == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->SetParams(Backend->Context, BufferBytes, PeriodBytes);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_Prepare(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Prepare == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->Prepare(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_Start(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Start == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->Start(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_Stop(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Stop == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->Stop(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_Release(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Release == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->Release(Backend->Context);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_Write(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_reads_bytes_(Bytes) const VOID *Pcm,
    _In_ SIZE_T Bytes
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Write == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->Write(Backend->Context, Pcm, Bytes);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
static __inline VOID
VirtIoSndBackend_Destroy(_In_opt_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->Destroy == NULL) {
        return;
    }

    Backend->Ops->Destroy(Backend->Context);
}

NTSTATUS
VirtIoSndBackendNull_Create(_Outptr_result_maybenull_ PVIRTIOSND_BACKEND *OutBackend);

NTSTATUS
VirtIoSndBackendVirtio_Create(
    _In_ PVIRTIOSND_DEVICE_EXTENSION Dx,
    _Outptr_result_maybenull_ PVIRTIOSND_BACKEND *OutBackend);
