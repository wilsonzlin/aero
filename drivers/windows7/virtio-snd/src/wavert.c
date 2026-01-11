#include <ntddk.h>

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

    PVOID Buffer;
    ULONG BufferSize;
    PMDL BufferMdl;

    KSAUDIO_POSITION *PositionRegister;
    ULONGLONG *ClockRegister;
    ULONG PacketCount;

    ULONG PeriodBytes;
    ULONGLONG LinearPositionBytes;
    ULONG RingPositionBytes;
    ULONGLONG LastQpc;
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
VirtIoSndWaveRtStopTimer(_Inout_ PVIRTIOSND_WAVERT_STREAM Stream)
{
    KIRQL oldIrql;
    BOOLEAN cancelled;

    KeAcquireSpinLock(&Stream->Lock, &oldIrql);
    Stream->Stopping = TRUE;
    KeResetEvent(&Stream->DpcIdleEvent);
    KeReleaseSpinLock(&Stream->Lock, oldIrql);

    cancelled = KeCancelTimer(&Stream->Timer);
    UNREFERENCED_PARAMETER(cancelled);
    KeRemoveQueueDpc(&Stream->TimerDpc);

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

    dueTime.QuadPart = -(LONGLONG)(10 * 1000 * 10); // 10ms relative (100ns units)
    KeSetTimerEx(&Stream->Timer, dueTime, 10, &Stream->TimerDpc);
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
        *Stream->ClockRegister = Qpc;
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
    ULONG startOffset;
    PVOID buffer;
    PKEVENT notifyEvent;
    PVIRTIOSND_BACKEND backend;
    LARGE_INTEGER qpc;
    ULONGLONG qpcValue;

    UNREFERENCED_PARAMETER(Dpc);
    UNREFERENCED_PARAMETER(SystemArgument1);
    UNREFERENCED_PARAMETER(SystemArgument2);

    if (stream == NULL) {
        return;
    }

    InterlockedIncrement(&stream->DpcActive);

    KeAcquireSpinLock(&stream->Lock, &oldIrql);

    if (stream->Stopping || stream->State != KSSTATE_RUN || stream->Buffer == NULL || stream->BufferSize == 0 || stream->PeriodBytes == 0 ||
        stream->PeriodBytes > stream->BufferSize) {
        KeReleaseSpinLock(&stream->Lock, oldIrql);
        goto Exit;
    }

    periodBytes = stream->PeriodBytes;
    bufferSize = stream->BufferSize;
    startOffset = stream->RingPositionBytes;
    buffer = stream->Buffer;
    notifyEvent = stream->NotificationEvent;
    backend = stream->Miniport->Backend;

    qpc = KeQueryPerformanceCounter(NULL);
    qpcValue = (ULONGLONG)qpc.QuadPart;

    stream->LastQpc = qpcValue;
    stream->LinearPositionBytes += periodBytes;
    stream->RingPositionBytes = (startOffset + periodBytes) % bufferSize;
    stream->PacketCount += 1;

    VirtIoSndWaveRtUpdateRegisters(stream, stream->RingPositionBytes, qpcValue);

    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (backend != NULL) {
        ULONG remaining = bufferSize - startOffset;
        ULONG first = (remaining < periodBytes) ? remaining : periodBytes;
        ULONG second = periodBytes - first;

        (VOID)VirtIoSndBackend_Write(backend, (const UCHAR *)buffer + startOffset, first);
        if (second != 0) {
            (VOID)VirtIoSndBackend_Write(backend, buffer, second);
        }
    }

    if (notifyEvent != NULL) {
        KeSetEvent(notifyEvent, IO_NO_INCREMENT, FALSE);
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
    NTSTATUS status;

    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);

    if (ServiceGroup != NULL) {
        *ServiceGroup = NULL;
    }

    if (miniport->Backend != NULL) {
        return STATUS_SUCCESS;
    }

    status = STATUS_NOT_SUPPORTED;
    if (miniport->Dx != NULL && miniport->Dx->Started && !miniport->Dx->Removed) {
        status = VirtIoSndBackendVirtio_Create(miniport->Dx, &miniport->Backend);
        if (NT_SUCCESS(status)) {
            return STATUS_SUCCESS;
        }
    }

    status = VirtIoSndBackendNull_Create(&miniport->Backend);
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

        VirtIoSndWaveRtStopTimer(stream);

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
    KIRQL oldIrql;
    KSSTATE oldState;

    if (State != KSSTATE_STOP && State != KSSTATE_ACQUIRE && State != KSSTATE_PAUSE && State != KSSTATE_RUN) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldState = stream->State;
    stream->State = State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (oldState == State) {
        return STATUS_SUCCESS;
    }

    switch (State) {
    case KSSTATE_ACQUIRE:
        VirtIoSndWaveRtStopTimer(stream);
        break;

    case KSSTATE_RUN:
        {
            LARGE_INTEGER qpc = KeQueryPerformanceCounter(NULL);
            KeAcquireSpinLock(&stream->Lock, &oldIrql);
            stream->LastQpc = (ULONGLONG)qpc.QuadPart;
            VirtIoSndWaveRtUpdateRegisters(stream, stream->RingPositionBytes, stream->LastQpc);
            KeReleaseSpinLock(&stream->Lock, oldIrql);
        }
        if (stream->Miniport->Backend != NULL) {
            (VOID)VirtIoSndBackend_Prepare(stream->Miniport->Backend);
            (VOID)VirtIoSndBackend_Start(stream->Miniport->Backend);
        }
        VirtIoSndWaveRtStartTimer(stream);
        break;

    case KSSTATE_PAUSE:
        VirtIoSndWaveRtStopTimer(stream);
        if (stream->Miniport->Backend != NULL) {
            (VOID)VirtIoSndBackend_Stop(stream->Miniport->Backend);
        }
        break;

    case KSSTATE_STOP:
        VirtIoSndWaveRtStopTimer(stream);

        KeAcquireSpinLock(&stream->Lock, &oldIrql);
        stream->LinearPositionBytes = 0;
        stream->RingPositionBytes = 0;
        stream->LastQpc = 0;
        stream->PacketCount = 0;
        if (stream->PositionRegister != NULL) {
            stream->PositionRegister->PlayOffset = 0;
            stream->PositionRegister->WriteOffset = 0;
        }
        if (stream->ClockRegister != NULL) {
            *stream->ClockRegister = 0;
        }
        KeReleaseSpinLock(&stream->Lock, oldIrql);

        if (stream->Miniport->Backend != NULL) {
            (VOID)VirtIoSndBackend_Stop(stream->Miniport->Backend);
            (VOID)VirtIoSndBackend_Release(stream->Miniport->Backend);
        }
        break;

    default:
        return STATUS_INVALID_PARAMETER;
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
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    *Position = stream->LinearPositionBytes;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
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
    ULONGLONG bytes;
    ULONGLONG qpc;
    if (Position == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    bytes = stream->LinearPositionBytes;
    qpc = stream->LastQpc;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    Position->u64PositionInFrames = bytes / VIRTIOSND_BLOCK_ALIGN;
    Position->u64QPCPosition = qpc;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_GetCurrentPadding(_In_ IMiniportWaveRTStream *This, _Out_ PULONG PaddingFrames)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    ULONG64 play;
    ULONG64 write;
    ULONG64 diff;

    if (PaddingFrames == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (stream->PositionRegister == NULL || stream->BufferSize == 0) {
        *PaddingFrames = 0;
        return STATUS_SUCCESS;
    }

    play = stream->PositionRegister->PlayOffset;
    write = stream->PositionRegister->WriteOffset;

    if (write >= play) {
        diff = write - play;
    } else {
        diff = (ULONG64)stream->BufferSize - play + write;
    }

    *PaddingFrames = (ULONG)(diff / VIRTIOSND_BLOCK_ALIGN);
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndWaveRtStream_SetNotificationEvent(_In_ IMiniportWaveRTStream *This, _In_opt_ PKEVENT NotificationEvent)
{
    PVIRTIOSND_WAVERT_STREAM stream = VirtIoSndWaveRtStreamFromInterface(This);
    KIRQL oldIrql;
    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    stream->NotificationEvent = NotificationEvent;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
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
    ULONG size;
    ULONG notifications;
    PMDL mdl;
    PVOID mem;
    PMDL oldMdl;
    PVOID oldBuffer;
    KIRQL oldIrql;
    KSSTATE state;

    UNREFERENCED_PARAMETER(RequestedNotificationCount);

    if (ActualBufferSize == NULL || ActualNotificationCount == NULL || BufferMdl == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    state = stream->State;
    KeReleaseSpinLock(&stream->Lock, oldIrql);
    if (state == KSSTATE_RUN || InterlockedCompareExchange(&stream->DpcActive, 0, 0) != 0) {
        return STATUS_DEVICE_BUSY;
    }

    if (RequestedBufferSize < VIRTIOSND_PERIOD_BYTES * 2) {
        size = VIRTIOSND_PERIOD_BYTES * 2;
    } else {
        size = RequestedBufferSize;
    }

    size = (size + (VIRTIOSND_PERIOD_BYTES - 1)) / VIRTIOSND_PERIOD_BYTES;
    size *= VIRTIOSND_PERIOD_BYTES;

    notifications = size / VIRTIOSND_PERIOD_BYTES;

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

    KeAcquireSpinLock(&stream->Lock, &oldIrql);
    oldMdl = stream->BufferMdl;
    oldBuffer = stream->Buffer;
    stream->Buffer = mem;
    stream->BufferSize = size;
    stream->BufferMdl = mdl;
    KeReleaseSpinLock(&stream->Lock, oldIrql);

    if (oldMdl != NULL) {
        IoFreeMdl(oldMdl);
    }
    if (oldBuffer != NULL) {
        ExFreePoolWithTag(oldBuffer, VIRTIOSND_POOL_TAG);
    }

    if (stream->Miniport->Backend != NULL) {
        (VOID)VirtIoSndBackend_SetParams(stream->Miniport->Backend, size, VIRTIOSND_PERIOD_BYTES);
    }

    *ActualBufferSize = size;
    *ActualNotificationCount = notifications;
    *BufferMdl = mdl;
    *Buffer = mem;
    return STATUS_SUCCESS;
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
        stream->BufferSize = 0;
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
VirtIoSndMiniportWaveRT_Create(_In_opt_ PVIRTIOSND_DEVICE_EXTENSION Dx, _Outptr_result_maybenull_ PUNKNOWN *OutUnknown)
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
    miniport->Dx = Dx;
    KeInitializeSpinLock(&miniport->Lock);

    *OutUnknown = (PUNKNOWN)&miniport->Interface;
    return STATUS_SUCCESS;
}
