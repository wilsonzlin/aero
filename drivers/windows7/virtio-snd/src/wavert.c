/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "adapter_context.h"
#include "backend.h"
#include "portcls_compat.h"
#include "trace.h"
#include "virtiosnd.h"
#include "wavert.h"

typedef struct _VIRTIOSND_WAVERT_STREAM VIRTIOSND_WAVERT_STREAM, *PVIRTIOSND_WAVERT_STREAM;

typedef struct _VIRTIOSND_WAVERT_MINIPORT {
    IMiniportWaveRT Interface;
    LONG RefCount;

    PVIRTIOSND_BACKEND Backend;
    PVIRTIOSND_DEVICE_EXTENSION Dx;

    KSPIN_LOCK Lock;
    PVIRTIOSND_WAVERT_STREAM Stream;
} VIRTIOSND_WAVERT_MINIPORT, *PVIRTIOSND_WAVERT_MINIPORT;

typedef struct _VIRTIOSND_WAVERT_STREAM {
    IMiniportWaveRTStream Interface;
    LONG RefCount;

    PVIRTIOSND_WAVERT_MINIPORT Miniport;
    KSSTATE State;

    KSPIN_LOCK Lock;

    KTIMER Timer;
    KDPC TimerDpc;
    KEVENT DpcIdleEvent;
    volatile LONG DpcActive;
    volatile BOOLEAN Stopping;

    PKEVENT NotificationEvent;

    VIRTIOSND_DMA_BUFFER BufferDma;
    ULONG BufferSize;
    PMDL BufferMdl;

    KSAUDIO_POSITION *PositionRegister;
    ULONGLONG *ClockRegister;
    ULONG PacketCount;

    ULONG PeriodBytes;
    ULONGLONG Period100ns;
    ULONG PeriodMs;

    ULONGLONG QpcFrequency;

    /*
     * Clock state (render-only, QPC-derived).
     *
     * While in KSSTATE_RUN:
     *   linearFrames = StartLinearFrames + floor((NowQpc - StartQpc) * SampleRate / QpcFrequency)
     *
     * While not running, position reporting is frozen at FrozenLinearFrames / FrozenQpc.
     */
    ULONGLONG StartQpc;
    ULONGLONG StartLinearFrames;
    ULONGLONG FrozenLinearFrames;
    ULONGLONG FrozenQpc;

    /*
     * Playback submission tracking (bytes).
     *
     * Submitted* describes the next period boundary to be submitted to the backend,
     * in the same linear/ring coordinate space as the WaveRT cyclic buffer.
     */
    ULONGLONG SubmittedLinearPositionBytes;
    ULONG SubmittedRingPositionBytes;
} VIRTIOSND_WAVERT_STREAM, *PVIRTIOSND_WAVERT_STREAM;

// Forward declarations for vtables.
static const IMiniportWaveRTVtbl g_VirtIoSndWaveRtMiniportVtbl;
static const IMiniportWaveRTStreamVtbl g_VirtIoSndWaveRtStreamVtbl;

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_AddRef(_In_ IMiniportWaveRT *This);
static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_Release(_In_ IMiniportWaveRT *This);

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtStream_AddRef(_In_ IMiniportWaveRTStream *This);
static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtStream_Release(_In_ IMiniportWaveRTStream *This);

static PVIRTIOSND_WAVERT_MINIPORT
VirtIoSndWaveRtMiniportFromInterface(_In_ IMiniportWaveRT *Interface)
{
    return CONTAINING_RECORD(Interface, VIRTIOSND_WAVERT_MINIPORT, Interface);
}

static PVIRTIOSND_WAVERT_STREAM
VirtIoSndWaveRtStreamFromInterface(_In_ IMiniportWaveRTStream *Interface)
{
    return CONTAINING_RECORD(Interface, VIRTIOSND_WAVERT_STREAM, Interface);
}

static BOOLEAN
VirtIoSndWaveRt_IsFormatSupported(_In_ const KSDATAFORMAT *DataFormat)
{
    const KSDATAFORMAT_WAVEFORMATEXTENSIBLE *fmt;
    const WAVEFORMATEX *wfx;

    if (DataFormat == NULL) {
        return FALSE;
    }

    if (!IsEqualGUID(&DataFormat->MajorFormat, &KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUID(&DataFormat->Specifier, &KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return FALSE;
    }

    if (DataFormat->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEX)) {
        return FALSE;
    }

    wfx = &((const KSDATAFORMAT_WAVEFORMATEX *)DataFormat)->WaveFormatEx;

    if (wfx->nSamplesPerSec != VIRTIOSND_SAMPLE_RATE ||
        wfx->nChannels != VIRTIOSND_CHANNELS ||
        wfx->wBitsPerSample != VIRTIOSND_BITS_PER_SAMPLE ||
        wfx->nBlockAlign != VIRTIOSND_BLOCK_ALIGN ||
        wfx->nAvgBytesPerSec != VIRTIOSND_AVG_BYTES_PER_SEC) {
        return FALSE;
    }

    if (wfx->wFormatTag == WAVE_FORMAT_PCM) {
        return TRUE;
    }

    if (wfx->wFormatTag != WAVE_FORMAT_EXTENSIBLE) {
        return FALSE;
    }

    if (DataFormat->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE)) {
        return FALSE;
    }

    fmt = (const KSDATAFORMAT_WAVEFORMATEXTENSIBLE *)DataFormat;
    if (!IsEqualGUID(&fmt->WaveFormatExt.SubFormat, &KSDATAFORMAT_SUBTYPE_PCM)) {
        return FALSE;
    }

    if (fmt->WaveFormatExt.dwChannelMask != KSAUDIO_SPEAKER_STEREO) {
        return FALSE;
    }

    if (fmt->WaveFormatExt.Samples.wValidBitsPerSample != VIRTIOSND_BITS_PER_SAMPLE) {
        return FALSE;
    }

    return TRUE;
}

static VOID
VirtIoSndWaveRtGetPositionSnapshot(
    _In_ PVIRTIOSND_WAVERT_STREAM Stream,
    _In_ ULONGLONG NowQpc,
    _Out_ ULONGLONG *OutLinearFrames,
    _Out_opt_ PULONG OutRingBytes,
    _Out_opt_ ULONGLONG *OutQpc
    )
{
    ULONGLONG linearFrames;
    ULONGLONG qpc;
    ULONG ringBytes;

    linearFrames = Stream->FrozenLinearFrames;
    qpc = Stream->FrozenQpc;

    if (Stream->State == KSSTATE_RUN && Stream->QpcFrequency != 0) {
        ULONGLONG deltaQpc;
        ULONGLONG elapsedFrames;

        qpc = NowQpc;

        deltaQpc = 0;
        if (NowQpc >= Stream->StartQpc) {
            deltaQpc = NowQpc - Stream->StartQpc;
        }

        elapsedFrames = (deltaQpc * (ULONGLONG)VIRTIOSND_SAMPLE_RATE) / Stream->QpcFrequency;
        linearFrames = Stream->StartLinearFrames + elapsedFrames;
    }

    ringBytes = 0;
    if (Stream->BufferSize != 0) {
        ringBytes = (ULONG)((linearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN) % (ULONGLONG)Stream->BufferSize);
    }

    *OutLinearFrames = linearFrames;
    if (OutRingBytes != NULL) {
        *OutRingBytes = ringBytes;
    }
    if (OutQpc != NULL) {
        *OutQpc = qpc;
    }
}

