/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

#include <ntddk.h>

#include "virtiosnd_rx.h"

typedef struct _VIRTIOSND_DEVICE_EXTENSION VIRTIOSND_DEVICE_EXTENSION, *PVIRTIOSND_DEVICE_EXTENSION;

typedef struct _VIRTIOSND_BACKEND VIRTIOSND_BACKEND, *PVIRTIOSND_BACKEND;

typedef struct _VIRTIOSND_BACKEND_OPS {
    NTSTATUS (*SetParams)(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);
    NTSTATUS (*Prepare)(_In_ PVOID Context);
    NTSTATUS (*Start)(_In_ PVOID Context);
    NTSTATUS (*Stop)(_In_ PVOID Context);
    NTSTATUS (*Release)(_In_ PVOID Context);
    NTSTATUS (*WritePeriod)(
        _In_ PVOID Context,
        _In_ UINT64 Pcm1DmaAddr,
        _In_ SIZE_T Pcm1Bytes,
        _In_ UINT64 Pcm2DmaAddr,
        _In_ SIZE_T Pcm2Bytes);

    /* Capture (stream 1 / RX) support. */
    NTSTATUS (*SetParamsCapture)(_In_ PVOID Context, _In_ ULONG BufferBytes, _In_ ULONG PeriodBytes);
    NTSTATUS (*PrepareCapture)(_In_ PVOID Context);
    NTSTATUS (*StartCapture)(_In_ PVOID Context);
    NTSTATUS (*StopCapture)(_In_ PVOID Context);
    NTSTATUS (*ReleaseCapture)(_In_ PVOID Context);

    _IRQL_requires_max_(DISPATCH_LEVEL)
    NTSTATUS (*SubmitCapturePeriodSg)(
        _In_ PVOID Context,
        _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT *Segments,
        _In_ USHORT SegmentCount,
        _In_opt_ void *Cookie);

    _IRQL_requires_max_(DISPATCH_LEVEL)
    ULONG (*DrainCaptureCompletions)(
        _In_ PVOID Context,
        _In_opt_ EVT_VIRTIOSND_RX_COMPLETION *Callback,
        _In_opt_ void *CallbackContext);

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
VirtIoSndBackend_WritePeriod(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_ UINT64 Pcm1DmaAddr,
    _In_ SIZE_T Pcm1Bytes,
    _In_ UINT64 Pcm2DmaAddr,
    _In_ SIZE_T Pcm2Bytes
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->WritePeriod == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->WritePeriod(Backend->Context, Pcm1DmaAddr, Pcm1Bytes, Pcm2DmaAddr, Pcm2Bytes);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_SetParamsCapture(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_ ULONG BufferBytes,
    _In_ ULONG PeriodBytes
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->SetParamsCapture == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->SetParamsCapture(Backend->Context, BufferBytes, PeriodBytes);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_PrepareCapture(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->PrepareCapture == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->PrepareCapture(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_StartCapture(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->StartCapture == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->StartCapture(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_StopCapture(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->StopCapture == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->StopCapture(Backend->Context);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_ReleaseCapture(_In_ PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->ReleaseCapture == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->ReleaseCapture(Backend->Context);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
static __inline NTSTATUS
VirtIoSndBackend_SubmitCapturePeriodSg(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_reads_(SegmentCount) const VIRTIOSND_RX_SEGMENT *Segments,
    _In_ USHORT SegmentCount,
    _In_opt_ void *Cookie
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->SubmitCapturePeriodSg == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }
    return Backend->Ops->SubmitCapturePeriodSg(Backend->Context, Segments, SegmentCount, Cookie);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
static __inline ULONG
VirtIoSndBackend_DrainCaptureCompletions(
    _In_ PVIRTIOSND_BACKEND Backend,
    _In_opt_ EVT_VIRTIOSND_RX_COMPLETION *Callback,
    _In_opt_ void *CallbackContext
    )
{
    if (Backend == NULL || Backend->Ops == NULL || Backend->Ops->DrainCaptureCompletions == NULL) {
        return 0;
    }
    return Backend->Ops->DrainCaptureCompletions(Backend->Context, Callback, CallbackContext);
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
    _Outptr_result_maybenull_ PVIRTIOSND_BACKEND *OutBackend
    );
