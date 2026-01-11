/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "adapter_context.h"
#include "portcls_compat.h"
#include "trace.h"
#include "virtiosnd.h"
#include "wavert.h"

typedef struct _VIRTIOSND_WAVERT_STREAM VIRTIOSND_WAVERT_STREAM, *PVIRTIOSND_WAVERT_STREAM;

typedef struct _VIRTIOSND_WAVERT_MINIPORT {
    IMiniportWaveRT Interface;
    LONG RefCount;

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

    PVOID Buffer;
    ULONG BufferBytes;
    PMDL BufferMdl;

    KSAUDIO_POSITION *PositionRegister;
    ULONGLONG *ClockRegister;
    ULONG PacketCount;

    ULONG PeriodBytes;
    ULONG PeriodFrames;
    ULONG PeriodMs;

    ULONGLONG QpcFrequency;
    ULONGLONG StartQpc;
    ULONGLONG StartLinearFrames;
    ULONGLONG FrozenLinearFrames;
    ULONGLONG FrozenQpc;
    ULONGLONG LastReportedLinearFrames;

    ULONG TxOffsetBytes;

    BOOLEAN FatalError;
    NTSTATUS FatalNtStatus;
    ULONG FatalVirtioStatus;
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

static NTSTATUS
VirtIoSndWaveRtAllocateCyclicBuffer(
    _Inout_ PVIRTIOSND_WAVERT_STREAM Stream,
    _In_ ULONG RequestedBufferSize,
    _Out_ ULONG *ActualBufferSize,
    _Out_ ULONG *ActualNotificationCount,
    _Outptr_ PMDL *BufferMdl,
    _Outptr_ PVOID *Buffer)
{
    ULONG size;
    ULONG notifications;
    ULONG periodBytes;
    PVOID mem;
    PMDL mdl;
    PMDL oldMdl;
    PVOID oldBuffer;
    KIRQL oldIrql;

    if (Stream == NULL || ActualBufferSize == NULL || ActualNotificationCount == NULL || BufferMdl == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    periodBytes = Stream->PeriodBytes;
    if (periodBytes == 0) {
        periodBytes = VIRTIOSND_PERIOD_BYTES;
    }

    if (RequestedBufferSize < periodBytes * 2u) {
        size = periodBytes * 2u;
    } else {
        size = RequestedBufferSize;
    }

    size = (size + (periodBytes - 1u)) / periodBytes;
    size *= periodBytes;

    notifications = size / periodBytes;

    mem = ExAllocatePoolWithTag(NonPagedPool, size, VIRTIOSND_POOL_TAG);
    if (mem == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(mem, size);

    mdl = IoAllocateMdl(mem, size, FALSE, FALSE, NULL);
    if (mdl == NULL) {
        ExFreePoolWithTag(mem, VIRTIOSND_POOL_TAG);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    MmBuildMdlForNonPagedPool(mdl);

    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    oldMdl = Stream->BufferMdl;
    oldBuffer = Stream->Buffer;
    Stream->Buffer = mem;
    Stream->BufferBytes = size;
    Stream->BufferMdl = mdl;
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    if (oldMdl != NULL) {
        IoFreeMdl(oldMdl);
    }
    if (oldBuffer != NULL) {
        ExFreePoolWithTag(oldBuffer, VIRTIOSND_POOL_TAG);
    }

    *ActualBufferSize = size;
    *ActualNotificationCount = notifications;
    *BufferMdl = mdl;
    *Buffer = mem;
    return STATUS_SUCCESS;
}

static __forceinline ULONGLONG VirtIoSndWaveRtQueryQpc(VOID)
{
    LARGE_INTEGER qpc = KeQueryPerformanceCounter(NULL);
    return (ULONGLONG)qpc.QuadPart;
}

static __forceinline ULONG VirtIoSndWaveRtStateRank(_In_ KSSTATE State)
{
    switch (State) {
    case KSSTATE_STOP:
        return 0;
    case KSSTATE_ACQUIRE:
        return 1;
    case KSSTATE_PAUSE:
        return 2;
    case KSSTATE_RUN:
        return 3;
    default:
        return 0;
    }
}

static VOID VirtIoSndWaveRtResetClockLocked(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream)
{
    Stream->StartQpc = 0;
    Stream->StartLinearFrames = 0;
    Stream->FrozenLinearFrames = 0;
    Stream->FrozenQpc = 0;
    Stream->LastReportedLinearFrames = 0;

    Stream->TxOffsetBytes = 0;
    Stream->PacketCount = 0;

    Stream->FatalError = FALSE;
    Stream->FatalNtStatus = STATUS_SUCCESS;
    Stream->FatalVirtioStatus = 0;

    if (Stream->PositionRegister != NULL) {
        Stream->PositionRegister->PlayOffset = 0;
        Stream->PositionRegister->WriteOffset = 0;
    }
    if (Stream->ClockRegister != NULL) {
        *Stream->ClockRegister = 0;
    }
}

static ULONGLONG VirtIoSndWaveRtComputeLinearFramesLocked(
    _Inout_ PVIRTIOSND_WAVERT_STREAM Stream,
    _In_ ULONGLONG QpcNow,
    _Out_opt_ ULONGLONG *OutQpcForPosition)
{
    ULONGLONG frames;
    ULONGLONG qpcForPos;

    if (Stream->State != KSSTATE_RUN) {
        frames = Stream->FrozenLinearFrames;
        qpcForPos = Stream->FrozenQpc;
    } else {
        ULONGLONG deltaQpc;
        ULONGLONG scaled;
        ULONGLONG elapsedFrames;

        qpcForPos = QpcNow;

        deltaQpc = (QpcNow >= Stream->StartQpc) ? (QpcNow - Stream->StartQpc) : 0;
        scaled = deltaQpc * (ULONGLONG)VIRTIOSND_SAMPLE_RATE;
        elapsedFrames = (Stream->QpcFrequency != 0) ? (scaled / Stream->QpcFrequency) : 0;
        frames = Stream->StartLinearFrames + elapsedFrames;

        if (frames < Stream->LastReportedLinearFrames) {
            frames = Stream->LastReportedLinearFrames;
        }

        Stream->LastReportedLinearFrames = frames;
        Stream->FrozenLinearFrames = frames;
        Stream->FrozenQpc = QpcNow;
    }

    if (OutQpcForPosition != NULL) {
        *OutQpcForPosition = qpcForPos;
    }
    return frames;
}

static ULONG VirtIoSndWaveRtLinearFramesToRingBytes(_In_ PVIRTIOSND_WAVERT_STREAM Stream, _In_ ULONGLONG LinearFrames)
{
    ULONGLONG bytes;
    ULONGLONG mod;

    if (Stream->BufferBytes == 0) {
        return 0;
    }

    bytes = LinearFrames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN;
    mod = bytes % (ULONGLONG)Stream->BufferBytes;
    return (ULONG)mod;
}

static VOID VirtIoSndWaveRtUpdateRegistersLocked(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream, _In_ ULONGLONG QpcNow)
{
    ULONGLONG qpcForPos;
    ULONGLONG frames;
    ULONG ringBytes;

    frames = VirtIoSndWaveRtComputeLinearFramesLocked(Stream, QpcNow, &qpcForPos);
    ringBytes = VirtIoSndWaveRtLinearFramesToRingBytes(Stream, frames);

    if (Stream->PositionRegister != NULL) {
        Stream->PositionRegister->PlayOffset = ringBytes;
    }
    if (Stream->ClockRegister != NULL) {
        *Stream->ClockRegister = qpcForPos;
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

    KeResetEvent(&Stream->DpcIdleEvent);

    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    Stream->Stopping = FALSE;
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    dueTime.QuadPart = -(LONGLONG)((ULONGLONG)Stream->PeriodMs * 10ull * 1000ull); // relative (100ns)
    KeSetTimerEx(&Stream->Timer, dueTime, (LONG)Stream->PeriodMs, &Stream->TimerDpc);
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
    ULONG bufferBytes;
    ULONG startOffset;
    PVOID buffer;
    PKEVENT notifyEvent;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONGLONG qpcValue;
    ULONGLONG frames;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (stream == NULL) {
        return;
    }

    InterlockedIncrement(&stream->DpcActive);

    notifyEvent = NULL;
    qpcValue = VirtIoSndWaveRtQueryQpc();

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
    if (stream->Stopping || stream->State != KSSTATE_RUN || dx == NULL || dx->Removed || !dx->Started || stream->Buffer == NULL ||
        stream->BufferBytes == 0 || stream->PeriodBytes == 0 || stream->PeriodBytes > stream->BufferBytes) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        goto Exit;
    }

    periodBytes = stream->PeriodBytes;
    bufferBytes = stream->BufferBytes;
    buffer = stream->Buffer;
    notifyEvent = stream->NotificationEvent;

    if (notifyEvent != NULL) {
        ObReferenceObject(notifyEvent);
    }

    frames = VirtIoSndWaveRtComputeLinearFramesLocked(stream, qpcValue, NULL);
    startOffset = VirtIoSndWaveRtLinearFramesToRingBytes(stream, frames);

    stream->PacketCount += 1;

    if (stream->PositionRegister != NULL) {
        stream->PositionRegister->PlayOffset = startOffset;
    }
    if (stream->ClockRegister != NULL) {
        *stream->ClockRegister = qpcValue;
    }

    KeReleaseSpinLock(&stream->Lock, oldIrql);

    /*
     * Poll TX completions each period. The TX engine is initialized with
     * SuppressInterrupts=TRUE so virtio doesn't generate an interrupt storm for
     * immediate completions in Aero.
     */
    (VOID)VirtIoSndHwDrainTxCompletions(dx);

    if (dx->Tx.FatalError) {
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->FatalError = TRUE;
        stream->FatalNtStatus = STATUS_DEVICE_HARDWARE_ERROR;
        stream->FatalVirtioStatus = dx->Tx.LastVirtioStatus;
        stream->Stopping = TRUE;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        KeCancelTimer(&stream->Timer);
        KeRemoveQueueDpc(&stream->TimerDpc);
        goto NotifyAndExit;
    }

    {
        ULONG remaining = bufferBytes - startOffset;
        ULONG first = (remaining < periodBytes) ? remaining : periodBytes;
        ULONG second = periodBytes - first;

        status = VirtIoSndHwSubmitTx(
            dx,
            (const UCHAR *)buffer + startOffset,
            first,
            (second != 0) ? buffer : NULL,
            second,
            TRUE);

        if (status == STATUS_INSUFFICIENT_RESOURCES) {
            /*
             * TX queue backpressure: drop the period and continue. This prevents
             * deadlocks in the audio engine while keeping our software clock
             * moving.
             */
            (VOID)VirtIoSndHwDrainTxCompletions(dx);
        }
    }

NotifyAndExit:
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

    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);

    if (ServiceGroup != NULL) {
        *ServiceGroup = NULL;
    }

    if (miniport->Dx == NULL) {
        miniport->Dx = VirtIoSndAdapterContext_Lookup(UnknownAdapter);
        if (miniport->Dx == NULL) {
            VIRTIOSND_TRACE_ERROR("WaveRT miniport: adapter context lookup failed\n");
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }
    return STATUS_SUCCESS;
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
    stream->PeriodFrames = VIRTIOSND_PERIOD_BYTES / VIRTIOSND_BLOCK_ALIGN;
    stream->PeriodMs = (stream->PeriodFrames * 1000u) / VIRTIOSND_SAMPLE_RATE;
    if (stream->PeriodMs == 0) {
        stream->PeriodMs = 1;
    }

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
    *stream->ClockRegister = 0;

    stream->FatalError = FALSE;
    stream->FatalNtStatus = STATUS_SUCCESS;
    stream->FatalVirtioStatus = 0;

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
        KSSTATE state;
        PVIRTIOSND_DEVICE_EXTENSION dx;
        PKEVENT oldEvent;

        VirtIoSndWaveRtStopTimer(stream);

        dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        state = stream->State;
        oldEvent = stream->NotificationEvent;
        stream->NotificationEvent = NULL;
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        if (oldEvent != NULL) {
            ObDereferenceObject(oldEvent);
        }

        if (KeGetCurrentIrql() == PASSIVE_LEVEL && dx != NULL && dx->Started && !dx->Removed) {
            if (state == KSSTATE_RUN) {
                (VOID)VirtIoSndHwDrainTxCompletions(dx);
                (VOID)VirtioSndCtrlStop(&dx->Control);
            }
            if (state != KSSTATE_STOP) {
                (VOID)VirtioSndCtrlRelease(&dx->Control);
                VirtIoSndUninitTxEngine(dx);
            }
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

        if (stream->Buffer != NULL) {
            ExFreePoolWithTag(stream->Buffer, VIRTIOSND_POOL_TAG);
        }

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
    PVIRTIOSND_DEVICE_EXTENSION dx;
    KIRQL oldIrql;
    KSSTATE current;
    NTSTATUS status;
    ULONGLONG qpcNow;

    if (State != KSSTATE_STOP && State != KSSTATE_ACQUIRE && State != KSSTATE_PAUSE && State != KSSTATE_RUN) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    dx = (stream->Miniport != NULL) ? stream->Miniport->Dx : NULL;

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    current = stream->State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (current == State) {
        return STATUS_SUCCESS;
    }

    status = STATUS_SUCCESS;

    /*
     * Apply state transitions in steps (STOP < ACQUIRE < PAUSE < RUN) so we can
     * correctly map them onto the virtio-snd PCM control state machine.
     */
    while (VirtIoSndWaveRtStateRank(current) < VirtIoSndWaveRtStateRank(State)) {
        if (current == KSSTATE_STOP) {
            ULONG bufBytes;
            ULONG notifCount;
            ULONG periodBytes;

            if (dx == NULL || dx->Removed || !dx->Started) {
                return STATUS_INVALID_DEVICE_STATE;
            }

            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            if (stream->Buffer == NULL || stream->BufferBytes == 0) {
                KeReleaseSpinLock(&stream->Lock, oldIrql);
                /* Default to a conservative 100ms buffer (10 periods) if not allocated yet. */
                status = VirtIoSndWaveRtAllocateCyclicBuffer(
                    stream,
                    VIRTIOSND_PERIOD_BYTES * 10u,
                    &bufBytes,
                    &notifCount,
                    &stream->BufferMdl,
                    &stream->Buffer);
                if (!NT_SUCCESS(status)) {
                    return status;
                }
            } else {
                KeReleaseSpinLock(&stream->Lock, oldIrql);
            }

            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            bufBytes = stream->BufferBytes;
            periodBytes = stream->PeriodBytes;
            VirtIoSndWaveRtResetClockLocked(stream);
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            if (bufBytes == 0 || periodBytes == 0 || periodBytes > bufBytes) {
                return STATUS_INVALID_DEVICE_STATE;
            }

            status = VirtioSndCtrlSetParams(&dx->Control, bufBytes, periodBytes);
            if (!NT_SUCCESS(status)) {
                return status;
            }
            status = VirtioSndCtrlPrepare(&dx->Control);
            if (!NT_SUCCESS(status)) {
                (VOID)VirtioSndCtrlRelease(&dx->Control);
                return status;
            }
            status = VirtIoSndInitTxEngine(dx, periodBytes, 0, TRUE);
            if (!NT_SUCCESS(status)) {
                (VOID)VirtioSndCtrlRelease(&dx->Control);
                return status;
            }

            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            stream->State = KSSTATE_ACQUIRE;
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            current = KSSTATE_ACQUIRE;
            continue;
        }

        if (current == KSSTATE_ACQUIRE) {
            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            stream->State = KSSTATE_PAUSE;
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            current = KSSTATE_PAUSE;
            continue;
        }

        if (current == KSSTATE_PAUSE) {
            if (dx == NULL || dx->Removed || !dx->Started) {
                return STATUS_INVALID_DEVICE_STATE;
            }

            status = VirtioSndCtrlStart(&dx->Control);
            if (!NT_SUCCESS(status)) {
                return status;
            }

            qpcNow = VirtIoSndWaveRtQueryQpc();
            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            stream->StartQpc = qpcNow;
            stream->StartLinearFrames = stream->FrozenLinearFrames;
            stream->LastReportedLinearFrames = stream->FrozenLinearFrames;
            stream->FrozenQpc = qpcNow;
            VirtIoSndWaveRtUpdateRegistersLocked(stream, qpcNow);
            stream->State = KSSTATE_RUN;
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            VirtIoSndWaveRtStartTimer(stream);

            /*
             * Prime the host with the first period immediately on RUN entry so playback can
             * start without waiting a full notification period.
             *
             * This is not a WaveRT notification; the notification event is signaled only
             * from the periodic DPC path.
             */
            {
                PVOID primeBuffer;
                ULONG primeBufferBytes;
                ULONG primeOffset;
                ULONG primePeriodBytes;

                primeBuffer = NULL;
                primeBufferBytes = 0;
                primeOffset = 0;
                primePeriodBytes = stream->PeriodBytes;

                KeAcquireSpinLock(&stream->Lock, &oldIrql);
                if (stream->Buffer != NULL && stream->BufferBytes != 0) {
                    primeBuffer = stream->Buffer;
                    primeBufferBytes = stream->BufferBytes;
                    primeOffset = VirtIoSndWaveRtLinearFramesToRingBytes(stream, stream->StartLinearFrames);
                }
                KeReleaseSpinLock(&stream->Lock, oldIrql);

                if (primePeriodBytes != 0 && dx != NULL && dx->Started && !dx->Removed) {
                    (VOID)VirtIoSndHwDrainTxCompletions(dx);

                    if (primeBuffer != NULL && primeBufferBytes != 0 && primePeriodBytes <= primeBufferBytes) {
                        ULONG remaining;
                        ULONG first;
                        ULONG second;

                        remaining = primeBufferBytes - primeOffset;
                        first = (remaining < primePeriodBytes) ? remaining : primePeriodBytes;
                        second = primePeriodBytes - first;
                        (VOID)VirtIoSndHwSubmitTx(
                            dx,
                            (const UCHAR *)primeBuffer + primeOffset,
                            first,
                            (second != 0) ? primeBuffer : NULL,
                            second,
                            TRUE);
                    } else {
                        (VOID)VirtIoSndHwSubmitTx(dx, NULL, primePeriodBytes, NULL, 0, TRUE);
                    }
                }
            }

            current = KSSTATE_RUN;
            continue;
        }

        break;
    }

    while (VirtIoSndWaveRtStateRank(current) > VirtIoSndWaveRtStateRank(State)) {
        if (current == KSSTATE_RUN) {
            VirtIoSndWaveRtStopTimer(stream);

            qpcNow = VirtIoSndWaveRtQueryQpc();
            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            (VOID)VirtIoSndWaveRtComputeLinearFramesLocked(stream, qpcNow, NULL);
            VirtIoSndWaveRtUpdateRegistersLocked(stream, qpcNow);
            stream->State = KSSTATE_PAUSE;
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            if (dx != NULL && dx->Started && !dx->Removed) {
                (VOID)VirtIoSndHwDrainTxCompletions(dx);
                (VOID)VirtioSndCtrlStop(&dx->Control);
            }

            current = KSSTATE_PAUSE;
            continue;
        }

        if (current == KSSTATE_PAUSE) {
            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            stream->State = KSSTATE_ACQUIRE;
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            current = KSSTATE_ACQUIRE;
            continue;
        }

        if (current == KSSTATE_ACQUIRE) {
            PKEVENT oldEvent;

            VirtIoSndWaveRtStopTimer(stream);

            if (dx != NULL && dx->Started && !dx->Removed) {
                (VOID)VirtioSndCtrlRelease(&dx->Control);
                VirtIoSndUninitTxEngine(dx);
            }

            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            oldEvent = stream->NotificationEvent;
            stream->NotificationEvent = NULL;
            VirtIoSndWaveRtResetClockLocked(stream);
            stream->State = KSSTATE_STOP;
            KeReleaseSpinLock(&stream->Lock, oldIrql);

            if (oldEvent != NULL) {
                ObDereferenceObject(oldEvent);
            }

            current = KSSTATE_STOP;
            continue;
        }

        break;
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
    ULONGLONG qpcNow;
    ULONGLONG frames;
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    qpcNow = VirtIoSndWaveRtQueryQpc();

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    if (stream->FatalError) {
        NTSTATUS st = stream->FatalNtStatus;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return st;
    }
    VirtIoSndWaveRtUpdateRegistersLocked(stream, qpcNow);
    frames = VirtIoSndWaveRtComputeLinearFramesLocked(stream, qpcNow, NULL);
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    *Position = frames * (ULONGLONG)VIRTIOSND_BLOCK_ALIGN;
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
    ULONGLONG frames;
    ULONGLONG qpc;
    ULONGLONG qpcNow;
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    qpcNow = VirtIoSndWaveRtQueryQpc();

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    if (stream->FatalError) {
        NTSTATUS st = stream->FatalNtStatus;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return st;
    }
    frames = VirtIoSndWaveRtComputeLinearFramesLocked(stream, qpcNow, &qpc);
    VirtIoSndWaveRtUpdateRegistersLocked(stream, qpcNow);
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    Position->u64PositionInFrames = frames;
    Position->u64QPCPosition = qpc;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetCurrentPadding(_In_ IMiniportWaveRTStream *This, _Out_ PULONG PaddingFrames)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    ULONG64 play;
    ULONG64 write;
    ULONG64 diff;
    ULONG bufferBytes;
    ULONGLONG qpcNow;

    if (PaddingFrames == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    qpcNow = VirtIoSndWaveRtQueryQpc();

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    if (stream->FatalError) {
        NTSTATUS st = stream->FatalNtStatus;
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return st;
    }

    if (stream->PositionRegister == NULL || stream->BufferBytes == 0) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        *PaddingFrames = 0;
        return STATUS_SUCCESS;
    }

    VirtIoSndWaveRtUpdateRegistersLocked(stream, qpcNow);
    bufferBytes = stream->BufferBytes;
    play = stream->PositionRegister->PlayOffset % bufferBytes;
    write = stream->PositionRegister->WriteOffset % bufferBytes;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

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
    KIRQL oldIrql;
    KSSTATE state;

    UNREFERENCED_PARAMETER(RequestedNotificationCount);

    if (ActualBufferSize == NULL || ActualNotificationCount == NULL || BufferMdl == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    state = stream->State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (state != KSSTATE_STOP) {
        /* Once acquired, the cyclic buffer size must remain stable. */
        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        if (stream->Buffer != NULL && stream->BufferMdl != NULL) {
            *ActualBufferSize = stream->BufferBytes;
            *ActualNotificationCount = (stream->PeriodBytes != 0) ? (stream->BufferBytes / stream->PeriodBytes) : 0;
            *BufferMdl = stream->BufferMdl;
            *Buffer = stream->Buffer;
            KeReleaseSpinLock(&stream->Lock, oldIrql);
            return STATUS_SUCCESS;
        }
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        return STATUS_DEVICE_BUSY;
    }

    if (InterlockedCompareExchange(&stream->DpcActive, 0, 0) != 0) {
        return STATUS_DEVICE_BUSY;
    }

    return VirtIoSndWaveRtAllocateCyclicBuffer(
        stream, RequestedBufferSize, ActualBufferSize, ActualNotificationCount, BufferMdl, Buffer);
}

static VOID STDMETHODCALLTYPE VirtIoSndWaveRtStream_FreeBufferWithNotification(
    _In_ IMiniportWaveRTStream *This,
    _In_ PMDL BufferMdl,
    _In_ PVOID Buffer
    )
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;

    VirtIoSndWaveRtStopTimer(stream);

    if (BufferMdl != NULL) {
        IoFreeMdl(BufferMdl);
    }

    if (Buffer != NULL) {
        ExFreePoolWithTag(Buffer, VIRTIOSND_POOL_TAG);
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    if (stream->Buffer == Buffer) {
        stream->Buffer = NULL;
        stream->BufferBytes = 0;
    }
    if (stream->BufferMdl == BufferMdl) {
        stream->BufferMdl = NULL;
    }
    KeReleaseSpinLock(&stream->Lock, oldIrql);
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
    KeInitializeSpinLock(&miniport->Lock);

    *OutUnknown = (PUNKNOWN)&miniport->Interface;
    return STATUS_SUCCESS;
}