static __forceinline VOID
VirtIoSndWaveRtWriteClockRegister(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream, _In_ ULONGLONG Value)
{
    if (Stream->ClockRegister != NULL) {
        (VOID)InterlockedExchange64((volatile LONGLONG *)Stream->ClockRegister, (LONGLONG)Value);
    }
}

static VOID
VirtIoSndWaveRtStopTimer(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream)
{
    KIRQL oldIrql;
    BOOLEAN removed;

    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    Stream->Stopping = TRUE;
    KeResetEvent(&Stream->DpcIdleEvent);
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    (VOID)KeCancelTimer(&Stream->Timer);
    removed = KeRemoveQueueDpc(&Stream->TimerDpc);
    if (!removed && KeGetCurrentIrql() == PASSIVE_LEVEL) {
        KeFlushQueuedDpcs();
    }

    if (InterlockedCompareExchange(&Stream->DpcActive, 0, 0) == 0) {
        KeSetEvent(&Stream->DpcIdleEvent, IO_NO_INCREMENT, FALSE);
        return;
    }

    KeWaitForSingleObject(&Stream->DpcIdleEvent, Executive, KernelMode, FALSE, NULL);
}

static VOID
VirtIoSndWaveRtStartTimer(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream)
{
    LARGE_INTEGER dueTime;
    KIRQL oldIrql;
    ULONGLONG period100ns;
    ULONG periodMs;

    KeResetEvent(&Stream->DpcIdleEvent);

    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    Stream->Stopping = FALSE;
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    period100ns = Stream->Period100ns;
    periodMs = Stream->PeriodMs;

    if (period100ns == 0 || periodMs == 0) {
        period100ns = 10 * 1000 * 10;
        periodMs = 10;
    }

    dueTime.QuadPart = -(LONGLONG)period100ns;
    KeSetTimerEx(&Stream->Timer, dueTime, (LONG)periodMs, &Stream->TimerDpc);
}

static VOID
VirtIoSndWaveRtUpdateRegisters(
    _Inout_ PVIRTIOSND_WAVERT_STREAM Stream,
    _In_ ULONG RingPositionBytes,
    _In_ ULONGLONG Qpc
    )
{
    if (Stream->PositionRegister != NULL) {
        Stream->PositionRegister->PlayOffset = RingPositionBytes;
    }

    if (Stream->ClockRegister != NULL) {
        VirtIoSndWaveRtWriteClockRegister(Stream, Qpc);
    }
}

static VOID
VirtIoSndWaveRtDpcRoutine(
    _In_ PKDPC Dpc,
    _In_opt_ PVOID DeferredContext,
    _In_opt_ PVOID SystemArgument1,
    _In_opt_ PVOID SystemArgument2
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = (PVIRTIOSND_WAVERT_STREAM)DeferredContext;
    KIRQL oldIrql;
    ULONG periodBytes;
    ULONG bufferSize;
    UINT64 bufferDma;
    PKEVENT notifyEvent;
    PVIRTIOSND_BACKEND backend;
    LARGE_INTEGER qpc;
    ULONGLONG qpcValue;
    ULONGLONG linearFrames;
    ULONG playOffsetBytes;
    ULONGLONG playLinearBytes;
    ULONGLONG submittedLinearBytes;
    ULONG submittedRingBytes;
    ULONG leadPeriods;
    ULONGLONG leadBytes;
    ULONG submitBudget;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (stream == NULL) {
        return;
    }

    InterlockedIncrement(&stream->DpcActive);

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    if (stream->Stopping || stream->State != KSSTATE_RUN || stream->BufferDma.Va == NULL || stream->BufferSize == 0 || stream->PeriodBytes == 0 ||
        stream->PeriodBytes > stream->BufferSize) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        goto Exit;
    }

    periodBytes = stream->PeriodBytes;
    bufferSize = stream->BufferSize;
    bufferDma = stream->BufferDma.DmaAddr;
    notifyEvent = stream->NotificationEvent;
    backend = stream->Miniport->Backend;

    if (notifyEvent != NULL) {
        ObReferenceObject(notifyEvent);
    }

    qpc = KeQueryPerformanceCounter(NULL);
    qpcValue = (ULONGLONG)qpc.QuadPart;

    VirtIoSndWaveRtGetPositionSnapshot(stream, qpcValue, &linearFrames, &playOffsetBytes, NULL);
    playLinearBytes = linearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN;

    stream->PacketCount += 1;
    VirtIoSndWaveRtUpdateRegisters(stream, playOffsetBytes, qpcValue);

    submittedLinearBytes = stream->SubmittedLinearPositionBytes;
    submittedRingBytes = stream->SubmittedRingPositionBytes;

    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (backend != NULL) {
        /*
         * Keep a small bounded lead of audio submitted to the device.
         *
         * Note: SubmittedLinearPositionBytes advances in whole periods, while the
         * play cursor can be fractional within a period due to QPC-based timing.
         */
        leadPeriods = bufferSize / periodBytes;
        if (leadPeriods > 0) {
            leadPeriods -= 1;
        }
        if (leadPeriods == 0) {
            leadPeriods = 1;
        }
        if (leadPeriods > 3) {
            leadPeriods = 3;
        }

        leadBytes = (ULONGLONG)leadPeriods * (ULONGLONG)periodBytes;

        /*
         * If we've fallen behind, realign the submission pointer to the current
         * play position. Any gap is treated as an underrun.
         */
        if (submittedLinearBytes < playLinearBytes) {
            submittedLinearBytes = playLinearBytes;
            submittedRingBytes = playOffsetBytes;
        }

        submitBudget = 8;

        while (submitBudget-- != 0) {
            ULONGLONG queuedBytes;
            ULONG remaining;
            ULONG first;
            ULONG second;
            NTSTATUS writeStatus;

            queuedBytes = submittedLinearBytes - playLinearBytes;
            if (queuedBytes >= leadBytes) {
                break;
            }

            remaining = bufferSize - submittedRingBytes;
            first = (remaining < periodBytes) ? remaining : periodBytes;
            second = periodBytes - first;

            writeStatus = VirtIoSndBackend_WritePeriod(backend,
                                                       bufferDma + (UINT64)submittedRingBytes,
                                                       first,
                                                       (second != 0) ? bufferDma : 0,
                                                       second);
            if (!NT_SUCCESS(writeStatus)) {
                break;
            }

            submittedRingBytes = (submittedRingBytes + periodBytes) % bufferSize;
            submittedLinearBytes += periodBytes;
        }
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    stream->SubmittedLinearPositionBytes = submittedLinearBytes;
    stream->SubmittedRingPositionBytes = submittedRingBytes;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (notifyEvent != NULL) {
        KeSetEvent(notifyEvent, IO_NO_INCREMENT, FALSE);
        ObDereferenceObject(notifyEvent);
    }

Exit:
    if (InterlockedDecrement(&stream->DpcActive) == 0) {
        if (stream->Stopping) {
            KeSetEvent(&stream->DpcIdleEvent, IO_NO_INCREMENT, FALSE);
        }
    }
}

// IUnknown / IMiniportWaveRT

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_QueryInterface(
    _In_ IMiniportWaveRT *This,
    _In_ REFIID Riid,
    _Outptr_ PVOID *Object
    )
{
    if (Object == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Object = NULL;

    if (IsEqualGUID(Riid, &IID_IUnknown) ||
        IsEqualGUID(Riid, &IID_IMiniport) ||
        IsEqualGUID(Riid, &IID_IMiniportWaveRT)) {
        *Object = This;
        (VOID)VirtIoSndWaveRtMiniport_AddRef(This);
        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_AddRef(_In_ IMiniportWaveRT *This)
{
    PVIRTIOSND_WAVERT_MINIPORT miniport = VirtIoSndWaveRtMiniportFromInterface(This);
    return (ULONG)InterlockedIncrement(&miniport->RefCount);
}

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_Release(_In_ IMiniportWaveRT *This)
{
    PVIRTIOSND_WAVERT_MINIPORT miniport = VirtIoSndWaveRtMiniportFromInterface(This);
    LONG ref = InterlockedDecrement(&miniport->RefCount);
    if (ref == 0) {
        VirtIoSndBackend_Destroy(miniport->Backend);
        miniport->Backend = NULL;
        miniport->Dx = NULL;
        ExFreePoolWithTag(miniport, VIRTIOSND_POOL_TAG);
        return 0;
    }
    return (ULONG)ref;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_Init(
    _In_ IMiniportWaveRT *This,
    _In_opt_ PUNKNOWN UnknownAdapter,
    _In_ PRESOURCELIST ResourceList,
    _In_ PPORTWAVERT Port,
    _Outptr_opt_result_maybenull_ PSERVICEGROUP *ServiceGroup
    )
{
    PVIRTIOSND_WAVERT_MINIPORT miniport = VirtIoSndWaveRtMiniportFromInterface(This);
    PVIRTIOSND_DEVICE_EXTENSION dx;
    BOOLEAN forceNullBackend;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);

    if (ServiceGroup != NULL) {
        *ServiceGroup = NULL;
    }

    if (miniport->Backend != NULL) {
        return STATUS_SUCCESS;
    }

    forceNullBackend = FALSE;
    dx = VirtIoSndAdapterContext_Lookup(UnknownAdapter, &forceNullBackend);
    miniport->Dx = dx;

    if (!forceNullBackend && dx != NULL) {
        status = VirtIoSndBackendVirtio_Create(dx, &miniport->Backend);
        if (NT_SUCCESS(status)) {
            VIRTIOSND_TRACE("wavert: using virtio backend\n");
            return STATUS_SUCCESS;
        }

        VIRTIOSND_TRACE_ERROR(
            "wavert: virtio backend create failed: 0x%08X (falling back to null)\n",
            (UINT)status);
    } else if (forceNullBackend) {
        VIRTIOSND_TRACE("wavert: ForceNullBackend=1; using null backend\n");
    } else {
        VIRTIOSND_TRACE_ERROR("wavert: adapter context lookup failed; using null backend\n");
    }

    status = VirtIoSndBackendNull_Create(&miniport->Backend);
    if (NT_SUCCESS(status)) {
        VIRTIOSND_TRACE("wavert: using null backend\n");
    }
    return status;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_GetDescription(
    _In_ IMiniportWaveRT *This,
    _Outptr_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor
    );

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_DataRangeIntersection(
    _In_ IMiniportWaveRT *This,
    _In_ ULONG PinId,
    _In_ PIRP Irp,
    _In_ PKSDATARANGE DataRange,
    _In_ PKSDATARANGE MatchingDataRange,
    _In_ ULONG OutputBufferLength,
    _Out_writes_bytes_to_(OutputBufferLength, *ResultantFormatLength) PVOID ResultantFormat,
    _Out_ PULONG ResultantFormatLength
    );

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_NewStream(
    _In_ IMiniportWaveRT *This,
    _Outptr_ PMINIPORTWAVERTSTREAM *OutStream,
    _In_opt_ PUNKNOWN OuterUnknown,
    _In_ POOL_TYPE PoolType,
    _In_ PPORTWAVERTSTREAM PortStream,
    _In_ ULONG Pin,
    _In_ BOOLEAN Capture,
    _In_ PKSDATAFORMAT DataFormat,
    _Out_opt_ PULONG StreamId
    );

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_GetDeviceDescription(
    _In_ IMiniportWaveRT *This,
    _Out_ PDEVICE_DESCRIPTION DeviceDescription
    );

static const KSDATARANGE_AUDIO g_VirtIoSndWaveRtDataRangePcm = {
    {
        sizeof(KSDATARANGE_AUDIO),
        0,
        0,
        0,
        {STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO)},
        {STATICGUIDOF(KSDATAFORMAT_SUBTYPE_PCM)},
        {STATICGUIDOF(KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)},
    },
    VIRTIOSND_CHANNELS,
    VIRTIOSND_BITS_PER_SAMPLE,
    VIRTIOSND_BITS_PER_SAMPLE,
    VIRTIOSND_SAMPLE_RATE,
    VIRTIOSND_SAMPLE_RATE,
};

static const PKSDATARANGE g_VirtIoSndWaveRtPinDataRanges[] = {
    (PKSDATARANGE)&g_VirtIoSndWaveRtDataRangePcm,
};

static const KSPIN_INTERFACE g_VirtIoSndWaveRtPinInterfaces[] = {
    {&KSINTERFACESETID_Standard, KSINTERFACE_STANDARD_STREAMING, 0},
};

static const KSPIN_MEDIUM g_VirtIoSndWaveRtPinMediums[] = {
    {&KSMEDIUMSETID_Standard, KSMEDIUM_TYPE_ANYINSTANCE, 0},
};

static const KSPIN_DESCRIPTOR g_VirtIoSndWaveRtKsPinDescriptorRender = {
    1,
    (PKSPIN_INTERFACE)g_VirtIoSndWaveRtPinInterfaces,
    1,
    (PKSPIN_MEDIUM)g_VirtIoSndWaveRtPinMediums,
    RTL_NUMBER_OF(g_VirtIoSndWaveRtPinDataRanges),
    (PKSDATARANGE *)g_VirtIoSndWaveRtPinDataRanges,
    KSPIN_DATAFLOW_IN,
    KSPIN_COMMUNICATION_SINK,
    &KSNODETYPE_SPEAKER,
    &KSPINNAME_SPEAKER,
};

static const KSPIN_DESCRIPTOR g_VirtIoSndWaveRtKsPinDescriptorBridge = {
    0,
    NULL,
    0,
    NULL,
    0,
    NULL,
    KSPIN_DATAFLOW_OUT,
    KSPIN_COMMUNICATION_BRIDGE,
    &KSNODETYPE_WAVE_OUT,
    &KSPINNAME_WAVE_OUT,
};

static const PCPIN_DESCRIPTOR g_VirtIoSndWaveRtPins[] = {
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorRender},
    {1, 1, 0, NULL, g_VirtIoSndWaveRtKsPinDescriptorBridge},
};

static const PCCONNECTION_DESCRIPTOR g_VirtIoSndWaveRtConnections[] = {
    {KSFILTER_NODE, VIRTIOSND_WAVE_PIN_RENDER, KSFILTER_NODE, VIRTIOSND_WAVE_PIN_BRIDGE},
};

static const GUID* g_VirtIoSndWaveRtCategories[] = {
    &KSCATEGORY_AUDIO,
    &KSCATEGORY_RENDER,
    &KSCATEGORY_REALTIME,
};

static const PCFILTER_DESCRIPTOR g_VirtIoSndWaveRtFilterDescriptor = {
    1,
    NULL,
    sizeof(PCPIN_DESCRIPTOR),
    RTL_NUMBER_OF(g_VirtIoSndWaveRtPins),
    g_VirtIoSndWaveRtPins,
    0,
    0,
    NULL,
    sizeof(PCCONNECTION_DESCRIPTOR),
    RTL_NUMBER_OF(g_VirtIoSndWaveRtConnections),
    g_VirtIoSndWaveRtConnections,
    RTL_NUMBER_OF(g_VirtIoSndWaveRtCategories),
    g_VirtIoSndWaveRtCategories,
};

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_GetDescription(
    _In_ IMiniportWaveRT *This,
    _Outptr_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor
    )
{
    UNREFERENCED_PARAMETER(This);
    if (OutFilterDescriptor == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *OutFilterDescriptor = (PPCFILTER_DESCRIPTOR)&g_VirtIoSndWaveRtFilterDescriptor;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_DataRangeIntersection(
    _In_ IMiniportWaveRT *This,
    _In_ ULONG PinId,
    _In_ PIRP Irp,
    _In_ PKSDATARANGE DataRange,
    _In_ PKSDATARANGE MatchingDataRange,
    _In_ ULONG OutputBufferLength,
    _Out_writes_bytes_to_(OutputBufferLength, *ResultantFormatLength) PVOID ResultantFormat,
    _Out_ PULONG ResultantFormatLength
    )
{
    KSDATAFORMAT_WAVEFORMATEXTENSIBLE format;
    KSDATARANGE_AUDIO *requested;

    UNREFERENCED_PARAMETER(This);
    UNREFERENCED_PARAMETER(Irp);
    UNREFERENCED_PARAMETER(MatchingDataRange);

    if (DataRange == NULL || ResultantFormatLength == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PinId != VIRTIOSND_WAVE_PIN_RENDER) {
        return STATUS_NO_MATCH;
    }

    if (DataRange->FormatSize < sizeof(KSDATARANGE_AUDIO)) {
        return STATUS_NO_MATCH;
    }

    if (!IsEqualGUID(&DataRange->MajorFormat, &KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUID(&DataRange->SubFormat, &KSDATAFORMAT_SUBTYPE_PCM) ||
        !IsEqualGUID(&DataRange->Specifier, &KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) {
        return STATUS_NO_MATCH;
    }

    requested = (KSDATARANGE_AUDIO *)DataRange;
    if (requested->MaximumChannels < VIRTIOSND_CHANNELS ||
        requested->MinimumBitsPerSample > VIRTIOSND_BITS_PER_SAMPLE ||
        requested->MaximumBitsPerSample < VIRTIOSND_BITS_PER_SAMPLE ||
        requested->MinimumSampleFrequency > VIRTIOSND_SAMPLE_RATE ||
        requested->MaximumSampleFrequency < VIRTIOSND_SAMPLE_RATE) {
        return STATUS_NO_MATCH;
    }

    RtlZeroMemory(&format, sizeof(format));

    format.DataFormat.FormatSize = sizeof(format);
    format.DataFormat.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
    format.DataFormat.SubFormat = KSDATAFORMAT_SUBTYPE_PCM;
    format.DataFormat.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
    format.DataFormat.SampleSize = VIRTIOSND_BLOCK_ALIGN;

    format.WaveFormatExt.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    format.WaveFormatExt.Format.nChannels = VIRTIOSND_CHANNELS;
    format.WaveFormatExt.Format.nSamplesPerSec = VIRTIOSND_SAMPLE_RATE;
    format.WaveFormatExt.Format.nAvgBytesPerSec = VIRTIOSND_AVG_BYTES_PER_SEC;
    format.WaveFormatExt.Format.nBlockAlign = VIRTIOSND_BLOCK_ALIGN;
    format.WaveFormatExt.Format.wBitsPerSample = VIRTIOSND_BITS_PER_SAMPLE;
    format.WaveFormatExt.Format.cbSize = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);

    format.WaveFormatExt.Samples.wValidBitsPerSample = VIRTIOSND_BITS_PER_SAMPLE;
    format.WaveFormatExt.dwChannelMask = KSAUDIO_SPEAKER_STEREO;
    format.WaveFormatExt.SubFormat = KSDATAFORMAT_SUBTYPE_PCM;

    if (OutputBufferLength < sizeof(format) || ResultantFormat == NULL) {
        *ResultantFormatLength = sizeof(format);
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlCopyMemory(ResultantFormat, &format, sizeof(format));
    *ResultantFormatLength = sizeof(format);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_NewStream(
    _In_ IMiniportWaveRT *This,
    _Outptr_ PMINIPORTWAVERTSTREAM *OutStream,
    _In_opt_ PUNKNOWN OuterUnknown,
    _In_ POOL_TYPE PoolType,
    _In_ PPORTWAVERTSTREAM PortStream,
    _In_ ULONG Pin,
    _In_ BOOLEAN Capture,
    _In_ PKSDATAFORMAT DataFormat,
    _Out_opt_ PULONG StreamId
    )
{
    PVIRTIOSND_WAVERT_MINIPORT miniport = VirtIoSndWaveRtMiniportFromInterface(This);
    PVIRTIOSND_WAVERT_STREAM stream;
    KIRQL oldIrql;

    UNREFERENCED_PARAMETER(OuterUnknown);
    UNREFERENCED_PARAMETER(PoolType);
    UNREFERENCED_PARAMETER(PortStream);

    if (OutStream == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutStream = NULL;

    if (Capture || Pin != VIRTIOSND_WAVE_PIN_RENDER) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!VirtIoSndWaveRt_IsFormatSupported(DataFormat)) {
        return STATUS_NO_MATCH;
    }

    stream = (PVIRTIOSND_WAVERT_STREAM)ExAllocatePoolWithTag(NonPagedPool, sizeof(*stream), VIRTIOSND_POOL_TAG);
    if (stream == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(stream, sizeof(*stream));
    stream->Interface.lpVtbl = &g_VirtIoSndWaveRtStreamVtbl;
    stream->RefCount = 1;
    stream->Miniport = miniport;
    stream->State = KSSTATE_STOP;
    KeInitializeSpinLock(&stream->Lock);

    KeInitializeTimerEx(&stream->Timer, NotificationTimer);
    KeInitializeDpc(&stream->TimerDpc, VirtIoSndWaveRtDpcRoutine, stream);
    KeInitializeEvent(&stream->DpcIdleEvent, NotificationEvent, TRUE);

    stream->PeriodBytes = VIRTIOSND_PERIOD_BYTES;
    stream->PeriodMs = VIRTIOSND_PERIOD_BYTES / (VIRTIOSND_AVG_BYTES_PER_SEC / 1000);
    stream->Period100ns = (ULONGLONG)stream->PeriodMs * 10u * 1000u;
    {
        LARGE_INTEGER qpcFreq;
        (VOID)KeQueryPerformanceCounter(&qpcFreq);
        stream->QpcFrequency = (ULONGLONG)qpcFreq.QuadPart;
    }

    stream->PositionRegister = (KSAUDIO_POSITION *)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSAUDIO_POSITION), VIRTIOSND_POOL_TAG);
    stream->ClockRegister = (ULONGLONG *)ExAllocatePoolWithTag(NonPagedPool, sizeof(ULONGLONG), VIRTIOSND_POOL_TAG);
    if (stream->PositionRegister == NULL || stream->ClockRegister == NULL) {
        if (stream->PositionRegister != NULL) {
            ExFreePoolWithTag(stream->PositionRegister, VIRTIOSND_POOL_TAG);
        }
        if (stream->ClockRegister != NULL) {
            ExFreePoolWithTag(stream->ClockRegister, VIRTIOSND_POOL_TAG);
        }
        ExFreePoolWithTag(stream, VIRTIOSND_POOL_TAG);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(stream->PositionRegister, sizeof(*stream->PositionRegister));
    VirtIoSndWaveRtWriteClockRegister(stream, 0);

    KeAcquireSpinLock(&miniport->Lock, &oldIrql);
    if (miniport->Stream != NULL) {
        KeReleaseSpinLock(&miniport->Lock, oldIrql);
        ExFreePoolWithTag(stream->PositionRegister, VIRTIOSND_POOL_TAG);
        ExFreePoolWithTag(stream->ClockRegister, VIRTIOSND_POOL_TAG);
        ExFreePoolWithTag(stream, VIRTIOSND_POOL_TAG);
        return STATUS_DEVICE_BUSY;
    }
    miniport->Stream = stream;
    KeReleaseSpinLock(&miniport->Lock, oldIrql);

    (VOID)VirtIoSndWaveRtMiniport_AddRef(This);

    if (StreamId != NULL) {
        *StreamId = 0;
    }

    *OutStream = &stream->Interface;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtMiniport_GetDeviceDescription(
    _In_ IMiniportWaveRT *This,
    _Out_ PDEVICE_DESCRIPTION DeviceDescription
    )
{
    UNREFERENCED_PARAMETER(This);
    if (DeviceDescription == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(DeviceDescription, sizeof(*DeviceDescription));
    DeviceDescription->Version = DEVICE_DESCRIPTION_VERSION;
    DeviceDescription->DmaChannel = 0;
    DeviceDescription->InterfaceType = PCIBus;
    DeviceDescription->MaximumLength = 0xFFFFFFFF;
    return STATUS_SUCCESS;
}

static const IMiniportWaveRTVtbl g_VirtIoSndWaveRtMiniportVtbl = {
    VirtIoSndWaveRtMiniport_QueryInterface,
    VirtIoSndWaveRtMiniport_AddRef,
    VirtIoSndWaveRtMiniport_Release,
    VirtIoSndWaveRtMiniport_Init,
    VirtIoSndWaveRtMiniport_GetDescription,
    VirtIoSndWaveRtMiniport_DataRangeIntersection,
    VirtIoSndWaveRtMiniport_NewStream,
    VirtIoSndWaveRtMiniport_GetDeviceDescription,
};

// IMiniportWaveRTStream

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_QueryInterface(
    _In_ IMiniportWaveRTStream *This,
    _In_ REFIID Riid,
    _Outptr_ PVOID *Object
    )
{
    if (Object == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Object = NULL;

    if (IsEqualGUID(Riid, &IID_IUnknown) || IsEqualGUID(Riid, &IID_IMiniportWaveRTStream)) {
        *Object = This;
        (VOID)VirtIoSndWaveRtStream_AddRef(This);
        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtStream_AddRef(_In_ IMiniportWaveRTStream *This)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    return (ULONG)InterlockedIncrement(&stream->RefCount);
}

static ULONG STDMETHODCALLTYPE VirtIoSndWaveRtStream_Release(_In_ IMiniportWaveRTStream *This)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    LONG ref = InterlockedDecrement(&stream->RefCount);
    if (ref == 0) {
        KIRQL oldIrql;
        PKEVENT oldEvent;

        VirtIoSndWaveRtStopTimer(stream);

        oldEvent = NULL;
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        oldEvent = stream->NotificationEvent;
        stream->NotificationEvent = NULL;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        if (oldEvent != NULL) {
            ObDereferenceObject(oldEvent);
        }

        if (stream->Miniport != NULL && stream->Miniport->Backend != NULL) {
            (VOID)VirtIoSndBackend_Stop(stream->Miniport->Backend);
            (VOID)VirtIoSndBackend_Release(stream->Miniport->Backend);
        }

        if (stream->Miniport != NULL) {
            KeAcquireSpinLock(&stream->Miniport->Lock, &oldIrql);
            if (stream->Miniport->Stream == stream) {
                stream->Miniport->Stream = NULL;
            }
            KeReleaseSpinLock(&stream->Miniport->Lock, oldIrql);
        }

        if (stream->BufferMdl != NULL) {
            IoFreeMdl(stream->BufferMdl);
        }

        VirtIoSndFreeCommonBuffer((stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL, &stream->BufferDma);

        ExFreePoolWithTag(stream->PositionRegister, VIRTIOSND_POOL_TAG);
        ExFreePoolWithTag(stream->ClockRegister, VIRTIOSND_POOL_TAG);

        if (stream->Miniport != NULL) {
            (VOID)VirtIoSndWaveRtMiniport_Release(&stream->Miniport->Interface);
        }

        ExFreePoolWithTag(stream, VIRTIOSND_POOL_TAG);
        return 0;
    }
    return (ULONG)ref;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_SetFormat(_In_ IMiniportWaveRTStream *This, _In_ PKSDATAFORMAT DataFormat)
{
    UNREFERENCED_PARAMETER(This);
    if (!VirtIoSndWaveRt_IsFormatSupported(DataFormat)) {
        return STATUS_NO_MATCH;
    }
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_SetState(_In_ IMiniportWaveRTStream *This, _In_ KSSTATE State)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    KSSTATE oldState;
    LARGE_INTEGER nowQpc;
    LARGE_INTEGER qpcFreq;
    ULONGLONG nowQpcValue;
    PVIRTIOSND_BACKEND backend;
    ULONG bufferSize;
    ULONG periodBytes;
    UINT64 bufferDma;
    PVOID bufferVa;
    NTSTATUS status;

    if (State != KSSTATE_STOP && State != KSSTATE_ACQUIRE && State != KSSTATE_PAUSE && State != KSSTATE_RUN) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    nowQpc = KeQueryPerformanceCounter(&qpcFreq);
    nowQpcValue = (ULONGLONG)nowQpc.QuadPart;

    backend = (stream->Miniport != NULL) ? stream->Miniport->Backend : NULL;
    status = STATUS_SUCCESS;
    bufferSize = 0;
    periodBytes = 0;
    bufferDma = 0;
    bufferVa = NULL;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldState = stream->State;

    if (oldState == State) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return STATUS_SUCCESS;
    }

    /*
     * Maintain QPC-derived position state:
     *  - Leaving RUN: freeze at the transition time.
     *  - Entering RUN: start a new QPC segment anchored at the frozen linear frame count.
     *  - STOP: reset counters and registers.
     */
    if (oldState == KSSTATE_RUN && State != KSSTATE_RUN) {
        ULONGLONG deltaQpc;
        ULONGLONG elapsedFrames;
        ULONG ringBytes;

        deltaQpc = 0;
        if (nowQpcValue >= stream->StartQpc) {
            deltaQpc = nowQpcValue - stream->StartQpc;
        }

        elapsedFrames = 0;
        if (stream->QpcFrequency != 0) {
            elapsedFrames = (deltaQpc * (ULONGLONG)VIRTIOSND_SAMPLE_RATE) / stream->QpcFrequency;
        }

        stream->FrozenLinearFrames = stream->StartLinearFrames + elapsedFrames;
        stream->FrozenQpc = nowQpcValue;

        ringBytes = 0;
        if (stream->BufferSize != 0) {
            ringBytes = (ULONG)((stream->FrozenLinearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN) % (ULONGLONG)stream->BufferSize);
        }
        VirtIoSndWaveRtUpdateRegisters(stream, ringBytes, nowQpcValue);
    }

    if (oldState == KSSTATE_STOP && State == KSSTATE_ACQUIRE) {
        stream->FrozenLinearFrames = 0;
        stream->FrozenQpc = 0;
        stream->StartQpc = 0;
        stream->StartLinearFrames = 0;
        stream->SubmittedLinearPositionBytes = 0;
        stream->SubmittedRingPositionBytes = 0;
        stream->PacketCount = 0;

        if (stream->PositionRegister != NULL) {
            stream->PositionRegister->PlayOffset = 0;
            stream->PositionRegister->WriteOffset = 0;
        }
        VirtIoSndWaveRtWriteClockRegister(stream, 0);
    }

    bufferSize = stream->BufferSize;
    periodBytes = stream->PeriodBytes;
    bufferDma = stream->BufferDma.DmaAddr;
    bufferVa = stream->BufferDma.Va;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    /*
     * Map stream state transitions onto the minimal virtio-snd PCM control state
     * machine (stream 0).
     */

    /*
     * Timer transitions.
     *
     * Stop the timer first on any transition away from RUN so no DPC can race
     * with backend control operations (which are PASSIVE_LEVEL only).
     */
    if (oldState == KSSTATE_RUN && State != KSSTATE_RUN) {
        VirtIoSndWaveRtStopTimer(stream);
    } else if (State == KSSTATE_STOP || State == KSSTATE_ACQUIRE || State == KSSTATE_PAUSE) {
        VirtIoSndWaveRtStopTimer(stream);
    }

    /*
     * KSSTATE <-> virtio-snd PCM control mapping (render stream 0):
     *
     *  STOP -> ACQUIRE : SET_PARAMS + PREPARE
     *  ACQUIRE/PAUSE -> RUN : START
     *  RUN -> PAUSE : STOP
     *  PAUSE/ACQUIRE -> STOP : RELEASE
     *  RUN -> STOP : STOP + RELEASE
     */
    status = STATUS_SUCCESS;
    if (backend != NULL) {
        if (oldState == KSSTATE_STOP && State == KSSTATE_ACQUIRE) {
            if (bufferSize != 0 && periodBytes != 0) {
                (VOID)VirtIoSndBackend_SetParams(backend, bufferSize, periodBytes);
                (VOID)VirtIoSndBackend_Prepare(backend);
            }
        } else if ((oldState == KSSTATE_ACQUIRE || oldState == KSSTATE_PAUSE) && State == KSSTATE_RUN) {
            status = VirtIoSndBackend_Start(backend);
        } else if (oldState == KSSTATE_RUN && State == KSSTATE_PAUSE) {
            status = VirtIoSndBackend_Stop(backend);
        } else if (State == KSSTATE_STOP) {
            if (oldState == KSSTATE_RUN) {
                (VOID)VirtIoSndBackend_Stop(backend);
            }
            status = VirtIoSndBackend_Release(backend);
        } else if (oldState == KSSTATE_RUN && State == KSSTATE_ACQUIRE) {
            status = VirtIoSndBackend_Stop(backend);
        } else if (oldState == KSSTATE_STOP && State == KSSTATE_RUN) {
            if (bufferSize != 0 && periodBytes != 0) {
                (VOID)VirtIoSndBackend_SetParams(backend, bufferSize, periodBytes);
                (VOID)VirtIoSndBackend_Prepare(backend);
            }
            status = VirtIoSndBackend_Start(backend);
        }
    }

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (State == KSSTATE_RUN) {
        ULONGLONG playLinearBytes;
        ULONGLONG submittedLinearBytes;
        ULONG submittedRingBytes;
        ULONG leadPeriods;
        ULONGLONG leadBytes;
        ULONG submitBudget;
        ULONG startOffsetBytes;
        ULONGLONG startLinearFrames;

        if (bufferVa == NULL || bufferSize == 0 || periodBytes == 0 || periodBytes > bufferSize) {
            return STATUS_INVALID_DEVICE_STATE;
        }

        /*
         * Anchor the RUN segment at the current frozen position and capture the
         * submission pointer. This happens after the backend START transition so
         * our software clock matches when the device is allowed to render.
         */
        nowQpc = KeQueryPerformanceCounter(&qpcFreq);
        nowQpcValue = (ULONGLONG)nowQpc.QuadPart;

        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->QpcFrequency = (ULONGLONG)qpcFreq.QuadPart;
        stream->StartQpc = nowQpcValue;
        stream->StartLinearFrames = stream->FrozenLinearFrames;
        stream->State = KSSTATE_RUN;

        startLinearFrames = stream->StartLinearFrames;
        startOffsetBytes = 0;
        if (bufferSize != 0) {
            startOffsetBytes = (ULONG)((startLinearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN) % (ULONGLONG)bufferSize);
        }

        VirtIoSndWaveRtUpdateRegisters(stream, startOffsetBytes, nowQpcValue);

        stream->SubmittedLinearPositionBytes = startLinearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN;
        stream->SubmittedRingPositionBytes = startOffsetBytes;

        playLinearBytes = stream->SubmittedLinearPositionBytes;
        submittedLinearBytes = stream->SubmittedLinearPositionBytes;
        submittedRingBytes = stream->SubmittedRingPositionBytes;
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        /* Prime the device with a small lead of audio before the periodic timer starts. */
        if (backend != NULL) {
            leadPeriods = bufferSize / periodBytes;
            if (leadPeriods > 0) {
                leadPeriods -= 1;
            }
            if (leadPeriods == 0) {
                leadPeriods = 1;
            }
            if (leadPeriods > 3) {
                leadPeriods = 3;
            }

            leadBytes = (ULONGLONG)leadPeriods * (ULONGLONG)periodBytes;
            submitBudget = 8;

            while (submitBudget-- != 0) {
                ULONGLONG queuedBytes;
                ULONG remaining;
                ULONG first;
                ULONG second;
                NTSTATUS writeStatus;

                queuedBytes = submittedLinearBytes - playLinearBytes;
                if (queuedBytes >= leadBytes) {
                    break;
                }

                remaining = bufferSize - submittedRingBytes;
                first = (remaining < periodBytes) ? remaining : periodBytes;
                second = periodBytes - first;

                writeStatus = VirtIoSndBackend_WritePeriod(backend,
                                                           bufferDma + (UINT64)submittedRingBytes,
                                                           first,
                                                           (second != 0) ? bufferDma : 0,
                                                           second);
                if (!NT_SUCCESS(writeStatus)) {
                    break;
                }

                submittedRingBytes = (submittedRingBytes + periodBytes) % bufferSize;
                submittedLinearBytes += periodBytes;
            }
        }

        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->SubmittedLinearPositionBytes = submittedLinearBytes;
        stream->SubmittedRingPositionBytes = submittedRingBytes;
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        VirtIoSndWaveRtStartTimer(stream);
    } else if (State == KSSTATE_STOP) {
        PKEVENT oldEvent;

        oldEvent = NULL;
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->State = KSSTATE_STOP;
        stream->FrozenLinearFrames = 0;
        stream->FrozenQpc = 0;
        stream->StartQpc = 0;
        stream->StartLinearFrames = 0;
        stream->SubmittedLinearPositionBytes = 0;
        stream->SubmittedRingPositionBytes = 0;
        stream->PacketCount = 0;
        oldEvent = stream->NotificationEvent;
        stream->NotificationEvent = NULL;
        if (stream->PositionRegister != NULL) {
            stream->PositionRegister->PlayOffset = 0;
            stream->PositionRegister->WriteOffset = 0;
        }
        VirtIoSndWaveRtWriteClockRegister(stream, 0);
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        if (oldEvent != NULL) {
            ObDereferenceObject(oldEvent);
        }
    } else {
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->State = State;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetState(_In_ IMiniportWaveRTStream *This, _Out_ PKSSTATE State)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    if (State == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    *State = stream->State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetPosition(_In_ IMiniportWaveRTStream *This, _Out_ PULONG64 Position)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    LARGE_INTEGER qpc;
    ULONGLONG linearFrames;
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    qpc = KeQueryPerformanceCounter(NULL);
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    VirtIoSndWaveRtGetPositionSnapshot(stream, (ULONGLONG)qpc.QuadPart, &linearFrames, NULL, NULL);
    KeReleaseSpinLock(&stream->Lock, oldIrql);
    *Position = linearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE
VirtIoSndWaveRtStream_GetPresentationPosition(
    _In_ IMiniportWaveRTStream *This,
    _Out_ PKSAUDIO_PRESENTATION_POSITION Position
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    LARGE_INTEGER nowQpc;
    ULONGLONG qpcValue;
    ULONGLONG linearFrames;
    ULONGLONG qpcForPosition;
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    nowQpc = KeQueryPerformanceCounter(NULL);
    qpcValue = (ULONGLONG)nowQpc.QuadPart;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    VirtIoSndWaveRtGetPositionSnapshot(stream, qpcValue, &linearFrames, NULL, &qpcForPosition);
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    Position->u64PositionInFrames = linearFrames;
    Position->u64QPCPosition = qpcForPosition;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetCurrentPadding(_In_ IMiniportWaveRTStream *This, _Out_ PULONG PaddingFrames)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    LARGE_INTEGER nowQpc;
    ULONGLONG qpcValue;
    ULONGLONG qpcForPosition;
    ULONGLONG linearFrames;
    ULONG playBytes;
    ULONG64 play;
    ULONG64 write;
    ULONG64 diff;
    KIRQL oldIrql;
    ULONG bufferBytes;

    if (PaddingFrames == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (stream->PositionRegister == NULL || stream->BufferSize == 0) {
        *PaddingFrames = 0;
        return STATUS_SUCCESS;
    }

    nowQpc = KeQueryPerformanceCounter(NULL);
    qpcValue = (ULONGLONG)nowQpc.QuadPart;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    VirtIoSndWaveRtGetPositionSnapshot(stream, qpcValue, &linearFrames, &playBytes, &qpcForPosition);
    VirtIoSndWaveRtUpdateRegisters(stream, playBytes, qpcForPosition);
    write = stream->PositionRegister->WriteOffset;
    bufferBytes = stream->BufferSize;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (bufferBytes == 0) {
        *PaddingFrames = 0;
        return STATUS_SUCCESS;
    }

    play = playBytes;

    if (write >= play) {
        diff = write - play;
    } else {
        diff = (ULONG64)bufferBytes - play + write;
    }

    *PaddingFrames = (ULONG)(diff / VIRTIOSND_BLOCK_ALIGN);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_SetNotificationEvent(_In_ IMiniportWaveRTStream *This, _In_opt_ PKEVENT NotificationEvent)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    PKEVENT oldEvent;
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldEvent = stream->NotificationEvent;
    if (NotificationEvent != NULL) {
        ObReferenceObject(NotificationEvent);
    }
    stream->NotificationEvent = NotificationEvent;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (oldEvent != NULL) {
        ObDereferenceObject(oldEvent);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetPacketCount(_In_ IMiniportWaveRTStream *This, _Out_ PULONG PacketCount)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    if (PacketCount == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    *PacketCount = stream->PacketCount;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetPositionRegister(
    _In_ IMiniportWaveRTStream *This,
    _Out_ PKSRTAUDIO_HWREGISTER PositionRegister
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    struct {
        PVOID Register;
        ULONG RegisterSize;
    } tmp;
    SIZE_T copySize;

    UNREFERENCED_PARAMETER(This);

    if (PositionRegister == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    tmp.Register = stream->PositionRegister;
    tmp.RegisterSize = sizeof(KSAUDIO_POSITION);
    RtlZeroMemory(PositionRegister, sizeof(*PositionRegister));
    copySize = sizeof(tmp);
    if (copySize > sizeof(*PositionRegister)) {
        copySize = sizeof(*PositionRegister);
    }
    RtlCopyMemory(PositionRegister, &tmp, copySize);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetClockRegister(
    _In_ IMiniportWaveRTStream *This,
    _Out_ PKSRTAUDIO_HWREGISTER ClockRegister
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    struct {
        PVOID Register;
        ULONG RegisterSize;
    } tmp;
    SIZE_T copySize;

    UNREFERENCED_PARAMETER(This);

    if (ClockRegister == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    tmp.Register = stream->ClockRegister;
    tmp.RegisterSize = sizeof(ULONGLONG);
    RtlZeroMemory(ClockRegister, sizeof(*ClockRegister));
    copySize = sizeof(tmp);
    if (copySize > sizeof(*ClockRegister)) {
        copySize = sizeof(*ClockRegister);
    }
    RtlCopyMemory(ClockRegister, &tmp, copySize);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_AllocateBufferWithNotification(
    _In_ IMiniportWaveRTStream *This,
    _In_ ULONG RequestedBufferSize,
    _In_ ULONG RequestedNotificationCount,
    _Out_ PULONG ActualBufferSize,
    _Out_ PULONG ActualNotificationCount,
    _Outptr_ PMDL *BufferMdl,
    _Outptr_ PVOID *Buffer
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    NTSTATUS status;
    PVIRTIOSND_DMA_CONTEXT dmaCtx;
    const ULONG bytesPerMs = VIRTIOSND_AVG_BYTES_PER_SEC / 1000;
    ULONG notifications;
    ULONG periodBytes;
    ULONG size;
    VIRTIOSND_DMA_BUFFER dmaBuf;
    PMDL mdl;
    PMDL oldMdl;
    VIRTIOSND_DMA_BUFFER oldDma;
    KIRQL oldIrql;
    KSSTATE state;

    if (ActualBufferSize == NULL || ActualNotificationCount == NULL || BufferMdl == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    state = stream->State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
    if (state == KSSTATE_RUN || InterlockedCompareExchange(&stream->DpcActive, 0, 0) != 0) {
        return STATUS_DEVICE_BUSY;
    }

    notifications = RequestedNotificationCount;
    if (notifications == 0) {
        notifications = 4;
    }
    if (notifications < 2) {
        notifications = 2;
    }
    if (notifications > 256) {
        notifications = 256;
    }

    size = RequestedBufferSize;
    if (size < bytesPerMs * notifications) {
        size = bytesPerMs * notifications;
    }

    periodBytes = (size + notifications - 1) / notifications;
    periodBytes = (periodBytes + (bytesPerMs - 1)) / bytesPerMs;
    periodBytes *= bytesPerMs;
    if (periodBytes < bytesPerMs) {
        periodBytes = bytesPerMs;
    }

    size = periodBytes * notifications;
    if ((size / notifications) != periodBytes) {
        return STATUS_INVALID_BUFFER_SIZE;
    }

    dmaCtx = (stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL;
    RtlZeroMemory(&dmaBuf, sizeof(dmaBuf));
    status = VirtIoSndAllocCommonBuffer(dmaCtx, size, FALSE, &dmaBuf);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    RtlZeroMemory(dmaBuf.Va, size);

    mdl = IoAllocateMdl(dmaBuf.Va, size, FALSE, FALSE, NULL);
    if (mdl == NULL) {
        VirtIoSndFreeCommonBuffer(dmaCtx, &dmaBuf);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    MmBuildMdlForNonPagedPool(mdl);

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldMdl = stream->BufferMdl;
    oldDma = stream->BufferDma;

    stream->BufferMdl = mdl;
    stream->BufferDma = dmaBuf;
    stream->BufferSize = size;

    stream->PeriodBytes = periodBytes;
    stream->PeriodMs = periodBytes / bytesPerMs;
    stream->Period100ns = (ULONGLONG)stream->PeriodMs * 10u * 1000u;

    stream->FrozenLinearFrames = 0;
    stream->FrozenQpc = 0;
    stream->StartQpc = 0;
    stream->StartLinearFrames = 0;
    stream->SubmittedLinearPositionBytes = 0;
    stream->SubmittedRingPositionBytes = 0;
    stream->PacketCount = 0;

    if (stream->PositionRegister != NULL) {
        stream->PositionRegister->PlayOffset = 0;
        stream->PositionRegister->WriteOffset = 0;
    }
    VirtIoSndWaveRtWriteClockRegister(stream, 0);
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (oldMdl != NULL) {
        IoFreeMdl(oldMdl);
    }
    VirtIoSndFreeCommonBuffer(dmaCtx, &oldDma);

    if (stream->Miniport != NULL && stream->Miniport->Backend != NULL) {
        (VOID)VirtIoSndBackend_SetParams(stream->Miniport->Backend, size, periodBytes);
        if (state != KSSTATE_STOP) {
            (VOID)VirtIoSndBackend_Prepare(stream->Miniport->Backend);
        }
    }

    *ActualBufferSize = size;
    *ActualNotificationCount = notifications;
    *BufferMdl = mdl;
    *Buffer = dmaBuf.Va;
    return STATUS_SUCCESS;
}

static VOID STDMETHODCALLTYPE VirtIoSndWaveRtStream_FreeBufferWithNotification(
    _In_ IMiniportWaveRTStream *This,
    _In_ PMDL BufferMdl,
    _In_ PVOID Buffer
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    PVIRTIOSND_DMA_CONTEXT dmaCtx;
    PMDL oldMdl;
    VIRTIOSND_DMA_BUFFER oldDma;
    KIRQL oldIrql;

    VirtIoSndWaveRtStopTimer(stream);

    dmaCtx = (stream->Miniport && stream->Miniport->Dx) ? &stream->Miniport->Dx->DmaCtx : NULL;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldMdl = stream->BufferMdl;
    oldDma = stream->BufferDma;
    if (oldMdl == BufferMdl && oldDma.Va == Buffer) {
        stream->BufferMdl = NULL;
        RtlZeroMemory(&stream->BufferDma, sizeof(stream->BufferDma));
        stream->BufferSize = 0;
    } else {
        RtlZeroMemory(&oldDma, sizeof(oldDma));
    }
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (BufferMdl != NULL) {
        IoFreeMdl(BufferMdl);
    }

    VirtIoSndFreeCommonBuffer(dmaCtx, &oldDma);
}

static const IMiniportWaveRTStreamVtbl g_VirtIoSndWaveRtStreamVtbl = {
    VirtIoSndWaveRtStream_QueryInterface,
    VirtIoSndWaveRtStream_AddRef,
    VirtIoSndWaveRtStream_Release,
    VirtIoSndWaveRtStream_SetFormat,
    VirtIoSndWaveRtStream_SetState,
    VirtIoSndWaveRtStream_GetState,
    VirtIoSndWaveRtStream_GetPosition,
    VirtIoSndWaveRtStream_GetCurrentPadding,
    VirtIoSndWaveRtStream_GetPresentationPosition,
    VirtIoSndWaveRtStream_AllocateBufferWithNotification,
    VirtIoSndWaveRtStream_FreeBufferWithNotification,
    VirtIoSndWaveRtStream_GetPositionRegister,
    VirtIoSndWaveRtStream_GetClockRegister,
    VirtIoSndWaveRtStream_SetNotificationEvent,
    VirtIoSndWaveRtStream_GetPacketCount,
};

NTSTATUS
VirtIoSndMiniportWaveRT_Create(_Outptr_result_maybenull_ PUNKNOWN *OutUnknown)
{
    PVIRTIOSND_WAVERT_MINIPORT miniport;

    if (OutUnknown == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutUnknown = NULL;

    miniport = (PVIRTIOSND_WAVERT_MINIPORT)ExAllocatePoolWithTag(NonPagedPool, sizeof(*miniport), VIRTIOSND_POOL_TAG);
    if (miniport == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(miniport, sizeof(*miniport));
    miniport->Interface.lpVtbl = &g_VirtIoSndWaveRtMiniportVtbl;
    miniport->RefCount = 1;
    miniport->Dx = NULL;
    miniport->Backend = NULL;
    KeInitializeSpinLock(&miniport->Lock);

    *OutUnknown = (PUNKNOWN)&miniport->Interface;
    return STATUS_SUCCESS;
}
